//! Tier-2 test for the HTTP↔NATS bridge (spec Part 6): a real NATS server,
//! a real core with the API handlers subscribed, and the axum router driven
//! via tower — login → create → release → inbox → resolve → Done, plus the
//! §6.5 auth/error contract and SSE replay.

use api::{ApiState, SharedState};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use dispatcher::core::{Core, CoreConfig, spawn};
use dispatcher::handlers::{spawn_api_handlers, spawn_container_handlers};
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::{FakeBackend, FakeProvider, repo::TempRepo};
use tower::ServiceExt;
use types::{ProjectRole, User};

const MANUAL: &str = r#"
name: manual
work:
  type: human
  prompt: prompts/manual.md
eval:
  - name: approval
    type: human
    prompt: prompts/approve.md
"#;

async fn call(
    router: &axum::Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value, Option<String>) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, cookie);
    }
    let req = match body {
        Some(v) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&v).unwrap()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let set_cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or("").to_string());
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value, set_cookie)
}

fn gen_jwt_keys(dir: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    let private = dir.join("jwt_private.pem");
    let public = dir.join("jwt_public.pem");
    let run = |args: &[&str]| {
        assert!(
            std::process::Command::new("openssl")
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    run(&[
        "genpkey",
        "-algorithm",
        "RSA",
        "-pkeyopt",
        "rsa_keygen_bits:2048",
        "-out",
        private.to_str().unwrap(),
    ]);
    run(&[
        "pkey",
        "-in",
        private.to_str().unwrap(),
        "-pubout",
        "-out",
        public.to_str().unwrap(),
    ]);
    (
        std::fs::read(&private).unwrap(),
        std::fs::read(&public).unwrap(),
    )
}

#[tokio::test]
async fn http_bridge_end_to_end() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/manual.yaml", MANUAL.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/manual.md", b"do the thing", "p")
        .await;
    clone
        .commit_file("prompts/approve.md", b"check it", "p")
        .await;
    clone
        .commit_file("tags/rust.md", b"# rust\nrust conventions", "tag")
        .await;
    clone.push("main").await;

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(&repos_root),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    spawn_container_handlers(&store, handle.clone())
        .await
        .unwrap();
    spawn_api_handlers(
        &store,
        handle,
        Arc::new(vcs::RepoManager::new(&repos_root)),
        None,
        None,
        None,
        Arc::new(FakeBackend::new()),
    )
    .await
    .unwrap();

    // A Member user in the users bucket, exactly as `admin user create` writes.
    let users = store.raw_bucket(store::buckets::USERS).await.unwrap();
    let user = User {
        id: "op".into(),
        email: "op@example.com".into(),
        password_hash: auth::hash_password("hunter2").unwrap(),
        project_roles: [("acme/api".to_string(), ProjectRole::Member)].into(),
        platform_admin: false,
        created_at: chrono::Utc::now(),
    };
    users
        .put_json(&store::keys::user_key(&user.email), &user)
        .await
        .unwrap();
    // A platform admin for the project-creation path.
    let root = User {
        id: "root".into(),
        email: "root@example.com".into(),
        password_hash: auth::hash_password("s3cret").unwrap(),
        project_roles: Default::default(),
        platform_admin: true,
        created_at: chrono::Utc::now(),
    };
    users
        .put_json(&store::keys::user_key(&root.email), &root)
        .await
        .unwrap();

    let keys_dir = tempfile::tempdir().unwrap();
    let (private, public) = gen_jwt_keys(keys_dir.path());
    // The API decrypts artifacts with the artifacts identity (never the
    // secrets one, §10.2); seed one and an artifact to serve.
    let (artifacts_identity, _) = store::secrets::generate_age_keypair();
    let artifacts = store
        .artifacts(store::ArtifactCrypto::with_identity(&artifacts_identity).unwrap())
        .await
        .unwrap();
    artifacts
        .put(
            "acme",
            "api",
            1,
            1,
            store::ArtifactKind::SessionTranscript,
            br#"{"type":"user","message":"do it"}"#,
        )
        .await
        .unwrap();
    let state: SharedState = Arc::new(ApiState {
        store: store.clone(),
        signer: auth::jwt::JwtSigner::from_pem(&private).unwrap(),
        verifier: auth::jwt::JwtVerifier::from_pem(&public).unwrap(),
        session_ttl: chrono::Duration::hours(1),
        artifacts: Some(artifacts),
    });
    let router = api::router(state, None);

    // Unauthenticated → 401; bad credentials → 401.
    let (status, _, _) = call(&router, "GET", "/api/v1/projects/acme/api/jobs", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(
        &router,
        "POST",
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "op@example.com", "password": "wrong"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Login → cookie; /auth/me echoes the identity.
    let (status, me, cookie) = call(
        &router,
        "POST",
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "op@example.com", "password": "hunter2"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["project_roles"]["acme/api"], "member");
    let cookie = cookie.unwrap();
    let (status, me, _) = call(&router, "GET", "/auth/me", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["sub"], "op@example.com");

    // Bearer tokens (machine callers, §7.1): the same JWT via the
    // Authorization header — what `admin user token` mints.
    let bearer = auth::jwt::JwtSigner::from_pem(&private)
        .unwrap()
        .issue(
            &types::Identity {
                sub: "op@example.com".into(),
                kind: types::IdentityKind::User,
                project_roles: [("acme/api".to_string(), ProjectRole::Member)].into(),
                platform_admin: false,
            },
            chrono::Duration::hours(1),
        )
        .unwrap();
    let req = Request::builder()
        .method("GET")
        .uri("/auth/me")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Project creation: members get 403; a platform admin creates a project
    // whose repo arrives seeded with the Code starter template.
    let (status, _, _) = call(
        &router,
        "POST",
        "/api/v1/projects",
        Some(&cookie),
        Some(serde_json::json!({"owner": "acme", "name": "web"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, _, admin_cookie) = call(
        &router,
        "POST",
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "root@example.com", "password": "s3cret"})),
    )
    .await;
    let admin_cookie = admin_cookie.unwrap();
    let (status, created, _) = call(
        &router,
        "POST",
        "/api/v1/projects",
        Some(&admin_cookie),
        Some(serde_json::json!({"owner": "acme", "name": "web"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["project"], "acme/web");
    let (status, types, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/web/job-types",
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(types[0]["name"], "code", "{types}");
    assert_eq!(types[0]["display_name"], "Code");
    // The seeded pre-receive hook is present and executable.
    let hook = repos_root.join("acme/web.git/hooks/pre-receive");
    assert!(hook.is_file(), "hook installed");
    // Duplicate creation is a conflict.
    let (status, _, _) = call(
        &router,
        "POST",
        "/api/v1/projects",
        Some(&admin_cookie),
        Some(serde_json::json!({"owner": "acme", "name": "web"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Members / role management (§7.5, platform admins only). A non-admin is
    // refused before the request ever reaches the dispatcher.
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/members",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(
        &router,
        "PUT",
        "/api/v1/projects/acme/api/members/op@example.com",
        Some(&cookie),
        Some(serde_json::json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Admin grants op `owner` (→ Admin) on acme/api; the write lands on the
    // user record via the dispatcher (single writer of users.*).
    let (status, granted, _) = call(
        &router,
        "PUT",
        "/api/v1/projects/acme/api/members/op@example.com",
        Some(&admin_cookie),
        Some(serde_json::json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{granted}");
    assert_eq!(granted["role"], "admin");
    assert_eq!(
        users
            .get_json::<User>(&store::keys::user_key("op@example.com"))
            .await
            .unwrap()
            .unwrap()
            .project_roles
            .get("acme/api"),
        Some(&ProjectRole::Admin)
    );

    // List returns op with its role.
    let (status, list, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/members",
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["members"][0]["email"], "op@example.com");
    assert_eq!(list["members"][0]["role"], "admin");

    // A bad role is a 400; an unknown user is a 404.
    let (status, _, _) = call(
        &router,
        "PUT",
        "/api/v1/projects/acme/api/members/op@example.com",
        Some(&admin_cookie),
        Some(serde_json::json!({ "role": "superuser" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = call(
        &router,
        "PUT",
        "/api/v1/projects/acme/api/members/ghost@example.com",
        Some(&admin_cookie),
        Some(serde_json::json!({ "role": "member" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Remove clears the grant.
    let (status, _, _) = call(
        &router,
        "DELETE",
        "/api/v1/projects/acme/api/members/op@example.com",
        Some(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !users
            .get_json::<User>(&store::keys::user_key("op@example.com"))
            .await
            .unwrap()
            .unwrap()
            .project_roles
            .contains_key("acme/api")
    );

    // Create (201) — and a bad type is a 422 validation envelope at release.
    let (status, job, _) = call(
        &router,
        "POST",
        "/api/v1/projects/acme/api/jobs",
        Some(&cookie),
        Some(serde_json::json!({"type": "manual"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(job["id"], 1);
    assert_eq!(job["state"], "Frozen");
    let (status, bogus, _) = call(
        &router,
        "POST",
        "/api/v1/projects/acme/api/jobs",
        Some(&cookie),
        Some(serde_json::json!({"type": "no-such-type"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let bogus_seq = bogus["id"].as_u64().unwrap();
    let (status, body, _) = call(
        &router,
        "POST",
        &format!("/api/v1/projects/acme/api/jobs/{bogus_seq}/release"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["errors"].is_array(),
        "expected §6.5 errors envelope: {body}"
    );

    // Criteria: a job created with an extra evaluator reports the type's
    // list plus its own, source-annotated, resolved at default HEAD pre-Ready.
    let (status, extra, _) = call(
        &router,
        "POST",
        "/api/v1/projects/acme/api/jobs",
        Some(&cookie),
        Some(serde_json::json!({
            "type": "manual",
            "eval": [{ "name": "linkcheck", "type": "command", "run": "lychee docs/",
                       "image": "img:latest", "required": false }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let extra_seq = extra["id"].as_u64().unwrap();
    let (status, criteria, _) = call(
        &router,
        "GET",
        &format!("/api/v1/projects/acme/api/jobs/{extra_seq}/criteria"),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(criteria["wrap_up"], "merge");
    assert_eq!(
        criteria["errors"].as_array().unwrap().len(),
        0,
        "{criteria}"
    );
    let evs = criteria["evaluators"].as_array().unwrap();
    assert_eq!(evs.len(), 2, "{criteria}");
    assert_eq!(
        (evs[0]["name"].as_str(), evs[0]["source"].as_str()),
        (Some("approval"), Some("type"))
    );
    assert_eq!(
        (evs[1]["name"].as_str(), evs[1]["source"].as_str()),
        (Some("linkcheck"), Some("job"))
    );

    // Library: one job type in full — raw YAML plus the parsed view.
    let (status, jt, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/job-types/manual",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(jt["name"], "manual");
    assert!(jt["yaml"].as_str().unwrap().contains("type: human"), "{jt}");
    assert_eq!(jt["job_type"]["work"]["type"], "human");
    assert_eq!(jt["job_type"]["wrap_up"]["type"], "merge");
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/job-types/no-such",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Tag vocabulary: tags/*.md stems at default HEAD.
    let (status, tags, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/tags",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tags, serde_json::json!(["rust"]));

    // Release job 1 → human work task lands in the inbox.
    let (status, released, _) = call(
        &router,
        "POST",
        "/api/v1/projects/acme/api/jobs/1/release",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(released["state"], "Ready");

    let mut work_task = None;
    for _ in 0..100 {
        let (_, pending, _) = call(
            &router,
            "GET",
            "/api/v1/projects/acme/api/tasks/pending",
            Some(&cookie),
            None,
        )
        .await;
        if let Some(t) = pending.as_array().and_then(|a| a.first()) {
            work_task = Some(t.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let work_task = work_task.expect("human work task in inbox");
    assert_eq!(work_task["phase"], "Work");
    let task_id = work_task["id"].as_u64().unwrap();

    // Job list/get/graph/diff all serve while the job is in flight.
    let (status, jobs, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(jobs.as_array().unwrap().len(), 3);
    let (status, graph, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/graph",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graph.as_array().unwrap().len(), 3);
    let (status, diff, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/diff/1",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(diff["files"].is_array());
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/99",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Resolve work → approval eval task appears → resolve → Done.
    let (status, _, _) = call(
        &router,
        "POST",
        &format!("/api/v1/projects/acme/api/jobs/1/tasks/{task_id}/resolve"),
        Some(&cookie),
        Some(serde_json::json!({"kind": "Pass", "structured": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut eval_task = None;
    for _ in 0..100 {
        let (_, pending, _) = call(
            &router,
            "GET",
            "/api/v1/projects/acme/api/tasks/pending",
            Some(&cookie),
            None,
        )
        .await;
        if let Some(t) = pending
            .as_array()
            .and_then(|a| a.iter().find(|t| t["phase"] == "Evaluation"))
        {
            eval_task = Some(t.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let eval_task = eval_task.expect("human eval task in inbox");
    assert_eq!(eval_task["evaluator"], "approval");
    let (status, _, _) = call(
        &router,
        "POST",
        &format!(
            "/api/v1/projects/acme/api/jobs/1/tasks/{}/resolve",
            eval_task["id"].as_u64().unwrap()
        ),
        Some(&cookie),
        Some(serde_json::json!({"kind": "Pass", "structured": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let mut done = false;
    for _ in 0..100 {
        let (_, job, _) = call(
            &router,
            "GET",
            "/api/v1/projects/acme/api/jobs/1",
            Some(&cookie),
            None,
        )
        .await;
        if job["state"] == "Done" {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(done, "job 1 should reach Done through the HTTP surface");

    // Per-job task log has both cycles' tasks.
    let (status, tasks, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(tasks.as_array().unwrap().len() >= 2);

    // SSE replay (§6.4): connecting with no Last-Event-ID streams the trail
    // from the start; the first frame carries an id and the job-created event.
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/projects/acme/api/events")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    use futures::StreamExt;
    let mut body = res.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("first SSE frame within 5s")
        .unwrap()
        .unwrap();
    let frame = String::from_utf8_lossy(&first);
    assert!(
        frame.contains("id:"),
        "frame should carry the stream seq: {frame}"
    );
    assert!(
        frame.contains("job-created"),
        "replay starts at the beginning: {frame}"
    );

    // ── Artifacts: the transcript reaches the operator, decrypted ──────────
    //
    // Served as raw bytes off the object store rather than through a dispatcher
    // req/reply, which could not carry a multi-MB transcript (1MB max_payload).
    let (status, body, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/artifacts",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["artifacts"], serde_json::json!(["session.jsonl"]));

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/projects/acme/api/jobs/1/tasks/1/artifacts/session.jsonl")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // JSONL, not JSON: one object per line is not a valid document.
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/x-ndjson"
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(&bytes[..], br#"{"type":"user","message":"do it"}"#);

    // Absent artifacts and unknown kinds are 404s, not 500s — a human task has
    // no transcript, and the kind comes straight off the URL.
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/artifacts/stdout.log",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/artifacts/..%2Fsecrets",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Transcripts can contain anything the agent saw: they are behind the same
    // project read authz as everything else, not public.
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/artifacts/session.jsonl",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // ── Job attachments: an operator uploads a screenshot on the job ───────
    //
    // Raw bytes off the object store (a screenshot exceeds 1MB max_payload),
    // encrypted with the same identity as transcripts, behind project authz.
    let png = b"\x89PNG\r\n\x1a\nmobile UI screenshot bytes".to_vec();
    let put = |name: &str, ctype: &str, cookie: Option<&str>, bytes: Vec<u8>| {
        let mut req = Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/v1/projects/acme/api/jobs/1/attachments/{name}"
            ))
            .header(header::CONTENT_TYPE, ctype);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        router.clone().oneshot(req.body(Body::from(bytes)).unwrap())
    };

    // Upload requires auth (Member+): anonymous is rejected.
    let res = put("bug.png", "image/png", None, png.clone())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = put("bug.png", "image/png", Some(&cookie), png.clone())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // A traversal-shaped filename is rejected before it can escape the prefix.
    let res = put("..", "image/png", Some(&cookie), png.clone())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Listing reports the file with its content type and original size.
    let (status, body, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/attachments",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["attachments"],
        serde_json::json!([{
            "name": "bug.png",
            "content_type": "image/png",
            "size": png.len(),
        }])
    );

    // Download returns the exact bytes under the stored content type.
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/projects/acme/api/jobs/1/attachments/bug.png")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(&bytes[..], &png[..]);

    // Delete removes it; the listing goes empty and the file 404s.
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/projects/acme/api/jobs/1/attachments/bug.png")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/attachments/bug.png",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// §6.x health probe: `GET /api/v1/health` proves the *dispatcher*, not just
/// the api. With the dispatcher's `req.health` handler on the bus it answers
/// `200 {"dispatcher":"ok","version"}` as `application/json`; with no responder
/// it answers `503` — never a masquerading `200`. Unauthenticated by design.
#[tokio::test]
async fn health_endpoint() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let keys_dir = tempfile::tempdir().unwrap();
    let (private, public) = gen_jwt_keys(keys_dir.path());
    let mk_state = || -> SharedState {
        Arc::new(ApiState {
            store: store.clone(),
            signer: auth::jwt::JwtSigner::from_pem(&private).unwrap(),
            verifier: auth::jwt::JwtVerifier::from_pem(&public).unwrap(),
            session_ttl: chrono::Duration::hours(1),
            artifacts: None,
        })
    };

    // GET /api/v1/health → (status, content-type, json body). No cookie: the
    // probe is unauthenticated (it leaks only liveness + version, spec §6.x).
    async fn get_health(router: &axum::Router) -> (StatusCode, String, serde_json::Value) {
        let req = Request::builder()
            .method("GET")
            .uri("/api/v1/health")
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let ctype = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, ctype, serde_json::from_slice(&bytes).unwrap())
    }

    // No dispatcher on the bus yet → no responder → 503 (not a fake 200).
    let router = api::router(mk_state(), None);
    let (status, ctype, body) = get_health(&router).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(ctype.starts_with("application/json"), "ctype: {ctype}");
    assert_eq!(body["dispatcher"], "error", "{body}");

    // Bring the dispatcher up with its req.health handler → 200 + health JSON.
    let repos_root = tempfile::tempdir().unwrap();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root.path()),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    spawn_api_handlers(
        &store,
        handle,
        Arc::new(vcs::RepoManager::new(repos_root.path())),
        None,
        None,
        None,
        Arc::new(FakeBackend::new()),
    )
    .await
    .unwrap();

    let router = api::router(mk_state(), None);
    let (status, ctype, body) = get_health(&router).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Content-type MUST be JSON: a text/html body is exactly the SPA fallback
    // masquerade the deploy gate now rejects (spec §6.x, #77/#81).
    assert!(ctype.starts_with("application/json"), "ctype: {ctype}");
    assert_eq!(body["dispatcher"], "ok", "{body}");
    assert!(
        body["version"].as_str().is_some(),
        "version missing: {body}"
    );
}

/// Generate an ed25519 keypair with ssh-keygen; return its public-key line.
fn keygen(path: &std::path::Path, comment: &str) -> String {
    assert!(
        std::process::Command::new("ssh-keygen")
            .args([
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                comment,
                "-f",
                path.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
    std::fs::read_to_string(path.with_extension("pub")).unwrap()
}

/// §7.3 user SSH cert minting: `POST /auth/ssh-cert` signs the caller's public
/// key into a 24h cert whose principals are `{email},git` and whose forced
/// command embeds the caller's roles as read from their user record.
#[tokio::test]
async fn ssh_cert_minting() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // A CA keypair for the dispatcher's user-cert handler, and JWT keys + user.
    let keys_dir = tempfile::tempdir().unwrap();
    let ca = keys_dir.path().join("ssh_ca");
    keygen(&ca, "ca");
    let (private, public) = gen_jwt_keys(keys_dir.path());

    let repos_root = tempfile::tempdir().unwrap();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root.path()),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    spawn_api_handlers(
        &store,
        handle,
        Arc::new(vcs::RepoManager::new(repos_root.path())),
        None,
        None,
        Some(ca.clone()),
        Arc::new(FakeBackend::new()),
    )
    .await
    .unwrap();

    let users = store.raw_bucket(store::buckets::USERS).await.unwrap();
    let user = User {
        id: "op".into(),
        email: "op@example.com".into(),
        password_hash: auth::hash_password("hunter2").unwrap(),
        project_roles: [("acme/api".to_string(), ProjectRole::Member)].into(),
        platform_admin: false,
        created_at: chrono::Utc::now(),
    };
    users
        .put_json(&store::keys::user_key(&user.email), &user)
        .await
        .unwrap();

    let state: SharedState = Arc::new(ApiState {
        store: store.clone(),
        signer: auth::jwt::JwtSigner::from_pem(&private).unwrap(),
        verifier: auth::jwt::JwtVerifier::from_pem(&public).unwrap(),
        session_ttl: chrono::Duration::hours(1),
        artifacts: None,
    });
    let router = api::router(state, None);

    // The caller's SSH public key to be signed.
    let user_key = keys_dir.path().join("id_ed25519");
    let pubkey = keygen(&user_key, "op@example.com");

    // Unauthenticated → 401.
    let (status, _, _) = call(
        &router,
        "POST",
        "/auth/ssh-cert",
        None,
        Some(serde_json::json!({ "public_key": pubkey })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, _, cookie) = call(
        &router,
        "POST",
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "op@example.com", "password": "hunter2"})),
    )
    .await;
    let cookie = cookie.unwrap();

    // Junk public key → 422 (rejected before it reaches the CA).
    let (status, _, _) = call(
        &router,
        "POST",
        "/auth/ssh-cert",
        Some(&cookie),
        Some(serde_json::json!({ "public_key": "not-a-real-key" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Authed with a real key → 200 + a signed certificate.
    let (status, body, _) = call(
        &router,
        "POST",
        "/auth/ssh-cert",
        Some(&cookie),
        Some(serde_json::json!({ "public_key": pubkey })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cert = body["certificate"].as_str().unwrap();
    assert!(cert.starts_with("ssh-ed25519-cert-v01@openssh.com"));

    // Inspect the cert: principals `op@example.com` + `git`, roles from the
    // user record baked into the forced command, and a 24h validity window.
    let cert_path = keys_dir.path().join("cert.pub");
    std::fs::write(&cert_path, cert).unwrap();
    let out = std::process::Command::new("ssh-keygen")
        .args(["-L", "-f", cert_path.to_str().unwrap()])
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(listing.contains("op@example.com"), "{listing}");
    assert!(listing.contains("git"), "{listing}");
    assert!(listing.contains("--kind user"), "{listing}");
    // base64url(no-pad) of {"acme/api":"member"} — the user's roles at signing.
    assert!(
        listing.contains("--roles eyJhY21lL2FwaSI6Im1lbWJlciJ9"),
        "{listing}"
    );
    // Validity window is ~24h (spec §7.3). `ssh-keygen -V +Ns` sets valid-to to
    // now+N exactly but *backdates* valid-from to a minute boundary (rounded
    // down, plus a minute of clock-skew allowance), so the printed window runs
    // 24h + [60s, 120s). Assert a tolerant band around 24h rather than a single
    // wall-clock-sensitive value — a bare `== 24h` only ever held when the cert
    // happened to be signed at exactly :00 seconds, hence the time-of-day flake.
    let window = cert_validity_seconds(&listing);
    assert!(
        (24 * 3600..24 * 3600 + 180).contains(&window),
        "expected ~24h window, got {window}s: {listing}"
    );
}

/// Live task output over HTTP (§4.2): the `/output` endpoint tails a running
/// task's container through the dispatcher, enforces viewer auth, and falls
/// back to the harvested `stdout.log` artifact once the task has finished.
#[tokio::test]
async fn task_output_endpoint() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let repos_root = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeBackend::new());
    backend.put_logs(b"compiling chuggernaut v0.1.0\ncompiling store v0.1.0\n".to_vec());
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root.path()),
        backend.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    spawn_api_handlers(
        &store,
        handle,
        Arc::new(vcs::RepoManager::new(repos_root.path())),
        None,
        None,
        None,
        backend.clone(),
    )
    .await
    .unwrap();

    // A Member user, JWT keys, and an artifacts identity to serve stdout.log.
    let users = store.raw_bucket(store::buckets::USERS).await.unwrap();
    let user = User {
        id: "op".into(),
        email: "op@example.com".into(),
        password_hash: auth::hash_password("hunter2").unwrap(),
        project_roles: [("acme/api".to_string(), ProjectRole::Member)].into(),
        platform_admin: false,
        created_at: chrono::Utc::now(),
    };
    users
        .put_json(&store::keys::user_key(&user.email), &user)
        .await
        .unwrap();
    let (private, public) = gen_jwt_keys(repos_root.path());
    let (artifacts_identity, _) = store::secrets::generate_age_keypair();
    let artifacts = store
        .artifacts(store::ArtifactCrypto::with_identity(&artifacts_identity).unwrap())
        .await
        .unwrap();
    let state: SharedState = Arc::new(ApiState {
        store: store.clone(),
        signer: auth::jwt::JwtSigner::from_pem(&private).unwrap(),
        verifier: auth::jwt::JwtVerifier::from_pem(&public).unwrap(),
        session_ttl: chrono::Duration::hours(1),
        artifacts: Some(artifacts),
    });
    let router = api::router(state, None);

    let mk_task = |id: u64, container: Option<&str>, state: types::TaskState| types::Task {
        id,
        job_seq: 1,
        project: "acme/api".into(),
        phase: types::TaskPhase::Work,
        cycle: 1,
        kind: types::TaskKind::Command {
            run: "cargo build".into(),
        },
        state,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: container.map(String::from),
        rework_reason: None,
        infra_loss: false,

        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
    };
    let tasks = store.tasks().await.unwrap();
    tasks
        .put(&mk_task(1, Some("fake/c1"), types::TaskState::Running))
        .await
        .unwrap();

    // Unauthenticated → 401 (viewer auth, same as artifacts).
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/output",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Login for a session cookie.
    let (_, _, cookie) = call(
        &router,
        "POST",
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "op@example.com", "password": "hunter2"})),
    )
    .await;
    let cookie = cookie.unwrap();

    // Running task → live tail, running: true, a non-zero cursor.
    let (status, body, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/output?since=0",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["running"], true);
    assert!(
        body["data"].as_str().unwrap().contains("compiling store"),
        "live tail missing output: {body}"
    );
    let offset = body["offset"].as_u64().unwrap();
    assert!(offset > 0);

    // A task with no container yet → 404.
    tasks
        .put(&mk_task(2, None, types::TaskState::Running))
        .await
        .unwrap();
    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/2/output",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Finish task 1 and harvest its stdout.log: the endpoint keeps working,
    // now running: false, serving the artifact at the same byte offsets.
    tasks
        .put(&mk_task(1, Some("fake/c1"), types::TaskState::Done))
        .await
        .unwrap();
    store
        .artifacts(store::ArtifactCrypto::with_identity(&artifacts_identity).unwrap())
        .await
        .unwrap()
        .put(
            "acme",
            "api",
            1,
            1,
            store::ArtifactKind::Stdout,
            b"compiling chuggernaut v0.1.0\ncompiling store v0.1.0\nFinished\n",
        )
        .await
        .unwrap();
    let (status, body, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/output?since=0",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["running"], false);
    assert!(
        body["data"].as_str().unwrap().contains("Finished"),
        "post-exit fallback missing artifact content: {body}"
    );
}

/// Parse the `Valid: from <ts> to <ts>` line ssh-keygen -L prints and return
/// the window in seconds.
fn cert_validity_seconds(listing: &str) -> i64 {
    let line = listing
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("Valid:"))
        .expect("no Valid: line");
    let mut parts = line.split_whitespace();
    // "Valid:" "from" <from> "to" <to>
    let from = parts.nth(2).unwrap();
    let to = parts.nth(1).unwrap();
    let parse =
        |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").expect("timestamp");
    (parse(to) - parse(from)).num_seconds()
}

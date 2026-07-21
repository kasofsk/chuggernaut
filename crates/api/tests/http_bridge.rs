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
}

//! Tier-2 test for the HTTP↔NATS bridge (spec Part 6): a real NATS server,
//! a real core with the API handlers subscribed, and the axum router driven
//! via tower — login → create → release → inbox → resolve → Done, plus the
//! §6.5 auth/error contract and SSE replay.

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn http_bridge_end_to_end() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
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
    let hook = repos_root.join("acme/web.git/hooks/pre-receive");
    assert!(hook.is_file(), "hook installed");
    let (status, _, _) = call(
        &router,
        "POST",
        "/api/v1/projects",
        Some(&admin_cookie),
        Some(serde_json::json!({"owner": "acme", "name": "web"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

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
    assert_eq!(jt["path"], "jobs/manual.yaml");
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

    let (status, tags, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/tags",
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        tags,
        serde_json::json!([{ "name": "rust", "path": "tags/rust.md" }])
    );

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

    let signal = store
        .tasks()
        .await
        .unwrap()
        .watch_job("acme", "api", 1)
        .await
        .unwrap();
    let work_task = test_utils::wait::on_kv_default(
        signal,
        "a pending work task in the inbox (via HTTP)",
        || async {
            let (_, pending, _) = call(
                &router,
                "GET",
                "/api/v1/projects/acme/api/tasks/pending",
                Some(&cookie),
                None,
            )
            .await;
            pending.as_array().and_then(|a| a.first()).cloned()
        },
    )
    .await;
    assert_eq!(work_task["phase"], "Work");
    let task_id = work_task["id"].as_u64().unwrap();

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

    let (status, _, _) = call(
        &router,
        "POST",
        &format!("/api/v1/projects/acme/api/jobs/1/tasks/{task_id}/resolve"),
        Some(&cookie),
        Some(serde_json::json!({"kind": "Pass", "structured": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let signal = store
        .tasks()
        .await
        .unwrap()
        .watch_job("acme", "api", 1)
        .await
        .unwrap();
    let eval_task = test_utils::wait::on_kv_default(
        signal,
        "a pending Evaluation task in the inbox (via HTTP)",
        || async {
            let (_, pending, _) = call(
                &router,
                "GET",
                "/api/v1/projects/acme/api/tasks/pending",
                Some(&cookie),
                None,
            )
            .await;
            pending
                .as_array()
                .and_then(|a| a.iter().find(|t| t["phase"] == "Evaluation"))
                .cloned()
        },
    )
    .await;
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

    let signal = store
        .jobs()
        .await
        .unwrap()
        .watch("acme", "api", 1)
        .await
        .unwrap();
    test_utils::wait::on_kv_default(
        signal,
        "job 1 to reach Done through the HTTP surface",
        || async {
            let (_, job, _) = call(
                &router,
                "GET",
                "/api/v1/projects/acme/api/jobs/1",
                Some(&cookie),
                None,
            )
            .await;
            (job["state"] == "Done").then_some(())
        },
    )
    .await;

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

    use futures::StreamExt;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/projects/acme/api/jobs/1/events")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
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
        "a job feed replays from the beginning: {frame}"
    );

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/projects/acme/api/events")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body().into_data_stream();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), body.next())
            .await
            .is_err(),
        "a fresh project feed must not replay the trail"
    );

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/projects/acme/api/events")
        .header(header::COOKIE, &cookie)
        .header("last-event-id", "0")
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut body = res.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("resumed feed replays within 5s")
        .unwrap()
        .unwrap();
    let frame = String::from_utf8_lossy(&first);
    assert!(
        frame.contains("job-created"),
        "resuming from 0 replays the trail: {frame}"
    );

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
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/x-ndjson"
    );
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(&bytes[..], br#"{"type":"user","message":"do it"}"#);

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

/// Job attachments over HTTP (§1.6), split from the mega-test (#196) so a
/// store-level hang localizes here instead of poisoning the whole bridge run.
/// Router-only rig: attachments need auth + artifact storage, no dispatcher.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn job_attachments_over_http() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

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

    let keys_dir = tempfile::tempdir().unwrap();
    let (private, public) = gen_jwt_keys(keys_dir.path());
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

    let (status, _, cookie) = call(
        &router,
        "POST",
        "/auth/login",
        None,
        Some(serde_json::json!({"email": "op@example.com", "password": "hunter2"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = cookie.expect("session cookie");

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

    let res = put("bug.png", "image/png", None, png.clone())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = put("bug.png", "image/png", Some(&cookie), png.clone())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let res = put("..", "image/png", Some(&cookie), png.clone())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

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
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn health_endpoint() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
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

    let router = api::router(mk_state(), None);
    let (status, ctype, body) = get_health(&router).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(ctype.starts_with("application/json"), "ctype: {ctype}");
    assert_eq!(body["dispatcher"], "error", "{body}");

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
        Arc::new(FakeBackend::new()),
    )
    .await
    .unwrap();

    let router = api::router(mk_state(), None);
    let (status, ctype, body) = get_health(&router).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(ctype.starts_with("application/json"), "ctype: {ctype}");
    assert_eq!(body["dispatcher"], "ok", "{body}");
    assert!(
        body["version"].as_str().is_some(),
        "version missing: {body}"
    );
}

/// One fleet roster entry. `endpoint` is what separates the two transports: a
/// `worker` node's capacity is operator-changeable, a docker-endpoint node's is
/// `DOCKER_NODES` config the dispatcher refuses to edit.
fn capacity_roster_node(name: &str, endpoint: &str, slots: u32) -> types::WorkerNode {
    types::WorkerNode {
        name: name.into(),
        endpoint: endpoint.into(),
        slots,
        available: true,
        version: None,
        refresh_outcome: None,
        capacity_source: None,
        capacity_observed_at: None,
    }
}

/// Bring up a core holding `roster`, with the api-facing subjects subscribed.
/// Returns the repo root, whose `TempDir` must outlive the core.
async fn capacity_spawn_core(
    server: &test_utils::nats::NatsTestServer,
    store: &NatsStore,
    backend: Arc<FakeBackend>,
    roster: Vec<types::WorkerNode>,
) -> tempfile::TempDir {
    let repos_root = tempfile::tempdir().unwrap();
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
    .unwrap()
    .with_fleet_roster(roster);
    spawn_api_handlers(
        store,
        spawn(core),
        Arc::new(vcs::RepoManager::new(repos_root.path())),
        None,
        None,
        backend,
    )
    .await
    .unwrap();
    repos_root
}

/// Seed a user record exactly as `admin user create` writes one.
async fn seed_user(store: &NatsStore, email: &str, password: &str, platform_admin: bool) {
    let user = User {
        id: email.into(),
        email: email.into(),
        password_hash: auth::hash_password(password).unwrap(),
        project_roles: Default::default(),
        platform_admin,
        created_at: chrono::Utc::now(),
    };
    store
        .raw_bucket(store::buckets::USERS)
        .await
        .unwrap()
        .put_json(&store::keys::user_key(&user.email), &user)
        .await
        .unwrap();
}

/// A router over `store` with no artifact storage — every platform route is a
/// KV read or a forward, so none of them needs one.
fn platform_router(store: &NatsStore, keys: &(Vec<u8>, Vec<u8>)) -> axum::Router {
    let state: SharedState = Arc::new(ApiState {
        store: store.clone(),
        signer: auth::jwt::JwtSigner::from_pem(&keys.0).unwrap(),
        verifier: auth::jwt::JwtVerifier::from_pem(&keys.1).unwrap(),
        session_ttl: chrono::Duration::hours(1),
        artifacts: None,
    });
    api::router(state, None)
}

/// Log in and return the session cookie.
async fn login(router: &axum::Router, email: &str, password: &str) -> String {
    let (status, body, cookie) = call(
        router,
        "POST",
        "/auth/login",
        None,
        Some(serde_json::json!({ "email": email, "password": password })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    cookie.expect("session cookie")
}

/// `PUT /api/v1/platform/fleet/{node}/capacity` with the given body value for
/// `slots` — a `Value` so the "not a slot count" case rides the same helper.
async fn put_capacity(
    router: &axum::Router,
    node: &str,
    slots: serde_json::Value,
    cookie: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let (status, body, _) = call(
        router,
        "PUT",
        &format!("/api/v1/platform/fleet/{node}/capacity"),
        cookie,
        Some(serde_json::json!({ "slots": slots })),
    )
    .await;
    (status, body)
}

/// `PUT /api/v1/platform/fleet/{node}/capacity` (spec §3.1/§6.1, design #293 §3):
/// the operator's desired slot count for one worker node, gated to platform
/// admins and answered **202** — the dispatcher records intent and *starts* the
/// push without waiting on the node RPC, so a synchronous 200 would be a lie.
/// The audit stamp is the authenticated identity, never anything a browser could
/// have put in the body.
#[tokio::test]
async fn platform_fleet_capacity_is_accepted_for_a_platform_admin() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    let _repos = capacity_spawn_core(
        server,
        &store,
        backend.clone(),
        vec![capacity_roster_node("air", "worker", 2)],
    )
    .await;
    seed_user(&store, "root@example.com", "s3cret", true).await;
    seed_user(&store, "op@example.com", "hunter2", false).await;

    let keys_dir = tempfile::tempdir().unwrap();
    let router = platform_router(&store, &gen_jwt_keys(keys_dir.path()));

    let (status, _) = put_capacity(&router, "air", serde_json::json!(1), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let member = login(&router, "op@example.com", "hunter2").await;
    let (status, _) = put_capacity(&router, "air", serde_json::json!(1), Some(&member)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        backend.slot_commands().is_empty(),
        "a refused caller must never reach the node"
    );

    let admin = login(&router, "root@example.com", "s3cret").await;
    let (status, ack) = put_capacity(&router, "air", serde_json::json!(1), Some(&admin)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{ack}");
    assert_eq!(ack["node"], "air");
    assert_eq!(ack["desired"], 1);
    assert_eq!(ack["observed"], serde_json::Value::Null, "{ack}");
    assert_eq!(ack["state"], "pending", "{ack}");

    let record: types::FleetCapacity = store
        .raw_bucket(store::buckets::PLATFORM)
        .await
        .unwrap()
        .get_json("fleet.capacity")
        .await
        .unwrap()
        .expect("the ask is durable before the 202");
    let air = record.nodes.get("air").expect("air intent");
    assert_eq!(air.slots, 1);
    assert_eq!(air.set_by, "root@example.com");
}

/// The refusals, passed through verbatim (§6.5): a docker-endpoint node is a
/// **409** because `DOCKER_NODES` owns its capacity outright, an unknown node a
/// 404 — never a silent no-op, which is the failure class design #293 exists to
/// remove. A body that is not a slot count is a 400, deliberately not the 422
/// §6.1 reserves for a value the *node* refuses as above its maximum.
#[tokio::test]
async fn platform_fleet_capacity_refusals_reach_the_operator() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    let _repos = capacity_spawn_core(
        server,
        &store,
        backend.clone(),
        vec![
            capacity_roster_node("air", "worker", 2),
            capacity_roster_node("local", "unix:///var/run/docker.sock", 4),
        ],
    )
    .await;
    seed_user(&store, "root@example.com", "s3cret", true).await;

    let keys_dir = tempfile::tempdir().unwrap();
    let router = platform_router(&store, &gen_jwt_keys(keys_dir.path()));
    let admin = login(&router, "root@example.com", "s3cret").await;

    let (status, body) = put_capacity(&router, "local", serde_json::json!(1), Some(&admin)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("docker endpoint"),
        "the 409 must say why: {body}"
    );

    let (status, body) = put_capacity(&router, "ghost", serde_json::json!(1), Some(&admin)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) =
        put_capacity(&router, "air", serde_json::json!("lots"), Some(&admin)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    assert!(backend.slot_commands().is_empty());
}

/// `GET /api/v1/platform/fleet` passes the capacity fields through to the UI
/// (design #293 §8/§10). The route reads `fleet.status` straight out of the
/// platform KV bucket and does **not** round-trip the dispatcher, so this rig
/// needs no core: seed the snapshot as the dispatcher writes it, read it back.
///
/// What is asserted is that the api drops **nothing**. It re-serializes no typed
/// view of the record, so a field a newer dispatcher writes — `slots_max` and the
/// intent's `set_by`/`set_at` are the ones design #293's wire shape still owes —
/// reaches the operator the moment the dispatcher publishes it, instead of
/// disappearing silently into an api built against older types.
#[tokio::test]
async fn platform_fleet_snapshot_passes_capacity_fields_through() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let at = "2026-07-26T23:14:02Z";
    let snapshot = serde_json::json!({
        "nodes": [{
            "name": "air",
            "slots": 4,
            "occupied": 0,
            "available": true,
            "version": "0.1.0+air",
            "capacity_source": "node",
            "capacity_observed_at": at,
            "slots_max": 6,
            "slots_desired": 8,
            "capacity_state": "rejected",
            "capacity_note": "node max is 6",
            "capacity_set_by": "root@example.com",
            "capacity_set_at": at,
            "running": [],
        }],
        "queue_depth": 0,
    });
    store
        .raw_bucket(store::buckets::PLATFORM)
        .await
        .unwrap()
        .put_json("fleet.status", &snapshot)
        .await
        .unwrap();
    seed_user(&store, "root@example.com", "s3cret", true).await;

    let keys_dir = tempfile::tempdir().unwrap();
    let router = platform_router(&store, &gen_jwt_keys(keys_dir.path()));
    let admin = login(&router, "root@example.com", "s3cret").await;

    let (status, fleet, _) =
        call(&router, "GET", "/api/v1/platform/fleet", Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK, "{fleet}");
    assert_eq!(
        fleet, snapshot,
        "the fleet snapshot must reach the UI field for field"
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
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn ssh_cert_minting() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

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

    let user_key = keys_dir.path().join("id_ed25519");
    let pubkey = keygen(&user_key, "op@example.com");

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

    let (status, _, _) = call(
        &router,
        "POST",
        "/auth/ssh-cert",
        Some(&cookie),
        Some(serde_json::json!({ "public_key": "not-a-real-key" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

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
    assert!(
        listing.contains("--roles eyJhY21lL2FwaSI6Im1lbWJlciJ9"),
        "{listing}"
    );
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
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn task_output_endpoint() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
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
        backend.clone(),
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

    let (status, _, _) = call(
        &router,
        "GET",
        "/api/v1/projects/acme/api/jobs/1/tasks/1/output",
        None,
        None,
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
    let from = parts.nth(2).unwrap();
    let to = parts.nth(1).unwrap();
    let parse =
        |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").expect("timestamp");
    (parse(to) - parse(from)).num_seconds()
}

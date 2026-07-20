//! Tier-2 test for the read-only settings endpoints (`GET .../config` and
//! `GET /platform/config`). These handlers read KV directly — no dispatcher —
//! so the test seeds the buckets and drives the router without a Core. The
//! load-bearing guarantees: secret VALUES are never returned (only names),
//! origin credentials are surfaced as presence flags, and the platform view is
//! admin-gated.

use api::{ApiState, SharedState};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use std::sync::Arc;
use store::NatsStore;
use tower::ServiceExt;
use types::{
    DispatcherConfigSnapshot, Identity, IdentityKind, OriginLink, ProjectRecord, ProjectRole,
    WorkerNode,
};

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

async fn get(router: &axum::Router, path: &str, bearer: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn config_endpoints() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // Seed KV directly. Secret VALUES here are arbitrary — the api lists names
    // and never decrypts, so their content must never surface.
    let secrets = store.raw_bucket(store::buckets::SECRETS).await.unwrap();
    secrets
        .put_json("acme.api.STRIPE_KEY", &"sk_live_TOPSECRET")
        .await
        .unwrap();
    secrets
        .put_json("acme.api.CHUG_ORIGIN_DEPLOY_KEY", &"ssh-key-material")
        .await
        .unwrap();
    secrets
        .put_json("global.agents.ANTHROPIC_API_KEY", &"ANTHROPIC_TOPSECRET")
        .await
        .unwrap();

    let vars = store.raw_bucket(store::buckets::VARS).await.unwrap();
    vars.put_json("acme.api.RUST_LOG", &"debug").await.unwrap();

    let projects = store.raw_bucket(store::buckets::PROJECTS).await.unwrap();
    projects
        .put_json(
            "acme.api",
            &ProjectRecord {
                origin: Some(OriginLink {
                    url: "ssh://git@github.com/acme/api.git".into(),
                    main_branch: "main".into(),
                    github_repo: Some("acme/api".into()),
                }),
                release: None,
                release_counter: 0,
            },
        )
        .await
        .unwrap();

    let platform = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    platform
        .put_json(
            "dispatcher.config",
            &DispatcherConfigSnapshot {
                nodes: vec![WorkerNode {
                    name: "local".into(),
                    endpoint: "unix:///var/run/docker.sock".into(),
                    slots: 4,
                }],
                agent_provider_default: "claude".into(),
                agent_model_default: Some("claude-sonnet-5".into()),
                triage_image: Some("registry.acme.com/triage:latest".into()),
                repos_root: "/data/repos".into(),
                repo_url_base: "ssh://git@host:2222".into(),
                nats_url: "nats://localhost:4222".into(),
                nats_url_container: None,
                channel_binary: None,
                hook_bin: None,
                secrets_encryption: true,
            },
        )
        .await
        .unwrap();

    // JWT signer + router (no Core — these routes are pure KV reads).
    let keys_dir = tempfile::tempdir().unwrap();
    let (private, public) = gen_jwt_keys(keys_dir.path());
    let signer = auth::jwt::JwtSigner::from_pem(&private).unwrap();
    let state: SharedState = Arc::new(ApiState {
        store: store.clone(),
        signer: auth::jwt::JwtSigner::from_pem(&private).unwrap(),
        verifier: auth::jwt::JwtVerifier::from_pem(&public).unwrap(),
        session_ttl: chrono::Duration::hours(1),
        artifacts: None,
    });
    let router = api::router(state, None);

    let token = |sub: &str, roles: &[(&str, ProjectRole)], admin: bool| {
        signer
            .issue(
                &Identity {
                    sub: sub.into(),
                    kind: IdentityKind::User,
                    project_roles: roles.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                    platform_admin: admin,
                },
                chrono::Duration::hours(1),
            )
            .unwrap()
    };
    let member = token(
        "op@example.com",
        &[("acme/api", ProjectRole::Member)],
        false,
    );
    let admin = token("root@example.com", &[], true);

    // ── Project config (Viewer+) ────────────────────────────────────────────
    let (status, cfg) = get(&router, "/api/v1/projects/acme/api/config", &member).await;
    assert_eq!(status, StatusCode::OK, "{cfg}");

    // vars: name + value.
    assert_eq!(
        cfg["vars"],
        serde_json::json!([{ "name": "RUST_LOG", "value": "debug" }])
    );

    // secrets: NAMES only, and the origin credential is NOT among them.
    assert_eq!(cfg["secrets"], serde_json::json!(["STRIPE_KEY"]));
    // The value must appear nowhere in the response.
    assert!(
        !cfg.to_string().contains("TOPSECRET"),
        "secret value leaked into config response: {cfg}"
    );

    // origin link + credential presence (deploy key set, PAT absent).
    assert_eq!(cfg["origin"]["url"], "ssh://git@github.com/acme/api.git");
    assert_eq!(cfg["origin_credentials"]["deploy_key"], true);
    assert_eq!(cfg["origin_credentials"]["pat"], false);

    // A user with no role on the project is refused.
    let stranger = token("nobody@example.com", &[], false);
    let (status, _) = get(&router, "/api/v1/projects/acme/api/config", &stranger).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // ── Platform config (admin only) ────────────────────────────────────────
    let (status, _) = get(&router, "/api/v1/platform/config", &member).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "members must not see platform config"
    );

    let (status, pc) = get(&router, "/api/v1/platform/config", &admin).await;
    assert_eq!(status, StatusCode::OK, "{pc}");
    assert_eq!(pc["dispatcher"]["nodes"][0]["name"], "local");
    assert_eq!(pc["dispatcher"]["agent_provider_default"], "claude");
    assert_eq!(
        pc["dispatcher"]["triage_image"],
        "registry.acme.com/triage:latest"
    );
    assert_eq!(
        pc["agent_secrets"],
        serde_json::json!(["ANTHROPIC_API_KEY"])
    );
    assert_eq!(pc["vapid_public"], false);
    assert!(
        !pc.to_string().contains("TOPSECRET"),
        "agent secret value leaked into platform config: {pc}"
    );
}

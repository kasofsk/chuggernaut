//! HTTP↔NATS bridge (spec Part 6, §10.4, Part 11, §13.2).
//!
//! A bridge, not a service: translate authenticated HTTP into NATS request-reply,
//! bridge streams to SSE, encrypt secrets on write, validate ingest tokens.
//! No orchestration logic. Never depends on the `dispatcher` crate.

pub mod ingest;
pub mod oidc;
pub mod push;
pub mod routes;
pub mod run;
pub mod sse;

use auth::jwt::{JwtSigner, JwtVerifier};
use std::path::PathBuf;
use std::sync::Arc;
use store::NatsStore;

pub struct ApiState {
    pub store: NatsStore,
    pub signer: JwtSigner,
    pub verifier: JwtVerifier,
    pub session_ttl: chrono::Duration,
    /// Reads transcripts and container logs for display. Holds the
    /// `age_artifacts` identity — deliberately a *different* key from the
    /// secrets one, which stays dispatcher-only (§10.2). None → the platform
    /// has no artifacts key and capture is off, so the routes 404.
    pub artifacts: Option<store::ArtifactStore>,
    /// The §6.7 issuer documents, over the mounted `oidc_public.pem` (§12.1).
    /// None → no issuer key is mounted, so the `.well-known` routes 404.
    pub oidc: Option<oidc::IssuerDocuments>,
}

pub type SharedState = Arc<ApiState>;

/// Build the axum router: auth + the §6.2 project surface implemented so far
/// plus §6.7's unauthenticated issuer documents, with the SPA (when `ui_dist`
/// is given) served as the fallback.
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
pub fn router(state: SharedState, ui_dist: Option<PathBuf>) -> axum::Router {
    use axum::routing::{get, post};

    let mut router = axum::Router::new()
        .route("/api/v1/health", get(routes::health))
        .route("/auth/login", post(routes::login))
        .route("/auth/logout", post(routes::logout))
        .route("/auth/me", get(routes::me))
        .route("/auth/ssh-cert", post(routes::ssh_cert))
        .route(
            "/api/v1/projects",
            get(routes::projects_list).post(routes::projects_create),
        )
        .route("/api/v1/projects/link", post(routes::projects_link))
        .route("/api/v1/platform/config", get(routes::platform_config_get))
        .route("/api/v1/platform/fleet", get(routes::platform_fleet_get))
        .route(
            "/api/v1/platform/fleet/{node}/capacity",
            axum::routing::put(routes::platform_fleet_capacity_set),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/config",
            get(routes::project_config_get),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/members",
            get(routes::members_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/members/{email}",
            axum::routing::put(routes::members_set).delete(routes::members_remove),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/origin",
            get(routes::origin_get),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/origin/release",
            post(routes::origin_release),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/origin/sync",
            post(routes::origin_sync),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/queue",
            get(routes::queue_get),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/groups",
            get(routes::groups_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/designs",
            get(routes::designs_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs",
            get(routes::jobs_list).post(routes::jobs_create),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/job-types",
            get(routes::job_types_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/job-types/{name}",
            get(routes::job_type_get),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/tags",
            get(routes::tags_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/file",
            get(routes::vcs_file),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/tree",
            get(routes::vcs_tree),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}",
            get(routes::jobs_get).patch(routes::jobs_update),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/criteria",
            get(routes::job_criteria),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/release",
            post(routes::jobs_release),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/revoke",
            post(routes::jobs_revoke),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/finalize",
            post(routes::jobs_finalize),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/draft",
            post(routes::jobs_draft),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/members",
            post(routes::jobs_members),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/groups",
            axum::routing::put(routes::jobs_groups),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/approval",
            axum::routing::put(routes::jobs_approval),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/triage",
            post(routes::jobs_triage),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/claim",
            post(routes::jobs_claim).delete(routes::jobs_unclaim),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/graph",
            get(routes::graph_get),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/tasks/pending",
            get(routes::tasks_pending),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/tasks",
            get(routes::tasks_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/resolve",
            post(routes::tasks_resolve),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/diff/{seq}",
            get(routes::diff),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/output",
            get(routes::task_output),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/artifacts",
            get(routes::artifacts_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/artifacts/{kind}",
            get(routes::artifact_get),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/attachments",
            get(routes::attachments_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/attachments/{name}",
            get(routes::attachment_get)
                .put(routes::attachment_put)
                .delete(routes::attachment_delete)
                .layer(axum::extract::DefaultBodyLimit::max(
                    routes::MAX_ATTACHMENT_BYTES,
                )),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/events",
            get(sse::project_events),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/events",
            get(sse::job_events),
        )
        .merge(oidc::public_routes())
        .with_state(state);

    if let Some(dist) = ui_dist {
        let index = dist.join("index.html");
        router = router
            .nest_service(
                "/assets",
                axum::routing::get_service(tower_http::services::ServeDir::new(
                    dist.join("assets"),
                ))
                .layer(
                    tower_http::set_header::SetResponseHeaderLayer::overriding(
                        axum::http::header::CACHE_CONTROL,
                        axum::http::HeaderValue::from_static("public, max-age=31536000, immutable"),
                    ),
                ),
            )
            .fallback_service(
                tower_http::services::ServeDir::new(&dist)
                    .fallback(tower_http::services::ServeFile::new(index)),
            );
    }
    router.layer(tower_http::compression::CompressionLayer::new())
}

/// Bind and serve until the process is killed.
pub async fn serve(
    state: SharedState,
    addr: std::net::SocketAddr,
    ui_dist: Option<PathBuf>,
) -> std::io::Result<()> {
    let app = router(state, ui_dist);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "api up");
    axum::serve(listener, app).await
}

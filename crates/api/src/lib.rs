//! HTTP↔NATS bridge (spec Part 6, §10.4, Part 11, §13.2).
//!
//! A bridge, not a service: translate authenticated HTTP into NATS request-reply,
//! bridge streams to SSE, encrypt secrets on write, validate ingest tokens.
//! No orchestration logic. Never depends on the `dispatcher` crate.

pub mod ingest;
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
}

pub type SharedState = Arc<ApiState>;

/// Build the axum router: auth + the §6.2 project surface implemented so far,
/// with the SPA (when `ui_dist` is given) served as the fallback.
pub fn router(state: SharedState, ui_dist: Option<PathBuf>) -> axum::Router {
    use axum::routing::{get, post};

    let mut router = axum::Router::new()
        // Health (§6.x): unauthenticated dispatcher-liveness probe.
        .route("/api/v1/health", get(routes::health))
        // Auth (§7.1)
        .route("/auth/login", post(routes::login))
        .route("/auth/logout", post(routes::logout))
        .route("/auth/me", get(routes::me))
        // User SSH cert minting (§7.3): any authenticated user.
        .route("/auth/ssh-cert", post(routes::ssh_cert))
        // Projects
        .route(
            "/api/v1/projects",
            get(routes::projects_list).post(routes::projects_create),
        )
        .route("/api/v1/projects/link", post(routes::projects_link))
        // Platform-wide config (read-only settings; platform admins only)
        .route("/api/v1/platform/config", get(routes::platform_config_get))
        // Live fleet occupancy (read-only; platform admins only)
        .route("/api/v1/platform/fleet", get(routes::platform_fleet_get))
        // Per-project config (read-only settings; Viewer+)
        .route(
            "/api/v1/projects/{owner}/{project}/config",
            get(routes::project_config_get),
        )
        // Project members / role management (platform admins only, §7.5)
        .route(
            "/api/v1/projects/{owner}/{project}/members",
            get(routes::members_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/members/{email}",
            axum::routing::put(routes::members_set).delete(routes::members_remove),
        )
        // Origin (linked projects)
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
        // Capacity launch-queue snapshot (read-only; Viewer+)
        .route(
            "/api/v1/projects/{owner}/{project}/queue",
            get(routes::queue_get),
        )
        // Jobs
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
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/triage",
            post(routes::jobs_triage),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/claim",
            post(routes::jobs_claim).delete(routes::jobs_unclaim),
        )
        // Graph
        .route(
            "/api/v1/projects/{owner}/{project}/graph",
            get(routes::graph_get),
        )
        // Tasks
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
        // VCS
        .route(
            "/api/v1/projects/{owner}/{project}/diff/{seq}",
            get(routes::diff),
        )
        // Artifacts (§4.2): transcripts and container logs
        // Live/paged container output for a running task, falling back to the
        // harvested stdout.log artifact after exit (§4.2)
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
        // Job attachments (§1.6): operator-uploaded files (screenshots, docs)
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/attachments",
            get(routes::attachments_list),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/attachments/{name}",
            get(routes::attachment_get)
                .put(routes::attachment_put)
                .delete(routes::attachment_delete)
                // Raise the body cap above axum's 2MB default so a screenshot
                // upload is not rejected before the handler runs.
                .layer(axum::extract::DefaultBodyLimit::max(
                    routes::MAX_ATTACHMENT_BYTES,
                )),
        )
        // SSE (§6.4)
        .route(
            "/api/v1/projects/{owner}/{project}/events",
            get(sse::project_events),
        )
        .route(
            "/api/v1/projects/{owner}/{project}/jobs/{seq}/events",
            get(sse::job_events),
        )
        .with_state(state);

    if let Some(dist) = ui_dist {
        let index = dist.join("index.html");
        router = router.fallback_service(
            tower_http::services::ServeDir::new(&dist)
                .fallback(tower_http::services::ServeFile::new(index)),
        );
    }
    router
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

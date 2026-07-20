//! Route handlers for the §6.2 HTTP surface: translate, authenticate, forward.
//!
//! Every proxied handler follows the same shape: authorize per §7.5, publish
//! to the §6.1 subject, map the dispatcher's reply envelope (resource JSON, or
//! `{"error":{"status",..}}`) onto the §6.5 HTTP contract.

use crate::SharedState;
use auth::{Action, authorize};
use axum::Json;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::{StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use std::time::Duration;
use types::{Identity, IdentityKind, TaskResolution, User};

/// §6.5 error envelope with the HTTP status to send it under.
pub struct ApiError {
    pub status: StatusCode,
    pub body: serde_json::Value,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: serde_json::json!({ "error": message.into() }),
        }
    }

    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "authentication required")
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Authenticated-identity extractor: JWT session cookie → `Identity` (§7.1).
pub struct Auth(pub Identity);

impl FromRequestParts<SharedState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        // `Authorization: Bearer <jwt>` first (machine callers — CLI-minted
        // tokens, §7.1), then the browser session cookie. Same JWT either way.
        let bearer = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let cookie_token = parts
            .headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(auth::jwt::token_from_cookie_header);
        let token = bearer.or(cookie_token).ok_or_else(ApiError::unauthorized)?;
        let identity = state
            .verifier
            .verify(token)
            .map_err(|_| ApiError::unauthorized())?;
        Ok(Auth(identity))
    }
}

fn require(identity: &Identity, action: Action) -> ApiResult<()> {
    authorize(identity, &action)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "insufficient role"))
}

fn read_project(identity: &Identity, owner: &str, project: &str) -> ApiResult<()> {
    require(
        identity,
        Action::ReadProject {
            project: format!("{owner}/{project}"),
        },
    )
}

/// Member+ gate (§7.5 "complete/fail a task" row) — job mutations ride the
/// same requirement.
fn member_on(identity: &Identity, owner: &str, project: &str) -> ApiResult<()> {
    require(
        identity,
        Action::ResolveTask {
            project: format!("{owner}/{project}"),
        },
    )
}

/// Project-Admin gate (§7.5 config row) — origin releases ship the project.
fn admin_on(identity: &Identity, owner: &str, project: &str) -> ApiResult<()> {
    require(
        identity,
        Action::ManageProjectConfig {
            project: format!("{owner}/{project}"),
        },
    )
}

/// Publish to a §6.1 subject and map the reply envelope to HTTP.
async fn forward(
    state: &SharedState,
    subject: &str,
    payload: serde_json::Value,
    success: StatusCode,
) -> ApiResult<Response> {
    let payload = serde_json::to_vec(&payload).map_err(|e| ApiError::internal(e.to_string()))?;
    let reply = state
        .store
        .request_with_retry(subject, &payload, 3, Duration::from_millis(300))
        .await
        .map_err(|e| ApiError::internal(format!("dispatcher unavailable: {e}")))?;
    let value: serde_json::Value = serde_json::from_slice(&reply.payload)
        .map_err(|e| ApiError::internal(format!("bad dispatcher reply: {e}")))?;
    if let Some(err) = value.get("error") {
        let status = err
            .get("status")
            .and_then(|s| s.as_u64())
            .and_then(|s| StatusCode::from_u16(s as u16).ok())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // §6.5: validation failures use the {"errors": [...]} envelope.
        let body = match err.get("errors") {
            Some(errors) => serde_json::json!({ "errors": errors }),
            None => {
                serde_json::json!({ "error": err.get("message").cloned().unwrap_or_default() })
            }
        };
        return Ok((status, Json(body)).into_response());
    }
    Ok((success, Json(value)).into_response())
}

// ── Auth (§7.1) ──────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
}

pub async fn login(
    State(state): State<SharedState>,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    let users = state
        .store
        .raw_bucket(store::buckets::USERS)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let user: Option<User> = users
        .get_json(&store::keys::user_key(&body.email))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let denied = || ApiError::new(StatusCode::UNAUTHORIZED, "invalid credentials");
    let user = user.ok_or_else(denied)?;
    if !auth::verify_password(&body.password, &user.password_hash)
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        return Err(denied());
    }
    let identity = Identity {
        sub: user.email.clone(),
        kind: IdentityKind::User,
        project_roles: user.project_roles.clone(),
        platform_admin: user.platform_admin,
    };
    let token = state
        .signer
        .issue(&identity, state.session_ttl)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let cookie = auth::jwt::session_cookie(&token, state.session_ttl);
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(identity),
    )
        .into_response())
}

pub async fn logout() -> Response {
    let cookie = auth::jwt::session_cookie("", chrono::Duration::zero());
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({"ok": true})),
    )
        .into_response()
}

pub async fn me(Auth(identity): Auth) -> Json<Identity> {
    Json(identity)
}

// ── Projects ─────────────────────────────────────────────────────────────

/// Projects visible to the caller. Platform admins see the whole registry
/// (the counters bucket, written at `admin project create`, §12.2); everyone
/// else sees the keys of their role map. Returns sorted `owner/project`
/// strings.
pub async fn projects_list(
    State(state): State<SharedState>,
    Auth(identity): Auth,
) -> ApiResult<Json<Vec<String>>> {
    let mut projects: Vec<String> = if identity.platform_admin {
        let counters = state
            .store
            .raw_bucket(store::buckets::COUNTERS)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        counters
            .keys_with_prefix("")
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .iter()
            .filter_map(|k| k.split_once('.').map(|(o, p)| format!("{o}/{p}")))
            .collect()
    } else {
        identity.project_roles.keys().cloned().collect()
    };
    projects.sort();
    Ok(Json(projects))
}

/// Create a project (§12.2 via the API): bare repo, pre-receive hook, the
/// Code starter template, and the job counter. Platform admins only — role
/// grants for other users remain an admin-CLI concern.
pub async fn projects_create(
    State(state): State<SharedState>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    if !identity.platform_admin {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "platform admin required"));
    }
    forward(&state, &store::subjects::projects_create(), body, StatusCode::CREATED).await
}

/// Link an existing external repo as a new project (linked-origin mode).
/// Platform admins only, like `projects_create`. Body: `{ owner, name,
/// origin_url, main_branch? }` — requires the `CHUG_ORIGIN_*` secrets to be
/// set on the project first.
pub async fn projects_link(
    State(state): State<SharedState>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    if !identity.platform_admin {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "platform admin required"));
    }
    forward(&state, &store::subjects::projects_link(), body, StatusCode::CREATED).await
}

// ── Origin (linked projects) ─────────────────────────────────────────────

/// Link + release state with an opportunistic PR check (Viewer+).
pub async fn origin_get(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::origin_status(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// Open an origin release PR (project Admin).
pub async fn origin_release(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    admin_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::origin_release(&owner, &project),
        serde_json::json!({}),
        StatusCode::CREATED,
    )
    .await
}

/// Fetch the origin and reconcile release state (project Admin).
pub async fn origin_sync(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    admin_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::origin_sync(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

// ── Jobs ─────────────────────────────────────────────────────────────────

pub async fn jobs_create(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_create(&owner, &project),
        body,
        StatusCode::CREATED,
    )
    .await
}

pub async fn jobs_list(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_list(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

pub async fn jobs_get(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_get(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// Resolved evaluation criteria for a job: the type's evaluators plus the
/// job's additive ones, annotated with `source`, at the ref execution uses.
pub async fn job_criteria(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_criteria(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

pub async fn job_types_list(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::job_types_list(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// One job type in full (raw YAML + parsed, defaults merged) for the library.
pub async fn job_type_get(
    State(state): State<SharedState>,
    Path((owner, project, name)): Path<(String, String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::job_types_get(&owner, &project),
        serde_json::json!({ "name": name }),
        StatusCode::OK,
    )
    .await
}

/// Available knowledge tags (`tags/*.md` stems at default HEAD).
pub async fn tags_list(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::tags_list(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct FileQuery {
    pub path: String,
}

/// One repo file at default-branch HEAD (`?path=...`) — the UI's prompt
/// viewer. Read access only.
pub async fn vcs_file(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<FileQuery>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::vcs_file(&owner, &project),
        serde_json::json!({ "path": q.path }),
        StatusCode::OK,
    )
    .await
}

/// Full recursive tree at default-branch HEAD — the repo browser.
pub async fn vcs_tree(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::vcs_tree(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

pub async fn jobs_release(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_release(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

pub async fn jobs_revoke(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_revoke(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

// ── Graph ────────────────────────────────────────────────────────────────

pub async fn graph_get(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::graph_get(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

// ── Tasks ────────────────────────────────────────────────────────────────

pub async fn tasks_pending(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::tasks_list_pending(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

pub async fn tasks_list(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::tasks_list(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

pub async fn tasks_resolve(
    State(state): State<SharedState>,
    Path((owner, project, seq, task_id)): Path<(String, String, u64, u64)>,
    Auth(identity): Auth,
    Json(resolution): Json<TaskResolution>,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::tasks_resolve(&owner, &project, seq, task_id),
        serde_json::json!({ "resolution": resolution, "operator": identity.sub }),
        StatusCode::OK,
    )
    .await
}

// ── VCS ──────────────────────────────────────────────────────────────────

pub async fn diff(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::vcs_diff(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

// ── Artifacts (§4.2): session transcripts and container logs ────────────────
//
// These stream from the object store rather than riding a req/reply through
// the dispatcher: a transcript routinely exceeds NATS's 1MB max_payload, which
// a reply cannot carry. Decryption happens here because the API holds the
// `age_artifacts` identity — a separate key from the secrets one, which stays
// dispatcher-only (§10.2).

/// Kinds present for a task, so the UI knows what to offer.
pub async fn artifacts_list(
    State(state): State<SharedState>,
    Path((owner, project, seq, task_id)): Path<(String, String, u64, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    let Some(artifacts) = &state.artifacts else {
        return Ok(Json(serde_json::json!({ "artifacts": [] })).into_response());
    };
    let kinds = artifacts
        .list_for_task(&owner, &project, seq, task_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    Ok(Json(serde_json::json!({ "artifacts": names })).into_response())
}

/// One artifact, decrypted. Served as bytes, not JSON: a transcript is JSONL
/// and a log is plain text, and both can be large.
pub async fn artifact_get(
    State(state): State<SharedState>,
    Path((owner, project, seq, task_id, kind)): Path<(String, String, u64, u64, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    let kind = store::ArtifactKind::parse(&kind)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown artifact kind"))?;
    let artifacts = state
        .artifacts
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "artifact capture is not configured"))?;
    let bytes = artifacts
        .get(&owner, &project, seq, task_id, kind)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "artifact not found"))?;
    let content_type = match kind {
        // JSONL, not JSON: one object per line, so it is not a valid document.
        store::ArtifactKind::SessionTranscript => "application/x-ndjson",
        store::ArtifactKind::Stdout => "text/plain; charset=utf-8",
    };
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        bytes,
    )
        .into_response())
}

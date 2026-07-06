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
        let cookie = parts
            .headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(ApiError::unauthorized)?;
        let token =
            auth::jwt::token_from_cookie_header(cookie).ok_or_else(ApiError::unauthorized)?;
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

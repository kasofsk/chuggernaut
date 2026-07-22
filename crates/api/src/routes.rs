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
    map_reply(&reply.payload, success)
}

/// Like [`forward`] but with a single attempt and a caller-owned timeout — for
/// requests that are slow and NOT cheap to repeat (the job wizard's LLM call),
/// where the default short timeout + 3× retry would fire duplicate model calls.
async fn forward_slow(
    state: &SharedState,
    subject: &str,
    payload: serde_json::Value,
    success: StatusCode,
    timeout: Duration,
) -> ApiResult<Response> {
    let payload = serde_json::to_vec(&payload).map_err(|e| ApiError::internal(e.to_string()))?;
    let reply = state
        .store
        .request_timeout(subject, &payload, timeout)
        .await
        .map_err(|e| ApiError::internal(format!("dispatcher unavailable: {e}")))?;
    map_reply(&reply.payload, success)
}

/// Map a dispatcher reply envelope (resource JSON, or the §6.5 error shape) to
/// an HTTP response.
fn map_reply(reply: &[u8], success: StatusCode) -> ApiResult<Response> {
    let value: serde_json::Value = serde_json::from_slice(reply)
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

// ── Health (§6.x) ──────────────────────────────────────────────────────────

/// `GET /api/v1/health` (spec §6.x): unauthenticated liveness probe that proves
/// the *dispatcher*, not just this api process. It issues a bounded `req.health`
/// NATS request that round-trips the dispatcher's core actor and echoes its
/// `{"dispatcher":"ok","version"}` reply as `200`. A crash-looping or wedged
/// dispatcher yields no responder / no reply, so the probe fails into a `503`
/// with the error — the deploy gate can never be fooled by the SPA fallback
/// answering `200` (the 2026-07-22 masquerade).
///
/// Deliberately unauthenticated: the body leaks only liveness and the build
/// version, never any project data (spec §6.x).
pub async fn health(State(state): State<SharedState>) -> Response {
    match state
        .store
        .request_timeout(&store::subjects::health(), b"{}", Duration::from_secs(3))
        .await
    {
        Ok(reply) => match serde_json::from_slice::<serde_json::Value>(&reply.payload) {
            // Only a genuine {"dispatcher":"ok",..} reply is healthy; anything
            // else (e.g. the actor's 503 envelope) maps to 503, never 200.
            Ok(body) if body.get("dispatcher").and_then(|d| d.as_str()) == Some("ok") => {
                (StatusCode::OK, Json(body)).into_response()
            }
            Ok(body) => (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response(),
            Err(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "dispatcher": "error",
                    "error": format!("bad dispatcher health reply: {e}"),
                })),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "dispatcher": "error", "error": e.to_string() })),
        )
            .into_response(),
    }
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

#[derive(serde::Deserialize)]
pub struct SshCertBody {
    pub public_key: String,
}

/// §7.3: mint a 24h user SSH certificate for the caller's public key. Any
/// authenticated user (§7.5). The email and roles are taken from the session
/// server-side — never the request body — so the dispatcher signs a cert whose
/// principal is the caller's email and whose forced command carries the caller's
/// roles as of signing time. Junk keys are rejected here (422) before they ever
/// reach the CA.
pub async fn ssh_cert(
    State(state): State<SharedState>,
    Auth(identity): Auth,
    Json(body): Json<SshCertBody>,
) -> ApiResult<Response> {
    // Size-cap: a public-key line is a few hundred bytes; anything larger is
    // not a key we would sign.
    if body.public_key.len() > 8 * 1024 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "public key too large",
        ));
    }
    if !auth::ssh::valid_public_key_line(&body.public_key) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unparseable SSH public key",
        ));
    }
    // The dispatcher loads the caller's roles from their user record and signs;
    // we forward only the authenticated email, never a client-supplied identity.
    forward(
        &state,
        &store::subjects::ssh_sign_user_cert(),
        serde_json::json!({ "public_key": body.public_key, "email": identity.sub }),
        StatusCode::OK,
    )
    .await
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

// ── Config (read-only settings overview) ────────────────────────────────────

/// Reserved secret names holding a linked project's git-origin credentials
/// (mirrors the dispatcher's `origin::SECRET_DEPLOY_KEY`/`SECRET_PAT`). Surfaced
/// as presence flags under the origin group, never listed among general secrets.
const ORIGIN_DEPLOY_KEY: &str = "CHUG_ORIGIN_DEPLOY_KEY";
const ORIGIN_PAT: &str = "CHUG_ORIGIN_PAT";

/// Strip `prefix` off each key and return the sorted remainders.
fn names_under(keys: Vec<String>, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = keys
        .iter()
        .filter_map(|k| k.strip_prefix(prefix).map(String::from))
        .collect();
    names.sort();
    names
}

/// Read a KV bucket's keys with the given prefix, mapping errors to 500.
async fn bucket_keys(state: &SharedState, bucket: &str, prefix: &str) -> ApiResult<Vec<String>> {
    state
        .store
        .raw_bucket(bucket)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .keys_with_prefix(prefix)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))
}

/// Read-only project config for the settings tab: vars (name+value), secret
/// NAMES (values never leave the dispatcher), and the git-origin link + which
/// origin credentials are present. Viewer+ (§7.5).
pub async fn project_config_get(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    let prefix = format!("{owner}.{project}.");

    // vars — non-sensitive, name + value.
    let vars_bucket = state
        .store
        .raw_bucket(store::buckets::VARS)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut vars = Vec::new();
    for key in vars_bucket
        .keys_with_prefix(&prefix)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        let Some(name) = key.strip_prefix(&prefix) else {
            continue;
        };
        let value: Option<String> = vars_bucket
            .get_json(&key)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        vars.push(serde_json::json!({ "name": name, "value": value.unwrap_or_default() }));
    }
    vars.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

    // secrets — NAMES ONLY (the api holds no decryption key). Split the reserved
    // origin credentials out into presence flags.
    let secret_names = names_under(
        bucket_keys(&state, store::buckets::SECRETS, &prefix).await?,
        &prefix,
    );
    let has = |n: &str| secret_names.iter().any(|s| s == n);
    let deploy_key = has(ORIGIN_DEPLOY_KEY);
    let pat = has(ORIGIN_PAT);
    let secrets: Vec<&String> = secret_names
        .iter()
        .filter(|n| n.as_str() != ORIGIN_DEPLOY_KEY && n.as_str() != ORIGIN_PAT)
        .collect();

    // origin link (from the project record; absent = classic self-hosted).
    let record: Option<types::ProjectRecord> = state
        .store
        .raw_bucket(store::buckets::PROJECTS)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .get_json(&format!("{owner}.{project}"))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let origin = record.and_then(|r| r.origin);

    Ok(Json(serde_json::json!({
        "vars": vars,
        "secrets": secrets,
        "origin": origin,
        "origin_credentials": { "deploy_key": deploy_key, "pat": pat },
    }))
    .into_response())
}

/// Read-only platform config for the platform settings page: the dispatcher's
/// published snapshot (fleet + defaults), the `global/agents` secret NAMES, and
/// whether web-push (VAPID) is configured. Platform admins only.
pub async fn platform_config_get(
    State(state): State<SharedState>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    if !identity.platform_admin {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "platform admin required",
        ));
    }
    let platform = state
        .store
        .raw_bucket(store::buckets::PLATFORM)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let dispatcher: Option<types::DispatcherConfigSnapshot> = platform
        .get_json("dispatcher.config")
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let vapid_public: Option<String> = platform
        .get_json("vapid.public")
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let agent_prefix = format!("{}.agents.", store::keys::RESERVED_OWNER);
    let agent_secrets = names_under(
        bucket_keys(&state, store::buckets::SECRETS, &agent_prefix).await?,
        &agent_prefix,
    );

    Ok(Json(serde_json::json!({
        "dispatcher": dispatcher,
        "agent_secrets": agent_secrets,
        "vapid_public": vapid_public.is_some(),
    }))
    .into_response())
}

/// Live fleet occupancy for the platform view: the dispatcher's `fleet.status`
/// snapshot (per-node slot usage + the running job/task in each busy slot, plus
/// the launch-queue depth — spec §3.1). Cross-project data, so platform admins
/// only, matching `platform_config_get`. An empty fleet is served before the
/// dispatcher has published anything (never a 404, so the UI needn't special-
/// case a cold start). Change events reach the UI over the existing SSE path:
/// every occupancy change coincides with a task lifecycle event already on the
/// job-event stream, on which the client refetches this snapshot.
pub async fn platform_fleet_get(
    State(state): State<SharedState>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    if !identity.platform_admin {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "platform admin required",
        ));
    }
    let platform = state
        .store
        .raw_bucket(store::buckets::PLATFORM)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let fleet: types::FleetStatus = platform
        .get_json("fleet.status")
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .unwrap_or_default();
    Ok(Json(fleet).into_response())
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
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "platform admin required",
        ));
    }
    forward(
        &state,
        &store::subjects::projects_create(),
        body,
        StatusCode::CREATED,
    )
    .await
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
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "platform admin required",
        ));
    }
    forward(
        &state,
        &store::subjects::projects_link(),
        body,
        StatusCode::CREATED,
    )
    .await
}

// ── Members (§7.5 project-role management) ───────────────────────────────

/// Platform-admin gate (§7.5 "platform-level config" row): role management is a
/// platform-admin concern, mirroring `admin user role` on the CLI. The API layer
/// already treats `platform_admin` as all-powerful.
fn platform_admin(identity: &Identity) -> ApiResult<()> {
    if identity.platform_admin {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "platform admin required",
        ))
    }
}

/// `GET /api/v1/projects/{owner}/{project}/members` — users holding a role on
/// the project, each `{ email, role }`. Platform admins only.
pub async fn members_list(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    platform_admin(&identity)?;
    forward(
        &state,
        &store::subjects::members("list", &owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

#[derive(serde::Deserialize)]
pub struct MemberRoleBody {
    /// `owner` | `member` | `viewer` (`owner` is the top project role, §7.5).
    pub role: String,
}

/// `PUT /api/v1/projects/{owner}/{project}/members/{email}` — grant/update the
/// user's role on the project. Body `{ role }`. Platform admins only; the
/// dispatcher owns the record mutation (single writer of `users.*`).
pub async fn members_set(
    State(state): State<SharedState>,
    Path((owner, project, email)): Path<(String, String, String)>,
    Auth(identity): Auth,
    Json(body): Json<MemberRoleBody>,
) -> ApiResult<Response> {
    platform_admin(&identity)?;
    forward(
        &state,
        &store::subjects::members("set", &owner, &project),
        serde_json::json!({ "email": email, "role": body.role }),
        StatusCode::OK,
    )
    .await
}

/// `DELETE /api/v1/projects/{owner}/{project}/members/{email}` — clear the
/// user's role on the project. Platform admins only.
pub async fn members_remove(
    State(state): State<SharedState>,
    Path((owner, project, email)): Path<(String, String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    platform_admin(&identity)?;
    forward(
        &state,
        &store::subjects::members("remove", &owner, &project),
        serde_json::json!({ "email": email }),
        StatusCode::OK,
    )
    .await
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

/// One turn of the New Job "job wizard" chat: bridge the conversation to the
/// dispatcher, which grounds it in repo/job context and calls the LLM. Same
/// Member+ gate as creating a job — the wizard is a create-job aid.
pub async fn wizard_chat(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    // The dispatcher gathers context and calls the LLM inline — well over the
    // default request timeout, and not idempotent-cheap, so: one attempt,
    // generous deadline (a touch above the model's own 60s timeout).
    forward_slow(
        &state,
        &store::subjects::wizard_chat(&owner, &project),
        body,
        StatusCode::OK,
        Duration::from_secs(75),
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

/// Edit a Draft job's definition (§2.1): full-field replace, same body shape
/// as create. Member+; the dispatcher rejects (409) any non-Draft job.
pub async fn jobs_update(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_update(&owner, &project, seq),
        body,
        StatusCode::OK,
    )
    .await
}

/// Move a Frozen (never-released) job back to Draft for editing (§2.1).
/// Member+; the dispatcher rejects (409) anything but a Frozen job.
pub async fn jobs_draft(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_draft(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// Claim the job's next work attempt for a human (§1.2 claims): the attempt
/// parks as a Pending task with the declared kind instead of launching.
/// Member+; the dispatcher enforces the in-flight guard (409).
pub async fn jobs_claim(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_claim(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// Clear a pending claim that has not materialized into a parked task (409
/// otherwise — a parked attempt is resolved via its task).
pub async fn jobs_unclaim(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_unclaim(&owner, &project, seq),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// Dispatch an advisory triage agent over an Escalated/Stalled job (§1.2).
/// Member+ like the other job mutations; the dispatcher enforces the state
/// guard (409) and TRIAGE_IMAGE availability (422).
pub async fn jobs_triage(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_triage(&owner, &project, seq),
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

// ── Live task output (§4.2): cursor-paged container stdout/stderr ───────────
//
// A running task's container is tailed live via the dispatcher (a bounded
// req/reply, no held-open stream through the core actor); once it exits, this
// falls back to the harvested `stdout.log` artifact at the SAME byte offsets,
// so a poller never loses the tail when the container is removed (job #10).

#[derive(serde::Deserialize)]
pub struct OutputQuery {
    /// Byte cursor: return output from here on. 0 (default) reads from the start.
    #[serde(default)]
    pub since: u64,
}

/// `GET .../tasks/{id}/output?since=<offset>` → `{ offset, data, running }`.
/// While the task's container runs the dispatcher returns its live tail
/// (`running: true`); after exit this serves the harvested `stdout.log` at the
/// same offsets (`running: false`). A UI/CLI polls with the last `offset`.
/// Viewer+ (§7.5), same as artifacts. 404 before a container exists; 502 if the
/// owning node is unreachable.
pub async fn task_output(
    State(state): State<SharedState>,
    Path((owner, project, seq, task_id)): Path<(String, String, u64, u64)>,
    axum::extract::Query(q): axum::extract::Query<OutputQuery>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    let payload = serde_json::to_vec(&serde_json::json!({ "since": q.since }))
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let reply = state
        .store
        .request_with_retry(
            &store::subjects::tasks_output(&owner, &project, seq, task_id),
            &payload,
            3,
            Duration::from_millis(300),
        )
        .await
        .map_err(|e| ApiError::internal(format!("dispatcher unavailable: {e}")))?;
    let value: serde_json::Value = serde_json::from_slice(&reply.payload)
        .map_err(|e| ApiError::internal(format!("bad dispatcher reply: {e}")))?;
    // The dispatcher signals errors (404 no container yet, 502 wedged node) in
    // the §6.5 envelope — propagate its status verbatim.
    if let Some(err) = value.get("error") {
        let status = err
            .get("status")
            .and_then(|s| s.as_u64())
            .and_then(|s| StatusCode::from_u16(s as u16).ok())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let message = err.get("message").cloned().unwrap_or_default();
        return Ok((status, Json(serde_json::json!({ "error": message }))).into_response());
    }
    // Live container output: pass the tail through as-is.
    if value.get("running").and_then(|r| r.as_bool()) == Some(true) {
        return Ok((StatusCode::OK, Json(value)).into_response());
    }
    // Finished (or the container is already gone): serve the harvested
    // stdout.log from `since` on, so a poller that had offset N off the live
    // stream continues seamlessly into the artifact.
    let (offset, data) = match &state.artifacts {
        Some(artifacts) => {
            let bytes = artifacts
                .get(&owner, &project, seq, task_id, store::ArtifactKind::Stdout)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?
                .unwrap_or_default();
            let start = (q.since as usize).min(bytes.len());
            (
                bytes.len() as u64,
                String::from_utf8_lossy(&bytes[start..]).into_owned(),
            )
        }
        // No artifact store configured: the task is finished with nothing to
        // replay. Hold the cursor so a poller sees `running: false` and stops.
        None => (q.since, String::new()),
    };
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "offset": offset, "data": data, "running": false })),
    )
        .into_response())
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
    let artifacts = state.artifacts.as_ref().ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "artifact capture is not configured")
    })?;
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

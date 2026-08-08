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
///
/// The reply also carries this *api* process's own build SHA (`api_sha`),
/// baked at build time and independent of the dispatcher's `version`: the api
/// and dispatcher restart at different moments, so surfacing both lets the
/// cluster view flag the real skew when one lands on a different commit.
pub async fn health(State(state): State<SharedState>) -> Response {
    match state
        .store
        .request_timeout(&store::subjects::health(), b"{}", Duration::from_secs(3))
        .await
    {
        Ok(reply) => match serde_json::from_slice::<serde_json::Value>(&reply.payload) {
            Ok(mut body) if body.get("dispatcher").and_then(|d| d.as_str()) == Some("ok") => {
                inject_api_sha(&mut body);
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

/// This api binary's own build SHA (`CHUG_GIT_SHA`, baked at build time — same
/// `option_env!` pattern as the dispatcher's `cd::deployed_sha`). `None` for a
/// local/dev build without it baked in, which the cluster view renders as a dash.
fn api_sha() -> Option<&'static str> {
    option_env!("CHUG_GIT_SHA")
}

/// Add this api process's `api_sha` to a healthy dispatcher reply so the cluster
/// view can show the api's deployed hash independently of the dispatcher's. A
/// no-op when the reply isn't a JSON object or no SHA was baked in.
fn inject_api_sha(body: &mut serde_json::Value) {
    inject_sha(body, api_sha());
}

/// The pure core of [`inject_api_sha`], split out so the injection is testable
/// without the build-time `option_env!` read (which is `None` under `cargo
/// test`). Inserts `api_sha` only when a SHA is present and `body` is an object.
fn inject_sha(body: &mut serde_json::Value, sha: Option<&str>) {
    if let (Some(obj), Some(sha)) = (body.as_object_mut(), sha) {
        obj.insert("api_sha".into(), serde_json::Value::String(sha.to_string()));
    }
}

#[derive(serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    forward(
        &state,
        &store::subjects::ssh_sign_user_cert(),
        serde_json::json!({ "public_key": body.public_key, "email": identity.sub }),
        StatusCode::OK,
    )
    .await
}

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

/// The `platform` bucket, for a caller that must be a platform admin first.
/// Every platform-scoped read is cross-project, so they share one gate and one
/// bucket handle; keeping the pair in one place is what stops the admin check
/// from being forgotten on the next one.
async fn platform_bucket_for_admin(
    state: &SharedState,
    identity: &Identity,
) -> ApiResult<store::Bucket> {
    if !identity.platform_admin {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "platform admin required",
        ));
    }
    state
        .store
        .raw_bucket(store::buckets::PLATFORM)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))
}

/// Read-only platform config for the platform settings page: the dispatcher's
/// published snapshot (fleet + defaults), the `global/agents` secret NAMES, and
/// whether web-push (VAPID) is configured. Platform admins only.
pub async fn platform_config_get(
    State(state): State<SharedState>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    let platform = platform_bucket_for_admin(&state, &identity).await?;
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
    let platform = platform_bucket_for_admin(&state, &identity).await?;
    let fleet: Option<serde_json::Value> = platform
        .get_json("fleet.status")
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let fleet = match fleet {
        Some(fleet) => fleet,
        None => serde_json::to_value(types::FleetStatus::default())
            .map_err(|e| ApiError::internal(e.to_string()))?,
    };
    Ok(Json(fleet).into_response())
}

/// `PUT /api/v1/platform/fleet/{node}/capacity` — set one worker node's desired
/// slot count (spec §3.1 operator capacity control, §6.1). Platform admins only,
/// gated exactly like `platform_config_get` / `platform_fleet_get` above:
/// fleet capacity is platform-level config (§7.5).
///
/// Replies **202 Accepted, not 200**. The dispatcher records the operator's
/// number as intent and *starts* the `set_slots` push without waiting on the
/// node RPC (the actor is single-threaded by design), so "recorded and
/// converging" is the honest status; convergence shows up in the next fleet
/// snapshot, typically within a second. The dispatcher's own refusals pass
/// through verbatim: 404 for a node the fleet does not hold, 409 for a
/// docker-endpoint node whose capacity `DOCKER_NODES` still owns.
///
/// `by` is the authenticated admin's identity taken from the session — never the
/// request body — so the record's `set_by` audit stamp cannot be spoofed.
pub async fn platform_fleet_capacity_set(
    State(state): State<SharedState>,
    Path(node): Path<String>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    platform_admin(&identity)?;
    let slots = capacity_slots(&body)?;
    forward(
        &state,
        &store::subjects::fleet_capacity_set(),
        serde_json::json!({ "node": node, "slots": slots, "by": identity.sub }),
        StatusCode::ACCEPTED,
    )
    .await
}

/// The `{ slots }` body of a capacity command. A missing, negative, fractional
/// or over-large value is a **400**, deliberately not the 422 axum's typed
/// extractor would produce: §6.1 reserves 422 for a value the *node* refuses as
/// above its maximum, and an operator's typo must not read to the UI as "above
/// the node's ceiling".
fn capacity_slots(body: &serde_json::Value) -> ApiResult<u32> {
    body.get("slots")
        .and_then(serde_json::Value::as_u64)
        .and_then(|slots| u32::try_from(slots).ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "slots must be a non-negative whole number",
            )
        })
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// Read-only snapshot of the capacity launch queue for this project (spec §3.5):
/// `{ depth, entries: [{ seq, task_id, position, queued_at }] }`. The UI derives
/// each queued task's "position N of M" from it; it omits the badge gracefully
/// when the dispatcher is unreachable.
pub async fn queue_get(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::queue_list(&owner, &project),
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

/// Every group the project's jobs name, with each group's members, per-state
/// counts and open count, plus the design document a `design/`-namespaced name
/// resolves to (spec §1.1 `groups`, design #321). Derived at read time: no
/// aggregate is stored anywhere, and a group with no members does not exist.
pub async fn groups_list(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::groups_list(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// The design registry: every `docs/design/*.md` at default HEAD with its
/// verbatim `Status:` line and the roll-up of the jobs grouped under it. The
/// complement of [`groups_list`] — a design nobody has filed a job against is a
/// row here and cannot be one there.
pub async fn designs_list(
    State(state): State<SharedState>,
    Path((owner, project)): Path<(String, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::designs_list(&owner, &project),
        serde_json::json!({}),
        StatusCode::OK,
    )
    .await
}

/// Available knowledge tags at default HEAD, as `{ name, path }` — the path each
/// `.chug/tags/*.md` resolved to (spec §1.1).
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// Finalize an edited Draft back to Frozen (#166): validate the definition
/// like release, but park it (re-batchable) instead of scheduling. Member+;
/// the dispatcher rejects (409) anything but a Draft job.
pub async fn jobs_finalize(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_finalize(&owner, &project, seq),
        serde_json::json!({}),
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

/// Add/remove the members of a Draft batch while composing it (§2.1 draft
/// batches). Body `{ add?: [seq], remove?: [seq] }`. Member+; the dispatcher
/// rejects (409) anything but a Draft batch and re-validates the adds (422).
pub async fn jobs_members(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_members(&owner, &project, seq),
        body,
        StatusCode::OK,
    )
    .await
}

/// Add/remove a job's group labels (§1.1 `groups`, design #321 Decision 5).
/// Body `{ add?: [name], remove?: [name] }`. Member+; accepted in **every**
/// state, terminal included — a group is an operator annotation, inert to
/// execution, so it can be set long after the job finished. The dispatcher
/// re-checks the resulting list against the §1.1 bounds (422).
pub async fn jobs_groups(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_groups(&owner, &project, seq),
        body,
        StatusCode::OK,
    )
    .await
}

/// Set/clear the job's operator sign-off gate (§1.1 require-approval), body
/// `{ require: bool }`. Member+ — the same privilege that resolves the resulting
/// task — and 422 from the dispatcher once the job has entered Work.
pub async fn jobs_approval(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::jobs_approval(&owner, &project, seq),
        body,
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

#[derive(serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiffQuery {
    /// Byte cursor into the diff text: return the diff from here on. 0
    /// (default) reads from the start and carries the file summary.
    #[serde(default)]
    pub since: u64,
}

/// `GET .../diff/{seq}?since=<offset>` → one cursor page of the job's diff:
/// `{ files, offset, data, done, diff }`. A caller loops on `offset` until
/// `done`, because a diff can exceed what one NATS reply carries (§6.2).
pub async fn diff(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    axum::extract::Query(q): axum::extract::Query<DiffQuery>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    forward(
        &state,
        &store::subjects::vcs_diff(&owner, &project, seq),
        serde_json::json!({ "since": q.since }),
        StatusCode::OK,
    )
    .await
}

#[derive(serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    if let Some(err) = value.get("error") {
        let status = err
            .get("status")
            .and_then(|s| s.as_u64())
            .and_then(|s| StatusCode::from_u16(s as u16).ok())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let message = err.get("message").cloned().unwrap_or_default();
        return Ok((status, Json(serde_json::json!({ "error": message }))).into_response());
    }
    if value.get("running").and_then(|r| r.as_bool()) == Some(true) {
        return Ok((StatusCode::OK, Json(value)).into_response());
    }
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
        None => (q.since, String::new()),
    };
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "offset": offset, "data": data, "running": false })),
    )
        .into_response())
}

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

/// Upper bound on a single uploaded attachment. Screenshots and short clips
/// fit comfortably; the same value caps the request body (413 over it), and it
/// is the platform-wide blob ceiling a harvested output shares (design #362).
pub const MAX_ATTACHMENT_BYTES: usize = store::MAX_BLOB_BYTES;

/// Reject path traversal and control characters in an uploaded filename — it
/// becomes the object-name suffix and a download path segment.
fn valid_attachment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
}

/// Attachments on a job, so the UI can list and offer them.
pub async fn attachments_list(
    State(state): State<SharedState>,
    Path((owner, project, seq)): Path<(String, String, u64)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    let Some(artifacts) = &state.artifacts else {
        return Ok(Json(serde_json::json!({ "attachments": [] })).into_response());
    };
    let list = artifacts
        .list_attachments(&owner, &project, seq)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "attachments": list })).into_response())
}

/// Upload (or replace) a job attachment. Member+; the raw request body is the
/// file bytes and the `Content-Type` header is stored and echoed on download.
pub async fn attachment_put(
    State(state): State<SharedState>,
    Path((owner, project, seq, name)): Path<(String, String, u64, String)>,
    Auth(identity): Auth,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    if !valid_attachment_name(&name) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid attachment name",
        ));
    }
    if body.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "empty attachment"));
    }
    if body.len() > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("attachment exceeds the {MAX_ATTACHMENT_BYTES}-byte limit"),
        ));
    }
    let artifacts = state.artifacts.as_ref().ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "artifact storage is not configured")
    })?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(store::DEFAULT_ATTACHMENT_CONTENT_TYPE);
    artifacts
        .put_attachment(&owner, &project, seq, &name, content_type, &body)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "name": name, "content_type": content_type, "size": body.len(),
        })),
    )
        .into_response())
}

/// One attachment, decrypted, served under its stored content type.
pub async fn attachment_get(
    State(state): State<SharedState>,
    Path((owner, project, seq, name)): Path<(String, String, u64, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    read_project(&identity, &owner, &project)?;
    let artifacts = state.artifacts.as_ref().ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "artifact storage is not configured")
    })?;
    let (meta, bytes) = artifacts
        .get_attachment(&owner, &project, seq, &name)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "attachment not found"))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, meta.content_type)],
        bytes,
    )
        .into_response())
}

/// Remove a job attachment. Member+.
pub async fn attachment_delete(
    State(state): State<SharedState>,
    Path((owner, project, seq, name)): Path<(String, String, u64, String)>,
    Auth(identity): Auth,
) -> ApiResult<Response> {
    member_on(&identity, &owner, &project)?;
    let artifacts = state.artifacts.as_ref().ok_or_else(|| {
        ApiError::new(StatusCode::NOT_FOUND, "artifact storage is not configured")
    })?;
    let removed = artifacts
        .delete_attachment(&owner, &project, seq, &name)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if removed {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "attachment not found"))
    }
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
        store::ArtifactKind::SessionTranscript => "application/x-ndjson",
        store::ArtifactKind::Stdout => "text/plain; charset=utf-8",
        store::ArtifactKind::Output => "application/gzip",
        store::ArtifactKind::TranscriptMissing => "application/json",
    };
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::{capacity_slots, inject_sha};
    use axum::http::StatusCode;

    /// A baked SHA is added as `api_sha` alongside the dispatcher's own fields,
    /// leaving them untouched — so the cluster view reads the api's build
    /// independently of the dispatcher `version`.
    #[test]
    fn inject_sha_adds_api_sha_when_present() {
        let mut body = serde_json::json!({ "dispatcher": "ok", "version": "0.1.0" });
        inject_sha(&mut body, Some("abc123"));
        assert_eq!(body.get("api_sha").and_then(|v| v.as_str()), Some("abc123"));
        assert_eq!(body.get("version").and_then(|v| v.as_str()), Some("0.1.0"));
    }

    /// A local/dev build with no SHA baked in leaves the reply untouched (no
    /// `api_sha` key), which the cluster view renders as a dash — never an error.
    #[test]
    fn inject_sha_noop_without_sha() {
        let mut body = serde_json::json!({ "dispatcher": "ok" });
        inject_sha(&mut body, None);
        assert!(body.get("api_sha").is_none());
    }

    /// A capacity command's `{ slots }` body (§6.1). `0` is a full drain, not an
    /// error; everything unusable is a 400 and never the 422 that §6.1 reserves
    /// for a value the node refuses as above its maximum — the UI shows those
    /// two very differently.
    #[test]
    fn capacity_slots_accepts_a_slot_count_and_400s_on_anything_else() {
        assert_eq!(
            capacity_slots(&serde_json::json!({ "slots": 4 })).ok(),
            Some(4)
        );
        assert_eq!(
            capacity_slots(&serde_json::json!({ "slots": 0 })).ok(),
            Some(0)
        );

        for body in [
            serde_json::json!({}),
            serde_json::json!({ "slots": "4" }),
            serde_json::json!({ "slots": -1 }),
            serde_json::json!({ "slots": 2.5 }),
            serde_json::json!({ "slots": u64::from(u32::MAX) + 1 }),
        ] {
            let status = capacity_slots(&body).err().map(|e| e.status);
            assert_eq!(status, Some(StatusCode::BAD_REQUEST), "accepted {body}");
        }
    }
}

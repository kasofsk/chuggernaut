use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use futures::stream::Stream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use chuggernaut_types::*;

use crate::error::DispatcherError;
use crate::jobs::kv_get;
use crate::state::DispatcherState;

type AppState = Arc<DispatcherState>;

// ---------------------------------------------------------------------------
// API manifest: single source of truth for route metadata
// ---------------------------------------------------------------------------

/// Helper: get the schemars schema name for a type (matches keys in the schema $defs).
fn schema_name<T: JsonSchema>() -> String {
    T::schema_name().into_owned()
}

#[derive(Debug, Clone, Serialize)]
struct RouteSpec {
    method: &'static str,
    path: &'static str,
    summary: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_inline: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sse_events: Option<serde_json::Value>,
}

/// The authoritative route list. Type names are derived from schemars at compile
/// time, so renaming a type updates the manifest automatically.
fn api_specs() -> Vec<RouteSpec> {
    let key_param = || serde_json::json!({ "key": "Job key (e.g. acme.repo.42)" });

    vec![
        RouteSpec {
            method: "GET",
            path: "/jobs",
            summary: "List jobs",
            params: None,
            query: Some(serde_json::json!({
                "state": { "type": "string", "required": false, "description": "Filter by job state" }
            })),
            request: None,
            request_inline: None,
            response: Some(schema_name::<JobListResponse>()),
            status: None,
            sse_events: None,
        },
        RouteSpec {
            method: "POST",
            path: "/jobs",
            summary: "Create a job",
            params: None,
            query: None,
            request: Some(schema_name::<CreateJobRequest>()),
            request_inline: None,
            response: Some(schema_name::<CreateJobResponse>()),
            status: Some(201),
            sse_events: None,
        },
        RouteSpec {
            method: "GET",
            path: "/jobs/{key}",
            summary: "Get job detail",
            params: Some(key_param()),
            query: None,
            request: None,
            request_inline: None,
            response: Some(schema_name::<JobDetailResponse>()),
            status: None,
            sse_events: None,
        },
        RouteSpec {
            method: "GET",
            path: "/jobs/{key}/deps",
            summary: "Get job dependencies",
            params: Some(key_param()),
            query: None,
            request: None,
            request_inline: None,
            response: Some(schema_name::<JobDepsResponse>()),
            status: None,
            sse_events: None,
        },
        RouteSpec {
            method: "POST",
            path: "/jobs/{key}/requeue",
            summary: "Requeue a job",
            params: Some(key_param()),
            query: None,
            request: None,
            request_inline: Some(serde_json::json!({
                "type": "object",
                "required": ["target"],
                "properties": { "target": { "type": "string", "enum": ["on-deck", "on-ice"] } }
            })),
            response: None,
            status: None,
            sse_events: None,
        },
        RouteSpec {
            method: "POST",
            path: "/jobs/{key}/close",
            summary: "Close or revoke a job",
            params: Some(key_param()),
            query: None,
            request: None,
            request_inline: Some(serde_json::json!({
                "type": "object",
                "properties": { "revoke": { "type": "boolean", "default": false } }
            })),
            response: None,
            status: None,
            sse_events: None,
        },
        RouteSpec {
            method: "POST",
            path: "/jobs/{key}/channel/send",
            summary: "Send a channel message to a running worker",
            params: Some(key_param()),
            query: None,
            request: None,
            request_inline: Some(serde_json::json!({
                "type": "object",
                "required": ["message"],
                "properties": {
                    "message": { "type": "string" },
                    "sender": { "type": "string", "default": "ui" }
                }
            })),
            response: None,
            status: None,
            sse_events: None,
        },
        RouteSpec {
            method: "GET",
            path: "/journal",
            summary: "List journal entries",
            params: None,
            query: Some(serde_json::json!({
                "limit": { "type": "integer", "required": false, "description": "Max entries (default 100)" }
            })),
            request: None,
            request_inline: None,
            response: Some(schema_name::<JournalListResponse>()),
            status: None,
            sse_events: None,
        },
        RouteSpec {
            method: "GET",
            path: "/events",
            summary: "Server-Sent Events stream",
            params: None,
            query: None,
            request: None,
            request_inline: None,
            response: None,
            status: None,
            sse_events: Some(serde_json::json!({
                "snapshot": {
                    "description": "Full state on connect and reconnect",
                    "payload": {
                        "type": { "const": "snapshot" },
                        "jobs": { "type": "array", "items": schema_name::<Job>() },
                        "claims": { "type": "map", "values": schema_name::<ClaimState>() },
                        "deps": { "type": "map", "values": schema_name::<DepRecord>() },
                        "channels": { "type": "map", "values": schema_name::<ChannelStatus>() }
                    }
                },
                "job_update": {
                    "description": "A job changed state",
                    "payload": schema_name::<Job>()
                },
                "claim_update": {
                    "description": "A claim was created, updated, or removed",
                    "payload": { "key": "string", "claim": format!("{} | null", schema_name::<ClaimState>()) }
                },
                "channel_update": {
                    "description": "A channel status changed",
                    "payload": { "key": "string", "status": schema_name::<ChannelStatus>() }
                }
            })),
        },
    ]
}

/// All (method, path) pairs from the specs, for test assertions.
#[cfg(test)]
fn api_spec_paths() -> std::collections::HashSet<(&'static str, &'static str)> {
    api_specs().iter().map(|s| (s.method, s.path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure every API route in the router has a corresponding spec entry
    /// and vice versa. If this test fails, you added/removed a route in
    /// router() without updating api_specs().
    #[test]
    fn api_specs_match_router() {
        // The routes registered in router() — keep this in sync manually.
        // The point of this test is that forgetting to update ONE of these
        // two places is caught, rather than needing to remember both.
        let router_routes: std::collections::HashSet<(&str, &str)> = [
            ("GET", "/jobs"),
            ("POST", "/jobs"),
            ("GET", "/jobs/{key}"),
            ("GET", "/jobs/{key}/deps"),
            ("POST", "/jobs/{key}/requeue"),
            ("POST", "/jobs/{key}/close"),
            ("POST", "/jobs/{key}/channel/send"),
            ("GET", "/journal"),
            ("GET", "/events"),
        ]
        .into();

        let spec_routes = api_spec_paths();

        let missing_from_specs: Vec<_> = router_routes.difference(&spec_routes).collect();
        let missing_from_router: Vec<_> = spec_routes.difference(&router_routes).collect();

        assert!(
            missing_from_specs.is_empty(),
            "Routes in router() but missing from api_specs(): {missing_from_specs:?}"
        );
        assert!(
            missing_from_router.is_empty(),
            "Routes in api_specs() but missing from router(): {missing_from_router:?}"
        );
    }

    /// Ensure every type name referenced in api_specs() exists in the
    /// generated JSON schema $defs.
    #[test]
    fn api_spec_types_exist_in_schema() {
        let schema = chuggernaut_types::generate_schema();
        let schema_json = serde_json::to_value(&schema).unwrap();
        let defs = schema_json.get("$defs").expect("schema should have $defs");

        for spec in api_specs() {
            for type_name in spec.request.iter().chain(spec.response.iter()) {
                assert!(
                    defs.get(type_name).is_some(),
                    "api_specs references type '{type_name}' (route: {} {}) but it's not in schema $defs",
                    spec.method,
                    spec.path
                );
            }
        }
    }
}

/// Resolve the static directory containing index.html.
/// Checks CHUGGERNAUT_STATIC_DIR env var first, then falls back to
/// the workspace static/ directory (relative to CARGO_MANIFEST_DIR).
fn resolve_static_dir() -> Option<std::path::PathBuf> {
    let candidates = [
        std::env::var("CHUGGERNAUT_STATIC_DIR")
            .map(std::path::PathBuf::from)
            .ok(),
        Some(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("static"),
        ),
    ];
    for candidate in candidates.into_iter().flatten() {
        if candidate.join("index.html").exists() {
            return Some(candidate);
        }
    }
    None
}

/// Call at startup to verify static files are available.
/// Panics with a helpful message if index.html is not found.
pub fn check_static_dir() {
    if resolve_static_dir().is_none() {
        panic!(
            "static/index.html not found. Set CHUGGERNAUT_STATIC_DIR or ensure static/ exists.\n\
             In Docker, mount the static directory: -v ./static:/static -e CHUGGERNAUT_STATIC_DIR=/static"
        );
    }
}

pub fn router(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/", get(serve_spa))
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{key}", get(get_job))
        .route("/jobs/{key}/deps", get(get_job_deps))
        .route("/jobs/{key}/requeue", post(requeue_job))
        .route("/jobs/{key}/close", post(close_job))
        .route("/jobs/{key}/channel/send", post(channel_send))
        .route("/journal", get(list_journal))
        .route("/events", get(sse_events))
        .route("/schema", get(get_schema))
        .route("/api.json", get(get_api_manifest));

    if let Some(dir) = resolve_static_dir() {
        router = router.fallback_service(tower_http::services::ServeDir::new(dir));
    }

    router.with_state(state)
}

// ---------------------------------------------------------------------------
// SPA
// ---------------------------------------------------------------------------

async fn serve_spa() -> impl IntoResponse {
    // Read from disk on every request so mounted volumes get live updates.
    match resolve_static_dir() {
        Some(dir) => match std::fs::read_to_string(dir.join("index.html")) {
            Ok(content) => Html(content),
            Err(e) => Html(format!("<pre>Failed to read index.html: {e}</pre>")),
        },
        None => Html("<pre>static/index.html not found</pre>".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct JobsQuery {
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JournalQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct RequeueBody {
    target: RequeueTarget,
}

#[derive(Debug, Deserialize)]
struct CloseBody {
    #[serde(default)]
    revoke: bool,
}

#[derive(Debug, Deserialize)]
struct ChannelSendBody {
    message: String,
    #[serde(default = "default_sender")]
    sender: String,
}

fn default_sender() -> String {
    "ui".to_string()
}

// ---------------------------------------------------------------------------
// Job endpoints
// ---------------------------------------------------------------------------

async fn list_jobs(
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> impl IntoResponse {
    let jobs: Vec<Job> = state
        .jobs
        .iter()
        .filter(|entry| {
            if let Some(ref filter) = query.state {
                let state_str = serde_json::to_string(&entry.value().state).unwrap_or_default();
                // Remove quotes from the serialized state string
                let state_str = state_str.trim_matches('"');
                state_str == filter
            } else {
                true
            }
        })
        .map(|entry| entry.value().clone())
        .collect();

    Json(JobListResponse { jobs })
}

async fn get_job(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let job = match state.jobs.get(&key) {
        Some(j) => j.clone(),
        None => {
            // Try KV (might be archived)
            match kv_get::<Job>(&state.kv.jobs, &key).await? {
                Some((j, _)) => j,
                None => return Err(AppError::NotFound(format!("job not found: {key}"))),
            }
        }
    };

    let claim = kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .ok()
        .flatten()
        .map(|(c, _)| c);

    let activities = kv_get::<ActivityLog>(&state.kv.activities, &key)
        .await
        .ok()
        .flatten()
        .map(|(log, _)| log.entries)
        .unwrap_or_default();

    Ok(Json(JobDetailResponse {
        job,
        claim,
        activities,
    }))
}

async fn get_job_deps(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let dep_record = kv_get::<DepRecord>(&state.kv.deps, &key)
        .await?
        .map(|(r, _)| r);

    let (dependencies, all_done) = match dep_record {
        Some(record) => {
            let mut deps = Vec::new();
            let mut all_done = true;
            for dep_key in &record.depends_on {
                if let Some(job) = state.jobs.get(dep_key) {
                    if job.state != JobState::Done {
                        all_done = false;
                    }
                    deps.push(job.clone());
                }
            }
            (deps, all_done)
        }
        None => (Vec::new(), true),
    };

    Ok(Json(JobDepsResponse {
        dependencies,
        all_done,
    }))
}

async fn create_job(
    State(state): State<AppState>,
    Json(req): Json<CreateJobRequest>,
) -> Result<impl IntoResponse, AppError> {
    let key = crate::jobs::create_job(&state, req).await?;

    // Try to assign if OnDeck
    if let Some(job) = state.jobs.get(&key) {
        if job.state == JobState::OnDeck {
            let _ = crate::assignment::try_assign_job(&state, &key).await;
        }
    }

    Ok((StatusCode::CREATED, Json(CreateJobResponse { key })))
}

async fn requeue_job(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<RequeueBody>,
) -> Result<impl IntoResponse, AppError> {
    let req = RequeueRequest {
        job_key: key,
        target: body.target,
    };
    // Reuse the NATS handler logic by calling directly
    let target_state = match req.target {
        RequeueTarget::OnIce => JobState::OnIce,
        RequeueTarget::OnDeck => {
            if let Some((deps, _)) = kv_get::<DepRecord>(&state.kv.deps, &req.job_key).await? {
                let all_done = deps.depends_on.iter().all(|dep_key| {
                    state
                        .jobs
                        .get(dep_key)
                        .map(|j| j.state == JobState::Done)
                        .unwrap_or(false)
                });
                if all_done {
                    JobState::OnDeck
                } else {
                    JobState::Blocked
                }
            } else {
                JobState::OnDeck
            }
        }
    };

    crate::jobs::transition_job(&state, &req.job_key, target_state, "admin_requeue", None).await?;

    if target_state == JobState::OnDeck {
        let _ = crate::assignment::try_assign_job(&state, &req.job_key).await;
    }

    Ok(StatusCode::OK)
}

async fn close_job(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<CloseBody>,
) -> Result<impl IntoResponse, AppError> {
    let target = if body.revoke {
        JobState::Revoked
    } else {
        JobState::Done
    };

    let trigger = if body.revoke {
        "admin_revoke"
    } else {
        "admin_close"
    };

    crate::jobs::transition_job(&state, &key, target, trigger, None).await?;

    if target == JobState::Done {
        let unblocked = crate::deps::propagate_unblock(&state, &key).await?;
        for k in &unblocked {
            let _ = crate::assignment::try_assign_job(&state, k).await;
        }
    }

    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Channel send endpoint
// ---------------------------------------------------------------------------

async fn channel_send(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<ChannelSendBody>,
) -> Result<impl IntoResponse, AppError> {
    info!(
        job_key = key,
        sender = body.sender,
        message = body.message,
        "channel_send: received request"
    );

    // Verify job exists
    if !state.jobs.contains_key(&key) {
        warn!(job_key = key, "channel_send: job not found");
        return Err(AppError::NotFound(format!("job not found: {key}")));
    }

    let msg = ChannelMessage {
        sender: body.sender,
        body: body.message,
        timestamp: chrono::Utc::now(),
        message_id: uuid::Uuid::new_v4().to_string(),
        in_reply_to: None,
    };

    info!(
        job_key = key,
        message_id = msg.message_id,
        "channel_send: publishing to NATS inbox"
    );

    state
        .nats
        .publish_to(&subjects::CHANNEL_INBOX, &key, &msg)
        .await
        .map_err(|e| {
            error!(job_key = key, error = %e, "channel_send: NATS publish failed");
            AppError::Internal(DispatcherError::Nats(e.to_string()))
        })?;

    info!(job_key = key, "channel_send: published successfully");
    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// Journal endpoint
// ---------------------------------------------------------------------------

async fn list_journal(
    State(state): State<AppState>,
    Query(query): Query<JournalQuery>,
) -> impl IntoResponse {
    use futures::StreamExt;

    let limit = query.limit.unwrap_or(100);
    let mut entries = Vec::new();

    match state.kv.journal.keys().await {
        Ok(keys) => {
            tokio::pin!(keys);
            while let Some(key) = keys.next().await {
                if let Ok(key_str) = key {
                    if let Ok(Some((entry, _))) =
                        kv_get::<JournalEntry>(&state.kv.journal, &key_str).await
                    {
                        entries.push(entry);
                    }
                }
                if entries.len() >= limit {
                    break;
                }
            }
        }
        Err(e) => {
            warn!("failed to list journal keys: {e}");
        }
    }

    // Sort by timestamp descending
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    entries.truncate(limit);

    Json(JournalListResponse { entries })
}

// ---------------------------------------------------------------------------
// SSE endpoint
// ---------------------------------------------------------------------------

async fn sse_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let state = state.clone();

    let stream = async_stream::stream! {
        // Send initial snapshot with jobs, claims, deps, and channel statuses
        let jobs: Vec<Job> = state.jobs.iter().map(|e| e.value().clone()).collect();

        // Collect claims for all jobs
        let mut claims_map = serde_json::Map::new();
        for job in &jobs {
            if let Ok(Some((claim, _))) = kv_get::<ClaimState>(&state.kv.claims, &job.key).await {
                claims_map.insert(job.key.clone(), serde_json::to_value(&claim).unwrap());
            }
        }

        // Collect deps for all jobs
        let mut deps_map = serde_json::Map::new();
        for job in &jobs {
            if let Ok(Some((dep, _))) = kv_get::<DepRecord>(&state.kv.deps, &job.key).await {
                deps_map.insert(job.key.clone(), serde_json::to_value(&dep).unwrap());
            }
        }

        // Collect channel statuses for all jobs
        let mut channels_map = serde_json::Map::new();
        for job in &jobs {
            if let Ok(Some((ch, _))) = kv_get::<ChannelStatus>(&state.kv.channels, &job.key).await {
                channels_map.insert(job.key.clone(), serde_json::to_value(&ch).unwrap());
            }
        }

        let snapshot = serde_json::json!({
            "type": "snapshot",
            "jobs": jobs,
            "claims": claims_map,
            "deps": deps_map,
            "channels": channels_map,
        });
        yield Ok(Event::default().event("snapshot").json_data(snapshot).unwrap());

        // Watch for changes by subscribing to NATS transition stream + KV watches
        // Subscribe to job transitions for real-time updates
        let mut trans_sub = match state.nats.subscribe("chuggernaut.transitions.>").await {
            Ok(sub) => sub,
            Err(e) => {
                warn!("failed to subscribe to transitions: {e}");
                return;
            }
        };

        // Subscribe to worker outcomes for claim changes
        let mut outcome_sub = match state.nats.subscribe_subject(&subjects::WORKER_OUTCOME).await {
            Ok(sub) => sub,
            Err(e) => {
                warn!("failed to subscribe to outcomes: {e}");
                return;
            }
        };

        // Subscribe to worker heartbeats for claim updates
        let mut hb_sub = match state.nats.subscribe_subject(&subjects::WORKER_HEARTBEAT).await {
            Ok(sub) => sub,
            Err(e) => {
                warn!("failed to subscribe to heartbeats: {e}");
                return;
            }
        };

        // Subscribe to channel outbox messages (replies from Claude)
        let mut channel_sub = match state.nats.subscribe("chuggernaut.channel.*.outbox").await {
            Ok(sub) => sub,
            Err(e) => {
                warn!("failed to subscribe to channel outbox: {e}");
                return;
            }
        };

        // Poll channel statuses periodically (KV watch is complex; polling is simpler for v1)
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
        let mut last_channels: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        loop {
            tokio::select! {
                Some(msg) = futures::StreamExt::next(&mut trans_sub) => {
                    // A job transitioned — send the updated job
                    if let Ok(transition) = serde_json::from_slice::<JobTransition>(&msg.payload) {
                        if let Some(job) = state.jobs.get(&transition.job_key) {
                            let event = Event::default()
                                .event("job_update")
                                .json_data(job.value().clone())
                                .unwrap();
                            yield Ok(event);

                            // Also send claim update
                            let claim = kv_get::<ClaimState>(&state.kv.claims, &transition.job_key).await.ok().flatten().map(|(c,_)| c);
                            let claim_event = serde_json::json!({ "key": transition.job_key, "claim": claim });
                            yield Ok(Event::default().event("claim_update").json_data(claim_event).unwrap());
                        }
                    }
                }

                Some(msg) = futures::StreamExt::next(&mut outcome_sub) => {
                    if let Ok(outcome) = serde_json::from_slice::<WorkerOutcome>(&msg.payload) {
                        // Job state changed — send update after a brief delay for the handler to process
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        if let Some(job) = state.jobs.get(&outcome.job_key) {
                            yield Ok(Event::default().event("job_update").json_data(job.value().clone()).unwrap());
                        }
                        let claim = kv_get::<ClaimState>(&state.kv.claims, &outcome.job_key).await.ok().flatten().map(|(c,_)| c);
                        let claim_event = serde_json::json!({ "key": outcome.job_key, "claim": claim });
                        yield Ok(Event::default().event("claim_update").json_data(claim_event).unwrap());
                    }
                }

                Some(msg) = futures::StreamExt::next(&mut hb_sub) => {
                    if let Ok(hb) = serde_json::from_slice::<WorkerHeartbeat>(&msg.payload) {
                        if let Ok(Some((claim, _))) = kv_get::<ClaimState>(&state.kv.claims, &hb.job_key).await {
                            let claim_event = serde_json::json!({ "key": hb.job_key, "claim": claim });
                            yield Ok(Event::default().event("claim_update").json_data(claim_event).unwrap());
                        }
                    }
                }

                Some(msg) = futures::StreamExt::next(&mut channel_sub) => {
                    // Channel outbox message from Claude — extract job key from subject
                    // Subject format: chuggernaut.channel.{job_key}.outbox
                    let subject = msg.subject.as_str();
                    let parts: Vec<&str> = subject.split('.').collect();
                    if parts.len() >= 4 {
                        let job_key = parts[2];
                        if let Ok(channel_msg) = serde_json::from_slice::<ChannelMessage>(&msg.payload) {
                            info!(job_key, sender = channel_msg.sender.as_str(), "channel outbox message received");
                            let event = serde_json::json!({
                                "key": job_key,
                                "message": channel_msg,
                            });
                            yield Ok(Event::default().event("channel_message").json_data(event).unwrap());
                        }
                    }
                }

                _ = interval.tick() => {
                    // Poll channel statuses for changes
                    for entry in state.jobs.iter() {
                        let key = entry.key().clone();
                        if let Ok(Some((ch, _))) = kv_get::<ChannelStatus>(&state.kv.channels, &key).await {
                            let ch_json = serde_json::to_string(&ch).unwrap_or_default();
                            let changed = last_channels.get(&key).map(|prev| prev != &ch_json).unwrap_or(true);
                            if changed {
                                last_channels.insert(key.clone(), ch_json);
                                let event = serde_json::json!({ "key": key, "status": ch });
                                yield Ok(Event::default().event("channel_update").json_data(event).unwrap());
                            }
                        }
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// Schema endpoint
// ---------------------------------------------------------------------------

async fn get_schema() -> impl IntoResponse {
    let schema = chuggernaut_types::generate_schema();
    Json(schema)
}

async fn get_api_manifest() -> impl IntoResponse {
    let schema = chuggernaut_types::generate_schema();

    Json(serde_json::json!({
        "version": "0.1.0",
        "routes": api_specs(),
        "error_response": schema_name::<ErrorResponse>(),
        "schema": schema
    }))
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

enum AppError {
    NotFound(String),
    Internal(DispatcherError),
}

impl From<DispatcherError> for AppError {
    fn from(e: DispatcherError) -> Self {
        match &e {
            DispatcherError::JobNotFound(_) => AppError::NotFound(e.to_string()),
            _ => AppError::Internal(e),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::NotFound(msg) => {
                (StatusCode::NOT_FOUND, Json(ErrorResponse { error: msg })).into_response()
            }
            AppError::Internal(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
                .into_response(),
        }
    }
}

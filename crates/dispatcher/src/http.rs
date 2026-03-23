use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use futures::stream::Stream;
use serde::Deserialize;
use tracing::warn;

use forge2_types::*;

use crate::error::DispatcherError;
use crate::jobs::kv_get;
use crate::state::DispatcherState;

type AppState = Arc<DispatcherState>;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{key}", get(get_job))
        .route("/jobs/{key}/deps", get(get_job_deps))
        .route("/jobs/{key}/requeue", post(requeue_job))
        .route("/jobs/{key}/close", post(close_job))
        .route("/workers", get(list_workers))
        .route("/journal", get(list_journal))
        .route("/events", get(sse_events))
        .route("/schema", get(get_schema))
        .with_state(state)
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
        .await?
        .map(|(c, _)| c);

    let activities = kv_get::<ActivityLog>(&state.kv.activities, &key)
        .await?
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

    crate::jobs::transition_job(&state, &req.job_key, target_state, "admin_requeue", None)
        .await?;

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
// Worker endpoints
// ---------------------------------------------------------------------------

async fn list_workers(State(state): State<AppState>) -> impl IntoResponse {
    let workers: Vec<WorkerInfo> = state
        .workers
        .iter()
        .map(|entry| entry.value().clone())
        .collect();

    Json(WorkerListResponse { workers })
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
        // Send initial snapshot
        let jobs: Vec<Job> = state.jobs.iter().map(|e| e.value().clone()).collect();
        let workers: Vec<WorkerInfo> = state.workers.iter().map(|e| e.value().clone()).collect();

        let snapshot = serde_json::json!({
            "type": "snapshot",
            "jobs": jobs,
            "workers": workers,
        });
        yield Ok(Event::default().event("snapshot").json_data(snapshot).unwrap());

        // Watch for changes via a polling loop
        // (A production impl would use NATS KV watchers, but this is simpler for v1)
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            // Send a heartbeat to keep the connection alive
            yield Ok(Event::default().comment("keepalive"));
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// Schema endpoint
// ---------------------------------------------------------------------------

async fn get_schema() -> impl IntoResponse {
    let schema = forge2_types::generate_schema();
    Json(schema)
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
            AppError::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse { error: msg }),
            )
                .into_response(),
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

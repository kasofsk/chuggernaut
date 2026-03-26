use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::Barrier;
use tracing::{debug, error, info, warn};

use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs;
use crate::state::DispatcherState;

const HANDLER_COUNT: usize = 8;

/// Start all NATS subscription handlers.
/// Returns only after every handler has established its NATS subscription,
/// so callers can send messages immediately.
pub async fn start_handlers(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let barrier = Arc::new(Barrier::new(HANDLER_COUNT + 1));

    macro_rules! spawn_handler {
        ($name:expr, $handler:ident) => {
            let s = state.clone();
            let b = barrier.clone();
            tokio::spawn(async move {
                if let Err(e) = $handler(s, b).await {
                    error!(concat!(stringify!($handler), " failed: {}"), e);
                }
            });
        };
    }

    spawn_handler!("worker_heartbeat", handle_worker_heartbeat);
    spawn_handler!("worker_outcome", handle_worker_outcome);
    spawn_handler!("admin_create_job", handle_admin_create_job);
    spawn_handler!("admin_requeue", handle_admin_requeue);
    spawn_handler!("admin_close_job", handle_admin_close_job);
    spawn_handler!("review_decision", handle_review_decision);
    spawn_handler!("activity_append", handle_activity_append);
    spawn_handler!("journal_append", handle_journal_append);
    // Monitor events are routed directly to the channel by monitor.rs
    // (no NATS round-trip needed since monitor runs in the same process).

    // Start the single-threaded assignment task that processes all events
    crate::assignment::start_assignment_task(state.clone()).await;

    barrier.wait().await;
    info!("all NATS handlers subscribed");
    Ok(())
}

// ---------------------------------------------------------------------------
// Worker events
// ---------------------------------------------------------------------------

/// Heartbeat handler — stays concurrent (only touches claims KV + token tracker,
/// never mutates job state).
async fn handle_worker_heartbeat(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_subject(&subjects::WORKER_HEARTBEAT)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<WorkerHeartbeat>(&msg.payload) {
                Ok(hb) => {
                    if let Err(e) = crate::claims::renew_lease(&s, &hb.job_key, &hb.worker_id).await
                    {
                        debug!(
                            job_key = hb.job_key,
                            worker_id = hb.worker_id,
                            "heartbeat lease renewal failed: {e}"
                        );
                    }
                    // Record token usage and rate limit info from heartbeat
                    let has_token_data = hb.token_usage.is_some() || hb.rate_limit.is_some();
                    if has_token_data {
                        let mut tracker = s.token_tracker.write().await;
                        if let Some(ref usage) = hb.token_usage {
                            tracker.update_usage(&hb.job_key, usage, hb.cost_usd.unwrap_or(0.0));
                        }
                        if let Some(ref rl) = hb.rate_limit {
                            tracker.update_rate_limit(&hb.job_key, rl);
                        }
                    }
                }
                Err(e) => warn!("invalid heartbeat payload: {e}"),
            }
        });
    }
    Ok(())
}

/// Worker outcome — thin: deserialize and send to assignment task.
async fn handle_worker_outcome(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_subject(&subjects::WORKER_OUTCOME)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        match serde_json::from_slice::<WorkerOutcome>(&msg.payload) {
            Ok(outcome) => {
                crate::assignment::request_dispatch(
                    &state,
                    crate::state::DispatchRequest::WorkerOutcome(outcome),
                );
            }
            Err(e) => warn!("invalid outcome payload: {e}"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin commands
// ---------------------------------------------------------------------------

/// Create job — stays concurrent (creates new KV keys, no conflict with
/// existing job state). The subsequent AssignJob goes through the channel.
async fn handle_admin_create_job(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_request(&subjects::ADMIN_CREATE_JOB)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            let reply_payload = match serde_json::from_slice::<CreateJobRequest>(&msg.payload) {
                Ok(req) => match jobs::create_job(&s, req).await {
                    Ok(key) => {
                        // Reply immediately so the client isn't blocked by action dispatch
                        let resp = serde_json::to_vec(&CreateJobResponse { key: key.clone() })
                            .unwrap_or_default();
                        if let Some(reply) = msg.reply {
                            if let Err(e) = s
                                .nats
                                .publish_raw(reply.to_string(), bytes::Bytes::from(resp))
                                .await
                            {
                                warn!("failed to reply to create-job: {e}");
                            }
                            let _ = s.nats.raw().flush().await;
                        }
                        // Spawn assignment in background — reply is already sent
                        crate::assignment::request_dispatch(
                            &s,
                            crate::state::DispatchRequest::AssignJob { job_key: key },
                        );
                        return;
                    }
                    Err(e) => serde_json::to_vec(&ErrorResponse {
                        error: e.to_string(),
                    })
                    .unwrap_or_default(),
                },
                Err(e) => serde_json::to_vec(&ErrorResponse {
                    error: format!("invalid payload: {e}"),
                })
                .unwrap_or_default(),
            };
            if let Some(reply) = msg.reply
                && let Err(e) = s
                    .nats
                    .publish_raw(reply.to_string(), bytes::Bytes::from(reply_payload))
                    .await
            {
                warn!("failed to reply to create-job: {e}");
            }
        });
    }
    Ok(())
}

/// Admin requeue — thin: deserialize, send to channel, await reply, respond.
async fn handle_admin_requeue(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_request(&subjects::ADMIN_REQUEUE)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            let reply_payload = match serde_json::from_slice::<RequeueRequest>(&msg.payload) {
                Ok(req) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    crate::assignment::request_dispatch(
                        &s,
                        crate::state::DispatchRequest::AdminRequeue { req, reply: tx },
                    );
                    match rx.await {
                        Ok(Ok(())) => b"{}".to_vec(),
                        Ok(Err(e)) => {
                            serde_json::to_vec(&ErrorResponse { error: e }).unwrap_or_default()
                        }
                        Err(_) => serde_json::to_vec(&ErrorResponse {
                            error: "assignment task dropped reply".to_string(),
                        })
                        .unwrap_or_default(),
                    }
                }
                Err(e) => serde_json::to_vec(&ErrorResponse {
                    error: format!("invalid payload: {e}"),
                })
                .unwrap_or_default(),
            };
            if let Some(reply) = msg.reply
                && let Err(e) = s
                    .nats
                    .publish_raw(reply.to_string(), bytes::Bytes::from(reply_payload))
                    .await
            {
                warn!("failed to reply to requeue: {e}");
            }
        });
    }
    Ok(())
}

/// Admin close — thin: deserialize, send to channel, await reply, respond.
async fn handle_admin_close_job(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_request(&subjects::ADMIN_CLOSE_JOB)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            let reply_payload = match serde_json::from_slice::<CloseJobRequest>(&msg.payload) {
                Ok(req) => {
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    crate::assignment::request_dispatch(
                        &s,
                        crate::state::DispatchRequest::AdminClose { req, reply: tx },
                    );
                    match rx.await {
                        Ok(Ok(())) => b"{}".to_vec(),
                        Ok(Err(e)) => {
                            serde_json::to_vec(&ErrorResponse { error: e }).unwrap_or_default()
                        }
                        Err(_) => serde_json::to_vec(&ErrorResponse {
                            error: "assignment task dropped reply".to_string(),
                        })
                        .unwrap_or_default(),
                    }
                }
                Err(e) => serde_json::to_vec(&ErrorResponse {
                    error: format!("invalid payload: {e}"),
                })
                .unwrap_or_default(),
            };
            if let Some(reply) = msg.reply
                && let Err(e) = s
                    .nats
                    .publish_raw(reply.to_string(), bytes::Bytes::from(reply_payload))
                    .await
            {
                warn!("failed to reply to close-job: {e}");
            }
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Review decisions
// ---------------------------------------------------------------------------

/// Review decision — thin: deserialize and send to assignment task.
async fn handle_review_decision(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_subject(&subjects::REVIEW_DECISION)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        match serde_json::from_slice::<ReviewDecision>(&msg.payload) {
            Ok(decision) => {
                info!(
                    job_key = decision.job_key,
                    decision_type = ?decision.decision,
                    "routing review decision to assignment task"
                );
                crate::assignment::request_dispatch(
                    &state,
                    crate::state::DispatchRequest::ReviewDecision(decision),
                );
            }
            Err(e) => warn!("invalid review decision payload: {e}"),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Activity / Journal append — stay concurrent (logging only)
// ---------------------------------------------------------------------------

async fn handle_activity_append(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_subject(&subjects::ACTIVITY_APPEND)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<ActivityAppend>(&msg.payload) {
                Ok(append) => jobs::append_activity(&s, &append.job_key, append.entry).await,
                Err(e) => warn!("invalid activity append payload: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle_journal_append(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe_subject(&subjects::JOURNAL_APPEND)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<JournalAppend>(&msg.payload) {
                Ok(append) => {
                    let entry = JournalEntry {
                        timestamp: append.timestamp,
                        action: append.action,
                        job_key: append.job_key,
                        worker_id: append.worker_id,
                        details: append.details,
                    };
                    let key = format!("{}.0", append.timestamp.timestamp_nanos_opt().unwrap_or(0));
                    if let Err(e) = jobs::kv_put(&s.kv.journal, &key, &entry).await {
                        warn!("journal append failed: {e}");
                    }
                }
                Err(e) => warn!("invalid journal append payload: {e}"),
            }
        });
    }
    Ok(())
}

// Monitor events are no longer routed through NATS handlers — monitor.rs
// dispatches directly to the assignment task channel via publish_and_dispatch.

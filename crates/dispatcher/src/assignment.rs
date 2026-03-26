use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, info, warn};

use chuggernaut_types::*;

use crate::error::DispatcherResult;
use crate::state::{DispatchRequest, DispatcherState};

/// Count currently active actions by counting claims in NATS KV.
async fn active_action_count(state: &DispatcherState) -> usize {
    match state.kv.claims.keys().await {
        Ok(keys) => {
            use futures::StreamExt;
            keys.count().await
        }
        Err(_) => 0,
    }
}

/// Check if we have capacity to dispatch another action.
pub async fn has_capacity(state: &DispatcherState) -> bool {
    let max = state
        .max_concurrent_actions
        .load(std::sync::atomic::Ordering::Relaxed);
    if active_action_count(state).await >= max {
        return false;
    }

    if state.config.pause_on_overage {
        let tracker = state.token_tracker.read().await;
        if let Some(ref rl) = tracker.rate_limit
            && rl.is_using_overage
            && rl.resets_at > Utc::now()
        {
            debug!(
                resets_at = %rl.resets_at,
                rate_limit_type = %rl.rate_limit_type,
                reported_by = %rl.reported_by,
                "pausing dispatch: API in overage"
            );
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Internal dispatch functions (called only from the assignment loop)
// ---------------------------------------------------------------------------

/// Assign a specific OnDeck job. Called from the assignment task.
/// Also available for integration tests.
pub async fn assign_job(state: &Arc<DispatcherState>, job_key: &str) -> DispatcherResult<bool> {
    let job = match state.jobs.get(job_key) {
        Some(j) => j.clone(),
        None => return Ok(false),
    };

    if job.state != JobState::OnDeck && job.state != JobState::ChangesRequested {
        return Ok(false);
    }

    if !has_capacity(state).await {
        let active = active_action_count(state).await;
        debug!(
            job_key,
            active,
            max = state.config.max_concurrent_actions,
            "at capacity, job stays on-deck"
        );
        return Ok(false);
    }

    match crate::action_dispatch::dispatch_action(state, job_key, None, false).await {
        Ok(()) => Ok(true),
        Err(e) => {
            debug!(job_key, "action dispatch failed: {e}");
            Ok(false)
        }
    }
}

async fn assign_rework(
    state: &Arc<DispatcherState>,
    job_key: &str,
    review_feedback: &str,
) -> DispatcherResult<bool> {
    if !has_capacity(state).await {
        let active = active_action_count(state).await;
        debug!(
            job_key,
            active,
            max = state.config.max_concurrent_actions,
            "at capacity, rework stays queued"
        );
        return Ok(false);
    }

    match crate::action_dispatch::dispatch_action(state, job_key, Some(review_feedback), true).await
    {
        Ok(()) => Ok(true),
        Err(e) => {
            debug!(job_key, "rework action dispatch failed: {e}");
            Ok(false)
        }
    }
}

pub async fn dispatch_next(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    while has_capacity(state).await {
        let mut candidates: Vec<(String, u8)> = state
            .jobs
            .iter()
            .filter(|e| {
                let s = e.value().state;
                s == JobState::OnDeck || s == JobState::ChangesRequested
            })
            .map(|e| (e.key().clone(), e.value().priority))
            .collect();

        if candidates.is_empty() {
            break;
        }

        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        let (key, _) = &candidates[0];
        let dispatched = assign_job(state, key).await?;
        if !dispatched {
            break;
        }
    }
    Ok(())
}

async fn dispatch_review(
    state: &Arc<DispatcherState>,
    job_key: &str,
    pr_url: &str,
    review_level: &str,
) {
    // Log the full job state so we can diagnose why reviews keep dispatching
    let job_info = state.jobs.get(job_key).map(|j| {
        let j = j.value();
        format!(
            "state={:?} ci={:?} review={:?} retry={}",
            j.state, j.ci_status, j.review, j.retry_count
        )
    });
    info!(
        job_key,
        pr_url,
        review_level,
        job = job_info.as_deref().unwrap_or("not found"),
        "dispatch_review requested"
    );

    if !has_capacity(state).await {
        info!(job_key, "review deferred — at runner capacity");
        return;
    }

    if let Err(e) =
        crate::action_dispatch::dispatch_review_action(state, job_key, pr_url, review_level).await
    {
        warn!(job_key, error = %e, "review action dispatch failed");
    }
}

// ---------------------------------------------------------------------------
// Public: send dispatch requests (non-blocking, used by handlers)
// ---------------------------------------------------------------------------

/// Send a dispatch request to the assignment task.
/// This is fire-and-forget — the caller doesn't wait for the result.
pub fn request_dispatch(state: &DispatcherState, req: DispatchRequest) {
    if let Err(e) = state.dispatch_tx.send(req) {
        warn!("dispatch channel closed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Assignment task — single-threaded dispatch loop
// ---------------------------------------------------------------------------

/// Start the assignment task that processes dispatch requests sequentially.
/// Must be called once at startup. Takes ownership of the channel receiver.
pub async fn start_assignment_task(state: Arc<DispatcherState>) {
    let mut rx = state
        .dispatch_rx
        .lock()
        .await
        .take()
        .expect("assignment task started twice");

    tokio::spawn(async move {
        info!("assignment task started");
        while let Some(req) = rx.recv().await {
            if let Err(e) = handle_request(&state, req).await {
                warn!("assignment task error: {e}");
            }
        }
        info!("assignment task stopped");
    });
}

async fn handle_request(
    state: &Arc<DispatcherState>,
    req: DispatchRequest,
) -> DispatcherResult<()> {
    match req {
        DispatchRequest::TryDispatchNext => {
            dispatch_next(state).await?;
        }
        DispatchRequest::AssignJob { job_key } => {
            assign_job(state, &job_key).await?;
        }
        DispatchRequest::DispatchReview {
            job_key,
            pr_url,
            review_level,
        } => {
            dispatch_review(state, &job_key, &pr_url, &review_level).await;
        }
        DispatchRequest::AssignRework { job_key, feedback } => {
            assign_rework(state, &job_key, &feedback).await?;
        }
    }
    Ok(())
}

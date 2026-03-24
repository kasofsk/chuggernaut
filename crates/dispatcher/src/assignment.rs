use std::sync::Arc;

use tracing::debug;

use chuggernaut_types::*;

use crate::error::DispatcherResult;
use crate::state::DispatcherState;

/// Count currently active actions (jobs in OnTheStack state).
fn active_action_count(state: &DispatcherState) -> usize {
    state
        .jobs
        .iter()
        .filter(|e| e.value().state == JobState::OnTheStack)
        .count()
}

/// Check if we have capacity to dispatch another action.
fn has_capacity(state: &DispatcherState) -> bool {
    active_action_count(state) < state.config.max_concurrent_actions
}

/// Try to assign a specific OnDeck job by dispatching a Forgejo Action.
/// Respects the max_concurrent_actions limit — returns Ok(false) if at capacity.
pub async fn try_assign_job(
    state: &Arc<DispatcherState>,
    job_key: &str,
) -> DispatcherResult<bool> {
    let job = match state.jobs.get(job_key) {
        Some(j) => j.clone(),
        None => return Ok(false),
    };

    if job.state != JobState::OnDeck && job.state != JobState::ChangesRequested {
        return Ok(false);
    }

    if !has_capacity(state) {
        debug!(
            job_key,
            active = active_action_count(state),
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

/// Try to assign a job as a rework with review feedback.
/// Respects the max_concurrent_actions limit.
pub async fn try_assign_rework(
    state: &Arc<DispatcherState>,
    job_key: &str,
    review_feedback: &str,
) -> DispatcherResult<bool> {
    if !has_capacity(state) {
        debug!(
            job_key,
            active = active_action_count(state),
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

/// Scan OnDeck jobs by priority and dispatch up to available capacity.
/// Called when a slot frees up (outcome received, lease expired, admin close).
pub async fn try_dispatch_next(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    while has_capacity(state) {
        // Collect OnDeck and ChangesRequested jobs, sorted by priority (highest first)
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

        candidates.sort_by(|a, b| b.1.cmp(&a.1)); // highest priority first

        let (key, _) = &candidates[0];
        let dispatched = try_assign_job(state, key).await?;
        if !dispatched {
            break; // capacity check inside try_assign_job failed, or dispatch error
        }
    }
    Ok(())
}

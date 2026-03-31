use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, info, warn};

use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs;
use crate::state::{DispatchRequest, DispatcherState};

// ---------------------------------------------------------------------------
// Capacity helpers
// ---------------------------------------------------------------------------

/// Count currently active actions via the in-memory atomic counter.
/// (Avoids streaming KV keys, which can hang on tombstone-only buckets.)
fn active_action_count(state: &DispatcherState) -> usize {
    state
        .active_claims
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// Check if we have capacity to dispatch another action.
pub async fn has_capacity(state: &DispatcherState) -> bool {
    if state.paused.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }

    let max = state
        .max_concurrent_actions
        .load(std::sync::atomic::Ordering::Relaxed);
    if active_action_count(state) >= max {
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
// Dispatch functions
// ---------------------------------------------------------------------------

/// Assign a specific OnDeck job.
pub async fn assign_job(state: &Arc<DispatcherState>, job_key: &str) -> DispatcherResult<bool> {
    let job = match state.jobs.get(job_key) {
        Some(j) => j.clone(),
        None => return Ok(false),
    };

    if job.state != JobState::OnDeck && job.state != JobState::ChangesRequested {
        return Ok(false);
    }

    if !has_capacity(state).await {
        let active = active_action_count(state);
        debug!(
            job_key,
            active,
            max = state.config.max_concurrent_actions,
            "at capacity, job stays on-deck"
        );
        return Ok(false);
    }

    let is_rework = job.state == JobState::ChangesRequested;
    match crate::action_dispatch::dispatch_action(state, job_key, None, is_rework).await {
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
        let active = active_action_count(state);
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
        // Prefer dispatching pending reviews over starting new work.
        // This reduces merge conflicts by finishing in-flight jobs first.
        let review_candidate: Option<(String, String, String)> = state
            .jobs
            .iter()
            .find(|e| {
                let job = e.value();
                job.state == JobState::InReview
                    && job.ci_status == Some(CiStatus::Success)
                    && job.pr_url.is_some()
                    && !job.merge_conflict
            })
            .map(|e| {
                let job = e.value();
                (
                    e.key().clone(),
                    job.pr_url.clone().unwrap(),
                    format!("{:?}", job.review).to_lowercase(),
                )
            });

        if let Some((job_key, pr_url, review_level)) = review_candidate {
            info!(job_key, "dispatch_next: prioritising pending review");
            if dispatch_review(state, &job_key, &pr_url, &review_level).await {
                continue;
            }
            // Review dispatch failed (job reverted to InReview) — stop to
            // avoid retrying the same candidate in a tight loop.
            break;
        }

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

/// Dispatch a review action if capacity is available. Returns true if a
/// review was successfully dispatched (capacity slot consumed).
async fn dispatch_review(
    state: &Arc<DispatcherState>,
    job_key: &str,
    pr_url: &str,
    review_level: &str,
) -> bool {
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
        return false;
    }

    match crate::action_dispatch::dispatch_review_action(state, job_key, pr_url, review_level).await
    {
        Ok(()) => true,
        Err(e) => {
            warn!(job_key, error = %e, "review action dispatch failed");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Event processing — all called sequentially from the assignment task
// ---------------------------------------------------------------------------

/// Append a token usage record for one action invocation onto the job.
async fn append_token_record(
    state: &Arc<DispatcherState>,
    job_key: &str,
    action_type: &str,
    usage: TokenUsage,
) -> DispatcherResult<()> {
    let at = action_type.to_string();
    let (job, _) = jobs::kv_cas_rmw::<Job, _>(
        &state.kv.jobs,
        job_key,
        state.config.cas_max_retries,
        |job| {
            job.token_usage.push(ActionTokenRecord {
                action_type: at.clone(),
                token_usage: usage.clone(),
                completed_at: Utc::now(),
            });
            job.updated_at = Utc::now();
            Ok(())
        },
    )
    .await?;
    state.jobs.insert(job_key.to_string(), job);
    Ok(())
}

/// Schedule auto-retry for a failed job (sets retry_count and retry_after).
pub async fn schedule_auto_retry(state: &Arc<DispatcherState>, job_key: &str) {
    let should_retry = state
        .jobs
        .get(job_key)
        .map(|job| job.retry_count < job.max_retries)
        .unwrap_or(false);

    if should_retry {
        match jobs::kv_cas_rmw::<Job, _>(
            &state.kv.jobs,
            job_key,
            state.config.cas_max_retries,
            |j| {
                let backoff_secs = std::cmp::min(30 * (1u64 << j.retry_count), 600);
                let retry_after = Utc::now() + chrono::Duration::seconds(backoff_secs as i64);
                j.retry_count += 1;
                j.retry_after = Some(retry_after);
                j.updated_at = Utc::now();
                Ok(())
            },
        )
        .await
        {
            Ok((j, _)) => {
                state.jobs.insert(job_key.to_string(), j.clone());
                debug!(job_key, retry_after = ?j.retry_after, "auto-retry scheduled");
            }
            Err(e) => {
                warn!(job_key, "failed to schedule auto-retry: {e}");
            }
        }
    }
}

/// Process a worker outcome (Yield or Fail).
/// Claim release is handled by transition_job's auto-release.
async fn process_outcome(
    state: &Arc<DispatcherState>,
    outcome: WorkerOutcome,
) -> DispatcherResult<()> {
    let job_key = &outcome.job_key;
    let worker_id = &outcome.worker_id;

    // Verify job is actually OnTheStack — ignore stale outcomes
    let current_state = state.jobs.get(job_key).map(|j| j.state);
    if current_state != Some(JobState::OnTheStack) {
        debug!(
            job_key,
            ?current_state,
            "ignoring stale worker outcome (not OnTheStack)"
        );
        return Ok(());
    }

    // Clean up token tracker and action URL for this worker
    state.token_tracker.write().await.remove_worker(job_key);
    state.action_urls.remove(job_key);

    // Record token usage for this action
    if let Some(usage) = outcome.token_usage {
        append_token_record(state, job_key, "work", usage).await?;
    }

    match outcome.outcome {
        OutcomeType::Yield { pr_url, partial } => {
            let url = pr_url.clone();
            let (job, _) = jobs::kv_cas_rmw::<Job, _>(
                &state.kv.jobs,
                job_key,
                state.config.cas_max_retries,
                |job| {
                    job.pr_url = Some(url.clone());
                    job.updated_at = Utc::now();
                    Ok(())
                },
            )
            .await?;
            state.jobs.insert(job_key.to_string(), job.clone());

            if partial && job.continuation_count < state.config.max_continuations {
                // --- Partial yield: dispatch continuation worker ---
                let cont_num = job.continuation_count + 1;
                let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
                    &state.kv.jobs,
                    job_key,
                    state.config.cas_max_retries,
                    |job| {
                        job.continuation_count += 1;
                        job.updated_at = Utc::now();
                        Ok(())
                    },
                )
                .await?;
                state.jobs.insert(job_key.to_string(), updated);

                // Transition OnTheStack -> OnDeck for re-dispatch
                // (auto-releases the claim)
                jobs::transition_job(
                    state,
                    job_key,
                    JobState::OnDeck,
                    "partial_yield_continuation",
                    Some(worker_id),
                )
                .await?;

                let entry = ActivityEntry {
                    timestamp: Utc::now(),
                    kind: "continuation".to_string(),
                    message: format!("Partial yield #{cont_num}, re-dispatching continuation"),
                };
                jobs::append_activity(state, job_key, entry).await;

                jobs::journal_append(
                    state,
                    "partial_yield",
                    Some(job_key),
                    Some(worker_id),
                    Some(&format!("pr_url: {pr_url}, continuation #{cont_num}")),
                )
                .await;

                // Dispatch continuation using rework path (checks out existing branch)
                let continuation_context = format!(
                    "CONTINUATION: A previous worker ran out of time/budget and yielded partial work. \
                     A PR exists at {pr_url}. You are on the same branch with the previous commits. \
                     Review what was accomplished, then complete the remaining work."
                );
                // Process rework inline (we're already in the sequential task)
                assign_rework(state, job_key, &continuation_context).await?;
            } else {
                // --- Complete yield (or max continuations reached): await CI ---
                if partial {
                    info!(job_key, "max continuations reached, proceeding to CI check");
                }

                // Set CI polling state
                let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
                    &state.kv.jobs,
                    job_key,
                    state.config.cas_max_retries,
                    |job| {
                        job.ci_status = Some(CiStatus::Pending);
                        job.ci_check_since = Some(Utc::now());
                        job.updated_at = Utc::now();
                        Ok(())
                    },
                )
                .await?;
                state.jobs.insert(job_key.to_string(), updated);

                // Transition to InReview (auto-releases the claim)
                jobs::transition_job(
                    state,
                    job_key,
                    JobState::InReview,
                    "worker_yield",
                    Some(worker_id),
                )
                .await?;

                jobs::journal_append(
                    state,
                    "yield",
                    Some(job_key),
                    Some(worker_id),
                    Some(&format!("pr_url: {pr_url}, awaiting CI")),
                )
                .await;
            }

            // Slot freed — try to dispatch next queued job
            dispatch_next(state).await?;
        }
        OutcomeType::Fail { reason, logs: _ } => {
            // Transition to Failed (auto-releases the claim)
            jobs::transition_job(
                state,
                job_key,
                JobState::Failed,
                "worker_fail",
                Some(worker_id),
            )
            .await?;

            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: "failed".to_string(),
                message: format!("Worker failed: {reason}"),
            };
            jobs::append_activity(state, job_key, entry).await;

            schedule_auto_retry(state, job_key).await;

            jobs::journal_append(
                state,
                "failed",
                Some(job_key),
                Some(worker_id),
                Some(&reason),
            )
            .await;

            // Slot freed — try to dispatch next queued job
            dispatch_next(state).await?;
        }
    }
    Ok(())
}

/// Process a review decision (Approved, ChangesRequested, Escalated).
/// Claim release is handled by transition_job's auto-release.
async fn process_review_decision(
    state: &Arc<DispatcherState>,
    decision: ReviewDecision,
) -> DispatcherResult<()> {
    let job_key = &decision.job_key;

    // Verify job is in a review-relevant state — ignore truly stale decisions.
    // Accept both Reviewing (normal) and InReview (recovery reverted, or review
    // decision arrived before dispatch completed).
    let current_state = state.jobs.get(job_key).map(|j| j.state);
    if current_state != Some(JobState::Reviewing) && current_state != Some(JobState::InReview) {
        debug!(
            job_key,
            ?current_state,
            "ignoring stale review decision (not Reviewing or InReview)"
        );
        return Ok(());
    }

    // Clean up token tracker and action URL for this worker
    state.token_tracker.write().await.remove_worker(job_key);
    state.action_urls.remove(job_key);

    // Record token usage for this review action
    if let Some(usage) = decision.token_usage {
        append_token_record(state, job_key, "review", usage).await?;
    }

    match decision.decision {
        DecisionType::Approved => {
            // Transition Reviewing → Done (auto-releases claim)
            jobs::transition_job(state, job_key, JobState::Done, "review_approved", None).await?;
            let unblocked = crate::deps::propagate_unblock(state, job_key).await?;
            for key in &unblocked {
                assign_job(state, key).await?;
            }
            jobs::journal_append(state, "review_approved", Some(job_key), None, None).await;
        }
        DecisionType::ChangesRequested { feedback } => {
            // Check rework limit before dispatching rework
            let (rework_count, effective_limit) = state
                .jobs
                .get(job_key)
                .map(|j| {
                    (
                        j.rework_count,
                        j.rework_limit.unwrap_or(state.config.rework_limit),
                    )
                })
                .unwrap_or((0, state.config.rework_limit));

            if rework_count >= effective_limit {
                // Rework limit exceeded — escalate to human
                jobs::transition_job(
                    state,
                    job_key,
                    JobState::Escalated,
                    "rework_limit_exceeded",
                    None,
                )
                .await?;
                jobs::journal_append(
                    state,
                    "rework_limit_exceeded",
                    Some(job_key),
                    None,
                    Some(&format!(
                        "rework_count={rework_count}, limit={effective_limit}",
                    )),
                )
                .await;
            } else {
                // Increment rework count
                let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
                    &state.kv.jobs,
                    job_key,
                    state.config.cas_max_retries,
                    |j| {
                        j.rework_count += 1;
                        j.updated_at = Utc::now();
                        Ok(())
                    },
                )
                .await?;
                state.jobs.insert(job_key.to_string(), updated);

                // Transition Reviewing → ChangesRequested (auto-releases claim)
                jobs::transition_job(
                    state,
                    job_key,
                    JobState::ChangesRequested,
                    "review_changes_requested",
                    None,
                )
                .await?;

                // Dispatch rework inline
                assign_rework(state, job_key, &feedback).await?;

                jobs::journal_append(
                    state,
                    "review_changes_requested",
                    Some(job_key),
                    None,
                    Some(&feedback),
                )
                .await;
            }
        }
        DecisionType::MergeConflict { error } => {
            // Code was approved but merge failed — flag for auto-merge after rework.
            // Don't increment rework_count (this isn't a code quality issue).
            let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
                &state.kv.jobs,
                job_key,
                state.config.cas_max_retries,
                |j| {
                    j.merge_conflict = true;
                    j.updated_at = Utc::now();
                    Ok(())
                },
            )
            .await?;
            state.jobs.insert(job_key.to_string(), updated);

            jobs::transition_job(
                state,
                job_key,
                JobState::ChangesRequested,
                "merge_conflict",
                None,
            )
            .await?;

            let feedback = format!(
                "The code review passed but the PR has merge conflicts and cannot be merged.\n\n\
                 Error: {error}\n\n\
                 Please rebase your branch onto main, resolve any merge conflicts, \
                 and push the result. Do NOT change any logic — only resolve conflicts \
                 to produce a clean merge."
            );
            assign_rework(state, job_key, &feedback).await?;

            jobs::journal_append(
                state,
                "merge_conflict",
                Some(job_key),
                None,
                Some(&format!("merge failed: {error}")),
            )
            .await;
        }
        DecisionType::Escalated { reviewer_login } => {
            // Transition Reviewing → Escalated (auto-releases claim)
            jobs::transition_job(
                state,
                job_key,
                JobState::Escalated,
                "review_escalated",
                None,
            )
            .await?;
            jobs::journal_append(
                state,
                "review_escalated",
                Some(job_key),
                None,
                Some(&format!("human reviewer: {reviewer_login}")),
            )
            .await;
        }
    }

    // Slot freed — try to dispatch next queued job
    dispatch_next(state).await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Monitor event handlers
// ---------------------------------------------------------------------------

async fn handle_lease_expired(
    state: &Arc<DispatcherState>,
    event: LeaseExpiredEvent,
) -> DispatcherResult<()> {
    // Re-verify: claim must still exist and still be expired
    if let Some((claim, _)) = jobs::kv_get::<ClaimState>(&state.kv.claims, &event.job_key).await?
        && claim.lease_deadline < Utc::now()
    {
        // Verify job is still in an active state
        let is_active = state
            .jobs
            .get(&event.job_key)
            .map(|j| j.state == JobState::OnTheStack || j.state == JobState::Reviewing)
            .unwrap_or(false);
        if !is_active {
            return Ok(());
        }

        // Reviewing jobs go back to InReview so the monitor can re-dispatch.
        // Work jobs (OnTheStack) go to Failed for retry.
        let current_state = state.jobs.get(&event.job_key).map(|j| j.state);
        let (target_state, trigger) = if current_state == Some(JobState::Reviewing) {
            (JobState::InReview, "review_lease_expired")
        } else {
            (JobState::Failed, "lease_expired")
        };

        info!(
            job_key = event.job_key,
            worker_id = event.worker_id,
            ?target_state,
            "lease expired, transitioning job"
        );
        jobs::transition_job(
            state,
            &event.job_key,
            target_state,
            trigger,
            Some(&event.worker_id),
        )
        .await?;
        let entry = ActivityEntry {
            timestamp: Utc::now(),
            kind: trigger.to_string(),
            message: format!("Worker {} lease expired", event.worker_id),
        };
        jobs::append_activity(state, &event.job_key, entry).await;

        if target_state == JobState::Failed {
            schedule_auto_retry(state, &event.job_key).await;
        }

        // Slot freed
        dispatch_next(state).await?;
    }
    Ok(())
}

async fn handle_job_timeout(
    state: &Arc<DispatcherState>,
    event: JobTimeoutEvent,
) -> DispatcherResult<()> {
    if let Some((claim, _)) = jobs::kv_get::<ClaimState>(&state.kv.claims, &event.job_key).await? {
        let elapsed = (Utc::now() - claim.claimed_at).num_seconds() as u64;
        if elapsed > claim.timeout_secs {
            // Verify job is still in an active state
            let is_active = state
                .jobs
                .get(&event.job_key)
                .map(|j| j.state == JobState::OnTheStack || j.state == JobState::Reviewing)
                .unwrap_or(false);
            if !is_active {
                return Ok(());
            }

            // Reviewing jobs go back to InReview so the monitor can re-dispatch.
            // Work jobs (OnTheStack) go to Failed for retry.
            let current_state = state.jobs.get(&event.job_key).map(|j| j.state);
            let (target_state, trigger) = if current_state == Some(JobState::Reviewing) {
                (JobState::InReview, "review_timeout")
            } else {
                (JobState::Failed, "job_timeout")
            };

            info!(
                job_key = event.job_key,
                worker_id = event.worker_id,
                ?target_state,
                "job timeout, transitioning job"
            );
            jobs::transition_job(
                state,
                &event.job_key,
                target_state,
                trigger,
                Some(&event.worker_id),
            )
            .await?;
            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: trigger.to_string(),
                message: format!(
                    "Job timed out after {}s (limit: {}s)",
                    elapsed, claim.timeout_secs
                ),
            };
            jobs::append_activity(state, &event.job_key, entry).await;

            if target_state == JobState::Failed {
                schedule_auto_retry(state, &event.job_key).await;
            }

            // Slot freed
            dispatch_next(state).await?;
        }
    }
    Ok(())
}

/// Orphan detected: OnTheStack job with no claim. Actually fix it now
/// (previously this was advisory-only and required a restart to repair).
async fn handle_orphan(
    state: &Arc<DispatcherState>,
    event: OrphanDetectedEvent,
) -> DispatcherResult<()> {
    match event.kind {
        OrphanKind::ClaimlessOnTheStack => {
            // Re-verify: job still OnTheStack and still no claim
            let is_on_stack = state
                .jobs
                .get(&event.job_key)
                .map(|j| j.state == JobState::OnTheStack)
                .unwrap_or(false);
            if !is_on_stack {
                return Ok(());
            }
            if let Ok(Some(_)) = jobs::kv_get::<ClaimState>(&state.kv.claims, &event.job_key).await
            {
                return Ok(()); // claim appeared, no longer orphaned
            }

            warn!(
                job_key = event.job_key,
                "orphan detected — transitioning to Failed"
            );
            jobs::transition_job(
                state,
                &event.job_key,
                JobState::Failed,
                "orphan_no_claim",
                None,
            )
            .await?;

            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: "orphan".to_string(),
                message: "Job was OnTheStack with no claim — failed by monitor".to_string(),
            };
            jobs::append_activity(state, &event.job_key, entry).await;

            schedule_auto_retry(state, &event.job_key).await;

            // Slot freed
            dispatch_next(state).await?;
        }
    }
    Ok(())
}

async fn handle_retry(
    state: &Arc<DispatcherState>,
    event: RetryEligibleEvent,
) -> DispatcherResult<()> {
    // Re-verify: job still Failed and retry conditions still met
    let eligible = state
        .jobs
        .get(&event.job_key)
        .map(|job| {
            job.state == JobState::Failed
                && job.retry_count <= job.max_retries
                && job.retry_after.is_some_and(|ra| ra <= Utc::now())
        })
        .unwrap_or(false);

    if eligible {
        info!(
            job_key = event.job_key,
            retry_count = event.retry_count,
            "retry eligible, requeueing"
        );
        jobs::transition_job(state, &event.job_key, JobState::OnDeck, "auto_retry", None).await?;
        assign_job(state, &event.job_key).await?;
    }
    Ok(())
}

async fn auto_merge_after_conflict(
    state: &Arc<DispatcherState>,
    job_key: &str,
    pr_url: &str,
) -> DispatcherResult<()> {
    let provider: Box<dyn chuggernaut_git_provider::GitProvider> =
        match (&state.config.git_url, &state.config.git_token) {
            (Some(url), Some(token)) => crate::provider::create_provider(url, token),
            _ => {
                warn!(job_key, "no git provider configured — cannot auto-merge");
                return Ok(());
            }
        };

    let job = state.jobs.get(job_key).map(|j| j.clone());
    let repo = match &job {
        Some(j) => &j.repo,
        None => return Ok(()),
    };
    let parts: Vec<&str> = repo.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Ok(());
    }
    let (owner, repo_name) = (parts[0], parts[1]);

    let pr_index = match chuggernaut_types::parse_pr_url_index(pr_url) {
        Some(idx) => idx,
        None => {
            warn!(job_key, pr_url, "cannot parse PR index for auto-merge");
            return Ok(());
        }
    };

    match provider
        .merge_pull_request(
            owner,
            repo_name,
            pr_index,
            chuggernaut_git_provider::MergePullRequest {
                method: chuggernaut_git_provider::MergeMethod::Merge,
                message: None,
            },
        )
        .await
    {
        Ok(()) => {
            info!(job_key, "auto-merged after merge-conflict rework");
            // Clear the flag and transition to Done
            let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
                &state.kv.jobs,
                job_key,
                state.config.cas_max_retries,
                |j| {
                    j.merge_conflict = false;
                    j.updated_at = Utc::now();
                    Ok(())
                },
            )
            .await?;
            state.jobs.insert(job_key.to_string(), updated);

            jobs::transition_job(state, job_key, JobState::Done, "auto_merge", None).await?;
            let unblocked = crate::deps::propagate_unblock(state, job_key).await?;
            for key in &unblocked {
                assign_job(state, key).await?;
            }

            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: "auto_merged".to_string(),
                message: "Auto-merged after merge-conflict resolution (review already passed)"
                    .to_string(),
            };
            jobs::append_activity(state, job_key, entry).await;
            jobs::journal_append(state, "auto_merged", Some(job_key), None, None).await;
        }
        Err(e) => {
            // Merge still failing — fall back to normal review dispatch
            warn!(
                job_key,
                error = %e,
                "auto-merge failed after conflict rework — dispatching review"
            );
            // Clear the flag so we don't loop
            let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
                &state.kv.jobs,
                job_key,
                state.config.cas_max_retries,
                |j| {
                    j.merge_conflict = false;
                    j.updated_at = Utc::now();
                    Ok(())
                },
            )
            .await?;
            state.jobs.insert(job_key.to_string(), updated);

            let review_level = state
                .jobs
                .get(job_key)
                .map(|j| format!("{:?}", j.review).to_lowercase())
                .unwrap_or_else(|| "high".to_string());
            dispatch_review(state, job_key, pr_url, &review_level).await;
        }
    }

    Ok(())
}

async fn handle_ci_check(
    state: &Arc<DispatcherState>,
    event: CiCheckEvent,
) -> DispatcherResult<()> {
    let job = match state.jobs.get(&event.job_key) {
        Some(j) => j.clone(),
        None => return Ok(()),
    };
    if job.state != JobState::InReview {
        return Ok(());
    }

    // Update CI status on job
    let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
        &state.kv.jobs,
        &event.job_key,
        state.config.cas_max_retries,
        |job| {
            job.ci_status = Some(event.ci_status);
            job.updated_at = Utc::now();
            Ok(())
        },
    )
    .await?;
    state.jobs.insert(event.job_key.clone(), updated);

    match event.ci_status {
        CiStatus::Success if job.merge_conflict => {
            // Merge-conflict rework complete + CI passed → auto-merge (skip re-review)
            info!(
                job_key = event.job_key,
                "CI passed after merge-conflict rework — attempting auto-merge"
            );
            auto_merge_after_conflict(state, &event.job_key, &event.pr_url).await?;
        }
        CiStatus::Success => {
            // CI passed — dispatch review inline
            let review_level = state
                .jobs
                .get(&event.job_key)
                .map(|j| format!("{:?}", j.review).to_lowercase())
                .unwrap_or_else(|| "high".to_string());

            dispatch_review(state, &event.job_key, &event.pr_url, &review_level).await;

            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: "ci_passed".to_string(),
                message: "CI checks passed, dispatching review".to_string(),
            };
            jobs::append_activity(state, &event.job_key, entry).await;

            jobs::journal_append(
                state,
                "ci_passed",
                Some(&event.job_key),
                None,
                Some("CI passed, review dispatched"),
            )
            .await;
        }
        CiStatus::Failure | CiStatus::Error => {
            // Check rework limit before dispatching CI fix
            let (rework_count, effective_limit) = state
                .jobs
                .get(&event.job_key)
                .map(|j| {
                    (
                        j.rework_count,
                        j.rework_limit.unwrap_or(state.config.rework_limit),
                    )
                })
                .unwrap_or((0, state.config.rework_limit));

            if rework_count >= effective_limit {
                // Rework limit exceeded — escalate
                jobs::transition_job(
                    state,
                    &event.job_key,
                    JobState::Escalated,
                    "ci_failed_rework_limit",
                    None,
                )
                .await?;
                jobs::journal_append(
                    state,
                    "ci_failed_rework_limit",
                    Some(&event.job_key),
                    None,
                    Some(&format!(
                        "CI failed but rework_count={rework_count} >= limit={effective_limit}",
                    )),
                )
                .await;
            } else {
                // Increment rework count
                let (updated, _) = jobs::kv_cas_rmw::<Job, _>(
                    &state.kv.jobs,
                    &event.job_key,
                    state.config.cas_max_retries,
                    |j| {
                        j.rework_count += 1;
                        j.updated_at = Utc::now();
                        Ok(())
                    },
                )
                .await?;
                state.jobs.insert(event.job_key.clone(), updated);

                // CI failed — transition to ChangesRequested and dispatch fix-up worker
                jobs::transition_job(
                    state,
                    &event.job_key,
                    JobState::ChangesRequested,
                    "ci_failed",
                    None,
                )
                .await?;

                let ci_feedback = format!(
                    "CI checks have failed on the PR at {}. \
                     Please investigate the CI failure, fix the issues, and push the fixes. \
                     Check the CI status and logs for details.",
                    event.pr_url
                );

                assign_rework(state, &event.job_key, &ci_feedback).await?;

                let entry = ActivityEntry {
                    timestamp: Utc::now(),
                    kind: "ci_failed".to_string(),
                    message: "CI checks failed, dispatching fix-up worker".to_string(),
                };
                jobs::append_activity(state, &event.job_key, entry).await;

                jobs::journal_append(
                    state,
                    "ci_failed",
                    Some(&event.job_key),
                    None,
                    Some(&format!("pr_url: {}, dispatching fix", event.pr_url)),
                )
                .await;
            }
        }
        CiStatus::Pending => {
            // Should not happen — monitor only emits non-pending events
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin command handlers
// ---------------------------------------------------------------------------

async fn process_requeue(
    state: &Arc<DispatcherState>,
    req: &RequeueRequest,
) -> DispatcherResult<()> {
    let target_state = match req.target {
        RequeueTarget::OnIce => JobState::OnIce,
        RequeueTarget::OnDeck => {
            if !state.jobs.contains_key(&req.job_key) {
                return Err(DispatcherError::JobNotFound(req.job_key.clone()));
            }
            if let Some((deps, _)) = jobs::kv_get::<DepRecord>(&state.kv.deps, &req.job_key).await?
            {
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

    // Transition (auto-releases claim if leaving OnTheStack/Reviewing)
    jobs::transition_job(state, &req.job_key, target_state, "admin_requeue", None).await?;
    if target_state == JobState::OnDeck {
        assign_job(state, &req.job_key).await?;
    }
    jobs::journal_append(
        state,
        "requeued",
        Some(&req.job_key),
        None,
        Some(&format!("target: {target_state:?}")),
    )
    .await;
    Ok(())
}

async fn process_close_job(
    state: &Arc<DispatcherState>,
    req: &CloseJobRequest,
) -> DispatcherResult<()> {
    let target = if req.revoke {
        JobState::Revoked
    } else {
        JobState::Done
    };
    let trigger = if req.revoke {
        "admin_revoke"
    } else {
        "admin_close"
    };

    // Transition (auto-releases claim if leaving OnTheStack/Reviewing)
    jobs::transition_job(state, &req.job_key, target, trigger, None).await?;
    if target == JobState::Done {
        let unblocked = crate::deps::propagate_unblock(state, &req.job_key).await?;
        for key in &unblocked {
            assign_job(state, key).await?;
        }
    }
    jobs::journal_append(state, trigger, Some(&req.job_key), None, None).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Public: send dispatch requests (non-blocking, used by handlers & monitor)
// ---------------------------------------------------------------------------

/// Send a dispatch request to the assignment task.
/// This is fire-and-forget — the caller doesn't wait for the result.
pub fn request_dispatch(state: &DispatcherState, req: DispatchRequest) {
    if let Err(e) = state.dispatch_tx.send(req) {
        warn!("dispatch channel closed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Assignment task — single-threaded event loop (the brain)
// ---------------------------------------------------------------------------

/// Start the assignment task that processes ALL dispatch requests and events
/// sequentially. Must be called once at startup. Takes ownership of the
/// channel receiver.
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
            let label = format!("{req:?}");
            if let Err(e) = handle_request(&state, req).await {
                warn!(request = %label, "assignment task error: {e}");
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
        // -- Dispatch commands ----------------------------------------------
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

        // -- Worker events --------------------------------------------------
        DispatchRequest::WorkerOutcome(outcome) => {
            process_outcome(state, outcome).await?;
        }
        DispatchRequest::ReviewDecision(decision) => {
            process_review_decision(state, decision).await?;
        }

        // -- Monitor events -------------------------------------------------
        DispatchRequest::LeaseExpired(event) => {
            handle_lease_expired(state, event).await?;
        }
        DispatchRequest::JobTimeout(event) => {
            handle_job_timeout(state, event).await?;
        }
        DispatchRequest::OrphanDetected(event) => {
            handle_orphan(state, event).await?;
        }
        DispatchRequest::RetryEligible(event) => {
            handle_retry(state, event).await?;
        }
        DispatchRequest::CiCheck(event) => {
            handle_ci_check(state, event).await?;
        }

        // -- Admin commands (reply via oneshot) ------------------------------
        DispatchRequest::AdminRequeue { req, reply } => {
            let result = process_requeue(state, &req).await;
            let _ = reply.send(result.map_err(|e| e.to_string()));
        }
        DispatchRequest::AdminClose { req, reply } => {
            let result = process_close_job(state, &req).await;
            let _ = reply.send(result.map_err(|e| e.to_string()));
        }
    }
    Ok(())
}

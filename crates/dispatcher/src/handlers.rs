use std::sync::Arc;

use chrono::Utc;
use futures::StreamExt;
use tokio::sync::Barrier;
use tracing::{debug, error, info, warn};

use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs::{self, kv_get};
use crate::state::DispatcherState;

const HANDLER_COUNT: usize = 9;

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
    spawn_handler!("monitor_events", handle_monitor_events);

    barrier.wait().await;
    info!("all NATS handlers subscribed");
    Ok(())
}

// ---------------------------------------------------------------------------
// Worker events
// ---------------------------------------------------------------------------

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
                }
                Err(e) => warn!("invalid heartbeat payload: {e}"),
            }
        });
    }
    Ok(())
}

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
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<WorkerOutcome>(&msg.payload) {
                Ok(outcome) => {
                    if let Err(e) = process_outcome(&s, outcome).await {
                        error!("outcome processing failed: {e}");
                    }
                }
                Err(e) => warn!("invalid outcome payload: {e}"),
            }
        });
    }
    Ok(())
}

async fn process_outcome(
    state: &Arc<DispatcherState>,
    outcome: WorkerOutcome,
) -> DispatcherResult<()> {
    let job_key = &outcome.job_key;
    let worker_id = &outcome.worker_id;

    // Release claim
    crate::claims::release_claim(state, job_key).await?;

    // Record token usage for this action
    if let Some(usage) = outcome.token_usage {
        append_token_record(state, job_key, "work", usage).await?;
    }

    match outcome.outcome {
        OutcomeType::Yield { pr_url } => {
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
            state.jobs.insert(job_key.to_string(), job);

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
                Some(&format!("pr_url: {pr_url}")),
            )
            .await;

            // Dispatch review action
            let review_level = state
                .jobs
                .get(job_key)
                .map(|j| format!("{:?}", j.review).to_lowercase())
                .unwrap_or_else(|| "high".to_string());
            if let Err(e) = crate::action_dispatch::dispatch_review_action(
                state,
                job_key,
                &pr_url,
                &review_level,
            )
            .await
            {
                warn!(job_key, error = %e, "review action dispatch failed");
            }

            // Slot freed — try to dispatch next queued job
            crate::assignment::try_dispatch_next(state).await?;
        }
        OutcomeType::Fail { reason, logs: _ } => {
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
            crate::assignment::try_dispatch_next(state).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin commands
// ---------------------------------------------------------------------------

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
                        // Assignment is handled by the monitor scan loop, not here.
                        // This keeps the create handler fast and non-blocking.
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
            if let Some(reply) = msg.reply {
                if let Err(e) = s
                    .nats
                    .publish_raw(reply.to_string(), bytes::Bytes::from(reply_payload))
                    .await
                {
                    warn!("failed to reply to create-job: {e}");
                }
            }
        });
    }
    Ok(())
}

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
                Ok(req) => match process_requeue(&s, &req).await {
                    Ok(_) => b"{}".to_vec(),
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
            if let Some(reply) = msg.reply {
                if let Err(e) = s
                    .nats
                    .publish_raw(reply.to_string(), bytes::Bytes::from(reply_payload))
                    .await
                {
                    warn!("failed to reply to requeue: {e}");
                }
            }
        });
    }
    Ok(())
}

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

    // If job is currently claimed, release the claim.
    let needs_release = state
        .jobs
        .get(&req.job_key)
        .map(|job| job.state == JobState::OnTheStack)
        .unwrap_or(false);

    if needs_release {
        crate::claims::release_claim(state, &req.job_key).await?;
    }

    jobs::transition_job(state, &req.job_key, target_state, "admin_requeue", None).await?;
    if target_state == JobState::OnDeck {
        // Spawn assignment in background so the reply isn't blocked by dispatch
        let s = state.clone();
        let key = req.job_key.clone();
        tokio::spawn(async move {
            let _ = crate::assignment::try_assign_job(&s, &key).await;
        });
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
                Ok(req) => match process_close_job(&s, &req).await {
                    Ok(_) => b"{}".to_vec(),
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
            if let Some(reply) = msg.reply {
                if let Err(e) = s
                    .nats
                    .publish_raw(reply.to_string(), bytes::Bytes::from(reply_payload))
                    .await
                {
                    warn!("failed to reply to close-job: {e}");
                }
            }
        });
    }
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

    // Release claim if held.
    let needs_release = state
        .jobs
        .get(&req.job_key)
        .map(|job| job.state == JobState::OnTheStack)
        .unwrap_or(false);

    if needs_release {
        crate::claims::release_claim(state, &req.job_key).await?;
    }

    jobs::transition_job(state, &req.job_key, target, trigger, None).await?;
    if target == JobState::Done {
        let unblocked = crate::deps::propagate_unblock(state, &req.job_key).await?;
        if !unblocked.is_empty() {
            let s = state.clone();
            tokio::spawn(async move {
                for key in &unblocked {
                    let _ = crate::assignment::try_assign_job(&s, key).await;
                }
            });
        }
    }
    jobs::journal_append(state, trigger, Some(&req.job_key), None, None).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Review decisions
// ---------------------------------------------------------------------------

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
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<ReviewDecision>(&msg.payload) {
                Ok(decision) => {
                    if let Err(e) = process_review_decision(&s, decision).await {
                        error!("review decision processing failed: {e}");
                    }
                }
                Err(e) => warn!("invalid review decision payload: {e}"),
            }
        });
    }
    Ok(())
}

async fn process_review_decision(
    state: &Arc<DispatcherState>,
    decision: ReviewDecision,
) -> DispatcherResult<()> {
    let job_key = &decision.job_key;

    // Record token usage for this review action
    if let Some(usage) = decision.token_usage {
        append_token_record(state, job_key, "review", usage).await?;
    }

    match decision.decision {
        DecisionType::Approved => {
            jobs::transition_job(state, job_key, JobState::Done, "review_approved", None).await?;
            let unblocked = crate::deps::propagate_unblock(state, job_key).await?;
            for key in &unblocked {
                crate::assignment::try_assign_job(state, key).await?;
            }
            jobs::journal_append(state, "review_approved", Some(job_key), None, None).await;
        }
        DecisionType::ChangesRequested { feedback } => {
            jobs::transition_job(
                state,
                job_key,
                JobState::ChangesRequested,
                "review_changes_requested",
                None,
            )
            .await?;
            // Dispatch a new action with review feedback
            crate::assignment::try_assign_rework(state, job_key, &feedback).await?;
            jobs::journal_append(
                state,
                "review_changes_requested",
                Some(job_key),
                None,
                Some(&feedback),
            )
            .await;
        }
        DecisionType::Escalated { reviewer_login } => {
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
    Ok(())
}

// ---------------------------------------------------------------------------
// Activity / Journal append
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

// ---------------------------------------------------------------------------
// Monitor advisories
// ---------------------------------------------------------------------------

async fn handle_monitor_events(
    state: Arc<DispatcherState>,
    ready: Arc<Barrier>,
) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe("chuggernaut.monitor.>")
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;
    ready.wait().await;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        let subject = msg.subject.to_string();
        tokio::spawn(async move {
            if let Err(e) = dispatch_monitor_event(&s, &subject, &msg.payload).await {
                warn!(subject, "monitor event handling failed: {e}");
            }
        });
    }
    Ok(())
}

async fn dispatch_monitor_event(
    state: &Arc<DispatcherState>,
    subject: &str,
    payload: &[u8],
) -> DispatcherResult<()> {
    // Strip namespace prefix from the incoming subject to compare against constants
    let base = state.nats.strip_prefix(subject);

    if base == subjects::MONITOR_LEASE_EXPIRED.name {
        let event: LeaseExpiredEvent = serde_json::from_slice(payload)?;
        handle_lease_expired(state, event).await
    } else if base == subjects::MONITOR_TIMEOUT.name {
        let event: JobTimeoutEvent = serde_json::from_slice(payload)?;
        handle_job_timeout(state, event).await
    } else if base == subjects::MONITOR_ORPHAN.name {
        let event: OrphanDetectedEvent = serde_json::from_slice(payload)?;
        handle_orphan(state, event).await
    } else if base == subjects::MONITOR_RETRY.name {
        let event: RetryEligibleEvent = serde_json::from_slice(payload)?;
        handle_retry(state, event).await
    } else {
        debug!(subject, "unknown monitor event subject");
        Ok(())
    }
}

/// Schedule auto-retry for a failed job (sets retry_count and retry_after).
/// Used by both worker-reported failures and monitor-detected failures (lease expiry, timeout).
async fn schedule_auto_retry(state: &Arc<DispatcherState>, job_key: &str) {
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

async fn handle_lease_expired(
    state: &Arc<DispatcherState>,
    event: LeaseExpiredEvent,
) -> DispatcherResult<()> {
    if let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &event.job_key).await? {
        if claim.lease_deadline < Utc::now() {
            info!(
                job_key = event.job_key,
                worker_id = event.worker_id,
                "lease expired, releasing claim"
            );
            crate::claims::release_claim(state, &event.job_key).await?;
            jobs::transition_job(
                state,
                &event.job_key,
                JobState::Failed,
                "lease_expired",
                Some(&event.worker_id),
            )
            .await?;
            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: "lease_expired".to_string(),
                message: format!("Worker {} lease expired", event.worker_id),
            };
            jobs::append_activity(state, &event.job_key, entry).await;

            schedule_auto_retry(state, &event.job_key).await;

            // Slot freed — try to dispatch next queued job
            crate::assignment::try_dispatch_next(state).await?;
        }
    }
    Ok(())
}

async fn handle_job_timeout(
    state: &Arc<DispatcherState>,
    event: JobTimeoutEvent,
) -> DispatcherResult<()> {
    if let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &event.job_key).await? {
        let elapsed = (Utc::now() - claim.claimed_at).num_seconds() as u64;
        if elapsed > claim.timeout_secs {
            info!(
                job_key = event.job_key,
                worker_id = event.worker_id,
                "job timeout, releasing claim"
            );
            crate::claims::release_claim(state, &event.job_key).await?;
            jobs::transition_job(
                state,
                &event.job_key,
                JobState::Failed,
                "job_timeout",
                Some(&event.worker_id),
            )
            .await?;
            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: "job_timeout".to_string(),
                message: format!(
                    "Job timed out after {}s (limit: {}s)",
                    elapsed, claim.timeout_secs
                ),
            };
            jobs::append_activity(state, &event.job_key, entry).await;

            schedule_auto_retry(state, &event.job_key).await;

            // Slot freed — try to dispatch next queued job
            crate::assignment::try_dispatch_next(state).await?;
        }
    }
    Ok(())
}

async fn handle_orphan(
    _state: &Arc<DispatcherState>,
    event: OrphanDetectedEvent,
) -> DispatcherResult<()> {
    match event.kind {
        OrphanKind::ClaimlessOnTheStack => {
            warn!(job_key = event.job_key, kind = ?event.kind, "orphan detected (bug indicator)");
        }
    }
    Ok(())
}

async fn handle_retry(
    state: &Arc<DispatcherState>,
    event: RetryEligibleEvent,
) -> DispatcherResult<()> {
    if let Some(job) = state.jobs.get(&event.job_key) {
        if job.state == JobState::Failed
            && job.retry_count <= job.max_retries
            && job.retry_after.is_some_and(|ra| ra <= Utc::now())
        {
            info!(
                job_key = event.job_key,
                retry_count = job.retry_count,
                "retry eligible, requeueing"
            );
            drop(job);
            jobs::transition_job(state, &event.job_key, JobState::OnDeck, "auto_retry", None)
                .await?;
            crate::assignment::try_assign_job(state, &event.job_key).await?;
        }
    }
    Ok(())
}

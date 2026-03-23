use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use futures::StreamExt;
use tracing::{debug, error, info, warn};

use forge2_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs::{self, kv_get};
use crate::state::DispatcherState;

/// Start all NATS subscription handlers.
pub async fn start_handlers(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    macro_rules! spawn_handler {
        ($name:expr, $handler:ident) => {
            let s = state.clone();
            tokio::spawn(async move {
                if let Err(e) = $handler(s).await {
                    error!(concat!(stringify!($handler), " failed: {}"), e);
                }
            });
        };
    }

    spawn_handler!("worker_register", handle_worker_register);
    spawn_handler!("worker_idle", handle_worker_idle);
    spawn_handler!("worker_heartbeat", handle_worker_heartbeat);
    spawn_handler!("worker_outcome", handle_worker_outcome);
    spawn_handler!("worker_unregister", handle_worker_unregister);
    spawn_handler!("admin_create_job", handle_admin_create_job);
    spawn_handler!("admin_requeue", handle_admin_requeue);
    spawn_handler!("admin_close_job", handle_admin_close_job);
    spawn_handler!("review_decision", handle_review_decision);
    spawn_handler!("interact_help", handle_interact_help);
    spawn_handler!("interact_respond", handle_interact_respond);
    spawn_handler!("activity_append", handle_activity_append);
    spawn_handler!("journal_append", handle_journal_append);
    spawn_handler!("monitor_events", handle_monitor_events);

    info!("all NATS handlers started");
    Ok(())
}

// ---------------------------------------------------------------------------
// Worker events
// ---------------------------------------------------------------------------

async fn handle_worker_register(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::WORKER_REGISTER)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<WorkerRegistration>(&msg.payload) {
                Ok(reg) => {
                    let result = crate::workers::register_worker(&s, reg).await;
                    let reply_payload = match &result {
                        Ok(_) => Bytes::from("{}"),
                        Err(e) => Bytes::from(
                            serde_json::to_vec(&ErrorResponse {
                                error: e.to_string(),
                            })
                            .unwrap_or_default(),
                        ),
                    };
                    if let Some(reply) = msg.reply {
                        if let Err(e) = s.nats.publish_raw(reply.to_string(), reply_payload).await {
                            warn!("failed to reply to register: {e}");
                        }
                    }
                }
                Err(e) => warn!("invalid worker registration payload: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle_worker_idle(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::WORKER_IDLE)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<IdleEvent>(&msg.payload) {
                Ok(event) => {
                    if let Err(e) = crate::workers::handle_idle(&s, &event.worker_id).await {
                        warn!(worker_id = event.worker_id, "idle handling failed: {e}");
                    }
                }
                Err(e) => warn!("invalid idle event payload: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle_worker_heartbeat(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::WORKER_HEARTBEAT)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<WorkerHeartbeat>(&msg.payload) {
                Ok(hb) => {
                    if let Err(e) =
                        crate::claims::renew_lease(&s, &hb.job_key, &hb.worker_id).await
                    {
                        debug!(
                            job_key = hb.job_key,
                            worker_id = hb.worker_id,
                            "heartbeat lease renewal failed: {e}"
                        );
                    }
                    // Also update worker last_seen
                    if let Some(mut w) = s.workers.get_mut(&hb.worker_id) {
                        w.last_seen = Utc::now();
                    }
                }
                Err(e) => warn!("invalid heartbeat payload: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle_worker_outcome(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::WORKER_OUTCOME)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

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

    match outcome.outcome {
        OutcomeType::Yield { pr_url } => {
            if let Some((mut job, revision)) = kv_get::<Job>(&state.kv.jobs, job_key).await? {
                job.pr_url = Some(pr_url.clone());
                job.updated_at = Utc::now();
                jobs::kv_cas_update(
                    &state.kv.jobs,
                    job_key,
                    &job,
                    revision,
                    state.config.cas_max_retries,
                )
                .await?;
                state.jobs.insert(job_key.to_string(), job);
            }

            jobs::transition_job(state, job_key, JobState::InReview, "worker_yield", Some(worker_id))
                .await?;

            jobs::journal_append(state, "yield", Some(job_key), Some(worker_id), Some(&format!("pr_url: {pr_url}"))).await;
        }
        OutcomeType::Fail { reason, logs: _ } => {
            jobs::transition_job(state, job_key, JobState::Failed, "worker_fail", Some(worker_id))
                .await?;

            let entry = ActivityEntry {
                timestamp: Utc::now(),
                kind: "failed".to_string(),
                message: format!("Worker failed: {reason}"),
            };
            jobs::append_activity(state, job_key, entry).await;

            // Check auto-retry
            if let Some(job) = state.jobs.get(job_key) {
                if job.retry_count < job.max_retries {
                    let backoff_secs = std::cmp::min(30 * (1u64 << job.retry_count), 600);
                    let retry_after = Utc::now() + chrono::Duration::seconds(backoff_secs as i64);

                    if let Some((mut j, rev)) = kv_get::<Job>(&state.kv.jobs, job_key).await? {
                        j.retry_count += 1;
                        j.retry_after = Some(retry_after);
                        j.updated_at = Utc::now();
                        jobs::kv_cas_update(&state.kv.jobs, job_key, &j, rev, state.config.cas_max_retries).await?;
                        state.jobs.insert(job_key.to_string(), j);
                    }
                    debug!(job_key, retry_after = %retry_after, "auto-retry scheduled");
                }
            }

            jobs::journal_append(state, "failed", Some(job_key), Some(worker_id), Some(&reason)).await;
        }
        OutcomeType::Abandon {} => {
            jobs::transition_job(state, job_key, JobState::OnDeck, "worker_abandon", Some(worker_id)).await?;
            crate::assignment::blacklist_worker(state, job_key, worker_id).await?;
            crate::assignment::try_assign_job(state, job_key).await?;
            jobs::journal_append(state, "abandoned", Some(job_key), Some(worker_id), None).await;
        }
    }
    Ok(())
}

async fn handle_worker_unregister(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::WORKER_UNREGISTER)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<UnregisterEvent>(&msg.payload) {
                Ok(event) => {
                    if let Err(e) = crate::workers::unregister_worker(&s, &event.worker_id).await {
                        warn!(worker_id = event.worker_id, "unregister failed: {e}");
                    }
                }
                Err(e) => warn!("invalid unregister payload: {e}"),
            }
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin commands
// ---------------------------------------------------------------------------

async fn handle_admin_create_job(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::ADMIN_CREATE_JOB)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            let reply_payload = match serde_json::from_slice::<CreateJobRequest>(&msg.payload) {
                Ok(req) => match jobs::create_job(&s, req).await {
                    Ok(key) => {
                        if let Some(job) = s.jobs.get(&key) {
                            if job.state == JobState::OnDeck {
                                let _ = crate::assignment::try_assign_job(&s, &key).await;
                            }
                        }
                        serde_json::to_vec(&CreateJobResponse { key }).unwrap_or_default()
                    }
                    Err(e) => serde_json::to_vec(&ErrorResponse { error: e.to_string() }).unwrap_or_default(),
                },
                Err(e) => serde_json::to_vec(&ErrorResponse { error: format!("invalid payload: {e}") }).unwrap_or_default(),
            };
            if let Some(reply) = msg.reply {
                if let Err(e) = s.nats.publish_raw(reply.to_string(), Bytes::from(reply_payload)).await {
                    warn!("failed to reply to create-job: {e}");
                }
            }
        });
    }
    Ok(())
}

async fn handle_admin_requeue(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::ADMIN_REQUEUE)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            let reply_payload = match serde_json::from_slice::<RequeueRequest>(&msg.payload) {
                Ok(req) => match process_requeue(&s, &req).await {
                    Ok(_) => b"{}".to_vec(),
                    Err(e) => serde_json::to_vec(&ErrorResponse { error: e.to_string() }).unwrap_or_default(),
                },
                Err(e) => serde_json::to_vec(&ErrorResponse { error: format!("invalid payload: {e}") }).unwrap_or_default(),
            };
            if let Some(reply) = msg.reply {
                if let Err(e) = s.nats.publish_raw(reply.to_string(), Bytes::from(reply_payload)).await {
                    warn!("failed to reply to requeue: {e}");
                }
            }
        });
    }
    Ok(())
}

async fn process_requeue(state: &Arc<DispatcherState>, req: &RequeueRequest) -> DispatcherResult<()> {
    let target_state = match req.target {
        RequeueTarget::OnIce => JobState::OnIce,
        RequeueTarget::OnDeck => {
            if !state.jobs.contains_key(&req.job_key) {
                return Err(DispatcherError::JobNotFound(req.job_key.clone()));
            }
            if let Some((deps, _)) = kv_get::<DepRecord>(&state.kv.deps, &req.job_key).await? {
                let all_done = deps.depends_on.iter().all(|dep_key| {
                    state.jobs.get(dep_key).map(|j| j.state == JobState::Done).unwrap_or(false)
                });
                if all_done { JobState::OnDeck } else { JobState::Blocked }
            } else {
                JobState::OnDeck
            }
        }
    };

    // If job is currently claimed, release the claim and preempt
    if let Some(job) = state.jobs.get(&req.job_key) {
        if job.state == JobState::OnTheStack || job.state == JobState::NeedsHelp {
            if let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &req.job_key).await? {
                let notice = PreemptNotice { reason: "Admin requeue".to_string(), new_job_key: String::new() };
                let subject = subjects::dispatch_preempt(&claim.worker_id);
                let payload = serde_json::to_vec(&notice)?;
                let _ = state.nats.publish(&subject, Bytes::from(payload)).await;
                crate::claims::release_claim(state, &req.job_key).await?;
            }
        }
    }

    jobs::transition_job(state, &req.job_key, target_state, "admin_requeue", None).await?;
    if target_state == JobState::OnDeck {
        crate::assignment::try_assign_job(state, &req.job_key).await?;
    }
    jobs::journal_append(state, "requeued", Some(&req.job_key), None, Some(&format!("target: {target_state:?}"))).await;
    Ok(())
}

async fn handle_admin_close_job(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::ADMIN_CLOSE_JOB)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            let reply_payload = match serde_json::from_slice::<CloseJobRequest>(&msg.payload) {
                Ok(req) => match process_close_job(&s, &req).await {
                    Ok(_) => b"{}".to_vec(),
                    Err(e) => serde_json::to_vec(&ErrorResponse { error: e.to_string() }).unwrap_or_default(),
                },
                Err(e) => serde_json::to_vec(&ErrorResponse { error: format!("invalid payload: {e}") }).unwrap_or_default(),
            };
            if let Some(reply) = msg.reply {
                if let Err(e) = s.nats.publish_raw(reply.to_string(), Bytes::from(reply_payload)).await {
                    warn!("failed to reply to close-job: {e}");
                }
            }
        });
    }
    Ok(())
}

async fn process_close_job(state: &Arc<DispatcherState>, req: &CloseJobRequest) -> DispatcherResult<()> {
    let target = if req.revoke { JobState::Revoked } else { JobState::Done };
    let trigger = if req.revoke { "admin_revoke" } else { "admin_close" };

    // Release claim if held
    if let Some(job) = state.jobs.get(&req.job_key) {
        if job.state == JobState::OnTheStack || job.state == JobState::NeedsHelp {
            if let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &req.job_key).await? {
                let notice = PreemptNotice {
                    reason: format!("Admin close ({})", if req.revoke { "revoke" } else { "done" }),
                    new_job_key: String::new(),
                };
                let subject = subjects::dispatch_preempt(&claim.worker_id);
                let payload = serde_json::to_vec(&notice)?;
                let _ = state.nats.publish(&subject, Bytes::from(payload)).await;
                crate::claims::release_claim(state, &req.job_key).await?;
            }
        }
    }

    jobs::transition_job(state, &req.job_key, target, trigger, None).await?;
    if target == JobState::Done {
        let unblocked = crate::deps::propagate_unblock(state, &req.job_key).await?;
        for key in &unblocked {
            crate::assignment::try_assign_job(state, key).await?;
        }
    }
    jobs::journal_append(state, trigger, Some(&req.job_key), None, None).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Review decisions
// ---------------------------------------------------------------------------

async fn handle_review_decision(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::REVIEW_DECISION)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

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

async fn process_review_decision(state: &Arc<DispatcherState>, decision: ReviewDecision) -> DispatcherResult<()> {
    let job_key = &decision.job_key;
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
            jobs::transition_job(state, job_key, JobState::ChangesRequested, "review_changes_requested", None).await?;
            if let Some(job) = state.jobs.get(job_key) {
                if let Some(ref last_worker) = job.last_worker_id {
                    if let Some(worker) = state.workers.get(last_worker) {
                        if worker.state == WorkerState::Idle {
                            crate::assignment::assign_job(state, job_key, last_worker, true, Some(&feedback)).await?;
                        } else {
                            let rework = PendingRework { worker_id: last_worker.clone(), review_feedback: feedback.clone() };
                            jobs::kv_put(&state.kv.pending_reworks, job_key, &rework).await?;
                        }
                    } else {
                        crate::assignment::try_assign_job(state, job_key).await?;
                    }
                } else {
                    crate::assignment::try_assign_job(state, job_key).await?;
                }
            }
            jobs::journal_append(state, "review_changes_requested", Some(job_key), None, Some(&feedback)).await;
        }
        DecisionType::Escalated { reviewer_login } => {
            jobs::transition_job(state, job_key, JobState::Escalated, "review_escalated", None).await?;
            jobs::journal_append(state, "review_escalated", Some(job_key), None, Some(&format!("human reviewer: {reviewer_login}"))).await;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Interaction
// ---------------------------------------------------------------------------

async fn handle_interact_help(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::INTERACT_HELP)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<HelpRequest>(&msg.payload) {
                Ok(req) => {
                    if let Err(e) = jobs::transition_job(&s, &req.job_key, JobState::NeedsHelp, "worker_help_request", Some(&req.worker_id)).await {
                        warn!(job_key = req.job_key, "help transition failed: {e}");
                    }
                    let entry = ActivityEntry { timestamp: Utc::now(), kind: "help_requested".to_string(), message: req.reason.clone() };
                    jobs::append_activity(&s, &req.job_key, entry).await;
                }
                Err(e) => warn!("invalid help request payload: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle_interact_respond(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe("forge2.interact.respond.>")
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<HelpResponse>(&msg.payload) {
                Ok(resp) => {
                    if let Ok(Some((claim, _))) = kv_get::<ClaimState>(&s.kv.claims, &resp.job_key).await {
                        let subject = subjects::interact_deliver(&claim.worker_id);
                        let payload = serde_json::to_vec(&resp).unwrap_or_default();
                        if let Err(e) = s.nats.publish(&subject, Bytes::from(payload)).await {
                            warn!(job_key = resp.job_key, "failed to deliver help response: {e}");
                        }
                        if let Err(e) = jobs::transition_job(&s, &resp.job_key, JobState::OnTheStack, "human_responded", None).await {
                            warn!(job_key = resp.job_key, "help response transition failed: {e}");
                        }
                    } else {
                        warn!(job_key = resp.job_key, "no claim found for help response");
                    }
                }
                Err(e) => warn!("invalid help response payload: {e}"),
            }
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Activity / Journal append
// ---------------------------------------------------------------------------

async fn handle_activity_append(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::ACTIVITY_APPEND)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

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

async fn handle_journal_append(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe(subjects::JOURNAL_APPEND)
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

    while let Some(msg) = sub.next().await {
        let s = state.clone();
        tokio::spawn(async move {
            match serde_json::from_slice::<JournalAppend>(&msg.payload) {
                Ok(append) => {
                    let entry = JournalEntry {
                        timestamp: append.timestamp, action: append.action,
                        job_key: append.job_key, worker_id: append.worker_id, details: append.details,
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

async fn handle_monitor_events(state: Arc<DispatcherState>) -> DispatcherResult<()> {
    let mut sub = state
        .nats
        .subscribe("forge2.monitor.>")
        .await
        .map_err(|e| DispatcherError::Nats(e.to_string()))?;

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

async fn dispatch_monitor_event(state: &Arc<DispatcherState>, subject: &str, payload: &[u8]) -> DispatcherResult<()> {
    // Strip namespace prefix from the incoming subject to compare against constants
    let base = state.nats.strip_prefix(subject);

    if base == subjects::MONITOR_LEASE_EXPIRED {
        let event: LeaseExpiredEvent = serde_json::from_slice(payload)?;
        handle_lease_expired(state, event).await
    } else if base == subjects::MONITOR_TIMEOUT {
        let event: JobTimeoutEvent = serde_json::from_slice(payload)?;
        handle_job_timeout(state, event).await
    } else if base == subjects::MONITOR_ORPHAN {
        let event: OrphanDetectedEvent = serde_json::from_slice(payload)?;
        handle_orphan(state, event).await
    } else if base == subjects::MONITOR_RETRY {
        let event: RetryEligibleEvent = serde_json::from_slice(payload)?;
        handle_retry(state, event).await
    } else {
        debug!(subject, "unknown monitor event subject");
        Ok(())
    }
}

async fn handle_lease_expired(state: &Arc<DispatcherState>, event: LeaseExpiredEvent) -> DispatcherResult<()> {
    if let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &event.job_key).await? {
        if claim.lease_deadline < Utc::now() {
            info!(job_key = event.job_key, worker_id = event.worker_id, "lease expired, releasing claim");
            crate::claims::release_claim(state, &event.job_key).await?;
            crate::workers::prune_worker(state, &event.worker_id).await?;
            jobs::transition_job(state, &event.job_key, JobState::Failed, "lease_expired", Some(&event.worker_id)).await?;
            let entry = ActivityEntry { timestamp: Utc::now(), kind: "lease_expired".to_string(), message: format!("Worker {} lease expired", event.worker_id) };
            jobs::append_activity(state, &event.job_key, entry).await;
        }
    }
    Ok(())
}

async fn handle_job_timeout(state: &Arc<DispatcherState>, event: JobTimeoutEvent) -> DispatcherResult<()> {
    if let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &event.job_key).await? {
        let elapsed = (Utc::now() - claim.claimed_at).num_seconds() as u64;
        if elapsed > claim.timeout_secs {
            info!(job_key = event.job_key, worker_id = event.worker_id, "job timeout, releasing claim");
            crate::claims::release_claim(state, &event.job_key).await?;
            crate::workers::prune_worker(state, &event.worker_id).await?;
            jobs::transition_job(state, &event.job_key, JobState::Failed, "job_timeout", Some(&event.worker_id)).await?;
            let entry = ActivityEntry { timestamp: Utc::now(), kind: "job_timeout".to_string(), message: format!("Job timed out after {}s (limit: {}s)", elapsed, claim.timeout_secs) };
            jobs::append_activity(state, &event.job_key, entry).await;
        }
    }
    Ok(())
}

async fn handle_orphan(state: &Arc<DispatcherState>, event: OrphanDetectedEvent) -> DispatcherResult<()> {
    match event.kind {
        OrphanKind::ClaimUnknownWorker => {
            info!(job_key = event.job_key, worker_id = ?event.worker_id, "orphan claim: unknown worker");
            crate::claims::release_claim(state, &event.job_key).await?;
            jobs::transition_job(state, &event.job_key, JobState::Failed, "orphan_claim", event.worker_id.as_deref()).await?;
        }
        OrphanKind::StaleSession | OrphanKind::ClaimlessOnTheStack => {
            warn!(job_key = event.job_key, kind = ?event.kind, "orphan detected (bug indicator)");
        }
    }
    Ok(())
}

async fn handle_retry(state: &Arc<DispatcherState>, event: RetryEligibleEvent) -> DispatcherResult<()> {
    if let Some(job) = state.jobs.get(&event.job_key) {
        if job.state == JobState::Failed
            && job.retry_count <= job.max_retries
            && job.retry_after.is_some_and(|ra| ra <= Utc::now())
        {
            info!(job_key = event.job_key, retry_count = job.retry_count, "retry eligible, requeueing");
            drop(job);
            jobs::transition_job(state, &event.job_key, JobState::OnDeck, "auto_retry", None).await?;
            crate::assignment::try_assign_job(state, &event.job_key).await?;
        }
    }
    Ok(())
}

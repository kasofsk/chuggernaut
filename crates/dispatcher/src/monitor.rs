use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use tracing::{debug, info, warn};

use chuggernaut_types::*;

use crate::error::DispatcherResult;
use crate::jobs::kv_get;
use crate::state::DispatcherState;

/// Start the monitor background task.
pub fn start(state: Arc<DispatcherState>) {
    let interval = Duration::from_secs(state.config.monitor_scan_interval_secs);

    tokio::spawn(async move {
        // Immediate first scan
        run_scans(&state).await;

        let mut tick = tokio::time::interval(interval);
        tick.tick().await; // skip first immediate tick
        loop {
            tick.tick().await;
            run_scans(&state).await;
        }
    });

    info!("monitor started");
}

async fn run_scans(state: &Arc<DispatcherState>) {
    if let Err(e) = scan_lease_expiry(state).await {
        warn!("lease expiry scan failed: {e}");
    }
    if let Err(e) = scan_job_timeout(state).await {
        warn!("job timeout scan failed: {e}");
    }
    if let Err(e) = scan_orphans(state).await {
        warn!("orphan scan failed: {e}");
    }
    if let Err(e) = scan_retry(state).await {
        warn!("retry scan failed: {e}");
    }
    if let Err(e) = scan_archival(state).await {
        warn!("archival scan failed: {e}");
    }
    if let Err(e) = scan_ci_status(state).await {
        warn!("CI status scan failed: {e}");
    }
    // Clear stale rate limit state so it doesn't permanently block dispatching
    scan_rate_limit_staleness(state).await;
}

async fn scan_lease_expiry(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let now = Utc::now();
    let keys = state.kv.claims.keys().await?;
    tokio::pin!(keys);

    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key
            && let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &key_str).await?
            && claim.lease_deadline < now
        {
            let event = LeaseExpiredEvent {
                job_key: key_str.clone(),
                worker_id: claim.worker_id.clone(),
                lease_deadline: claim.lease_deadline,
                detected_at: now,
            };
            publish_monitor_event(state, &subjects::MONITOR_LEASE_EXPIRED, &event).await;
        }
    }
    Ok(())
}

async fn scan_job_timeout(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let now = Utc::now();
    let keys = state.kv.claims.keys().await?;
    tokio::pin!(keys);

    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key
            && let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &key_str).await?
        {
            let elapsed = (now - claim.claimed_at).num_seconds() as u64;
            if elapsed > claim.timeout_secs {
                let event = JobTimeoutEvent {
                    job_key: key_str.clone(),
                    worker_id: claim.worker_id.clone(),
                    claimed_at: claim.claimed_at,
                    timeout_secs: claim.timeout_secs,
                    detected_at: now,
                };
                publish_monitor_event(state, &subjects::MONITOR_TIMEOUT, &event).await;
            }
        }
    }
    Ok(())
}

async fn scan_orphans(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let now = Utc::now();

    // Claimless on-the-stack jobs
    for entry in state.jobs.iter() {
        let job = entry.value();
        if job.state == JobState::OnTheStack
            && let Ok(None) = kv_get::<ClaimState>(&state.kv.claims, &job.key).await
        {
            let event = OrphanDetectedEvent {
                job_key: job.key.clone(),
                worker_id: None,
                kind: OrphanKind::ClaimlessOnTheStack,
                detected_at: now,
            };
            publish_monitor_event(state, &subjects::MONITOR_ORPHAN, &event).await;
        }
    }

    Ok(())
}

async fn scan_retry(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let now = Utc::now();

    for entry in state.jobs.iter() {
        let job = entry.value();
        if job.state == JobState::Failed
            && job.retry_count < job.max_retries
            && job.retry_after.is_some_and(|ra| ra <= now)
        {
            let event = RetryEligibleEvent {
                job_key: job.key.clone(),
                retry_count: job.retry_count,
                retry_after: job.retry_after.unwrap(),
                detected_at: now,
            };
            publish_monitor_event(state, &subjects::MONITOR_RETRY, &event).await;
        }
    }

    Ok(())
}

async fn scan_archival(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let now = Utc::now();
    let retention = chrono::Duration::seconds(state.config.job_retention_secs as i64);

    let mut candidates: Vec<String> = Vec::new();

    for entry in state.jobs.iter() {
        let job = entry.value();
        let is_terminal = job.state == JobState::Done
            || job.state == JobState::Revoked
            || (job.state == JobState::Failed && job.retry_count >= job.max_retries);

        if is_terminal && (now - job.updated_at) > retention {
            candidates.push(job.key.clone());
        }
    }

    for key in candidates {
        // Check that all dependents are also terminal
        if let Some((deps, _)) = kv_get::<DepRecord>(&state.kv.deps, &key).await? {
            let all_terminal = deps.depended_on_by.iter().all(|dep_key| {
                state
                    .jobs
                    .get(dep_key)
                    .map(|j| {
                        j.state == JobState::Done
                            || j.state == JobState::Revoked
                            || (j.state == JobState::Failed && j.retry_count >= j.max_retries)
                    })
                    .unwrap_or(true) // if already archived, treat as terminal
            });

            if !all_terminal {
                continue;
            }
        }

        // Archive: remove from in-memory structures
        state.jobs.remove(&key);
        {
            let mut graph = state.graph.write().await;
            graph.remove_node(&key);
        }

        // Clean up KV entries
        let _ = state.kv.deps.delete(&key).await;
        let _ = state.kv.activities.delete(&key).await;

        // Set TTL on jobs KV entry (NATS KV doesn't support per-key TTL post-creation,
        // so we just leave it — the job will be queryable via API until the bucket ages it out)
        debug!(key, "job archived");
    }

    Ok(())
}

async fn scan_ci_status(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let now = Utc::now();

    // Find InReview jobs with ci_status == Pending and a PR URL
    let candidates: Vec<(String, String, String)> = state
        .jobs
        .iter()
        .filter(|e| {
            let job = e.value();
            job.state == JobState::InReview
                && job.ci_status == Some(CiStatus::Pending)
                && job.pr_url.is_some()
        })
        .map(|e| {
            let job = e.value();
            (
                e.key().clone(),
                job.pr_url.clone().unwrap(),
                job.repo.clone(),
            )
        })
        .collect();

    if candidates.is_empty() {
        return Ok(());
    }

    let (forgejo_url, forgejo_token) =
        match (&state.config.forgejo_url, &state.config.forgejo_token) {
            (Some(url), Some(token)) => (url.clone(), token.clone()),
            _ => return Ok(()), // No Forgejo config, skip CI check
        };

    let forgejo = chuggernaut_forgejo_api::ForgejoClient::new(&forgejo_url, &forgejo_token);

    for (job_key, pr_url, repo) in candidates {
        let job = match state.jobs.get(&job_key) {
            Some(j) => j.clone(),
            None => continue,
        };

        // Check CI poll timeout
        if let Some(check_since) = job.ci_check_since {
            let elapsed = (now - check_since).num_seconds() as u64;
            if elapsed > state.config.ci_poll_timeout_secs {
                info!(
                    job_key,
                    elapsed, "CI poll timeout, treating as success (no CI configured?)"
                );
                let event = CiCheckEvent {
                    job_key: job_key.clone(),
                    pr_url: pr_url.clone(),
                    ci_status: CiStatus::Success,
                    detected_at: now,
                };
                publish_monitor_event(state, &subjects::MONITOR_CI_CHECK, &event).await;
                continue;
            }
        }

        // Parse owner/repo from job.repo (format: "owner/repo")
        let parts: Vec<&str> = repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            continue;
        }
        let (owner, repo_name) = (parts[0], parts[1]);

        // Get PR index from URL
        let pr_index = match parse_pr_url_index(&pr_url) {
            Some(idx) => idx,
            None => continue,
        };

        // Get PR to find head SHA
        let pr = match forgejo.get_pull_request(owner, repo_name, pr_index).await {
            Ok(pr) => pr,
            Err(e) => {
                debug!(job_key, error = %e, "failed to get PR for CI check");
                continue;
            }
        };

        // Get combined commit status for the head SHA
        match forgejo
            .get_combined_status(owner, repo_name, &pr.head.sha)
            .await
        {
            Ok(status) => {
                // No CI configured (no statuses at all) — treat as success
                if status.total_count == 0 {
                    info!(job_key, "no CI statuses found, treating as success");
                    let event = CiCheckEvent {
                        job_key: job_key.clone(),
                        pr_url: pr_url.clone(),
                        ci_status: CiStatus::Success,
                        detected_at: now,
                    };
                    publish_monitor_event(state, &subjects::MONITOR_CI_CHECK, &event).await;
                    continue;
                }

                let ci_status = match status.state.as_str() {
                    "success" => CiStatus::Success,
                    "failure" => CiStatus::Failure,
                    "error" => CiStatus::Error,
                    _ => CiStatus::Pending, // "pending" or ""
                };

                if ci_status != CiStatus::Pending {
                    let event = CiCheckEvent {
                        job_key: job_key.clone(),
                        pr_url: pr_url.clone(),
                        ci_status,
                        detected_at: now,
                    };
                    publish_monitor_event(state, &subjects::MONITOR_CI_CHECK, &event).await;
                }
                // else: still pending, will check again next scan
            }
            Err(e) => {
                debug!(job_key, error = %e, "CI status check failed, will retry next scan");
            }
        }
    }

    Ok(())
}

/// Clear stale rate limit state so it doesn't permanently block dispatching.
/// If the rate limit's `resets_at` is in the past, the overage window has ended.
async fn scan_rate_limit_staleness(state: &Arc<DispatcherState>) {
    let mut tracker = state.token_tracker.write().await;
    if tracker.rate_limit.is_some() {
        tracker.clear_stale_rate_limit();
        if tracker.rate_limit.is_none() {
            info!("rate limit overage window expired, dispatch unblocked");
        }
    }
}

async fn publish_monitor_event<T: serde::Serialize>(
    state: &Arc<DispatcherState>,
    subject: &Subject<T>,
    event: &T,
) {
    if let Err(e) = state.nats.publish_msg(subject, event).await {
        warn!(subject.name, "failed to publish monitor event: {e}");
    }
}

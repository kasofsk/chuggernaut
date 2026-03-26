use std::sync::Arc;

use futures::StreamExt;
use tracing::{info, warn};

use chuggernaut_types::*;

use crate::error::DispatcherResult;
use crate::jobs::kv_get;
use crate::state::DispatcherState;

/// Rebuild all in-memory state from NATS KV on startup.
pub async fn recover(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    info!("starting recovery — rebuilding state from KV");

    // 1. Rebuild job index
    rebuild_job_index(state).await?;

    // 2. Rebuild petgraph
    crate::deps::rebuild_graph(state).await?;

    // 3. Repair dep indexes
    crate::deps::repair_reverse_indexes(state).await?;

    // 4. Reconcile claims against jobs
    reconcile_claims_against_jobs(state).await?;

    // 5. Reconcile jobs against claims
    reconcile_jobs_against_claims(state).await?;

    // 6. Repair InReview/Reviewing jobs by checking actual PR state on Forgejo.
    //    Handles missed review decisions from dispatcher downtime.
    repair_in_review_jobs(state).await;

    // 6b. Reviewing jobs without claims = crashed review action → revert to InReview
    repair_reviewing_without_claims(state).await;

    // 7. Queue OnDeck jobs for dispatch (processed after assignment task starts)
    assign_on_deck_jobs(state).await;

    info!(jobs = state.jobs.len(), "recovery complete");

    Ok(())
}

async fn rebuild_job_index(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let keys = state.kv.jobs.keys().await?;
    tokio::pin!(keys);

    let mut count = 0u32;
    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key
            && let Some((job, _)) = kv_get::<Job>(&state.kv.jobs, &key_str).await?
        {
            state.jobs.insert(key_str, job);
            count += 1;
        }
    }

    info!(count, "job index rebuilt");
    Ok(())
}

/// For each active claim, verify the job is in on-the-stack.
/// If not, the claim is stale from a mid-transition crash — delete it.
async fn reconcile_claims_against_jobs(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let keys = state.kv.claims.keys().await?;
    tokio::pin!(keys);

    let mut stale = 0u32;
    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key
            && let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &key_str).await?
        {
            let valid = state
                .jobs
                .get(&key_str)
                .map(|j| j.state == JobState::OnTheStack || j.state == JobState::Reviewing)
                .unwrap_or(false);

            if !valid {
                warn!(
                    job_key = key_str,
                    worker_id = claim.worker_id,
                    "stale claim detected — deleting"
                );
                let _ = state.kv.claims.delete(&key_str).await;
                stale += 1;
            }
        }
    }

    if stale > 0 {
        info!(stale, "stale claims cleaned up");
    }
    Ok(())
}

/// Try to assign OnDeck jobs that were waiting when the dispatcher last stopped.
async fn assign_on_deck_jobs(state: &Arc<DispatcherState>) {
    let on_deck: Vec<String> = state
        .jobs
        .iter()
        .filter(|entry| entry.value().state == JobState::OnDeck)
        .map(|entry| entry.key().clone())
        .collect();

    if on_deck.is_empty() {
        return;
    }

    info!(
        count = on_deck.len(),
        "attempting to assign on-deck jobs from recovery"
    );
    for key in on_deck {
        crate::assignment::request_dispatch(
            state,
            crate::state::DispatchRequest::AssignJob { job_key: key },
        );
    }
}

/// Repair InReview jobs by checking the actual PR state on Forgejo.
/// If a PR is already merged, transition to Done. If it has REQUEST_CHANGES
/// reviews, transition to ChangesRequested. This repairs state corruption
/// from missed review decisions (e.g. dispatcher was down when decision arrived).
async fn repair_in_review_jobs(state: &Arc<DispatcherState>) {
    let (forgejo_url, forgejo_token) =
        match (&state.config.forgejo_url, &state.config.forgejo_token) {
            (Some(url), Some(token)) => (url.clone(), token.clone()),
            _ => return,
        };

    let candidates: Vec<(String, String, String)> = state
        .jobs
        .iter()
        .filter(|e| {
            let job = e.value();
            job.state == JobState::InReview && job.pr_url.is_some()
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
        return;
    }

    info!(
        count = candidates.len(),
        "checking InReview jobs against Forgejo PR state"
    );
    let forgejo = chuggernaut_forgejo_api::ForgejoClient::new(&forgejo_url, &forgejo_token);

    for (job_key, pr_url, repo) in candidates {
        let parts: Vec<&str> = repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            continue;
        }
        let (owner, repo_name) = (parts[0], parts[1]);

        let pr_index = match parse_pr_url_index(&pr_url) {
            Some(idx) => idx,
            None => continue,
        };

        // Check PR state
        let pr = match forgejo.get_pull_request(owner, repo_name, pr_index).await {
            Ok(pr) => pr,
            Err(e) => {
                warn!(job_key, error = %e, "failed to check PR state during recovery");
                continue;
            }
        };

        if pr.merged {
            info!(job_key, pr_url, "PR already merged — repairing to Done");
            if let Err(e) = crate::jobs::transition_job(
                state,
                &job_key,
                JobState::Done,
                "recovery_pr_merged",
                None,
            )
            .await
            {
                warn!(job_key, error = %e, "failed to repair merged job");
            } else {
                // Propagate unblock for dependents
                if let Ok(unblocked) = crate::deps::propagate_unblock(state, &job_key).await {
                    for key in &unblocked {
                        crate::assignment::request_dispatch(
                            state,
                            crate::state::DispatchRequest::AssignJob {
                                job_key: key.clone(),
                            },
                        );
                    }
                }
            }
            continue;
        }

        // Check for REQUEST_CHANGES reviews
        if let Ok(reviews) = forgejo.list_reviews(owner, repo_name, pr_index).await
            && let Some(latest) = reviews.iter().rev().find(|r| {
                r.user
                    .as_ref()
                    .is_some_and(|u| u.login.contains("reviewer"))
            })
            && latest.state == "REQUEST_CHANGES"
        {
            info!(
                job_key,
                pr_url, "PR has REQUEST_CHANGES — repairing to ChangesRequested"
            );
            let feedback = latest.body.clone();
            if let Err(e) = crate::jobs::transition_job(
                state,
                &job_key,
                JobState::ChangesRequested,
                "recovery_changes_requested",
                None,
            )
            .await
            {
                warn!(job_key, error = %e, "failed to repair changes-requested job");
            } else {
                crate::assignment::request_dispatch(
                    state,
                    crate::state::DispatchRequest::AssignRework {
                        job_key: job_key.clone(),
                        feedback,
                    },
                );
            }
            continue;
        }

        // Job is InReview and PR is open with no changes-requested — needs review dispatch
        info!(
            job_key,
            "InReview job needs review dispatch (queued for after startup)"
        );
        let review_level = state
            .jobs
            .get(&job_key)
            .map(|j| format!("{:?}", j.review).to_lowercase())
            .unwrap_or_else(|| "high".to_string());
        crate::assignment::request_dispatch(
            state,
            crate::state::DispatchRequest::DispatchReview {
                job_key,
                pr_url,
                review_level,
            },
        );
    }
}

/// Reviewing jobs without an active claim = review action crashed or was lost.
/// Revert to InReview so the monitor can re-dispatch the review.
async fn repair_reviewing_without_claims(state: &Arc<DispatcherState>) {
    let reviewing: Vec<String> = state
        .jobs
        .iter()
        .filter(|e| e.value().state == JobState::Reviewing)
        .map(|e| e.key().clone())
        .collect();

    for key in reviewing {
        match kv_get::<ClaimState>(&state.kv.claims, &key).await {
            Ok(Some(_)) => {} // claim exists, review is running
            _ => {
                warn!(
                    job_key = key,
                    "Reviewing job with no claim — reverting to InReview"
                );
                if let Err(e) = crate::jobs::transition_job(
                    state,
                    &key,
                    JobState::InReview,
                    "recovery_review_no_claim",
                    None,
                )
                .await
                {
                    warn!(job_key = key, error = %e, "failed to revert Reviewing to InReview");
                }
            }
        }
    }
}

/// For each job in on-the-stack, verify a matching claim exists.
/// If not, the claim was lost — transition to Failed.
async fn reconcile_jobs_against_claims(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let claimless: Vec<String> = state
        .jobs
        .iter()
        .filter(|entry| entry.value().state == JobState::OnTheStack)
        .map(|entry| entry.key().clone())
        .collect();

    let mut orphaned = 0u32;
    for key in claimless {
        match kv_get::<ClaimState>(&state.kv.claims, &key).await? {
            Some(_) => {} // claim exists, all good
            None => {
                warn!(job_key = key, "job in on-the-stack with no claim — failing");
                crate::jobs::transition_job(
                    state,
                    &key,
                    JobState::Failed,
                    "recovery_no_claim",
                    None,
                )
                .await?;
                orphaned += 1;
            }
        }
    }

    if orphaned > 0 {
        info!(orphaned, "claimless jobs transitioned to failed");
    }
    Ok(())
}

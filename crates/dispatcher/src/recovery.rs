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

    // 6. Try to assign any OnDeck jobs that weren't dispatched before shutdown
    assign_on_deck_jobs(state).await;

    info!(jobs = state.jobs.len(), "recovery complete");

    Ok(())
}

async fn rebuild_job_index(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let keys = state.kv.jobs.keys().await?;
    tokio::pin!(keys);

    let mut count = 0u32;
    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key {
            if let Some((job, _)) = kv_get::<Job>(&state.kv.jobs, &key_str).await? {
                state.jobs.insert(key_str, job);
                count += 1;
            }
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
        if let Ok(key_str) = key {
            if let Some((claim, _)) = kv_get::<ClaimState>(&state.kv.claims, &key_str).await? {
                let valid = state
                    .jobs
                    .get(&key_str)
                    .map(|j| j.state == JobState::OnTheStack)
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

    info!(count = on_deck.len(), "attempting to assign on-deck jobs from recovery");
    for key in on_deck {
        match crate::assignment::try_assign_job(state, &key).await {
            Ok(true) => info!(job_key = key, "assigned on-deck job from recovery"),
            Ok(false) => {} // at capacity or not assignable — normal
            Err(e) => warn!(job_key = key, error = %e, "failed to assign on-deck job"),
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

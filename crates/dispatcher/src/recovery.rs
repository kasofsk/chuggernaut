use std::sync::Arc;

use futures::StreamExt;
use tracing::{debug, info, warn};

use forge2_types::*;

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

    // 4. Rebuild worker registry
    rebuild_worker_registry(state).await?;

    // 5. Reconcile claims against jobs
    reconcile_claims_against_jobs(state).await?;

    // 6. Reconcile jobs against claims
    reconcile_jobs_against_claims(state).await?;

    // 7. Scan pending-reworks for pruned workers
    scan_pending_reworks(state).await?;

    info!(
        jobs = state.jobs.len(),
        workers = state.workers.len(),
        "recovery complete"
    );

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

async fn rebuild_worker_registry(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let keys = state.kv.workers.keys().await?;
    tokio::pin!(keys);

    let mut count = 0u32;
    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key {
            if let Some((worker, _)) = kv_get::<WorkerInfo>(&state.kv.workers, &key_str).await? {
                state.workers.insert(key_str, worker);
                count += 1;
            }
        }
    }

    info!(count, "worker registry rebuilt (may be partially stale)");
    Ok(())
}

/// For each active claim, verify the job is in on-the-stack or needs-help.
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
                    .map(|j| {
                        j.state == JobState::OnTheStack || j.state == JobState::NeedsHelp
                    })
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

/// For each job in on-the-stack or needs-help, verify a matching claim exists.
/// If not, the claim was lost — transition to Failed.
async fn reconcile_jobs_against_claims(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let claimless: Vec<String> = state
        .jobs
        .iter()
        .filter(|entry| {
            let s = entry.value().state;
            s == JobState::OnTheStack || s == JobState::NeedsHelp
        })
        .map(|entry| entry.key().clone())
        .collect();

    let mut orphaned = 0u32;
    for key in claimless {
        match kv_get::<ClaimState>(&state.kv.claims, &key).await? {
            Some(_) => {} // claim exists, all good
            None => {
                warn!(job_key = key, "job in on-the-stack/needs-help with no claim — failing");
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

/// For each pending rework, verify the target worker still exists.
/// If not, clear the entry so the rework can be reassigned.
async fn scan_pending_reworks(state: &Arc<DispatcherState>) -> DispatcherResult<()> {
    let keys = state.kv.pending_reworks.keys().await?;
    tokio::pin!(keys);

    let mut cleared = 0u32;
    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key {
            if let Some((rework, _)) =
                kv_get::<PendingRework>(&state.kv.pending_reworks, &key_str).await?
            {
                if !state.workers.contains_key(&rework.worker_id) {
                    let _ = state.kv.pending_reworks.delete(&key_str).await;
                    cleared += 1;
                    debug!(
                        job_key = key_str,
                        worker_id = rework.worker_id,
                        "cleared pending rework for missing worker"
                    );
                }
            }
        }
    }

    if cleared > 0 {
        info!(cleared, "pending reworks cleared for missing workers");
    }
    Ok(())
}

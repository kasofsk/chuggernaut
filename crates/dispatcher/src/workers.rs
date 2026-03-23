use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, info, warn};

use forge2_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs::{kv_cas_create, kv_cas_update, kv_get};
use crate::state::DispatcherState;

/// Register a new worker. CAS-create to prevent duplicate IDs.
pub async fn register_worker(
    state: &Arc<DispatcherState>,
    reg: WorkerRegistration,
) -> DispatcherResult<()> {
    let now = Utc::now();
    let info = WorkerInfo {
        worker_id: reg.worker_id.clone(),
        state: WorkerState::Idle,
        capabilities: reg.capabilities,
        worker_type: reg.worker_type,
        platform: reg.platform,
        current_job: None,
        last_seen: now,
    };

    match kv_cas_create(&state.kv.workers, &reg.worker_id, &info).await {
        Ok(_) => {
            state.workers.insert(reg.worker_id.clone(), info);
            info!(worker_id = reg.worker_id, "worker registered");
            Ok(())
        }
        Err(DispatcherError::Kv(msg)) if msg.contains("already exists") => {
            // Re-registration: CAS-update the existing entry
            if let Some((_, revision)) =
                kv_get::<WorkerInfo>(&state.kv.workers, &reg.worker_id).await?
            {
                kv_cas_update(
                    &state.kv.workers,
                    &reg.worker_id,
                    &info,
                    revision,
                    state.config.cas_max_retries,
                )
                .await?;
                state.workers.insert(reg.worker_id.clone(), info);
                debug!(worker_id = reg.worker_id, "worker re-registered");
                Ok(())
            } else {
                Err(DispatcherError::WorkerNotFound(reg.worker_id))
            }
        }
        Err(e) => Err(e),
    }
}

/// Handle a worker going idle. Check pending reworks first, then try assignment.
pub async fn handle_idle(
    state: &Arc<DispatcherState>,
    worker_id: &str,
) -> DispatcherResult<()> {
    // Update worker state to Idle
    if let Some((mut info, revision)) =
        kv_get::<WorkerInfo>(&state.kv.workers, worker_id).await?
    {
        info.state = WorkerState::Idle;
        info.current_job = None;
        info.last_seen = Utc::now();
        kv_cas_update(
            &state.kv.workers,
            worker_id,
            &info,
            revision,
            state.config.cas_max_retries,
        )
        .await?;
        state.workers.insert(worker_id.to_string(), info);
    }

    // Check pending reworks first
    if let Some(job_key) = check_pending_rework(state, worker_id).await? {
        debug!(worker_id, job_key, "assigning pending rework");
        return Ok(());
    }

    // Try normal assignment
    crate::assignment::try_assign_to_worker(state, worker_id).await?;

    Ok(())
}

/// Check if there's a pending rework for this worker.
async fn check_pending_rework(
    state: &Arc<DispatcherState>,
    worker_id: &str,
) -> DispatcherResult<Option<String>> {
    use futures::StreamExt;

    let keys = state.kv.pending_reworks.keys().await?;
    tokio::pin!(keys);

    while let Some(key) = keys.next().await {
        match key {
            Ok(key_str) => {
                if let Some((rework, _)) =
                    kv_get::<PendingRework>(&state.kv.pending_reworks, &key_str).await?
                {
                    if rework.worker_id == worker_id {
                        // Found a pending rework — assign it
                        // Delete the pending rework entry
                        if let Err(e) = state.kv.pending_reworks.delete(&key_str).await {
                            warn!(key_str, "failed to delete pending rework: {e}");
                        }

                        // Assign the rework
                        crate::assignment::assign_job(
                            state,
                            &key_str,
                            worker_id,
                            true,
                            Some(&rework.review_feedback),
                        )
                        .await?;

                        return Ok(Some(key_str));
                    }
                }
            }
            Err(e) => {
                warn!("error reading pending rework key: {e}");
            }
        }
    }

    Ok(None)
}

/// Unregister a worker — release any held claim, remove from registry.
pub async fn unregister_worker(
    state: &Arc<DispatcherState>,
    worker_id: &str,
) -> DispatcherResult<()> {
    // Check if worker has a current job
    if let Some(info) = state.workers.get(worker_id) {
        if let Some(ref job_key) = info.current_job {
            // Release claim and requeue the job
            crate::claims::release_claim(state, job_key).await?;
            crate::jobs::transition_job(
                state,
                job_key,
                JobState::OnDeck,
                "worker_unregistered",
                Some(worker_id),
            )
            .await?;
        }
    }

    // Remove from KV and in-memory
    if let Err(e) = state.kv.workers.delete(worker_id).await {
        warn!(worker_id, "failed to delete worker from KV: {e}");
    }
    state.workers.remove(worker_id);

    // Clean up any pending reworks for this worker
    clean_pending_reworks_for_worker(state, worker_id).await;

    info!(worker_id, "worker unregistered");
    Ok(())
}

/// Remove pending reworks targeting a specific worker and leave the job
/// in ChangesRequested for reassignment.
pub async fn clean_pending_reworks_for_worker(
    state: &Arc<DispatcherState>,
    worker_id: &str,
) {
    use futures::StreamExt;

    let keys = match state.kv.pending_reworks.keys().await {
        Ok(k) => k,
        Err(e) => {
            warn!(worker_id, "failed to list pending reworks: {e}");
            return;
        }
    };
    tokio::pin!(keys);

    while let Some(key) = keys.next().await {
        if let Ok(key_str) = key {
            if let Ok(Some((rework, _))) =
                kv_get::<PendingRework>(&state.kv.pending_reworks, &key_str).await
            {
                if rework.worker_id == worker_id {
                    if let Err(e) = state.kv.pending_reworks.delete(&key_str).await {
                        warn!(key_str, "failed to delete pending rework: {e}");
                    }
                    // Job stays in ChangesRequested — will be picked up by
                    // normal assignment when a capable worker idles.
                }
            }
        }
    }
}

/// Prune a worker (remove from registry + KV). Called on lease expiry or job timeout.
pub async fn prune_worker(
    state: &Arc<DispatcherState>,
    worker_id: &str,
) -> DispatcherResult<()> {
    if let Err(e) = state.kv.workers.delete(worker_id).await {
        debug!(worker_id, "failed to delete worker during prune: {e}");
    }
    state.workers.remove(worker_id);

    clean_pending_reworks_for_worker(state, worker_id).await;

    info!(worker_id, "worker pruned");
    Ok(())
}

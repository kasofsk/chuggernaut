use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, warn};

use forge2_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs::{kv_cas_create, kv_cas_update, kv_get};
use crate::state::DispatcherState;

/// Acquire a claim for a job by a worker. CAS-create ensures exclusivity.
pub async fn acquire_claim(
    state: &Arc<DispatcherState>,
    job_key: &str,
    worker_id: &str,
    timeout_secs: u64,
) -> DispatcherResult<ClaimState> {
    let now = Utc::now();
    let lease_secs = state.config.lease_secs;
    let claim = ClaimState {
        worker_id: worker_id.to_string(),
        claimed_at: now,
        last_heartbeat: now,
        lease_deadline: now + chrono::Duration::seconds(lease_secs as i64),
        timeout_secs,
        lease_secs,
    };

    kv_cas_create(&state.kv.claims, job_key, &claim).await?;
    debug!(job_key, worker_id, "claim acquired");
    Ok(claim)
}

/// Update claim lease on heartbeat.
pub async fn renew_lease(
    state: &Arc<DispatcherState>,
    job_key: &str,
    worker_id: &str,
) -> DispatcherResult<()> {
    let (mut claim, revision) = match kv_get::<ClaimState>(&state.kv.claims, job_key).await? {
        Some(r) => r,
        None => {
            debug!(job_key, worker_id, "heartbeat for missing claim — ignoring");
            return Ok(());
        }
    };

    if claim.worker_id != worker_id {
        debug!(
            job_key,
            worker_id,
            claim_worker = claim.worker_id,
            "heartbeat from wrong worker — ignoring"
        );
        return Ok(());
    }

    let now = Utc::now();
    claim.last_heartbeat = now;
    claim.lease_deadline = now + chrono::Duration::seconds(claim.lease_secs as i64);

    // CAS collision on heartbeat is benign — another heartbeat won the race
    match kv_cas_update(
        &state.kv.claims,
        job_key,
        &claim,
        revision,
        state.config.cas_max_retries,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(DispatcherError::CasExhausted { .. }) => {
            debug!(job_key, "heartbeat CAS exhausted — benign");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Release a claim by deleting it from KV (tombstone).
pub async fn release_claim(
    state: &Arc<DispatcherState>,
    job_key: &str,
) -> DispatcherResult<()> {
    match state.kv.claims.delete(job_key).await {
        Ok(_) => {
            debug!(job_key, "claim released");
            Ok(())
        }
        Err(e) => {
            warn!(job_key, "failed to release claim: {e}");
            // Not fatal — monitor will catch stale claims
            Ok(())
        }
    }
}

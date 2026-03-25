use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, warn};

use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs::{kv_cas_create, kv_cas_rmw};
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
    let wid = worker_id.to_string();
    // CAS collision on heartbeat is benign — another heartbeat won the race
    match kv_cas_rmw::<ClaimState, _>(
        &state.kv.claims,
        job_key,
        state.config.cas_max_retries,
        |claim| {
            if claim.worker_id != wid {
                return Err(DispatcherError::Validation(
                    format!("heartbeat from wrong worker {wid}, claim held by {}", claim.worker_id),
                ));
            }
            let now = Utc::now();
            claim.last_heartbeat = now;
            claim.lease_deadline = now + chrono::Duration::seconds(claim.lease_secs as i64);
            Ok(())
        },
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(DispatcherError::Kv(msg)) if msg.contains("not found") => {
            debug!(job_key, worker_id, "heartbeat for missing claim — ignoring");
            Ok(())
        }
        Err(DispatcherError::Validation(msg)) if msg.contains("wrong worker") => {
            debug!(job_key, worker_id, "{msg}");
            Ok(())
        }
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

use std::sync::Arc;

use bytes::Bytes;
use chrono::Utc;
use tracing::{debug, info};

use forge2_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::jobs::{kv_cas_update, kv_get};
use crate::state::DispatcherState;

/// Try to assign any OnDeck job to a specific idle worker.
pub async fn try_assign_to_worker(
    state: &Arc<DispatcherState>,
    worker_id: &str,
) -> DispatcherResult<bool> {
    let worker = match state.workers.get(worker_id) {
        Some(w) => w.clone(),
        None => return Ok(false),
    };

    // Find best matching OnDeck job for this worker
    let mut candidates: Vec<Job> = state
        .jobs
        .iter()
        .filter(|entry| entry.value().state == JobState::OnDeck)
        .map(|entry| entry.value().clone())
        .collect();

    // Sort by priority descending
    candidates.sort_by(|a, b| b.priority.cmp(&a.priority));

    for job in candidates {
        if !matches_worker(&job, &worker) {
            continue;
        }
        if is_blacklisted(state, &job.key, worker_id).await {
            continue;
        }
        match assign_job(state, &job.key, worker_id, false, None).await {
            Ok(_) => return Ok(true),
            Err(e) => {
                debug!(job_key = job.key, worker_id, "assignment failed: {e}");
                continue;
            }
        }
    }

    Ok(false)
}

/// Try to assign a specific OnDeck job to any idle worker.
pub async fn try_assign_job(
    state: &Arc<DispatcherState>,
    job_key: &str,
) -> DispatcherResult<bool> {
    let job = match state.jobs.get(job_key) {
        Some(j) => j.clone(),
        None => return Ok(false),
    };

    if job.state != JobState::OnDeck && job.state != JobState::ChangesRequested {
        return Ok(false);
    }

    // Get all idle workers, sorted by longest idle (oldest last_seen first)
    let mut idle_workers: Vec<WorkerInfo> = state
        .workers
        .iter()
        .filter(|entry| entry.value().state == WorkerState::Idle)
        .map(|entry| entry.value().clone())
        .collect();

    idle_workers.sort_by(|a, b| a.last_seen.cmp(&b.last_seen));

    for worker in idle_workers {
        if !matches_worker(&job, &worker) {
            continue;
        }
        if is_blacklisted(state, job_key, &worker.worker_id).await {
            continue;
        }
        match assign_job(state, job_key, &worker.worker_id, false, None).await {
            Ok(_) => return Ok(true),
            Err(e) => {
                debug!(job_key, worker_id = worker.worker_id, "assignment failed: {e}");
                continue;
            }
        }
    }

    // No idle workers — check for preemption
    if job.priority > 50 {
        try_preempt(state, &job).await?;
    }

    Ok(false)
}

/// Check if a worker matches a job's requirements.
fn matches_worker(job: &Job, worker: &WorkerInfo) -> bool {
    // Capabilities: worker must have all required
    for cap in &job.capabilities {
        if !worker.capabilities.contains(cap) {
            return false;
        }
    }
    // Worker type
    if let Some(ref wt) = job.worker_type {
        if *wt != worker.worker_type {
            return false;
        }
    }
    // Platform
    if let Some(ref plat) = job.platform {
        if !worker.platform.contains(plat) {
            return false;
        }
    }
    true
}

/// Check if a (job, worker) pair is blacklisted.
async fn is_blacklisted(
    state: &Arc<DispatcherState>,
    job_key: &str,
    worker_id: &str,
) -> bool {
    let bl_key = format!("{job_key}.{worker_id}");
    matches!(
        state.kv.abandon_blacklist.entry(&bl_key).await,
        Ok(Some(_))
    )
}

/// Assign a specific job to a specific worker.
pub async fn assign_job(
    state: &Arc<DispatcherState>,
    job_key: &str,
    worker_id: &str,
    is_rework: bool,
    review_feedback: Option<&str>,
) -> DispatcherResult<()> {
    let job = match state.jobs.get(job_key) {
        Some(j) => j.clone(),
        None => return Err(DispatcherError::JobNotFound(job_key.to_string())),
    };

    // Acquire claim (CAS-create ensures exclusivity)
    let timeout_secs = job.timeout_secs;
    let claim = crate::claims::acquire_claim(state, job_key, worker_id, timeout_secs).await?;

    // Transition job to OnTheStack
    let trigger = if is_rework {
        "rework_assigned"
    } else {
        "dispatcher_assigned"
    };
    let mut updated_job =
        crate::jobs::transition_job(state, job_key, JobState::OnTheStack, trigger, Some(worker_id))
            .await?;

    // Set last_worker_id
    updated_job.last_worker_id = Some(worker_id.to_string());
    // Re-read to get fresh revision for the update
    if let Some((_, revision)) = kv_get::<Job>(&state.kv.jobs, job_key).await? {
        kv_cas_update(
            &state.kv.jobs,
            job_key,
            &updated_job,
            revision,
            state.config.cas_max_retries,
        )
        .await?;
        state.jobs.insert(job_key.to_string(), updated_job.clone());
    }

    // Update worker state to Busy
    if let Some((mut worker_info, revision)) =
        kv_get::<WorkerInfo>(&state.kv.workers, worker_id).await?
    {
        worker_info.state = WorkerState::Busy;
        worker_info.current_job = Some(job_key.to_string());
        worker_info.last_seen = Utc::now();
        kv_cas_update(
            &state.kv.workers,
            worker_id,
            &worker_info,
            revision,
            state.config.cas_max_retries,
        )
        .await?;
        state.workers.insert(worker_id.to_string(), worker_info);
    }

    // Send assignment to worker
    let assignment = Assignment {
        job: updated_job.clone(),
        claim,
        is_rework,
        review_feedback: review_feedback.map(String::from),
    };
    let subject = subjects::dispatch_assign(worker_id);
    let payload = serde_json::to_vec(&assignment)?;
    state
        .nats
        .publish(&subject, Bytes::from(payload))
        .await?;

    // Journal
    crate::jobs::journal_append(
        state,
        "assigned",
        Some(job_key),
        Some(worker_id),
        Some(&format!(
            "priority {}, is_rework: {is_rework}",
            updated_job.priority
        )),
    )
    .await;

    info!(job_key, worker_id, is_rework, "job assigned");
    Ok(())
}

/// Add a (job, worker) pair to the abandon blacklist.
pub async fn blacklist_worker(
    state: &Arc<DispatcherState>,
    job_key: &str,
    worker_id: &str,
) -> DispatcherResult<()> {
    let bl_key = format!("{job_key}.{worker_id}");
    let data = Bytes::from("1");
    state
        .kv
        .abandon_blacklist
        .put(&bl_key, data)
        .await
        .map_err(|e| DispatcherError::Kv(e.to_string()))?;
    debug!(job_key, worker_id, "blacklisted");
    Ok(())
}

/// Try to preempt a lower-priority worker for a high-priority job.
async fn try_preempt(
    state: &Arc<DispatcherState>,
    high_prio_job: &Job,
) -> DispatcherResult<()> {
    let mut lowest_prio_worker: Option<(String, u8)> = None;

    for entry in state.workers.iter() {
        let worker = entry.value();
        if worker.state != WorkerState::Busy {
            continue;
        }
        if !matches_worker(high_prio_job, worker) {
            continue;
        }
        if let Some(ref current_job_key) = worker.current_job {
            if let Some(current_job) = state.jobs.get(current_job_key) {
                if current_job.priority < high_prio_job.priority {
                    match &lowest_prio_worker {
                        None => {
                            lowest_prio_worker =
                                Some((worker.worker_id.clone(), current_job.priority));
                        }
                        Some((_, lowest_prio)) => {
                            if current_job.priority < *lowest_prio {
                                lowest_prio_worker =
                                    Some((worker.worker_id.clone(), current_job.priority));
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some((worker_id, _)) = lowest_prio_worker {
        let notice = PreemptNotice {
            reason: format!(
                "Higher-priority job {} (priority {}) needs this worker",
                high_prio_job.key, high_prio_job.priority
            ),
            new_job_key: high_prio_job.key.clone(),
        };
        let subject = subjects::dispatch_preempt(&worker_id);
        let payload = serde_json::to_vec(&notice)?;
        state
            .nats
            .publish(&subject, Bytes::from(payload))
            .await?;
        info!(
            worker_id,
            new_job = high_prio_job.key,
            "preempt notice sent"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_job(caps: &[&str], worker_type: Option<&str>, platform: Option<&str>) -> Job {
        Job {
            key: "test.repo.1".to_string(),
            repo: "test/repo".to_string(),
            state: JobState::OnDeck,
            title: "test".to_string(),
            body: String::new(),
            priority: 50,
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            worker_type: worker_type.map(String::from),
            platform: platform.map(String::from),
            timeout_secs: 3600,
            review: ReviewLevel::High,
            max_retries: 3,
            retry_count: 0,
            retry_after: None,
            pr_url: None,
            last_worker_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn make_worker(
        id: &str,
        caps: &[&str],
        worker_type: &str,
        platform: &[&str],
    ) -> WorkerInfo {
        WorkerInfo {
            worker_id: id.to_string(),
            state: WorkerState::Idle,
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            worker_type: worker_type.to_string(),
            platform: platform.iter().map(|s| s.to_string()).collect(),
            current_job: None,
            last_seen: Utc::now(),
        }
    }

    #[test]
    fn matches_no_requirements() {
        let job = make_job(&[], None, None);
        let worker = make_worker("w1", &["rust"], "sim", &["linux"]);
        assert!(matches_worker(&job, &worker));
    }

    #[test]
    fn matches_capabilities() {
        let job = make_job(&["rust", "wasm"], None, None);
        let worker = make_worker("w1", &["rust", "wasm", "python"], "sim", &["linux"]);
        assert!(matches_worker(&job, &worker));
    }

    #[test]
    fn rejects_missing_capability() {
        let job = make_job(&["rust", "wasm"], None, None);
        let worker = make_worker("w1", &["rust"], "sim", &["linux"]);
        assert!(!matches_worker(&job, &worker));
    }

    #[test]
    fn matches_worker_type() {
        let job = make_job(&[], Some("interactive"), None);
        let worker = make_worker("w1", &[], "interactive", &["linux"]);
        assert!(matches_worker(&job, &worker));
    }

    #[test]
    fn rejects_wrong_worker_type() {
        let job = make_job(&[], Some("interactive"), None);
        let worker = make_worker("w1", &[], "sim", &["linux"]);
        assert!(!matches_worker(&job, &worker));
    }

    #[test]
    fn matches_platform() {
        let job = make_job(&[], None, Some("linux"));
        let worker = make_worker("w1", &[], "sim", &["linux", "macos"]);
        assert!(matches_worker(&job, &worker));
    }

    #[test]
    fn rejects_wrong_platform() {
        let job = make_job(&[], None, Some("windows"));
        let worker = make_worker("w1", &[], "sim", &["linux"]);
        assert!(!matches_worker(&job, &worker));
    }

    #[test]
    fn matches_all_constraints() {
        let job = make_job(&["rust"], Some("sim"), Some("linux"));
        let worker = make_worker("w1", &["rust", "go"], "sim", &["linux"]);
        assert!(matches_worker(&job, &worker));
    }

    #[test]
    fn rejects_when_one_constraint_fails() {
        let job = make_job(&["rust"], Some("interactive"), Some("linux"));
        let worker = make_worker("w1", &["rust"], "sim", &["linux"]);
        assert!(!matches_worker(&job, &worker));
    }
}

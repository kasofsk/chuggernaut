use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::kv;
use bytes::Bytes;
use chrono::Utc;
use tracing::{debug, error, warn};

use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::state::DispatcherState;

// ---------------------------------------------------------------------------
// CAS helpers
// ---------------------------------------------------------------------------

/// Read a value from KV, returning (value, revision).
pub async fn kv_get<T: serde::de::DeserializeOwned>(
    store: &kv::Store,
    key: &str,
) -> DispatcherResult<Option<(T, u64)>> {
    match store.entry(key).await {
        Ok(Some(entry)) => {
            let val: T = serde_json::from_slice(&entry.value)?;
            Ok(Some((val, entry.revision)))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(DispatcherError::Kv(e.to_string())),
    }
}

/// CAS-update a KV key with retry.
/// Read-modify-write with CAS retry. On each attempt (including the first),
/// reads the current value, passes it to `mutate`, then CAS-writes the result.
/// If CAS fails, the entire cycle restarts with a fresh read so the closure
/// always sees the latest state.
pub async fn kv_cas_rmw<T, F>(
    store: &kv::Store,
    key: &str,
    max_retries: u32,
    mutate: F,
) -> DispatcherResult<(T, u64)>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Clone,
    F: Fn(&mut T) -> DispatcherResult<()>,
{
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let backoff = Duration::from_millis(10 * (1 << (attempt - 1)));
            tokio::time::sleep(backoff).await;
        }

        let (mut value, revision) = match kv_get::<T>(store, key).await? {
            Some(r) => r,
            None => {
                return Err(DispatcherError::Kv(format!(
                    "key {key} not found during CAS rmw"
                )));
            }
        };

        mutate(&mut value)?;

        let data = Bytes::from(serde_json::to_vec(&value)?);
        match store.update(key, data, revision).await {
            Ok(rev) => return Ok((value, rev)),
            Err(e) => {
                debug!(key, attempt, "CAS rmw retry: {e}");
            }
        }
    }

    Err(DispatcherError::CasExhausted {
        key: key.to_string(),
        retries: max_retries,
    })
}

/// CAS-create a KV key (must not exist or be a tombstone).
pub async fn kv_cas_create<T: serde::Serialize>(
    store: &kv::Store,
    key: &str,
    value: &T,
) -> DispatcherResult<u64> {
    let data = Bytes::from(serde_json::to_vec(value)?);
    store
        .create(key, data)
        .await
        .map_err(|e| DispatcherError::Kv(e.to_string()))
}

/// Put a KV value (unconditional write).
pub async fn kv_put<T: serde::Serialize>(
    store: &kv::Store,
    key: &str,
    value: &T,
) -> DispatcherResult<u64> {
    let data = Bytes::from(serde_json::to_vec(value)?);
    store
        .put(key, data)
        .await
        .map_err(|e| DispatcherError::Kv(e.to_string()))
}

// ---------------------------------------------------------------------------
// Job creation
// ---------------------------------------------------------------------------

pub async fn create_job(
    state: &Arc<DispatcherState>,
    req: CreateJobRequest,
) -> DispatcherResult<String> {
    // Validate repo name
    let parts: Vec<&str> = req.repo.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Err(DispatcherError::Validation(format!(
            "repo must be in owner/repo format, got: {:?}",
            req.repo
        )));
    }
    let (owner, repo) = (parts[0], parts[1]);
    validate_repo_name(owner, repo).map_err(DispatcherError::Validation)?;

    // Validate claude args if provided
    if let Some(ref args) = req.claude_args {
        validate_claude_args(args, &state.config.allowed_claude_flags)
            .map_err(DispatcherError::Validation)?;
    }

    // Increment counter
    let counter_key = format!("{owner}.{repo}");
    let seq = increment_counter(&state.kv.counters, &counter_key).await?;
    let key = job_key(owner, repo, seq);

    // Compute initial state
    let initial_state = if let Some(s) = req.initial_state {
        match s {
            JobState::OnIce => JobState::OnIce,
            _ => {
                return Err(DispatcherError::Validation(
                    "initial_state must be on-ice if set".to_string(),
                ));
            }
        }
    } else if req.depends_on.is_empty() {
        JobState::OnDeck
    } else {
        // Check if all deps are done
        let all_done = check_deps_done(state, owner, repo, &req.depends_on).await?;
        if all_done {
            JobState::OnDeck
        } else {
            JobState::Blocked
        }
    };

    let mut builder = Job::builder(key.clone(), req.repo.clone(), req.title)
        .body(req.body)
        .state(initial_state)
        .priority(req.priority)
        .capabilities(req.capabilities)
        .timeout_secs(req.timeout_secs)
        .review(req.review)
        .max_retries(req.max_retries);
    if let Some(p) = req.platform {
        builder = builder.platform(p);
    }
    if let Some(args) = req.claude_args {
        builder = builder.claude_args(args);
    }
    if let Some(limit) = req.rework_limit {
        builder = builder.rework_limit(limit);
    }
    let job = builder.build();

    // Write job to KV
    kv_cas_create(&state.kv.jobs, &key, &job).await?;

    // Write deps if any
    if !req.depends_on.is_empty() {
        let dep_keys: Vec<String> = req
            .depends_on
            .iter()
            .map(|seq| job_key(owner, repo, *seq))
            .collect();
        crate::deps::create_deps(state, &key, &dep_keys).await?;
    }

    // Insert into in-memory index
    state.jobs.insert(key.clone(), job.clone());

    // Log activity
    let activity = ActivityEntry {
        timestamp: Utc::now(),
        kind: "created".to_string(),
        message: format!("Job created with state {initial_state:?}"),
    };
    append_activity(state, &key, activity).await;

    // Log to journal
    journal_append(
        state,
        "created",
        Some(&key),
        None,
        Some(&format!(
            "priority {}, state {:?}",
            req.priority, initial_state
        )),
    )
    .await;

    // Publish a "created" transition so stream consumers (e.g. issue sync)
    // can react to new jobs without coupling to the creation code.
    publish_transition(state, &key, initial_state, initial_state, "created", None).await;

    debug!(key, ?initial_state, "job created");

    Ok(key)
}

async fn increment_counter(store: &kv::Store, counter_key: &str) -> DispatcherResult<u64> {
    // Try to read current value
    match store.entry(counter_key).await {
        Ok(Some(entry)) => {
            let current: u64 = String::from_utf8_lossy(&entry.value).parse().unwrap_or(0);
            let next = current + 1;
            let data = Bytes::from(next.to_string());
            store
                .update(counter_key, data, entry.revision)
                .await
                .map_err(|e| DispatcherError::Kv(e.to_string()))?;
            Ok(next)
        }
        Ok(None) => {
            let data = Bytes::from("1".to_string());
            store
                .create(counter_key, data)
                .await
                .map_err(|e| DispatcherError::Kv(e.to_string()))?;
            Ok(1)
        }
        Err(e) => Err(DispatcherError::Kv(e.to_string())),
    }
}

async fn check_deps_done(
    state: &Arc<DispatcherState>,
    owner: &str,
    repo: &str,
    deps: &[u64],
) -> DispatcherResult<bool> {
    for seq in deps {
        let dep_key = job_key(owner, repo, *seq);
        if let Some(job) = state.jobs.get(&dep_key) {
            if job.state != JobState::Done {
                return Ok(false);
            }
        } else {
            // Check KV
            match kv_get::<Job>(&state.kv.jobs, &dep_key).await? {
                Some((job, _)) => {
                    if job.state != JobState::Done {
                        return Ok(false);
                    }
                }
                None => {
                    return Err(DispatcherError::JobNotFound(dep_key));
                }
            }
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// State transitions
// ---------------------------------------------------------------------------

/// Transition a job to a new state with CAS update.
pub async fn transition_job(
    state: &Arc<DispatcherState>,
    job_key: &str,
    to: JobState,
    trigger: &str,
    worker_id: Option<&str>,
) -> DispatcherResult<Job> {
    use std::sync::Mutex;

    let from_cell: Mutex<Option<JobState>> = Mutex::new(None);

    let (job, _rev) = kv_cas_rmw::<Job, _>(
        &state.kv.jobs,
        job_key,
        state.config.cas_max_retries,
        |job| {
            let from = job.state;
            validate_transition(from, to)?;
            *from_cell.lock().unwrap() = Some(from);
            job.state = to;
            job.updated_at = Utc::now();
            Ok(())
        },
    )
    .await
    .map_err(|e| match e {
        DispatcherError::Kv(msg) if msg.contains("not found") => {
            DispatcherError::JobNotFound(job_key.to_string())
        }
        other => other,
    })?;

    let from = from_cell.into_inner().unwrap().unwrap();

    // Update in-memory index
    state.jobs.insert(job_key.to_string(), job.clone());

    // Auto-release claim when leaving an active state (OnTheStack or InReview).
    // This prevents claim leaks — callers don't need to remember to release.
    if (from == JobState::OnTheStack || from == JobState::Reviewing)
        && to != JobState::OnTheStack
        && to != JobState::Reviewing
        && let Err(e) = crate::claims::release_claim(state, job_key).await
    {
        tracing::debug!(job_key, error = %e, "auto claim release (may already be released)");
    }

    // Publish transition event
    publish_transition(state, job_key, from, to, trigger, worker_id).await;

    debug!(job_key, ?from, ?to, trigger, "state transition");

    Ok(job)
}

/// Validate that a state transition is allowed.
fn validate_transition(from: JobState, to: JobState) -> DispatcherResult<()> {
    use JobState::*;

    // Terminal states reject all further transitions.
    if from.is_terminal() {
        return Err(DispatcherError::InvalidTransition { from, to });
    }

    let valid = matches!(
        (from, to),
        // Normal flow
        (OnIce, OnDeck | Blocked)
            | (Blocked, OnDeck)
            | (OnDeck, OnTheStack)
            | (OnTheStack, InReview | Failed | OnDeck)
            | (InReview, Reviewing | ChangesRequested | Escalated)
            | (Reviewing, Done | Escalated | ChangesRequested | InReview)
            | (Escalated, Done | ChangesRequested)
            | (ChangesRequested, OnTheStack)
            | (Failed, OnDeck | Blocked | OnIce)
            // Admin close from any non-terminal state (guarded above)
            | (_, Done | Revoked)
    );

    if valid {
        Ok(())
    } else {
        Err(DispatcherError::InvalidTransition { from, to })
    }
}

// ---------------------------------------------------------------------------
// Transition publishing
// ---------------------------------------------------------------------------

async fn publish_transition(
    state: &Arc<DispatcherState>,
    job_key_str: &str,
    from: JobState,
    to: JobState,
    trigger: &str,
    worker_id: Option<&str>,
) {
    let transition = JobTransition {
        job_key: job_key_str.to_string(),
        from_state: from,
        to_state: to,
        timestamp: Utc::now(),
        trigger: trigger.to_string(),
        worker_id: worker_id.map(String::from),
    };

    // Use regular publish (fire-and-forget) for transitions.
    // The stream captures them via subject matching. No need to await JS ack.
    if let Err(e) = state
        .nats
        .publish_to(&subjects::TRANSITIONS, job_key_str, &transition)
        .await
    {
        error!(job_key = job_key_str, "failed to publish transition: {e}");
    }
}

// ---------------------------------------------------------------------------
// Activity / Journal helpers
// ---------------------------------------------------------------------------

pub async fn append_activity(state: &Arc<DispatcherState>, job_key: &str, entry: ActivityEntry) {
    let result: Result<(), DispatcherError> = async {
        let mut log = match kv_get::<ActivityLog>(&state.kv.activities, job_key).await? {
            Some((log, _)) => log,
            None => ActivityLog {
                entries: Vec::new(),
            },
        };

        if log.entries.len() >= state.config.activity_limit {
            return Ok(()); // silently drop
        }

        log.entries.push(entry);
        kv_put(&state.kv.activities, job_key, &log).await?;
        Ok(())
    }
    .await;

    if let Err(e) = result {
        warn!(job_key, "failed to append activity: {e}");
    }
}

pub async fn journal_append(
    state: &Arc<DispatcherState>,
    action: &str,
    job_key: Option<&str>,
    worker_id: Option<&str>,
    details: Option<&str>,
) {
    let now = Utc::now();
    let entry = JournalEntry {
        timestamp: now,
        action: action.to_string(),
        job_key: job_key.map(String::from),
        worker_id: worker_id.map(String::from),
        details: details.map(String::from),
    };

    let key = format!("{}.{}", now.timestamp_nanos_opt().unwrap_or(0), fastrand());
    if let Err(e) = kv_put(&state.kv.journal, &key, &entry).await {
        warn!("failed to append journal: {e}");
    }
}

/// Simple fast random for journal key uniqueness.
fn fastrand() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_transitions() {
        use JobState::*;
        // Normal flow
        assert!(validate_transition(OnIce, OnDeck).is_ok());
        assert!(validate_transition(OnIce, Blocked).is_ok());
        assert!(validate_transition(Blocked, OnDeck).is_ok());
        assert!(validate_transition(OnDeck, OnTheStack).is_ok());
        assert!(validate_transition(OnTheStack, InReview).is_ok());
        assert!(validate_transition(OnTheStack, Failed).is_ok());
        assert!(validate_transition(OnTheStack, OnDeck).is_ok()); // requeue / partial yield
        // Review flow
        assert!(validate_transition(InReview, Reviewing).is_ok());
        assert!(validate_transition(InReview, ChangesRequested).is_ok()); // CI failure rework
        assert!(validate_transition(Reviewing, Done).is_ok());
        assert!(validate_transition(Reviewing, Escalated).is_ok());
        assert!(validate_transition(Reviewing, ChangesRequested).is_ok());
        assert!(validate_transition(Reviewing, InReview).is_ok()); // review dispatch failed, revert
        // Escalation
        assert!(validate_transition(Escalated, Done).is_ok());
        assert!(validate_transition(Escalated, ChangesRequested).is_ok());
        // Rework
        assert!(validate_transition(ChangesRequested, OnTheStack).is_ok());
        // Retry
        assert!(validate_transition(Failed, OnDeck).is_ok());
        assert!(validate_transition(Failed, Blocked).is_ok());
        // Admin close from any
        assert!(validate_transition(OnTheStack, Done).is_ok());
        assert!(validate_transition(Blocked, Revoked).is_ok());
        assert!(validate_transition(Reviewing, Done).is_ok());
    }

    #[test]
    fn invalid_transitions() {
        use JobState::*;
        assert!(validate_transition(Blocked, OnTheStack).is_err());
        assert!(validate_transition(OnDeck, InReview).is_err());
        assert!(validate_transition(OnIce, OnTheStack).is_err());
        assert!(validate_transition(InReview, OnDeck).is_err());
        // InReview → Escalated is valid (rework limit exceeded while InReview)
        assert!(validate_transition(InReview, Escalated).is_ok());
        // Note: InReview → Done is valid via admin close (_, Done | Revoked)
        // Can't skip Reviewing
        assert!(validate_transition(OnTheStack, Reviewing).is_err());
        // Terminal states reject all further transitions
        assert!(validate_transition(Done, Done).is_err());
        assert!(validate_transition(Done, Revoked).is_err());
        assert!(validate_transition(Revoked, Done).is_err());
        assert!(validate_transition(Revoked, Revoked).is_err());
        assert!(validate_transition(Done, OnDeck).is_err());
    }
}

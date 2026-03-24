use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use chuggernaut_forgejo_api::MergePullRequestOption;
use chuggernaut_types::parse_job_key;

use crate::error::{ReviewerError, ReviewerResult};
use crate::state::ReviewerState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeLock {
    pub holder: String,
    pub acquired_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueState {
    pub entries: Vec<MergeQueueItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueItem {
    pub job_key: String,
    pub pr_url: String,
    pub queued_at: chrono::DateTime<Utc>,
}

/// Try to acquire the merge lock for a repo. Returns Ok(true) if acquired,
/// Ok(false) if held by another job (caller should enqueue).
pub async fn try_acquire_lock(
    state: &Arc<ReviewerState>,
    owner: &str,
    repo: &str,
    job_key: &str,
) -> ReviewerResult<bool> {
    let lock_key = format!("{owner}.{repo}.lock");
    let lock = MergeLock {
        holder: job_key.to_string(),
        acquired_at: Utc::now(),
    };
    let payload = serde_json::to_vec(&lock)?;

    match state
        .kv
        .merge_queue
        .create(&lock_key, payload.into())
        .await
    {
        Ok(_) => {
            info!(job_key, repo = format!("{owner}/{repo}"), "merge lock acquired");
            Ok(true)
        }
        Err(_) => {
            debug!(job_key, repo = format!("{owner}/{repo}"), "merge lock held by another job");
            Ok(false)
        }
    }
}

/// Release the merge lock for a repo.
pub async fn release_lock(
    state: &Arc<ReviewerState>,
    owner: &str,
    repo: &str,
) -> ReviewerResult<()> {
    let lock_key = format!("{owner}.{repo}.lock");
    let _ = state.kv.merge_queue.delete(&lock_key).await;
    debug!(repo = format!("{owner}/{repo}"), "merge lock released");
    Ok(())
}

/// Enqueue a job for merge after the current lock holder finishes.
pub async fn enqueue(
    state: &Arc<ReviewerState>,
    owner: &str,
    repo: &str,
    job_key: &str,
    pr_url: &str,
) -> ReviewerResult<()> {
    let queue_key = format!("{owner}.{repo}.queue");
    let item = MergeQueueItem {
        job_key: job_key.to_string(),
        pr_url: pr_url.to_string(),
        queued_at: Utc::now(),
    };

    // Read existing queue, append, write back
    let (mut queue, revision) = match state.kv.merge_queue.entry(&queue_key).await? {
        Some(entry) => {
            let q: MergeQueueState = serde_json::from_slice(&entry.value)?;
            (q, Some(entry.revision))
        }
        None => (MergeQueueState { entries: vec![] }, None),
    };

    queue.entries.push(item);
    queue.entries.sort_by(|a, b| a.queued_at.cmp(&b.queued_at));

    let payload = serde_json::to_vec(&queue)?;
    match revision {
        Some(rev) => {
            state
                .kv
                .merge_queue
                .update(&queue_key, payload.into(), rev)
                .await
                .map_err(|e| ReviewerError::Kv(e.to_string()))?;
        }
        None => {
            state
                .kv
                .merge_queue
                .put(&queue_key, payload.into())
                .await?;
        }
    }

    info!(job_key, repo = format!("{owner}/{repo}"), "enqueued for merge");
    Ok(())
}

/// Merge a PR using rebase strategy with retry.
/// Returns Ok(()) on success, or Err with details on conflict.
pub async fn merge_pr(
    state: &Arc<ReviewerState>,
    owner: &str,
    repo: &str,
    pr_index: u64,
) -> ReviewerResult<()> {
    let opts = MergePullRequestOption {
        method: "rebase".to_string(),
        merge_message_field: None,
    };

    let forgejo = state.forgejo.clone();
    let o = owner.to_string();
    let r = repo.to_string();
    crate::forgejo_retry::retry("merge_pull_request", || {
        let forgejo = forgejo.clone();
        let o = o.clone();
        let r = r.clone();
        let opts = opts.clone();
        async move { forgejo.merge_pull_request(&o, &r, pr_index, &opts).await }
    })
    .await?;

    info!(pr_index, repo = format!("{owner}/{repo}"), "PR merged");
    Ok(())
}

/// Process the next item in the merge queue after releasing a lock.
pub async fn process_queue(
    state: &Arc<ReviewerState>,
    owner: &str,
    repo: &str,
) -> ReviewerResult<Option<MergeQueueItem>> {
    let queue_key = format!("{owner}.{repo}.queue");

    let entry = match state.kv.merge_queue.entry(&queue_key).await? {
        Some(e) => e,
        None => return Ok(None),
    };

    let mut queue: MergeQueueState = serde_json::from_slice(&entry.value)?;
    if queue.entries.is_empty() {
        return Ok(None);
    }

    let next = queue.entries.remove(0);
    let payload = serde_json::to_vec(&queue)?;

    match state
        .kv
        .merge_queue
        .update(&queue_key, payload.into(), entry.revision)
        .await
    {
        Ok(_) => Ok(Some(next)),
        Err(e) => {
            warn!("CAS conflict processing merge queue: {e}");
            Ok(None)
        }
    }
}

/// Clean up stale merge locks. A lock is stale if the holding job is no longer
/// in a reviewable state (i.e., it's Done, Failed, or gone). Returns the number
/// of locks cleaned.
pub async fn cleanup_stale_locks(
    state: &Arc<ReviewerState>,
) -> ReviewerResult<usize> {
    use futures::StreamExt;

    let mut cleaned = 0;
    let keys = state.kv.merge_queue.keys().await
        .map_err(|e| ReviewerError::Kv(e.to_string()))?;
    tokio::pin!(keys);

    while let Some(key) = keys.next().await {
        let key = match key {
            Ok(k) => k,
            Err(_) => continue,
        };
        // Only check lock keys (not queue keys)
        if !key.ends_with(".lock") {
            continue;
        }
        if let Some(entry) = state.kv.merge_queue.entry(&key).await? {
            if let Ok(lock) = serde_json::from_slice::<MergeLock>(&entry.value) {
                // Check if the holding job still exists and is in a review-related state
                match crate::dispatcher_client::get_job(state, &lock.holder).await {
                    Ok(job) => {
                        // If job is terminal, the lock is stale
                        let is_terminal = matches!(
                            job.state,
                            chuggernaut_types::JobState::Done
                                | chuggernaut_types::JobState::Failed
                                | chuggernaut_types::JobState::Revoked
                        );
                        if is_terminal {
                            let _ = state.kv.merge_queue.delete(&key).await;
                            info!(key, holder = lock.holder, "released stale merge lock (job terminal)");
                            cleaned += 1;
                        }
                    }
                    Err(_) => {
                        // Job not found — lock is definitely stale
                        let _ = state.kv.merge_queue.delete(&key).await;
                        info!(key, holder = lock.holder, "released stale merge lock (job not found)");
                        cleaned += 1;
                    }
                }
            }
        }
    }

    Ok(cleaned)
}

/// Extract (owner, repo, pr_index) from a PR URL.
/// Expected format: http://host/owner/repo/pulls/123
pub fn parse_pr_url(pr_url: &str) -> Option<(String, String, u64)> {
    let parts: Vec<&str> = pr_url.trim_end_matches('/').rsplit('/').collect();
    if parts.len() < 4 || parts[1] != "pulls" {
        return None;
    }
    let index: u64 = parts[0].parse().ok()?;
    let repo = parts[2].to_string();
    let owner = parts[3].to_string();
    Some((owner, repo, index))
}

/// Extract (owner, repo) from a job key like "owner.repo.seq".
pub fn parse_repo_from_key(job_key: &str) -> Option<(String, String)> {
    let (owner, repo, _) = parse_job_key(job_key)?;
    Some((owner.to_string(), repo.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_url_valid() {
        let (owner, repo, index) =
            parse_pr_url("http://localhost:3000/test/myrepo/pulls/42").unwrap();
        assert_eq!(owner, "test");
        assert_eq!(repo, "myrepo");
        assert_eq!(index, 42);
    }

    #[test]
    fn parse_pr_url_trailing_slash() {
        let (owner, repo, index) =
            parse_pr_url("http://localhost:3000/org/repo/pulls/7/").unwrap();
        assert_eq!(owner, "org");
        assert_eq!(repo, "repo");
        assert_eq!(index, 7);
    }

    #[test]
    fn parse_pr_url_invalid() {
        assert!(parse_pr_url("http://localhost:3000/bad/url").is_none());
        assert!(parse_pr_url("not a url").is_none());
    }

    #[test]
    fn parse_repo_from_key_valid() {
        let (owner, repo) = parse_repo_from_key("myorg.myrepo.42").unwrap();
        assert_eq!(owner, "myorg");
        assert_eq!(repo, "myrepo");
    }

    #[test]
    fn parse_repo_from_key_invalid() {
        assert!(parse_repo_from_key("invalid").is_none());
    }
}

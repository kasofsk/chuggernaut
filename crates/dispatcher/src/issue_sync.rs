use std::sync::Arc;

use async_nats::jetstream::consumer::pull;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use chuggernaut_git_provider::{CreateIssue, UpdateIssue};
use chuggernaut_types::{Job, JobTransition};

use crate::jobs::kv_get;
use crate::state::DispatcherState;

/// Mapping stored in the issue_map KV bucket: job_key → IssueMapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IssueMapping {
    issue_number: u64,
    issue_url: String,
}

/// Start the issue-sync background task.
///
/// Subscribes to the CHUGGERNAUT-TRANSITIONS JetStream stream and
/// creates/updates git-provider issues to mirror job state.
/// Best-effort: errors are logged and skipped — never blocks job flow.
pub fn start(state: Arc<DispatcherState>) {
    // Only run when git provider is configured.
    if state.config.git_url.is_none() || state.config.git_token.is_none() {
        info!("issue sync disabled (no git provider configured)");
        return;
    }

    tokio::spawn(async move {
        if let Err(e) = run(&state).await {
            error!("issue sync task failed: {e}");
        }
    });

    info!("issue sync started");
}

async fn run(state: &Arc<DispatcherState>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Backfill: create issues for any existing jobs that don't have one yet.
    if let Err(e) = backfill(state).await {
        warn!("issue sync: backfill failed: {e}");
    }

    let js = state.nats.jetstream();
    let prefix = state.nats.prefix();

    // Derive stream name and filter subject (respects test prefix).
    let stream_name = match prefix {
        Some(p) => format!("{}_{p}", chuggernaut_types::streams::TRANSITIONS),
        None => chuggernaut_types::streams::TRANSITIONS.to_string(),
    };
    let filter_subject = match prefix {
        Some(p) => format!("{p}_chuggernaut.transitions.>"),
        None => "chuggernaut.transitions.>".to_string(),
    };

    let stream = js.get_stream(&stream_name).await?;

    let consumer_name = match prefix {
        Some(p) => format!("issue-sync-{p}"),
        None => "issue-sync".to_string(),
    };

    let consumer = stream
        .create_consumer(pull::Config {
            durable_name: Some(consumer_name),
            filter_subject,
            ..Default::default()
        })
        .await?;

    let mut messages = consumer.messages().await?;

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                warn!("issue sync: error receiving message: {e}");
                continue;
            }
        };

        let transition: JobTransition = match serde_json::from_slice(&msg.payload) {
            Ok(t) => t,
            Err(e) => {
                warn!("issue sync: failed to deserialize transition: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        if let Err(e) = sync_issue(state, &transition).await {
            warn!(
                job_key = transition.job_key,
                "issue sync: failed to sync issue: {e}"
            );
        }

        if let Err(e) = msg.ack().await {
            warn!("issue sync: failed to ack message: {e}");
        }
    }

    Ok(())
}

async fn backfill(
    state: &Arc<DispatcherState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let keys = state.kv.jobs.keys().await?;
    tokio::pin!(keys);

    let mut created = 0u32;
    let mut skipped = 0u32;

    while let Some(key) = keys.next().await {
        let key_str = match key {
            Ok(k) => k,
            Err(_) => continue,
        };

        // Skip if we already have an issue for this job.
        if let Ok(Some(_)) = kv_get::<IssueMapping>(&state.kv.issue_map, &key_str).await {
            skipped += 1;
            continue;
        }

        let job = match kv_get::<Job>(&state.kv.jobs, &key_str).await {
            Ok(Some((j, _))) => j,
            _ => continue,
        };

        let transition = JobTransition {
            job_key: key_str,
            from_state: job.state,
            to_state: job.state,
            trigger: "backfill".to_string(),
            worker_id: None,
            timestamp: chrono::Utc::now(),
        };

        if let Err(e) = sync_issue(state, &transition).await {
            warn!(
                job_key = transition.job_key,
                "issue sync backfill: failed: {e}"
            );
        } else {
            created += 1;
        }
    }

    info!(created, skipped, "issue sync backfill complete");
    Ok(())
}

async fn sync_issue(
    state: &Arc<DispatcherState>,
    transition: &JobTransition,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let job_key = &transition.job_key;

    // Read the current job state.
    let job = match state.jobs.get(job_key) {
        Some(j) => j.clone(),
        None => {
            // Fall back to KV if not in memory (e.g. during startup).
            match kv_get::<Job>(&state.kv.jobs, job_key).await {
                Ok(Some((j, _))) => j,
                _ => {
                    debug!(job_key, "issue sync: job not found, skipping");
                    return Ok(());
                }
            }
        }
    };

    let (git_url, git_token) = match (&state.config.git_url, &state.config.git_token) {
        (Some(u), Some(t)) => (u.as_str(), t.as_str()),
        _ => return Ok(()),
    };

    let provider = crate::provider::create_provider(git_url, git_token);
    let parts: Vec<&str> = job.repo.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Ok(());
    }
    let (owner, repo) = (parts[0], parts[1]);

    // Check if we already have an issue mapped for this job.
    let existing: Option<IssueMapping> = match kv_get(&state.kv.issue_map, job_key).await {
        Ok(Some((m, _))) => Some(m),
        _ => None,
    };

    let title = format!("[{}] {}", job.key, job.title);
    let body = render_issue_body(&job);
    let is_terminal = job.state.is_terminal();

    match existing {
        Some(mapping) => {
            // Update existing issue.
            let update = UpdateIssue {
                title: Some(title),
                body: Some(body),
                state: if is_terminal {
                    Some("closed".to_string())
                } else {
                    None
                },
            };
            provider
                .update_issue(owner, repo, mapping.issue_number, update)
                .await?;
            debug!(job_key, issue = mapping.issue_number, "issue updated");
        }
        None => {
            // Create new issue.
            let issue = provider
                .create_issue(
                    owner,
                    repo,
                    CreateIssue {
                        title,
                        body: Some(body),
                    },
                )
                .await?;

            let mapping = IssueMapping {
                issue_number: issue.number,
                issue_url: issue.html_url.clone(),
            };
            let data = serde_json::to_vec(&mapping)?;
            state
                .kv
                .issue_map
                .put(job_key, data.into())
                .await
                .map_err(|e| format!("KV put issue_map: {e}"))?;

            debug!(
                job_key,
                issue = issue.number,
                url = issue.html_url,
                "issue created"
            );
        }
    }

    Ok(())
}

fn render_issue_body(job: &Job) -> String {
    let state_str = serde_json::to_value(&job.state)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", job.state));

    let review_str = serde_json::to_value(&job.review)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", job.review));

    let pr_line = match &job.pr_url {
        Some(url) => format!("| PR | {url} |\n"),
        None => String::new(),
    };

    let ci_line = match &job.ci_status {
        Some(s) => {
            let ci_str = serde_json::to_value(s)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("{s:?}"));
            format!("| CI | {ci_str} |\n")
        }
        None => String::new(),
    };

    format!(
        "| Field | Value |\n\
         |-------|-------|\n\
         | Key | `{}` |\n\
         | Repository | `{}` |\n\
         | State | **{}** |\n\
         | Priority | {} |\n\
         | Review | {} |\n\
         {pr_line}\
         {ci_line}\
         | Retries | {}/{} |\n\
         | Created | {} |\n\
         | Updated | {} |\n\
         \n\
         ## Description\n\
         \n\
         {}\n\
         \n\
         ---\n\
         *This issue is managed by chuggernaut. Do not edit.*\n",
        job.key,
        job.repo,
        state_str,
        job.priority,
        review_str,
        job.retry_count,
        job.max_retries,
        job.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
        job.updated_at.format("%Y-%m-%d %H:%M:%S UTC"),
        job.body,
    )
}

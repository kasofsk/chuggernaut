use std::sync::Arc;

use tracing::{info, warn};

use chuggernaut_forgejo_api::{DispatchWorkflowOption, ForgejoClient};
use chuggernaut_types::*;

use crate::config::Config;
use crate::error::{DispatcherError, DispatcherResult};
use crate::state::DispatcherState;

/// Resolve the runner label for a job based on its capabilities.
/// Checks the job's capabilities against the configured label map;
/// falls back to the given default label if no capability matches.
fn resolve_runner_label(config: &Config, job: &Job, default_label: &str) -> String {
    for cap in &job.capabilities {
        if let Some(label) = config.runner_label_map.get(cap) {
            return label.clone();
        }
    }
    default_label.to_string()
}

/// Extract (owner, repo) from a job's repo field and get Forgejo credentials.
fn forgejo_context(
    state: &DispatcherState,
    job: &Job,
) -> DispatcherResult<(String, String, String, String)> {
    let (forgejo_url, forgejo_token) =
        match (&state.config.forgejo_url, &state.config.forgejo_token) {
            (Some(url), Some(token)) => (url.clone(), token.clone()),
            _ => {
                return Err(DispatcherError::Validation(
                "CHUGGERNAUT_FORGEJO_URL and CHUGGERNAUT_FORGEJO_TOKEN required for action dispatch"
                    .to_string(),
            ));
            }
        };

    let parts: Vec<&str> = job.repo.splitn(2, '/').collect();
    if parts.len() != 2 {
        return Err(DispatcherError::Validation(format!(
            "invalid repo format: {}",
            job.repo
        )));
    }

    Ok((
        parts[0].to_string(),
        parts[1].to_string(),
        forgejo_url,
        forgejo_token,
    ))
}

/// Dispatch a Forgejo Action for a job.
/// Creates a claim, transitions to OnTheStack, and dispatches the workflow.
/// The worker binary running inside the action will send heartbeats and report outcomes.
pub async fn dispatch_action(
    state: &Arc<DispatcherState>,
    job_key: &str,
    review_feedback: Option<&str>,
    is_rework: bool,
) -> DispatcherResult<()> {
    let job = state
        .jobs
        .get(job_key)
        .map(|j| j.clone())
        .ok_or_else(|| DispatcherError::JobNotFound(job_key.to_string()))?;

    let (owner, repo, forgejo_url, forgejo_token) = forgejo_context(state, &job)?;

    // Create a claim so the monitor can track this job
    let worker_id = format!("action-{job_key}");
    crate::claims::acquire_claim(state, job_key, &worker_id, job.timeout_secs).await?;

    // Transition to OnTheStack
    let trigger = if is_rework {
        "rework_dispatched"
    } else {
        "action_dispatched"
    };
    crate::jobs::transition_job(
        state,
        job_key,
        JobState::OnTheStack,
        trigger,
        Some(&worker_id),
    )
    .await?;

    // Dispatch the Forgejo Action
    let forgejo = ForgejoClient::new(&forgejo_url, &forgejo_token);
    let workflow = &state.config.action_workflow;

    let mut inputs = serde_json::json!({
        "job_key": job_key,
        "nats_url": state.config.nats_worker_url,
        "is_rework": is_rework.to_string(),
        "runner_label": resolve_runner_label(&state.config, &job, &state.config.action_runner_label),
    });

    if let Some(feedback) = review_feedback {
        inputs["review_feedback"] = serde_json::Value::String(feedback.to_string());
    }

    if let Err(e) = forgejo
        .dispatch_workflow(
            &owner,
            &repo,
            workflow,
            &DispatchWorkflowOption {
                ref_field: "main".to_string(),
                inputs: Some(inputs),
            },
        )
        .await
    {
        // Dispatch failed — release claim and fail the job
        warn!(job_key, error = %e, "action dispatch failed");
        crate::claims::release_claim(state, job_key).await?;
        crate::jobs::transition_job(
            state,
            job_key,
            JobState::Failed,
            "action_dispatch_failed",
            None,
        )
        .await?;
        return Err(DispatcherError::Validation(format!(
            "failed to dispatch action: {e}"
        )));
    }

    info!(
        job_key,
        workflow,
        repo = format!("{owner}/{repo}"),
        is_rework,
        "action dispatched"
    );
    crate::jobs::journal_append(
        state,
        "action_dispatched",
        Some(job_key),
        Some(&worker_id),
        Some(&format!("workflow: {workflow}, is_rework: {is_rework}")),
    )
    .await;

    Ok(())
}

/// Dispatch a Forgejo Action to review a PR for a job.
/// The review action runs Claude in review mode, posts a PR review,
/// and attempts merge if approved. It publishes a ReviewDecision when done.
pub async fn dispatch_review_action(
    state: &Arc<DispatcherState>,
    job_key: &str,
    pr_url: &str,
    review_level: &str,
) -> DispatcherResult<()> {
    let job = state
        .jobs
        .get(job_key)
        .map(|j| j.clone())
        .ok_or_else(|| DispatcherError::JobNotFound(job_key.to_string()))?;

    let (owner, repo, forgejo_url, forgejo_token) = forgejo_context(state, &job)?;

    // Transition InReview → Reviewing before dispatching.
    // This makes the state machine explicit: Reviewing = review action in-flight.
    let worker_id = format!("action-{job_key}");
    crate::claims::acquire_claim(state, job_key, &worker_id, job.timeout_secs).await?;
    crate::jobs::transition_job(state, job_key, JobState::Reviewing, "review_dispatched", Some(&worker_id)).await?;

    let forgejo = ForgejoClient::new(&forgejo_url, &forgejo_token);
    let workflow = &state.config.review_workflow;

    let inputs = serde_json::json!({
        "job_key": job_key,
        "nats_url": state.config.nats_worker_url,
        "pr_url": pr_url,
        "review_level": review_level,
        "runner_label": resolve_runner_label(&state.config, &job, &state.config.review_runner_label),
    });

    if let Err(e) = forgejo
        .dispatch_workflow(
            &owner,
            &repo,
            workflow,
            &DispatchWorkflowOption {
                ref_field: "main".to_string(),
                inputs: Some(inputs),
            },
        )
        .await
    {
        warn!(job_key, error = %e, "review action dispatch failed");
        // Revert to InReview so it can be retried
        let _ = crate::jobs::transition_job(state, job_key, JobState::InReview, "review_dispatch_failed", None).await;
        return Err(DispatcherError::Validation(format!(
            "failed to dispatch review action: {e}"
        )));
    }

    info!(
        job_key,
        workflow,
        repo = format!("{owner}/{repo}"),
        "review action dispatched"
    );
    crate::jobs::journal_append(
        state,
        "review_dispatched",
        Some(job_key),
        None,
        Some(&format!("workflow: {workflow}, pr_url: {pr_url}")),
    )
    .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_config(label_map: HashMap<String, String>) -> Config {
        Config {
            nats_url: String::new(),
            nats_worker_url: String::new(),
            http_listen: String::new(),
            lease_secs: 60,
            default_timeout_secs: 3600,
            cas_max_retries: 3,
            monitor_scan_interval_secs: 10,
            job_retention_secs: 86400,
            activity_limit: 50,
            forgejo_url: None,
            forgejo_token: None,
            action_workflow: "work.yml".to_string(),
            action_runner_label: "ubuntu-latest".to_string(),
            max_concurrent_actions: 2,
            review_workflow: "review.yml".to_string(),
            review_runner_label: "ubuntu-latest".to_string(),
            max_continuations: 3,
            ci_poll_timeout_secs: 120,
            rework_limit: 3,
            human_login: "you".to_string(),
            allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
            pause_on_overage: true,
            runner_label_map: label_map,
        }
    }

    fn test_job(capabilities: Vec<String>) -> Job {
        Job {
            key: "test.repo.1".to_string(),
            repo: "test/repo".to_string(),
            title: "test".to_string(),
            body: String::new(),
            state: JobState::OnDeck,
            priority: 50,
            capabilities,
            platform: None,
            timeout_secs: 3600,
            review: ReviewLevel::High,
            max_retries: 3,
            retry_count: 0,
            retry_after: None,
            pr_url: None,
            token_usage: vec![],
            claude_args: None,
            continuation_count: 0,
            ci_status: None,
            ci_check_since: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn resolve_label_uses_default_when_no_capabilities() {
        let config = test_config(HashMap::new());
        let job = test_job(vec![]);
        assert_eq!(
            resolve_runner_label(&config, &job, &config.action_runner_label),
            "ubuntu-latest"
        );
    }

    #[test]
    fn resolve_label_uses_default_when_no_match() {
        let config = test_config(HashMap::from([("rust".to_string(), "rust".to_string())]));
        let job = test_job(vec!["flutter".to_string()]);
        assert_eq!(
            resolve_runner_label(&config, &job, &config.action_runner_label),
            "ubuntu-latest"
        );
    }

    #[test]
    fn resolve_label_matches_capability() {
        let config = test_config(HashMap::from([
            ("flutter".to_string(), "flutter".to_string()),
            ("rust".to_string(), "rust".to_string()),
        ]));
        let job = test_job(vec!["flutter".to_string()]);
        assert_eq!(
            resolve_runner_label(&config, &job, &config.action_runner_label),
            "flutter"
        );
    }

    #[test]
    fn resolve_label_first_matching_capability_wins() {
        let config = test_config(HashMap::from([
            ("flutter".to_string(), "flutter".to_string()),
            ("python".to_string(), "python".to_string()),
        ]));
        let job = test_job(vec!["flutter".to_string(), "python".to_string()]);
        assert_eq!(
            resolve_runner_label(&config, &job, &config.action_runner_label),
            "flutter"
        );
    }
}

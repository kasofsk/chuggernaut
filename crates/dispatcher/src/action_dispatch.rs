use std::sync::Arc;

use chrono::Utc;
use tracing::{info, warn};

use chuggernaut_git_provider::{DispatchWorkflow, GitProvider};
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

/// Extract (owner, repo) from a job's repo field and create a GitProvider.
fn git_provider_context(
    state: &DispatcherState,
    job: &Job,
) -> DispatcherResult<(String, String, Box<dyn GitProvider>)> {
    let (git_url, git_token) = match (&state.config.git_url, &state.config.git_token) {
        (Some(url), Some(token)) => (url.clone(), token.clone()),
        _ => {
            return Err(DispatcherError::Validation(
                "CHUGGERNAUT_GIT_URL and CHUGGERNAUT_GIT_TOKEN required for action dispatch"
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

    let provider = crate::provider::create_provider(&git_url, &git_token);

    Ok((parts[0].to_string(), parts[1].to_string(), provider))
}

/// How long a queued action can sit in "waiting" before we consider runners
/// unavailable and stop dispatching more work.
const STALE_QUEUE_THRESHOLD_SECS: i64 = 60;

/// Count how many waiting runs are older than the staleness threshold.
/// Returns 0 if all waiting runs are fresh (recently dispatched).
fn count_stale_waiting(runs: &chuggernaut_git_provider::ActionRunList) -> usize {
    let now = Utc::now();
    runs.workflow_runs
        .iter()
        .filter(|r| {
            r.created
                .as_ref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| (now - dt.with_timezone(&Utc)).num_seconds() > STALE_QUEUE_THRESHOLD_SECS)
                .unwrap_or(false)
        })
        .count()
}

/// Check that there are no stale actions stuck waiting for a runner.
/// Only blocks dispatch if waiting actions are older than the threshold,
/// which means runners haven't picked them up in a reasonable time.
async fn check_queue_clear(
    provider: &dyn GitProvider,
    owner: &str,
    repo: &str,
) -> DispatcherResult<()> {
    let runs = match provider
        .list_action_runs(owner, repo, Some("waiting"))
        .await
    {
        Ok(runs) => runs,
        Err(e) => {
            warn!(error = %e, "failed to check action queue — proceeding with dispatch");
            return Ok(());
        }
    };

    let stale_count = count_stale_waiting(&runs);
    if stale_count > 0 {
        Err(DispatcherError::Validation(format!(
            "{stale_count} action(s) waiting for a runner for >{STALE_QUEUE_THRESHOLD_SECS}s — not dispatching more"
        )))
    } else {
        Ok(())
    }
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

    let (owner, repo, provider) = git_provider_context(state, &job)?;

    // Bail out if actions are already waiting for a runner — avoids pile-up.
    check_queue_clear(provider.as_ref(), &owner, &repo).await?;

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

    // Dispatch the CI action
    let workflow = &state.config.action_workflow;

    let mut inputs = serde_json::json!({
        "job_key": job_key,
        "nats_url": state.config.nats_worker_url,
        "is_rework": is_rework.to_string(),
        "runner_label": resolve_runner_label(&state.config, &job, &state.config.action_runner_label),
        "dispatched_at": Utc::now().to_rfc3339(),
    });

    if let Some(feedback) = review_feedback {
        inputs["review_feedback"] = serde_json::Value::String(feedback.to_string());
    }

    if is_rework && let Some(ref url) = job.pr_url {
        inputs["pr_url"] = serde_json::Value::String(url.clone());
    }

    if let Err(e) = provider
        .dispatch_workflow(
            &owner,
            &repo,
            workflow,
            DispatchWorkflow {
                git_ref: "main".to_string(),
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

    let (owner, repo, provider) = git_provider_context(state, &job)?;

    // Bail out if actions are already waiting for a runner — avoids pile-up.
    check_queue_clear(provider.as_ref(), &owner, &repo).await?;

    // Transition InReview → Reviewing before dispatching.
    // This makes the state machine explicit: Reviewing = review action in-flight.
    let worker_id = format!("action-{job_key}");
    crate::claims::acquire_claim(state, job_key, &worker_id, job.timeout_secs).await?;
    crate::jobs::transition_job(
        state,
        job_key,
        JobState::Reviewing,
        "review_dispatched",
        Some(&worker_id),
    )
    .await?;

    let workflow = &state.config.review_workflow;

    let inputs = serde_json::json!({
        "job_key": job_key,
        "nats_url": state.config.nats_worker_url,
        "pr_url": pr_url,
        "review_level": review_level,
        "runner_label": resolve_runner_label(&state.config, &job, &state.config.review_runner_label),
        "dispatched_at": Utc::now().to_rfc3339(),
    });

    if let Err(e) = provider
        .dispatch_workflow(
            &owner,
            &repo,
            workflow,
            DispatchWorkflow {
                git_ref: "main".to_string(),
                inputs: Some(inputs),
            },
        )
        .await
    {
        warn!(job_key, error = %e, "review action dispatch failed");
        // Revert to InReview so it can be retried
        let _ = crate::jobs::transition_job(
            state,
            job_key,
            JobState::InReview,
            "review_dispatch_failed",
            None,
        )
        .await;
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
    use chuggernaut_git_provider::{ActionRun, ActionRunList};
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
            git_url: None,
            git_token: None,
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
            archive_threshold: 200,
        }
    }

    fn test_job(capabilities: Vec<String>) -> Job {
        Job::builder("test.repo.1", "test/repo", "test")
            .capabilities(capabilities)
            .build()
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

    // -- count_stale_waiting tests ------------------------------------------

    fn make_run(id: u64, created: Option<&str>) -> ActionRun {
        ActionRun {
            id,
            status: "waiting".to_string(),
            conclusion: None,
            html_url: None,
            created: created.map(|s| s.to_string()),
        }
    }

    fn make_run_list(runs: Vec<ActionRun>) -> ActionRunList {
        let total_count = runs.len() as u64;
        ActionRunList {
            workflow_runs: runs,
            total_count,
        }
    }

    #[test]
    fn stale_count_empty_queue() {
        let runs = make_run_list(vec![]);
        assert_eq!(count_stale_waiting(&runs), 0);
    }

    #[test]
    fn stale_count_fresh_action_not_stale() {
        let now = Utc::now().to_rfc3339();
        let runs = make_run_list(vec![make_run(1, Some(&now))]);
        assert_eq!(count_stale_waiting(&runs), 0);
    }

    #[test]
    fn stale_count_old_action_is_stale() {
        let old = (Utc::now() - chrono::Duration::seconds(300)).to_rfc3339();
        let runs = make_run_list(vec![make_run(1, Some(&old))]);
        assert_eq!(count_stale_waiting(&runs), 1);
    }

    #[test]
    fn stale_count_mixed_fresh_and_stale() {
        let now = Utc::now().to_rfc3339();
        let old = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
        let runs = make_run_list(vec![make_run(1, Some(&now)), make_run(2, Some(&old))]);
        assert_eq!(count_stale_waiting(&runs), 1);
    }

    #[test]
    fn stale_count_no_timestamp_not_stale() {
        let runs = make_run_list(vec![make_run(1, None)]);
        assert_eq!(count_stale_waiting(&runs), 0);
    }

    #[test]
    fn stale_count_at_threshold_not_stale() {
        // Exactly at the threshold boundary — should NOT be stale (> not >=)
        let at_boundary =
            (Utc::now() - chrono::Duration::seconds(STALE_QUEUE_THRESHOLD_SECS)).to_rfc3339();
        let runs = make_run_list(vec![make_run(1, Some(&at_boundary))]);
        assert_eq!(count_stale_waiting(&runs), 0);
    }

    #[test]
    fn stale_count_just_past_threshold_is_stale() {
        let past =
            (Utc::now() - chrono::Duration::seconds(STALE_QUEUE_THRESHOLD_SECS + 1)).to_rfc3339();
        let runs = make_run_list(vec![make_run(1, Some(&past))]);
        assert_eq!(count_stale_waiting(&runs), 1);
    }
}

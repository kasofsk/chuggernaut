use std::sync::Arc;

use tracing::{info, warn};

use chuggernaut_forgejo_api::{DispatchWorkflowOption, ForgejoClient};
use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::state::DispatcherState;

/// Extract (owner, repo) from a job's repo field and get Forgejo credentials.
fn forgejo_context(state: &DispatcherState, job: &Job) -> DispatcherResult<(String, String, String, String)> {
    let (forgejo_url, forgejo_token) = match (
        &state.config.forgejo_url,
        &state.config.forgejo_token,
    ) {
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

    Ok((parts[0].to_string(), parts[1].to_string(), forgejo_url, forgejo_token))
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
    let trigger = if is_rework { "rework_dispatched" } else { "action_dispatched" };
    crate::jobs::transition_job(state, job_key, JobState::OnTheStack, trigger, Some(&worker_id))
        .await?;

    // Dispatch the Forgejo Action
    let forgejo = ForgejoClient::new(&forgejo_url, &forgejo_token);
    let workflow = &state.config.action_workflow;

    let mut inputs = serde_json::json!({
        "job_key": job_key,
        "nats_url": state.config.nats_worker_url,
        "is_rework": is_rework.to_string(),
        "runner_label": state.config.action_runner_label,
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
        crate::jobs::transition_job(state, job_key, JobState::Failed, "action_dispatch_failed", None)
            .await?;
        return Err(DispatcherError::Validation(format!(
            "failed to dispatch action: {e}"
        )));
    }

    info!(job_key, workflow, repo = format!("{owner}/{repo}"), is_rework, "action dispatched");
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

    let forgejo = ForgejoClient::new(&forgejo_url, &forgejo_token);
    let workflow = &state.config.review_workflow;

    let inputs = serde_json::json!({
        "job_key": job_key,
        "nats_url": state.config.nats_worker_url,
        "pr_url": pr_url,
        "review_level": review_level,
        "runner_label": state.config.review_runner_label,
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
        return Err(DispatcherError::Validation(format!(
            "failed to dispatch review action: {e}"
        )));
    }

    info!(job_key, workflow, repo = format!("{owner}/{repo}"), "review action dispatched");
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

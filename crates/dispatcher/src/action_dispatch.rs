use std::sync::Arc;

use tracing::{info, warn};

use chuggernaut_forgejo_api::{DispatchWorkflowOption, ForgejoClient};
use chuggernaut_types::*;

use crate::error::{DispatcherError, DispatcherResult};
use crate::state::DispatcherState;

/// Dispatch a Forgejo Action for an action-type job.
/// Creates a claim, transitions to OnTheStack, and dispatches the workflow.
/// The worker binary running inside the action will send heartbeats and report outcomes.
pub async fn dispatch_action(
    state: &Arc<DispatcherState>,
    job_key: &str,
) -> DispatcherResult<()> {
    let job = state
        .jobs
        .get(job_key)
        .map(|j| j.clone())
        .ok_or_else(|| DispatcherError::JobNotFound(job_key.to_string()))?;

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

    let (owner, repo) = {
        let parts: Vec<&str> = job.repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(DispatcherError::Validation(format!(
                "invalid repo format: {}",
                job.repo
            )));
        }
        (parts[0].to_string(), parts[1].to_string())
    };

    // Create a claim so the monitor can track this job
    let worker_id = format!("action-{job_key}");
    crate::claims::acquire_claim(state, job_key, &worker_id, job.timeout_secs).await?;

    // Transition to OnTheStack
    crate::jobs::transition_job(state, job_key, JobState::OnTheStack, "action_dispatched", Some(&worker_id))
        .await?;

    // Update job's last_worker_id
    if let Some(mut job) = state.jobs.get_mut(job_key) {
        job.last_worker_id = Some(worker_id.clone());
    }

    // Dispatch the Forgejo Action
    let forgejo = ForgejoClient::new(&forgejo_url, &forgejo_token);
    let workflow = &state.config.action_workflow;

    let inputs = serde_json::json!({
        "job_key": job_key,
        "nats_url": state.config.nats_url,
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
        // Dispatch failed — release claim and fail the job
        warn!(job_key, error = %e, "action dispatch failed");
        crate::claims::release_claim(state, job_key).await?;
        crate::jobs::transition_job(state, job_key, JobState::Failed, "action_dispatch_failed", None)
            .await?;
        return Err(DispatcherError::Validation(format!(
            "failed to dispatch action: {e}"
        )));
    }

    info!(job_key, workflow, repo = format!("{owner}/{repo}"), "action dispatched");
    crate::jobs::journal_append(
        state,
        "action_dispatched",
        Some(job_key),
        Some(&worker_id),
        Some(&format!("workflow: {workflow}")),
    )
    .await;

    Ok(())
}

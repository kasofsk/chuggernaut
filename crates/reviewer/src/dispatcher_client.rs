use std::sync::Arc;

use chuggernaut_types::{Job, JobDetailResponse, JobListResponse};
use tracing::debug;

use crate::error::{ReviewerError, ReviewerResult};
use crate::state::ReviewerState;

/// Fetch a job from the dispatcher HTTP API.
pub async fn get_job(state: &Arc<ReviewerState>, job_key: &str) -> ReviewerResult<Job> {
    let url = format!("{}/jobs/{}", state.config.dispatcher_url, job_key);
    debug!(url, "fetching job from dispatcher");

    let resp = state
        .http
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await?;

    let status = resp.status().as_u16();
    if status == 404 {
        return Err(ReviewerError::JobNotFound(job_key.to_string()));
    }
    if status >= 400 {
        let body = resp.text().await.unwrap_or_default();
        return Err(ReviewerError::Dispatcher(format!(
            "GET {url} returned {status}: {body}"
        )));
    }

    let detail: JobDetailResponse = resp.json().await?;
    Ok(detail.job)
}

/// Fetch all jobs in a given state from the dispatcher HTTP API.
pub async fn get_jobs_by_state(
    state: &Arc<ReviewerState>,
    job_state: &str,
) -> ReviewerResult<Vec<Job>> {
    let url = format!(
        "{}/jobs?state={}",
        state.config.dispatcher_url, job_state
    );
    debug!(url, "fetching jobs by state from dispatcher");

    let resp = state
        .http
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await?;

    let status = resp.status().as_u16();
    if status >= 400 {
        let body = resp.text().await.unwrap_or_default();
        return Err(ReviewerError::Dispatcher(format!(
            "GET {url} returned {status}: {body}"
        )));
    }

    let list: JobListResponse = resp.json().await?;
    Ok(list.jobs)
}

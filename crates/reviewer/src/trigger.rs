use std::sync::Arc;

use futures::StreamExt;
use tracing::{debug, error, info, warn};

use chuggernaut_types::*;

use crate::dispatcher_client;
use crate::error::ReviewerResult;
use crate::review;
use crate::state::ReviewerState;

/// Start the main trigger loop: subscribe to CHUGGERNAUT-TRANSITIONS stream,
/// filter for InReview transitions, and spawn review processing.
pub async fn start(state: Arc<ReviewerState>) -> ReviewerResult<()> {
    // Create a durable pull consumer on the TRANSITIONS stream
    let consumer = state
        .js
        .create_consumer_on_stream(
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("chuggernaut-reviewer".to_string()),
                filter_subject: "chuggernaut.transitions.>".to_string(),
                ..Default::default()
            },
            streams::TRANSITIONS,
        )
        .await
        .map_err(|e| crate::error::ReviewerError::Nats(e.to_string()))?;

    info!("reviewer trigger started, consuming from {}", streams::TRANSITIONS);

    let mut messages = consumer.messages().await?;

    while let Some(msg) = messages.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("error receiving message: {e}");
                continue;
            }
        };

        // Parse the transition event
        let transition: JobTransition = match serde_json::from_slice(&msg.payload) {
            Ok(t) => t,
            Err(e) => {
                warn!("invalid transition payload: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        // Only process transitions TO InReview state
        if transition.to_state != JobState::InReview {
            let _ = msg.ack().await;
            continue;
        }

        let job_key = transition.job_key.clone();
        debug!(job_key, "received InReview transition");

        // Dedup check
        if !state.in_flight.insert(job_key.clone()) {
            debug!(job_key, "already in-flight, skipping");
            let _ = msg.ack().await;
            continue;
        }

        // Ack before processing (we track state via dedup set)
        let _ = msg.ack().await;

        // Spawn processing task
        let s = state.clone();
        tokio::spawn(async move {
            if let Err(e) = review::process_job(&s, &job_key).await {
                error!(job_key, error = %e, "review processing failed");
            }
            s.in_flight.remove(&job_key);
        });
    }

    Ok(())
}

/// Startup reconciliation: clean stale locks, resume InReview and Escalated jobs.
pub async fn reconcile(state: &Arc<ReviewerState>) -> ReviewerResult<()> {
    info!("starting reconciliation");

    // Clean stale merge locks from a previous crash.
    // The merge-queue KV has a TTL so truly stale locks expire on their own,
    // but we can proactively release locks held by jobs that are no longer InReview.
    match crate::merge::cleanup_stale_locks(state).await {
        Ok(count) if count > 0 => info!(count, "cleaned stale merge locks"),
        Ok(_) => {}
        Err(e) => warn!(error = %e, "failed to clean stale merge locks"),
    }

    // Resume InReview jobs
    match dispatcher_client::get_jobs_by_state(state, "in-review").await {
        Ok(jobs) => {
            info!(count = jobs.len(), "reconciling in-review jobs");
            for job in jobs {
                if state.in_flight.insert(job.key.clone()) {
                    let s = state.clone();
                    let key = job.key.clone();
                    tokio::spawn(async move {
                        if let Err(e) = review::process_job(&s, &key).await {
                            error!(job_key = key, error = %e, "reconcile review failed");
                        }
                        s.in_flight.remove(&key);
                    });
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to fetch in-review jobs for reconciliation");
        }
    }

    // Resume Escalated jobs — these already have a human reviewer assigned,
    // so we only need to poll for their decision (not re-escalate).
    match dispatcher_client::get_jobs_by_state(state, "escalated").await {
        Ok(jobs) => {
            info!(count = jobs.len(), "reconciling escalated jobs");
            for job in jobs {
                let pr_url = match job.pr_url.as_deref() {
                    Some(url) => url.to_string(),
                    None => {
                        warn!(job_key = job.key, "escalated job has no PR URL, skipping");
                        continue;
                    }
                };
                let (owner, repo, pr_index) = match crate::merge::parse_pr_url(&pr_url) {
                    Some(parsed) => parsed,
                    None => {
                        warn!(job_key = job.key, pr_url, "could not parse PR URL, skipping");
                        continue;
                    }
                };
                if state.in_flight.insert(job.key.clone()) {
                    let s = state.clone();
                    let key = job.key.clone();
                    tokio::spawn(async move {
                        if let Err(e) = review::poll_escalation(&s, &key, &owner, &repo, pr_index).await {
                            error!(job_key = key, error = %e, "escalation polling failed");
                        }
                        s.in_flight.remove(&key);
                    });
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to fetch escalated jobs for reconciliation");
        }
    }

    info!("reconciliation complete");
    Ok(())
}

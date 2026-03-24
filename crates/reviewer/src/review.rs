use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tracing::{info, warn};

use chuggernaut_types::*;

use crate::decision::{self, ReviewOutcome};
use crate::dispatcher_client;
use crate::error::ReviewerResult;
use crate::forgejo_retry;
use crate::merge;
use crate::state::ReviewerState;

/// Process a single job that has entered InReview state.
/// This is the main review pipeline for one job.
pub async fn process_job(state: &Arc<ReviewerState>, job_key: &str) -> ReviewerResult<()> {
    // Delay to let PR creation settle
    tokio::time::sleep(Duration::from_secs(state.config.delay_secs)).await;

    // Fetch job from dispatcher
    let job = dispatcher_client::get_job(state, job_key).await?;
    let pr_url = job.pr_url.as_deref().ok_or_else(|| {
        crate::error::ReviewerError::NoPrUrl(job_key.to_string())
    })?;

    publish_activity(state, job_key, "review_started", "Review process started").await;

    // Check if review level is "human" — skip automated review
    if job.review == ReviewLevel::Human {
        info!(job_key, "review level is Human, escalating directly");
        escalate(state, job_key, pr_url).await?;
        return Ok(());
    }

    // Dispatch Forgejo Actions review workflow and poll for result
    let review_outcome = dispatch_and_poll_review(state, &job, pr_url).await?;

    match review_outcome {
        ReviewOutcome::Approved => {
            handle_approved(state, job_key, pr_url).await?;
        }
        ReviewOutcome::ChangesRequested { feedback } => {
            if let ChangesOutcome::Escalated { owner, repo, pr_index } =
                handle_changes_requested(state, job_key, &feedback).await?
            {
                poll_escalation(state, job_key, &owner, &repo, pr_index).await?;
            }
        }
        ReviewOutcome::Inconclusive => {
            info!(job_key, "review inconclusive, escalating to human");
            escalate(state, job_key, pr_url).await?;
        }
    }

    Ok(())
}

/// Dispatch a Forgejo Actions workflow for review and poll until complete.
/// Returns the review outcome from the PR reviews after the action finishes.
async fn dispatch_and_poll_review(
    state: &Arc<ReviewerState>,
    job: &Job,
    pr_url: &str,
) -> ReviewerResult<ReviewOutcome> {
    let (owner, repo) = merge::parse_repo_from_key(&job.key)
        .ok_or_else(|| crate::error::ReviewerError::JobNotFound(job.key.clone()))?;

    let action_start = Utc::now().to_rfc3339();

    // Dispatch the review workflow
    let inputs = serde_json::json!({
        "pr_url": pr_url,
        "review_level": format!("{:?}", job.review).to_lowercase(),
        "job_key": job.key,
    });

    publish_activity(state, &job.key, "review_action_dispatched", &format!("Dispatching {}", state.config.workflow)).await;

    let workflow = state.config.workflow.clone();
    let opts = chuggernaut_forgejo_api::DispatchWorkflowOption {
        ref_field: "main".to_string(),
        inputs: Some(inputs),
    };
    let forgejo = state.forgejo.clone();
    let o = owner.clone();
    let r = repo.clone();
    forgejo_retry::retry("dispatch_workflow", || {
        let forgejo = forgejo.clone();
        let o = o.clone();
        let r = r.clone();
        let workflow = workflow.clone();
        let opts = opts.clone();
        async move { forgejo.dispatch_workflow(&o, &r, &workflow, &opts).await }
    })
    .await
    .map_err(|e| {
        crate::error::ReviewerError::Forgejo(e)
    })?;

    // Poll for the action to complete
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(state.config.action_timeout_secs);
    let poll_interval = Duration::from_secs(state.config.poll_secs);

    loop {
        if tokio::time::Instant::now() >= deadline {
            warn!(job_key = job.key, "review action timed out, escalating");
            return Ok(ReviewOutcome::Inconclusive);
        }

        tokio::time::sleep(poll_interval).await;

        // Check PR reviews for a decision submitted after the action started
        match state.forgejo.list_reviews(&owner, &repo, extract_pr_index(pr_url).unwrap_or(0)).await {
            Ok(reviews) => {
                if let Some(review) = decision::pick_latest_review(&reviews, &action_start) {
                    return Ok(decision::map_review(review));
                }
            }
            Err(e) => {
                warn!(job_key = job.key, error = %e, "failed to fetch reviews, will retry");
            }
        }
    }
}

/// Handle an approved review: merge via queue, publish decision.
async fn handle_approved(
    state: &Arc<ReviewerState>,
    job_key: &str,
    pr_url: &str,
) -> ReviewerResult<()> {
    let (owner, repo, pr_index) = merge::parse_pr_url(pr_url)
        .ok_or_else(|| crate::error::ReviewerError::NoPrUrl(job_key.to_string()))?;

    // Check if PR was already merged (crash recovery: merged but decision not published)
    if let Ok(pr) = state.forgejo.get_pull_request(&owner, &repo, pr_index).await {
        if pr.merged {
            info!(job_key, "PR already merged, publishing decision without re-merge");
            publish_decision(state, job_key, DecisionType::Approved, Some(pr_url)).await?;
            publish_activity(state, job_key, "review_approved", "PR was already merged").await;
            return Ok(());
        }
    }

    // Try to acquire merge lock
    if merge::try_acquire_lock(state, &owner, &repo, job_key).await? {
        // We hold the lock — merge
        match merge::merge_pr(state, &owner, &repo, pr_index).await {
            Ok(()) => {
                publish_decision(state, job_key, DecisionType::Approved, Some(pr_url)).await?;
                publish_activity(state, job_key, "review_approved", "PR approved and merged").await;
            }
            Err(e) => {
                // Merge conflict — request changes
                let feedback = format!("Merge conflict: {e}");
                publish_decision(
                    state,
                    job_key,
                    DecisionType::ChangesRequested { feedback: feedback.clone() },
                    Some(pr_url),
                )
                .await?;
                publish_activity(state, job_key, "merge_conflict", &feedback).await;
            }
        }
        merge::release_lock(state, &owner, &repo).await?;

        // Process next in queue — the queued job will go through InReview
        // transition again when it's ready, so we just need to re-attempt
        // its merge. We do this inline to avoid recursive spawn issues.
        if let Some(next) = merge::process_queue(state, &owner, &repo).await? {
            info!(job_key = next.job_key, "processing next job from merge queue");
            if let Some((q_owner, q_repo, q_pr_index)) = merge::parse_pr_url(&next.pr_url) {
                if merge::try_acquire_lock(state, &q_owner, &q_repo, &next.job_key).await? {
                    match merge::merge_pr(state, &q_owner, &q_repo, q_pr_index).await {
                        Ok(()) => {
                            publish_decision(state, &next.job_key, DecisionType::Approved, Some(&next.pr_url)).await?;
                            publish_activity(state, &next.job_key, "review_approved", "PR approved and merged (from queue)").await;
                        }
                        Err(e) => {
                            let feedback = format!("Merge conflict: {e}");
                            publish_decision(state, &next.job_key, DecisionType::ChangesRequested { feedback: feedback.clone() }, Some(&next.pr_url)).await?;
                            publish_activity(state, &next.job_key, "merge_conflict", &feedback).await;
                        }
                    }
                    merge::release_lock(state, &q_owner, &q_repo).await?;
                }
            }
        }
    } else {
        // Lock held — enqueue
        merge::enqueue(state, &owner, &repo, job_key, pr_url).await?;
    }

    Ok(())
}

/// Handle changes requested: check rework count, publish decision or escalate.
/// Returns `EscalationNeeded` if rework limit exceeded, so the caller can
/// decide whether to poll (avoids async recursion).
async fn handle_changes_requested(
    state: &Arc<ReviewerState>,
    job_key: &str,
    feedback: &str,
) -> ReviewerResult<ChangesOutcome> {
    let rework_count = increment_rework_count(state, job_key).await?;

    if rework_count > state.config.rework_limit {
        info!(job_key, rework_count, "rework limit exceeded, escalating");
        let job = dispatcher_client::get_job(state, job_key).await?;
        let pr_url = job.pr_url.as_deref().unwrap_or("");
        let (owner, repo, pr_index) = merge::parse_pr_url(pr_url)
            .ok_or_else(|| crate::error::ReviewerError::NoPrUrl(job_key.to_string()))?;
        escalate_and_notify(state, job_key, &owner, &repo, pr_index).await?;
        Ok(ChangesOutcome::Escalated { owner, repo, pr_index })
    } else {
        publish_decision(
            state,
            job_key,
            DecisionType::ChangesRequested {
                feedback: feedback.to_string(),
            },
            None,
        )
        .await?;
        publish_activity(
            state,
            job_key,
            "review_changes_requested",
            &format!("Rework #{rework_count}: {feedback}"),
        )
        .await;
        Ok(ChangesOutcome::Reworking)
    }
}

enum ChangesOutcome {
    /// Rework requested, worker will pick it up.
    Reworking,
    /// Rework limit exceeded, escalated to human. Caller should poll.
    Escalated { owner: String, repo: String, pr_index: u64 },
}

/// Escalate to human reviewer, then poll until they act.
async fn escalate(
    state: &Arc<ReviewerState>,
    job_key: &str,
    pr_url: &str,
) -> ReviewerResult<()> {
    let (owner, repo, pr_index) = merge::parse_pr_url(pr_url)
        .ok_or_else(|| crate::error::ReviewerError::NoPrUrl(job_key.to_string()))?;

    escalate_and_notify(state, job_key, &owner, &repo, pr_index).await?;
    poll_escalation(state, job_key, &owner, &repo, pr_index).await
}

/// Add human reviewer to PR and publish the Escalated decision.
/// Does NOT poll — call `poll_escalation` separately when resuming
/// from reconciliation.
async fn escalate_and_notify(
    state: &Arc<ReviewerState>,
    job_key: &str,
    owner: &str,
    repo: &str,
    pr_index: u64,
) -> ReviewerResult<()> {
    let human = &state.config.human_login;

    // Add human reviewer to PR
    if let Err(e) = state
        .forgejo
        .add_reviewers(
            owner,
            repo,
            pr_index,
            &chuggernaut_forgejo_api::PullReviewRequestOptions {
                reviewers: vec![human.clone()],
            },
        )
        .await
    {
        warn!(job_key, error = %e, "failed to add human reviewer");
    }

    publish_decision(
        state,
        job_key,
        DecisionType::Escalated {
            reviewer_login: human.clone(),
        },
        None,
    )
    .await?;
    publish_activity(
        state,
        job_key,
        "review_escalated",
        &format!("Escalated to human reviewer: {human}"),
    )
    .await;

    Ok(())
}

/// Poll a PR for a human review decision. Loops until the human
/// submits APPROVED or REQUEST_CHANGES, then acts on it.
/// Used both after live escalation and during startup reconciliation.
pub async fn poll_escalation(
    state: &Arc<ReviewerState>,
    job_key: &str,
    owner: &str,
    repo: &str,
    pr_index: u64,
) -> ReviewerResult<()> {
    let poll_interval = Duration::from_secs(state.config.escalation_poll_secs);
    let escalated_at = Utc::now().to_rfc3339();

    info!(job_key, poll_secs = state.config.escalation_poll_secs, "polling for human review decision");

    loop {
        tokio::time::sleep(poll_interval).await;

        // Check if PR was already merged externally
        match state.forgejo.get_pull_request(owner, repo, pr_index).await {
            Ok(pr) if pr.merged => {
                info!(job_key, "PR already merged by human");
                publish_decision(state, job_key, DecisionType::Approved, Some(&pr.html_url)).await?;
                publish_activity(state, job_key, "review_approved", "Human merged PR directly").await;
                return Ok(());
            }
            Ok(pr) if pr.state == "closed" => {
                // PR closed without merge — treat as changes requested.
                // If rework limit hit, we're already escalated so just keep polling.
                info!(job_key, "PR closed without merge by human");
                match handle_changes_requested(state, job_key, "PR closed by human reviewer").await? {
                    ChangesOutcome::Reworking => return Ok(()),
                    ChangesOutcome::Escalated { .. } => continue,
                }
            }
            Err(e) => {
                warn!(job_key, error = %e, "failed to fetch PR status, will retry");
                continue;
            }
            _ => {}
        }

        // Check for new reviews submitted after escalation
        match state.forgejo.list_reviews(owner, repo, pr_index).await {
            Ok(reviews) => {
                if let Some(review) = decision::pick_latest_review(&reviews, &escalated_at) {
                    let outcome = decision::map_review(review);
                    match outcome {
                        ReviewOutcome::Approved => {
                            info!(job_key, "human approved");
                            let pr_url = format!(
                                "{}/{owner}/{repo}/pulls/{pr_index}",
                                state.config.forgejo_url
                            );
                            handle_approved(state, job_key, &pr_url).await?;
                            return Ok(());
                        }
                        ReviewOutcome::ChangesRequested { feedback } => {
                            info!(job_key, "human requested changes");
                            match handle_changes_requested(state, job_key, &feedback).await? {
                                ChangesOutcome::Reworking => return Ok(()),
                                ChangesOutcome::Escalated { .. } => continue,
                            }
                        }
                        ReviewOutcome::Inconclusive => {
                            // COMMENT reviews — keep polling
                            continue;
                        }
                    }
                }
            }
            Err(e) => {
                warn!(job_key, error = %e, "failed to fetch reviews, will retry");
            }
        }
    }
}

/// Increment and return the rework count for a job.
async fn increment_rework_count(
    state: &Arc<ReviewerState>,
    job_key: &str,
) -> ReviewerResult<u32> {
    let (count, revision) = match state.kv.rework_counts.entry(job_key).await? {
        Some(entry) => {
            let c: u32 = serde_json::from_slice(&entry.value).unwrap_or(0);
            (c, Some(entry.revision))
        }
        None => (0, None),
    };

    let new_count = count + 1;
    let payload = serde_json::to_vec(&new_count)?;

    match revision {
        Some(rev) => {
            state
                .kv
                .rework_counts
                .update(job_key, payload.into(), rev)
                .await
                .map_err(|e| crate::error::ReviewerError::Kv(e.to_string()))?;
        }
        None => {
            state
                .kv
                .rework_counts
                .put(job_key, payload.into())
                .await?;
        }
    }

    Ok(new_count)
}

/// Publish a ReviewDecision to the dispatcher via NATS.
async fn publish_decision(
    state: &Arc<ReviewerState>,
    job_key: &str,
    decision: DecisionType,
    pr_url: Option<&str>,
) -> ReviewerResult<()> {
    let msg = ReviewDecision {
        job_key: job_key.to_string(),
        decision,
        pr_url: pr_url.map(|s| s.to_string()),
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &msg)
        .await?;
    info!(job_key, "published review decision");
    Ok(())
}

/// Fire-and-forget activity append.
async fn publish_activity(
    state: &Arc<ReviewerState>,
    job_key: &str,
    kind: &str,
    message: &str,
) {
    let msg = ActivityAppend {
        job_key: job_key.to_string(),
        entry: ActivityEntry {
            timestamp: Utc::now(),
            kind: kind.to_string(),
            message: message.to_string(),
        },
    };
    let _ = state
        .nats
        .publish_msg(&subjects::ACTIVITY_APPEND, &msg)
        .await;
}

/// Extract PR index from a URL.
fn extract_pr_index(pr_url: &str) -> Option<u64> {
    merge::parse_pr_url(pr_url).map(|(_, _, idx)| idx)
}

//! The container-facing subjects (spec §4.2): a running agent's work result,
//! an evaluator's verdict, and the channel posts an agent streams to the
//! operator. Every one of them is job-addressed and idempotent — the container
//! retries under a bounded budget, so a duplicate submission must be safe.
//!
//! Channel posts route through the core actor rather than writing `channels`
//! KV directly: the dispatcher is the single writer, and the round trip is what
//! turns each post into durable event history.
//!
//! - **Accepts:** `req.work.submit.{owner}.{project}.{seq}`,
//!   `req.eval.submit.{owner}.{project}.{seq}.{task_id}`,
//!   `req.channel.{update,reply}.{owner}.{project}.{seq}`.
//! - **Emits:** `CoreHandle::{submit_result, submit_eval, channel_post}` calls
//!   and `{"ok":true}` / `{"error": …}` replies.
//! - **Guarantees:** a malformed subject is rejected, never guessed at — routing
//!   a submission into another project's job record is worse than failing it.
//!   An oversized agent cover is rejected at ingest, before the record is
//!   stored.
//! - **Spec:** §4.2, §6.1.

use crate::core::{ChannelPost, CoreHandle, EvalSubmission, WorkSubmission};
use store::NatsStore;

/// `req.{family}.{verb}.{owner}.{project}.{seq}` — the job-addressing tail every
/// container-facing subject shares. `None` is a malformed subject: a request the
/// dispatcher must reject rather than guess an owner for, since guessing would
/// route a submission into another project's job record.
fn subject_job_parts(subject: &str) -> Option<(String, String, u64)> {
    let parts: Vec<&str> = subject.split('.').collect();
    let seq = parts.get(5)?.parse::<u64>().ok()?;
    Some((parts.get(3)?.to_string(), parts.get(4)?.to_string(), seq))
}

/// Subscribe the container-facing subjects. Returns after subscriptions are
/// established; handler tasks run for the life of the NATS connection.
pub async fn spawn_container_handlers(store: &NatsStore, handle: CoreHandle) -> store::Result<()> {
    spawn_work_submit(store, handle.clone()).await?;
    spawn_eval_submit(store, handle.clone()).await?;
    spawn_channel_posts(store, handle).await
}

/// `req.work.submit.{owner}.{project}.{seq}` — the work agent's result.
async fn spawn_work_submit(store: &NatsStore, handle: CoreHandle) -> store::Result<()> {
    let mut work_sub = store.subscribe_requests("req.work.submit.>").await?;
    tokio::spawn(async move {
        while let Some(req) = work_sub.next().await {
            let Some((owner, project, seq)) = subject_job_parts(&req.subject) else {
                req.respond(br#"{"error":"malformed subject"}"#.to_vec())
                    .await;
                continue;
            };
            let submission: WorkSubmission =
                serde_json::from_slice(&req.payload).unwrap_or_default();
            // Reject an oversized cover at ingest, before the record is stored
            // (job #143) — the text summary still lands only via a resubmission.
            let body = if let Some(err) = agent_cover_rejection(&submission.cover_html) {
                format!(r#"{{"error":{}}}"#, serde_json::json!(err))
            } else {
                match handle
                    .submit_result(&owner, &project, seq, submission)
                    .await
                {
                    Ok(()) => r#"{"ok":true}"#.to_string(),
                    Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                }
            };
            req.respond(body.into_bytes()).await;
        }
    });
    Ok(())
}

/// `req.eval.submit.{owner}.{project}.{seq}.{task_id}` — an evaluator's verdict.
async fn spawn_eval_submit(store: &NatsStore, handle: CoreHandle) -> store::Result<()> {
    let mut eval_sub = store.subscribe_requests("req.eval.submit.>").await?;
    tokio::spawn(async move {
        while let Some(req) = eval_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project), Some(seq), Some(task_id)) = (
                parts.get(3).copied(),
                parts.get(4).copied(),
                parts.get(5).and_then(|s| s.parse::<u64>().ok()),
                parts.get(6).and_then(|s| s.parse::<u64>().ok()),
            ) else {
                req.respond(br#"{"error":"malformed subject"}"#.to_vec())
                    .await;
                continue;
            };
            // §4.2: payload must include pass — malformed submissions are
            // rejected, not defaulted.
            let body = match serde_json::from_slice::<EvalSubmission>(&req.payload) {
                Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                Ok(submission) => match agent_cover_rejection(&submission.cover_html) {
                    Some(err) => format!(r#"{{"error":{}}}"#, serde_json::json!(err)),
                    None => match handle
                        .submit_eval(owner, project, seq, task_id, submission)
                        .await
                    {
                        Ok(()) => r#"{"ok":true}"#.to_string(),
                        Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                    },
                },
            };
            req.respond(body.into_bytes()).await;
        }
    });
    Ok(())
}

/// `req.channel.{update,reply}.{owner}.{project}.{seq}` — the container used to
/// write `channels` KV directly; routing through the core restores the
/// single-writer rule and turns each post into durable event history.
async fn spawn_channel_posts(store: &NatsStore, handle: CoreHandle) -> store::Result<()> {
    for kind in ["update", "reply"] {
        let mut sub = store
            .subscribe_requests(&format!("req.channel.{kind}.>"))
            .await?;
        let handle = handle.clone();
        tokio::spawn(async move {
            while let Some(req) = sub.next().await {
                let Some((owner, project, seq)) = subject_job_parts(&req.subject) else {
                    req.respond(br#"{"error":"malformed subject"}"#.to_vec())
                        .await;
                    continue;
                };
                let post = match kind {
                    "update" => serde_json::from_slice(&req.payload).map(ChannelPost::Update),
                    _ => serde_json::from_slice(&req.payload).map(ChannelPost::Reply),
                };
                let body = match post {
                    Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                    Ok(post) => match handle.channel_post(&owner, &project, seq, post).await {
                        Ok(()) => r#"{"ok":true}"#.to_string(),
                        Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                    },
                };
                req.respond(body.into_bytes()).await;
            }
        });
    }
    Ok(())
}

/// Byte cap on an agent-authored `cover_html` attached to a work/eval
/// submission (job #143). Tighter than a job brief's cover: an agent cover is a
/// compact "what I did" visual, not a whole authored page, and it rides on
/// every task record. Over → the submission is rejected (never truncated) with
/// an actionable error the agent can react to.
const AGENT_COVER_HTML_MAX_BYTES: usize = 64 * 1024;

/// `Some(error)` when a submitted `cover_html` is over
/// [`AGENT_COVER_HTML_MAX_BYTES`]; `None` when absent or within bound. The text
/// summary is canonical, so an oversized cover is rejected rather than dropped
/// silently — the agent learns to omit or shrink it.
fn agent_cover_rejection(cover_html: &Option<String>) -> Option<String> {
    cover_html
        .as_ref()
        .filter(|h| h.len() > AGENT_COVER_HTML_MAX_BYTES)
        .map(|_| {
            format!(
                "cover_html exceeds the {AGENT_COVER_HTML_MAX_BYTES}-byte limit \
                 (cover pages are presentational — omit it or shrink it; the text \
                 summary is what matters)"
            )
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::{AGENT_COVER_HTML_MAX_BYTES, agent_cover_rejection};
    use crate::handlers::jobs::COVER_HTML_MAX_BYTES;

    /// Agent-authored cover cap (job #143): absent is always fine (full back-
    /// compat), at the cap is fine, one byte over is rejected with an actionable
    /// error the agent can react to — never truncated.
    #[test]
    fn agent_cover_over_limit_rejected_with_actionable_error() {
        assert!(agent_cover_rejection(&None).is_none());
        assert!(agent_cover_rejection(&Some("x".repeat(AGENT_COVER_HTML_MAX_BYTES))).is_none());
        let err = agent_cover_rejection(&Some("x".repeat(AGENT_COVER_HTML_MAX_BYTES + 1)))
            .expect("over-limit cover rejected");
        // The message names the field and the fact it's optional/presentational.
        assert!(err.contains("cover_html"), "{err}");
        assert!(err.contains("summary"), "{err}");
        // The agent cap is tighter than a job brief's cover cap.
        const { assert!(AGENT_COVER_HTML_MAX_BYTES < COVER_HTML_MAX_BYTES) };
    }
}

//! Agent → operator channel posts (spec §4.2).
//!
//! Two things changed here versus the original design, for one reason each:
//!
//! - **The dispatcher writes `channels` KV, not the container.** The channel
//!   binary used to write the bucket itself, making it a second writer to
//!   platform state and leaving the dispatcher unable to see, event, or audit
//!   anything an agent reported.
//! - **Every post is also an event.** `ChannelEntry` holds one `update` and one
//!   `last_reply`, overwritten in place, in a bucket with a 7-day TTL — so the
//!   KV entry is a *status cache*, not a record. Publishing each post to
//!   `job-events` gives the durable history (90d) and reaches the UI over the
//!   existing SSE stream for free.
//!
//! The KV entry is still maintained, because §6.2's `GET .../status` is defined
//! in terms of it.
//!
//! - **Accepts:** `ChannelUpdate` / `AgentReply` posts from the channel MCP
//!   server (via `handlers`).
//! - **Emits:** the `channels` KV entry (a status cache, 7-day TTL) and a
//!   `job-events` publish per post.
//! - **Guarantees:** the dispatcher is the sole writer of `channels` KV; every
//!   post is durably evented (90d) and reaches SSE.
//! - **Spec:** §4.2; §6.2 `GET .../status`.

use crate::core::{ChannelPost, Core, Result};
use store::{buckets, keys};
use types::ChannelEntry;

impl Core {
    pub(crate) async fn on_channel_post(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        post: ChannelPost,
    ) -> Result<()> {
        let job = self.must_get(owner, project, seq)?;
        if job.state.is_terminal() {
            return Ok(());
        }

        let key = keys::channel_key(owner, project, seq);
        let bucket = self.store.raw_bucket(buckets::CHANNELS).await?;
        let mut entry: ChannelEntry = bucket.get_json(&key).await?.unwrap_or(ChannelEntry {
            update: None,
            last_reply: None,
        });

        match &post {
            ChannelPost::Update(update) => {
                let mut update = update.clone();
                update.at = Some(chrono::Utc::now());
                entry.update = Some(update);
            }
            ChannelPost::Reply(reply) => entry.last_reply = Some(reply.clone()),
        }
        let (event_type, payload) = channel_event(&post);
        bucket.put_json(&key, &entry).await?;

        self.publish(owner, project, seq, event_type, payload).await
    }
}

/// The `job-events` frame for a channel post (spec §6.3). Carries the post's
/// content plus its originating task's identity (`task_id` / `phase` /
/// `evaluator`) when the channel binary stamped one — omitted for legacy posts
/// so old events render exactly as before.
fn channel_event(post: &ChannelPost) -> (&'static str, serde_json::Value) {
    let (event_type, mut payload, origin) = match post {
        ChannelPost::Update(update) => (
            "channel-update",
            serde_json::json!({
                "message": update.message,
                "percent": update.percent,
            }),
            &update.origin,
        ),
        ChannelPost::Reply(reply) => (
            "channel-reply",
            serde_json::json!({
                "text": reply.text,
                "sent_at": reply.sent_at,
            }),
            &reply.origin,
        ),
    };
    if let Some(obj) = payload.as_object_mut() {
        if let Some(task_id) = origin.task_id {
            obj.insert("task_id".into(), serde_json::json!(task_id));
        }
        if let Some(phase) = &origin.phase {
            obj.insert("phase".into(), serde_json::json!(phase));
        }
        if let Some(evaluator) = &origin.evaluator {
            obj.insert("evaluator".into(), serde_json::json!(evaluator));
        }
    }
    (event_type, payload)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::channel_event;
    use crate::core::ChannelPost;
    use types::{AgentReply, ChannelOrigin, ChannelUpdate};

    /// A work agent's post carries its task_id and phase onto the event (§6.3).
    #[test]
    fn work_update_carries_task_id_and_phase() {
        let (event_type, payload) = channel_event(&ChannelPost::Update(ChannelUpdate {
            message: "running tests".into(),
            percent: Some(60),
            at: None,
            origin: ChannelOrigin {
                task_id: Some(1),
                phase: Some("Work".into()),
                evaluator: None,
            },
        }));
        assert_eq!(event_type, "channel-update");
        assert_eq!(payload["message"], "running tests");
        assert_eq!(payload["task_id"], 1);
        assert_eq!(payload["phase"], "Work");
        assert!(payload.get("evaluator").is_none());
    }

    /// An evaluator's post carries the evaluator name (§6.3) — the case the UI
    /// could previously only guess by timestamp when tasks overlapped.
    #[test]
    fn evaluator_post_carries_evaluator_name() {
        let (_t, payload) = channel_event(&ChannelPost::Reply(AgentReply {
            text: "looks good".into(),
            sent_at: chrono::Utc::now(),
            origin: ChannelOrigin {
                task_id: Some(4),
                phase: Some("Evaluation".into()),
                evaluator: Some("review".into()),
            },
        }));
        assert_eq!(payload["task_id"], 4);
        assert_eq!(payload["phase"], "Evaluation");
        assert_eq!(payload["evaluator"], "review");
    }

    /// A legacy post (no origin) still produces a well-formed event — none of
    /// the origin keys appear, so old consumers render it as today.
    #[test]
    fn legacy_post_without_origin_still_events() {
        let (event_type, payload) = channel_event(&ChannelPost::Update(ChannelUpdate {
            message: "cloning".into(),
            percent: Some(10),
            at: None,
            origin: ChannelOrigin::default(),
        }));
        assert_eq!(event_type, "channel-update");
        assert_eq!(payload["message"], "cloning");
        assert!(payload.get("task_id").is_none());
        assert!(payload.get("phase").is_none());
        assert!(payload.get("evaluator").is_none());
    }
}

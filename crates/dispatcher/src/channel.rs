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
        // Reject posts for jobs that do not exist: the subject is per-job and
        // the container's credentials are scoped to it, but a stale container
        // from a revoked job could still be talking.
        let job = self.must_get(owner, project, seq)?;
        if job.state.is_terminal() {
            return Ok(()); // late post from a container we already finished with
        }

        let key = keys::channel_key(owner, project, seq);
        let bucket = self.store.raw_bucket(buckets::CHANNELS).await?;
        let mut entry: ChannelEntry = bucket
            .get_json(&key)
            .await?
            .unwrap_or(ChannelEntry { update: None, last_reply: None });

        let (event_type, payload) = match &post {
            ChannelPost::Update(update) => {
                entry.update = Some(update.clone());
                (
                    "channel-update",
                    serde_json::json!({
                        "message": update.message,
                        "percent": update.percent,
                    }),
                )
            }
            ChannelPost::Reply(reply) => {
                entry.last_reply = Some(reply.clone());
                (
                    "channel-reply",
                    serde_json::json!({
                        "text": reply.text,
                        "sent_at": reply.sent_at,
                    }),
                )
            }
        };
        bucket.put_json(&key, &entry).await?;

        // The event is the history; the KV write above is only the latest-value
        // cache that `GET .../status` reads.
        self.publish(owner, project, seq, event_type, payload).await
    }
}

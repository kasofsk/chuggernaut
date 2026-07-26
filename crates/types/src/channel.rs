//! Operator↔agent channel message types (spec §4.2, §1.4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// KV entry at `channels.{owner}.{project}.jobs.{seq}`. `update_status` overwrites
/// `update`; `reply` overwrites `last_reply`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelEntry {
    pub update: Option<ChannelUpdate>,
    pub last_reply: Option<AgentReply>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChannelUpdate {
    pub message: String,
    pub percent: Option<u8>,
    /// When the dispatcher accepted the post. Stamped on write, not by the
    /// container — a container's clock is not ours to trust, and this is what
    /// the operator UI ages the message against ("2m ago") when it reads the
    /// latest post off the jobs list instead of the event stream. None on posts
    /// written before this field existed; the bucket's 7-day TTL ages those out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    /// Which task produced this post (spec §4.2, §6.3). Stamped by the channel
    /// binary from its container env; absent on legacy posts.
    #[serde(flatten)]
    pub origin: ChannelOrigin,
}

/// The originating task's identity, carried end to end on every channel post so
/// the UI attributes a post to a task directly rather than by timestamp
/// correlation (spec §6.3 events). Every field is optional for back-compat:
/// legacy events carry none and render as before.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChannelOrigin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The evaluator's name when the post came from an evaluator task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator: Option<String>,
}

/// Appended to the `channel-inbox` stream — never overwritten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorMessage {
    pub text: String,
    pub sent_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentReply {
    pub text: String,
    pub sent_at: DateTime<Utc>,
    /// Which task produced this reply (spec §4.2, §6.3); absent on legacy posts.
    #[serde(flatten)]
    pub origin: ChannelOrigin,
}

/// Response body for `GET .../jobs/{seq}/status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub job_seq: u64,
    pub update: Option<ChannelUpdate>,
    pub last_reply: Option<AgentReply>,
}

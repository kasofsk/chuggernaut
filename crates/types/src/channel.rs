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
pub struct ChannelUpdate {
    pub message: String,
    pub percent: Option<u8>,
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
}

/// Response body for `GET .../jobs/{seq}/status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub job_seq: u64,
    pub update: Option<ChannelUpdate>,
    pub last_reply: Option<AgentReply>,
}

//! Fixed KV buckets and streams (spec §1.4–1.5).
//!
//! Each constant is one NATS KV bucket created at platform init (§12.1); the
//! dotted remainder of the documented key pattern is the key within that bucket.
//! No buckets are created dynamically.

pub const JOBS: &str = "jobs";
pub const RDEPS: &str = "rdeps";
pub const COUNTERS: &str = "counters";
pub const TASKS: &str = "tasks";
pub const STEPS: &str = "steps";
pub const CHANNELS: &str = "channels";
pub const VARS: &str = "vars";
pub const SECRETS: &str = "secrets";
pub const USERS: &str = "users";
pub const KNOWLEDGE: &str = "knowledge";
pub const PLATFORM: &str = "platform";
pub const PUSH: &str = "push";
pub const INGEST_TOKENS: &str = "ingest-tokens";

pub const ALL_BUCKETS: &[&str] = &[
    JOBS,
    RDEPS,
    COUNTERS,
    TASKS,
    STEPS,
    CHANNELS,
    VARS,
    SECRETS,
    USERS,
    KNOWLEDGE,
    PLATFORM,
    PUSH,
    INGEST_TOKENS,
];

pub const STREAM_JOB_EVENTS: &str = "job-events";
pub const STREAM_CHANNEL_INBOX: &str = "channel-inbox";
pub const STREAM_INGEST: &str = "ingest";

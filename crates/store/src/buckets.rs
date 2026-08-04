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

/// Cloud identity records (spec §8.3, design #313 A5): plaintext cloud
/// coordinates a `workload_identities:` name resolves to. Deliberately its own
/// bucket — a cloud identity is never a secret and never rides the
/// `global/agents` grant.
pub const CLOUD_IDENTITIES: &str = "cloud-identities";
pub const USERS: &str = "users";
pub const KNOWLEDGE: &str = "knowledge";
pub const PLATFORM: &str = "platform";
pub const PROJECTS: &str = "projects";
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
    CLOUD_IDENTITIES,
    USERS,
    KNOWLEDGE,
    PLATFORM,
    PROJECTS,
    PUSH,
    INGEST_TOKENS,
];

pub const STREAM_JOB_EVENTS: &str = "job-events";
pub const STREAM_CHANNEL_INBOX: &str = "channel-inbox";
pub const STREAM_INGEST: &str = "ingest";

/// JetStream **Object** Store for per-task blobs (session transcripts,
/// container logs). Not a KV bucket: object store chunks internally, so blobs
/// are not bound by the 1MB `max_payload` a req/reply route would hit.
pub const OBJECT_ARTIFACTS: &str = "artifacts";

/// JetStream **Object** Store for harvested work-container output archives
/// (design #362 R1). Separate from [`OBJECT_ARTIFACTS`] so it carries its own,
/// shorter retention and its own byte ceiling: transcripts are the audit record
/// of what an agent did and must not be displaceable by a build byproduct.
pub const OBJECT_OUTPUTS: &str = "outputs";

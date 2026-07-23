//! Shared domain types for chuggernaut v2 (spec Part 1).
//!
//! Pure data — no I/O, no async. Everything downstream depends on this crate.

pub mod channel;
pub mod duration;
pub mod ingest;
pub mod job;
pub mod job_type;
pub mod knowledge;
pub mod platform;
pub mod project;
pub mod queue;
pub mod resources;
pub mod step;
pub mod task;
pub mod user;
pub mod version;
pub mod worker;

pub use channel::{
    AgentReply, ChannelEntry, ChannelOrigin, ChannelStatus, ChannelUpdate, OperatorMessage,
};
pub use duration::{DurationParseError, parse_duration};
pub use ingest::IngestEvent;
pub use job::{Escalation, Job, JobState};
pub use job_type::{
    ConfigWarning, Evaluator, EvaluatorType, JobType, Placement, ProjectDefaults, ReviewSpec,
    WorkSpec, WorkType, WrapUpMode, WrapUpSpec,
};
pub use knowledge::{KnowledgeObject, KnowledgeScope};
pub use platform::{DispatcherConfigSnapshot, FleetNode, FleetStatus, SlotOccupant, WorkerNode};
pub use project::{OriginLink, ProjectRecord, ReleaseState, ReleaseStatus, github_repo_from_url};
pub use queue::{QueueEntry, QueueSnapshot};
pub use resources::{MEMORY_PATTERN, MemoryParseError, parse_memory};
pub use step::{StepKind, StepRecord, StepStatus};
pub use task::{
    EscalationAction, EvalResult, PendingReason, Performer, ReworkReason, Task, TaskKind,
    TaskPhase, TaskResolution, TaskResult, TaskState, TokenUsage,
};
pub use user::{Identity, IdentityKind, ProjectRole, User};
pub use version::{CHANNEL_PROTOCOL_VERSION, CONFIG_SCHEMA_EPOCH, WORKER_RPC_VERSION};

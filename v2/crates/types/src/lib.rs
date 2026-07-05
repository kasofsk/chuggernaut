//! Shared domain types for chuggernaut v2 (spec Part 1).
//!
//! Pure data — no I/O, no async. Everything downstream depends on this crate.

pub mod channel;
pub mod ingest;
pub mod job;
pub mod job_type;
pub mod knowledge;
pub mod task;
pub mod user;

pub use channel::{AgentReply, ChannelEntry, ChannelStatus, ChannelUpdate, OperatorMessage};
pub use ingest::IngestEvent;
pub use job::{Job, JobState};
pub use job_type::{Evaluator, EvaluatorType, JobType, WorkSpec, WorkType};
pub use knowledge::{KnowledgeObject, KnowledgeScope};
pub use task::{
    EscalationAction, EvalResult, Task, TaskKind, TaskPhase, TaskResolution, TaskResult, TaskState,
    TokenUsage,
};
pub use user::{Identity, IdentityKind, ProjectRole, User};

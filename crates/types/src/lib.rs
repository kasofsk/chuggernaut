//! Shared domain types for chuggernaut v2 (spec Part 1).
//!
//! Pure data — no I/O, no async. Everything downstream depends on this crate.

pub mod channel;
pub mod cloud_identity;
pub mod config_paths;
pub mod cron;
pub mod deploy;
pub mod duration;
pub mod groups;
pub mod ingest;
pub mod inputs;
pub mod job;
pub mod job_type;
pub mod knowledge;
pub mod platform;
pub mod project;
pub mod queue;
pub mod resources;
pub mod rollup;
pub mod schedule;
pub mod step;
pub mod task;
pub mod user;
pub mod version;
pub mod worker;

pub use channel::{
    AgentReply, ChannelEntry, ChannelOrigin, ChannelStatus, ChannelUpdate, OperatorMessage,
};
pub use cloud_identity::CloudIdentity;
pub use config_paths::{CONFIG_DIR, config_entry_name, config_path, config_path_candidates};
pub use cron::{CRON_FIELD_COUNT, CronExpr, CronParseError};
pub use deploy::{DeployLeg, DeployReport, LegStatus};
pub use duration::{DurationParseError, parse_duration};
pub use groups::{
    DESIGN_DOC_DIR, DESIGN_GROUP_PREFIX, GROUP_NAME_LEN_MAX, GROUP_NAME_PATTERN, GROUPS_COUNT_MAX,
    GroupsError, check_groups, design_doc_path,
};
pub use ingest::IngestEvent;
pub use inputs::{
    INPUT_ENV_PREFIX, INPUT_NAME_PATTERN, INPUT_VALUE_LEN_MAX, INPUT_VALUE_PATTERN,
    INPUTS_COUNT_MAX, InputValueError, SuppliedInputError, check_supplied, input_env_key,
};
pub use job::{
    BatchComposition, CreateSpec, Escalation, JOB_SUMMARY_EXTRA_FIELDS, Job, JobState, JobSummary,
};
pub use job_type::{
    AgentTool, ConfigWarning, Evaluator, EvaluatorType, Input, InputKind, JobType, Placement,
    ProjectDefaults, ReviewSpec, Runtime, RuntimeMode, SECRET_FILE_ENV_SUFFIX, WorkSpec, WorkType,
    WrapUpMode, WrapUpSpec, secret_file_env_name,
};
pub use knowledge::{KnowledgeObject, KnowledgeScope};
pub use platform::{
    CapacityState, DispatcherConfigSnapshot, FleetCapacity, FleetNode, FleetStatus,
    NodeCapacityAck, NodeCapacityDisplay, NodeCapacityIntent, SlotOccupant, WorkerNode,
};
pub use project::{OriginLink, ProjectRecord, ReleaseState, ReleaseStatus, github_repo_from_url};
pub use queue::{QueueEntry, QueueSnapshot};
pub use resources::{MEMORY_PATTERN, MemoryParseError, parse_memory};
pub use rollup::{
    DESIGNS_MAX, DOC_HEAD_LINES_MAX, DOC_STATUS_LEN_MAX, DesignDocHead, DesignEntry, GroupEntry,
    GroupJob, GroupRollup, design_doc_head, design_group_name, design_seq, design_slug,
    group_rollups,
};
pub use schedule::{SCHEDULES_DIR, SCHEDULES_MAX, Schedule};
pub use step::{StepKind, StepRecord, StepStatus};
pub use task::{
    EscalationAction, EvalResult, PendingReason, Performer, ReworkReason, Task, TaskKind,
    TaskPhase, TaskResolution, TaskResult, TaskState, TokenUsage, WorkloadIdentityGrant,
    task_time_ms,
};
pub use user::{Identity, IdentityKind, ProjectRole, User};
pub use version::{
    CHANNEL_PROTOCOL_VERSION, CONFIG_SCHEMA_EPOCH, ConfigSkew, INPUTS_SCHEMA_EPOCH,
    RUNTIME_SCHEMA_EPOCH, SCHEDULE_INPUTS_SCHEMA_EPOCH, SECRET_FILES_SCHEMA_EPOCH,
    TOOLS_SCHEMA_EPOCH, WORKER_RPC_VERSION, WORKLOAD_IDENTITY_SCHEMA_EPOCH,
    config_requires_dispatcher, declared_min_dispatcher,
};
pub use worker::{
    CapacityObservation, CapacitySource, CapacityTransport, NodeCapabilities, ObservedCapabilities,
    ObservedCapacity, capacity_applies,
};

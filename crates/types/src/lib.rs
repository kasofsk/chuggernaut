use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    OnIce,
    Blocked,
    OnDeck,
    OnTheStack,
    InReview,
    Reviewing,
    Escalated,
    ChangesRequested,
    Done,
    Failed,
    Revoked,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Revoked)
    }

    pub fn is_claimable(self) -> bool {
        matches!(self, Self::OnDeck | Self::ChangesRequested)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewLevel {
    Human,
    Low,
    Medium,
    #[default]
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OutcomeType {
    Yield {
        pr_url: String,
        /// True when the worker wrapped up early (deadline/budget/overage warning).
        #[serde(default)]
        partial: bool,
    },
    Fail {
        reason: String,
        logs: Option<String>,
    },
}

/// Token usage from a single Claude invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cache_write_tokens += rhs.cache_write_tokens;
    }
}

/// A single action's token usage record, stored on the Job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionTokenRecord {
    /// "work", "review", "rework"
    pub action_type: String,
    pub token_usage: TokenUsage,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum DecisionType {
    Approved,
    ChangesRequested { feedback: String },
    Escalated { reviewer_login: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CiStatus {
    Pending,
    Success,
    Failure,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrphanKind {
    ClaimlessOnTheStack,
}

// ---------------------------------------------------------------------------
// Core domain structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    pub key: String,
    pub repo: String,
    pub state: JobState,
    pub title: String,
    pub body: String,
    pub priority: u8,
    #[serde(default)]
    pub capabilities: Vec<String>,

    pub platform: Option<String>,
    pub timeout_secs: u64,
    pub review: ReviewLevel,
    pub max_retries: u32,
    pub retry_count: u32,
    pub retry_after: Option<DateTime<Utc>>,
    pub pr_url: Option<String>,
    #[serde(default)]
    pub token_usage: Vec<ActionTokenRecord>,
    /// Extra CLI args forwarded to Claude (e.g. "--model claude-sonnet-4-5-20250514 --max-turns 10").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_args: Option<String>,
    /// Number of continuation dispatches for partial yields.
    #[serde(default)]
    pub continuation_count: u32,
    /// Number of rework cycles (review → changes_requested → rework).
    #[serde(default)]
    pub rework_count: u32,
    /// CI status for the PR. None = not yet checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_status: Option<CiStatus>,
    /// When CI polling started (set on transition to InReview).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_check_since: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClaimState {
    pub worker_id: String,
    pub claimed_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub lease_deadline: DateTime<Utc>,
    pub timeout_secs: u64,
    pub lease_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DepRecord {
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub depended_on_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActivityEntry {
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActivityLog {
    #[serde(default)]
    pub entries: Vec<ActivityEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JournalEntry {
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub job_key: Option<String>,
    pub worker_id: Option<String>,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MergeQueueEntry {
    pub job_key: String,
    pub pr_url: String,
    pub queued_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// NATS message types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateJobRequest {
    pub repo: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub depends_on: Vec<u64>,
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub capabilities: Vec<String>,

    pub platform: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub review: ReviewLevel,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    pub initial_state: Option<JobState>,
    /// Extra CLI args forwarded to Claude (e.g. "--model claude-sonnet-4-5-20250514 --max-turns 10").
    #[serde(default)]
    pub claude_args: Option<String>,
}

fn default_priority() -> u8 {
    50
}

fn default_timeout() -> u64 {
    3600
}

fn default_max_retries() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateJobResponse {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerOutcome {
    pub worker_id: String,
    pub job_key: String,
    pub outcome: OutcomeType,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReviewDecision {
    pub job_key: String,
    pub decision: DecisionType,
    pub pr_url: Option<String>,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActivityAppend {
    pub job_key: String,
    pub entry: ActivityEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JournalAppend {
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub job_key: Option<String>,
    pub worker_id: Option<String>,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobTransition {
    pub job_key: String,
    pub from_state: JobState,
    pub to_state: JobState,
    pub timestamp: DateTime<Utc>,
    pub trigger: String,
    pub worker_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RequeueRequest {
    pub job_key: String,
    pub target: RequeueTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RequeueTarget {
    OnDeck,
    OnIce,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloseJobRequest {
    pub job_key: String,
    #[serde(default)]
    pub revoke: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerHeartbeat {
    pub worker_id: String,
    pub job_key: String,
    /// Cumulative token usage for this session (from stream-json parsing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
    /// Cumulative cost in USD for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Number of assistant turns completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
    /// Rate limit info from the most recent rate_limit_event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<HeartbeatRateLimit>,
}

/// Rate limit info forwarded from Claude CLI's `rate_limit_event`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HeartbeatRateLimit {
    /// Unix timestamp when the rate limit window resets.
    pub resets_at: i64,
    /// Type of rate limit window (e.g. "five_hour").
    pub rate_limit_type: String,
    /// Whether the session is consuming overage tokens.
    pub is_using_overage: bool,
}

// ---------------------------------------------------------------------------
// Monitor event types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LeaseExpiredEvent {
    pub job_key: String,
    pub worker_id: String,
    pub lease_deadline: DateTime<Utc>,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobTimeoutEvent {
    pub job_key: String,
    pub worker_id: String,
    pub claimed_at: DateTime<Utc>,
    pub timeout_secs: u64,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OrphanDetectedEvent {
    pub job_key: String,
    pub worker_id: Option<String>,
    pub kind: OrphanKind,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RetryEligibleEvent {
    pub job_key: String,
    pub retry_count: u32,
    pub retry_after: DateTime<Utc>,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CiCheckEvent {
    pub job_key: String,
    pub pr_url: String,
    pub ci_status: CiStatus,
    pub detected_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// HTTP API response types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobListResponse {
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobDetailResponse {
    pub job: Job,
    pub claim: Option<ClaimState>,
    pub activities: Vec<ActivityEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JobDepsResponse {
    pub dependencies: Vec<Job>,
    pub all_done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JournalListResponse {
    pub entries: Vec<JournalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ErrorResponse {
    pub error: String,
}

// ---------------------------------------------------------------------------
// Channel types (MCP server ↔ CLI communication)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChannelMessage {
    pub sender: String,
    pub body: String,
    pub timestamp: DateTime<Utc>,
    pub message_id: String,
    pub in_reply_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChannelStatus {
    pub job_key: String,
    pub status: String,
    pub progress: Option<f32>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Typed NATS subjects
// ---------------------------------------------------------------------------

/// A typed NATS subject for fire-and-forget or subscribe patterns.
pub struct Subject<T> {
    pub name: &'static str,
    _t: PhantomData<T>,
}

impl<T> Subject<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _t: PhantomData,
        }
    }
}

/// A typed NATS subject for request-reply patterns.
pub struct RequestSubject<Req, Resp> {
    pub name: &'static str,
    _r: PhantomData<(Req, Resp)>,
}

impl<Req, Resp> RequestSubject<Req, Resp> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _r: PhantomData,
        }
    }
}

/// A typed parametric NATS subject (e.g., "chuggernaut.channel.{job_key}.inbox").
pub struct SubjectFn<T> {
    pub pattern: &'static str,
    _t: PhantomData<T>,
}

impl<T> SubjectFn<T> {
    pub const fn new(pattern: &'static str) -> Self {
        Self {
            pattern,
            _t: PhantomData,
        }
    }

    pub fn format(&self, param: &str) -> String {
        self.pattern.replacen("{}", param, 1)
    }
}

/// Metadata for the subject registry.
#[derive(Debug, Clone, Serialize)]
pub struct SubjectSchema {
    pub subject: &'static str,
    pub payload_type: &'static str,
    pub direction: SubjectDirection,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubjectDirection {
    Publish,
    RequestReply,
    Dynamic,
}

/// Define all NATS subjects with their payload types in one place.
/// Generates typed constants, format functions, and a complete registry.
macro_rules! subject_registry {
    (
        $(
            // Static publish subjects: pub CONST_NAME: Subject<PayloadType> = "subject.name";
            pub $name:ident : Subject< $payload:ty > = $subject:literal ;
        )*
        ---request---
        $(
            // Request-reply subjects: pub CONST_NAME: RequestSubject<Req, Resp> = "subject.name";
            pub $rr_name:ident : RequestSubject< $req:ty , $resp:ty > = $rr_subject:literal ;
        )*
        ---dynamic---
        $(
            // Dynamic subjects: pub FN_NAME: SubjectFn<PayloadType> = "pattern.with.{}";
            pub $fn_name:ident : SubjectFn< $fn_payload:ty > = $pattern:literal ;
        )*
    ) => {
        pub mod subjects {
            use super::*;

            // Static publish subjects
            $(
                pub static $name: Subject<$payload> = Subject::new($subject);
            )*

            // Request-reply subjects
            $(
                pub static $rr_name: RequestSubject<$req, $resp> = RequestSubject::new($rr_subject);
            )*

            // Dynamic subjects
            $(
                pub static $fn_name: SubjectFn<$fn_payload> = SubjectFn::new($pattern);
            )*

            /// Complete registry of all NATS subjects and their payload types.
            pub fn registry() -> Vec<SubjectSchema> {
                vec![
                    $(
                        SubjectSchema {
                            subject: $subject,
                            payload_type: std::any::type_name::<$payload>(),
                            direction: SubjectDirection::Publish,
                        },
                    )*
                    $(
                        SubjectSchema {
                            subject: $rr_subject,
                            payload_type: std::any::type_name::<$req>(),
                            direction: SubjectDirection::RequestReply,
                        },
                    )*
                    $(
                        SubjectSchema {
                            subject: $pattern,
                            payload_type: std::any::type_name::<$fn_payload>(),
                            direction: SubjectDirection::Dynamic,
                        },
                    )*
                ]
            }
        }
    };
}

subject_registry! {
    // Worker events (action workers publish, dispatcher subscribes)
    pub WORKER_HEARTBEAT: Subject<WorkerHeartbeat> = "chuggernaut.worker.heartbeat";
    pub WORKER_OUTCOME: Subject<WorkerOutcome> = "chuggernaut.worker.outcome";

    // Review
    pub REVIEW_DECISION: Subject<ReviewDecision> = "chuggernaut.review.decision";

    // Activity / journal (fire-and-forget)
    pub ACTIVITY_APPEND: Subject<ActivityAppend> = "chuggernaut.activity.append";
    pub JOURNAL_APPEND: Subject<JournalAppend> = "chuggernaut.journal.append";

    // Monitor advisories (dispatcher internal)
    pub MONITOR_LEASE_EXPIRED: Subject<LeaseExpiredEvent> = "chuggernaut.monitor.lease-expired";
    pub MONITOR_TIMEOUT: Subject<JobTimeoutEvent> = "chuggernaut.monitor.timeout";
    pub MONITOR_ORPHAN: Subject<OrphanDetectedEvent> = "chuggernaut.monitor.orphan";
    pub MONITOR_RETRY: Subject<RetryEligibleEvent> = "chuggernaut.monitor.retry";
    pub MONITOR_CI_CHECK: Subject<CiCheckEvent> = "chuggernaut.monitor.ci-check";

    ---request---

    // Admin commands (CLI → dispatcher, request-reply)
    pub ADMIN_CREATE_JOB: RequestSubject<CreateJobRequest, CreateJobResponse> = "chuggernaut.admin.create-job";
    pub ADMIN_REQUEUE: RequestSubject<RequeueRequest, ()> = "chuggernaut.admin.requeue";
    pub ADMIN_CLOSE_JOB: RequestSubject<CloseJobRequest, ()> = "chuggernaut.admin.close-job";

    ---dynamic---

    // Transitions (dispatcher publishes per-job, reviewer consumes via JetStream)
    pub TRANSITIONS: SubjectFn<JobTransition> = "chuggernaut.transitions.{}";

    // Channel (bidirectional CLI ↔ Claude session)
    pub CHANNEL_INBOX: SubjectFn<ChannelMessage> = "chuggernaut.channel.{}.inbox";
    pub CHANNEL_OUTBOX: SubjectFn<ChannelMessage> = "chuggernaut.channel.{}.outbox";
}

// ---------------------------------------------------------------------------
// KV bucket name constants
// ---------------------------------------------------------------------------

pub mod buckets {
    // NATS KV bucket names must be alphanumeric + underscores (no dots or hyphens)
    pub const JOBS: &str = "chuggernaut_jobs";
    pub const CLAIMS: &str = "chuggernaut_claims";
    pub const DEPS: &str = "chuggernaut_deps";
    pub const COUNTERS: &str = "chuggernaut_counters";
    pub const ACTIVITIES: &str = "chuggernaut_activities";
    pub const MERGE_QUEUE: &str = "chuggernaut_merge_queue";
    pub const REWORK_COUNTS: &str = "chuggernaut_rework_counts";
    pub const JOURNAL: &str = "chuggernaut_journal";
    pub const CHANNELS: &str = "chuggernaut_channels";
}

// ---------------------------------------------------------------------------
// Stream name constants
// ---------------------------------------------------------------------------

pub mod streams {
    pub const TRANSITIONS: &str = "CHUGGERNAUT-TRANSITIONS";
    pub const MONITOR: &str = "CHUGGERNAUT-MONITOR";
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a job key into (owner, repo, seq).
/// Key format: `{owner}.{repo}.{seq}`
pub fn parse_job_key(key: &str) -> Option<(&str, &str, u64)> {
    let (rest, seq_str) = key.rsplit_once('.')?;
    let seq: u64 = seq_str.parse().ok()?;
    let dot = rest.rfind('.')?;
    let owner = &rest[..dot];
    let repo = &rest[dot + 1..];
    Some((owner, repo, seq))
}

/// Build a job key from components.
pub fn job_key(owner: &str, repo: &str, seq: u64) -> String {
    format!("{owner}.{repo}.{seq}")
}

/// Parse PR index from a Forgejo/Gitea PR URL.
/// e.g. "https://forgejo.example.com/owner/repo/pulls/42" -> Some(42)
pub fn parse_pr_url_index(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

/// Validate that owner and repo names contain no dots.
pub fn validate_repo_name(owner: &str, repo: &str) -> Result<(), String> {
    if owner.contains('.') {
        return Err(format!("owner name must not contain dots: {owner:?}"));
    }
    if repo.contains('.') {
        return Err(format!("repo name must not contain dots: {repo:?}"));
    }
    if owner.is_empty() || repo.is_empty() {
        return Err("owner and repo must not be empty".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Claude args validation
// ---------------------------------------------------------------------------

/// How to validate a Claude CLI flag's value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ClaudeFlagValueType {
    /// Alphanumeric, hyphens, underscores, dots, colons, slashes (model names, etc.)
    Alphanumeric,
    /// Positive integer
    PositiveInt,
    /// Positive float
    PositiveFloat,
    /// Must be one of the listed values
    OneOf { choices: Vec<String> },
}

/// One allowed Claude CLI flag and its value type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowedClaudeFlag {
    pub flag: String,
    pub value_type: ClaudeFlagValueType,
    /// When set, the value must be one of these (checked after the type validation).
    /// For example, restrict `--model` to `["opus", "sonnet"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<String>>,
}

/// Sane defaults: the flags that are safe for jobs to customize.
/// No `allowed_values` restrictions — any value that passes the type check is accepted.
/// Override via dispatcher config to restrict further.
pub fn default_allowed_claude_flags() -> Vec<AllowedClaudeFlag> {
    vec![
        AllowedClaudeFlag {
            flag: "--model".into(),
            value_type: ClaudeFlagValueType::Alphanumeric,
            allowed_values: None,
        },
        AllowedClaudeFlag {
            flag: "--max-turns".into(),
            value_type: ClaudeFlagValueType::PositiveInt,
            allowed_values: None,
        },
        AllowedClaudeFlag {
            flag: "--max-budget-usd".into(),
            value_type: ClaudeFlagValueType::PositiveFloat,
            allowed_values: None,
        },
        AllowedClaudeFlag {
            flag: "--effort".into(),
            value_type: ClaudeFlagValueType::OneOf {
                choices: vec!["low".into(), "medium".into(), "high".into(), "max".into()],
            },
            allowed_values: None,
        },
        AllowedClaudeFlag {
            flag: "--permission-mode".into(),
            value_type: ClaudeFlagValueType::OneOf {
                choices: vec!["acceptEdits".into(), "default".into(), "plan".into()],
            },
            allowed_values: None,
        },
    ]
}

impl ClaudeFlagValueType {
    fn validate(&self, flag: &str, value: &str) -> Result<(), String> {
        match self {
            ClaudeFlagValueType::Alphanumeric => {
                if value.is_empty() {
                    return Err(format!("{flag} requires a value"));
                }
                if !value
                    .chars()
                    .all(|c| c.is_alphanumeric() || "-_.:/@".contains(c))
                {
                    return Err(format!(
                        "{flag} value {value:?} contains invalid characters \
                         (only alphanumeric, hyphens, underscores, dots, colons, slashes)"
                    ));
                }
                Ok(())
            }
            ClaudeFlagValueType::PositiveInt => {
                let n: u64 = value
                    .parse()
                    .map_err(|_| format!("{flag} value {value:?} is not a valid integer"))?;
                if n == 0 {
                    return Err(format!("{flag} value must be > 0"));
                }
                Ok(())
            }
            ClaudeFlagValueType::PositiveFloat => {
                let n: f64 = value
                    .parse()
                    .map_err(|_| format!("{flag} value {value:?} is not a valid number"))?;
                if n <= 0.0 {
                    return Err(format!("{flag} value must be > 0"));
                }
                Ok(())
            }
            ClaudeFlagValueType::OneOf { choices } => {
                if choices.iter().any(|c| c == value) {
                    Ok(())
                } else {
                    Err(format!(
                        "{flag} value {value:?} is not valid (expected one of: {})",
                        choices.join(", ")
                    ))
                }
            }
        }
    }
}

/// Validate user-supplied Claude CLI args against an allowlist.
///
/// Only flags present in `allowed` are accepted, and each value is
/// type-checked. This catches typos and invalid flags at job creation time
/// rather than at runtime in the action.
pub fn validate_claude_args(args: &str, allowed: &[AllowedClaudeFlag]) -> Result<(), String> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(());
    }

    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];

        // Every token in flag position must be a known flag
        let Some(entry) = allowed.iter().find(|a| a.flag == token) else {
            let known: Vec<&str> = allowed.iter().map(|a| a.flag.as_str()).collect();
            return Err(format!(
                "unknown claude arg {token:?} (allowed flags: {})",
                known.join(", ")
            ));
        };

        // Consume the value token
        i += 1;
        if i >= tokens.len() {
            return Err(format!("{token} requires a value"));
        }
        let value = tokens[i];

        // Must not look like another flag (missing value)
        if value.starts_with('-') {
            return Err(format!("{token} requires a value, got flag {value:?}"));
        }

        entry.value_type.validate(token, value)?;

        // Check allowed_values restriction if configured
        if let Some(ref allowed) = entry.allowed_values
            && !allowed.iter().any(|v| v == value)
        {
            return Err(format!(
                "{token} value {value:?} is not permitted (allowed: {})",
                allowed.join(", ")
            ));
        }

        i += 1;
    }

    Ok(())
}

/// All Claude CLI flags that are safe to appear in a worker's command string.
/// This includes both job-level flags (--model, etc.) and action-level flags
/// (--verbose, --output-format, etc.) that the action YAML hardcodes.
///
/// Used by the worker as a final safety check before spawning Claude.
const WORKER_SAFE_FLAGS: &[&str] = &[
    // Job-level (customizable per job)
    "--model",
    "--max-turns",
    "--max-budget-usd",
    "--effort",
    "--permission-mode",
    // Action-level (hardcoded in work.yml / review.yml)
    "--verbose",
    "--output-format",
    "--include-partial-messages",
    "--allowedTools",
];

/// Validate the full command_args string that will be passed to Claude.
///
/// This is a worker-side safety check: it scans every flag-like token and
/// rejects the string if any flag is not in the known-safe set. This guards
/// against tampered NATS records injecting dangerous flags like
/// `--dangerously-skip-permissions`.
///
/// Values are not type-checked here (the dispatcher already did that).
pub fn validate_worker_command_args(args: &str) -> Result<(), String> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    for token in &tokens {
        if token.starts_with("--") && !WORKER_SAFE_FLAGS.contains(token) {
            return Err(format!(
                "command contains disallowed flag {token:?} (not in worker safe list)"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON Schema generation
// ---------------------------------------------------------------------------

/// Generate a JSON Schema covering all public API types.
/// This schema can be used to build clients in any language.
pub fn generate_schema() -> schemars::Schema {
    let mut generator = schemars::SchemaGenerator::default();

    // Generate schemas for all public types — referencing them ensures they
    // appear in the definitions. We use a wrapper object that includes them all.
    generator.subschema_for::<Job>();
    generator.subschema_for::<ClaimState>();
    generator.subschema_for::<DepRecord>();
    generator.subschema_for::<ActivityEntry>();
    generator.subschema_for::<ActivityLog>();
    generator.subschema_for::<JournalEntry>();
    generator.subschema_for::<MergeQueueEntry>();
    generator.subschema_for::<TokenUsage>();
    generator.subschema_for::<ActionTokenRecord>();
    generator.subschema_for::<JobTransition>();

    // Messages
    generator.subschema_for::<CreateJobRequest>();
    generator.subschema_for::<CreateJobResponse>();
    generator.subschema_for::<WorkerOutcome>();
    generator.subschema_for::<ReviewDecision>();
    generator.subschema_for::<ActivityAppend>();
    generator.subschema_for::<RequeueRequest>();
    generator.subschema_for::<CloseJobRequest>();
    generator.subschema_for::<WorkerHeartbeat>();
    generator.subschema_for::<HeartbeatRateLimit>();
    generator.subschema_for::<CiStatus>();
    generator.subschema_for::<CiCheckEvent>();

    // Channel
    generator.subschema_for::<ChannelMessage>();
    generator.subschema_for::<ChannelStatus>();

    // HTTP API responses
    generator.subschema_for::<JobListResponse>();
    generator.subschema_for::<JobDetailResponse>();
    generator.subschema_for::<JobDepsResponse>();
    generator.subschema_for::<JournalListResponse>();
    generator.subschema_for::<ErrorResponse>();

    generator.into_root_schema_for::<Job>()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_state_serde_kebab_case() {
        assert_eq!(
            serde_json::to_string(&JobState::OnDeck).unwrap(),
            "\"on-deck\""
        );
        assert_eq!(
            serde_json::to_string(&JobState::OnTheStack).unwrap(),
            "\"on-the-stack\""
        );
        assert_eq!(
            serde_json::to_string(&JobState::ChangesRequested).unwrap(),
            "\"changes-requested\""
        );
        assert_eq!(
            serde_json::from_str::<JobState>("\"in-review\"").unwrap(),
            JobState::InReview
        );
    }

    #[test]
    fn review_level_serde() {
        assert_eq!(
            serde_json::to_string(&ReviewLevel::High).unwrap(),
            "\"high\""
        );
        assert_eq!(
            serde_json::from_str::<ReviewLevel>("\"human\"").unwrap(),
            ReviewLevel::Human
        );
    }

    #[test]
    fn outcome_type_serde() {
        let yield_outcome = OutcomeType::Yield {
            pr_url: "http://forgejo/acme/payments/pulls/3".to_string(),
            partial: false,
        };
        let json = serde_json::to_string(&yield_outcome).unwrap();
        assert!(json.contains("\"type\":\"yield\""));
        assert!(json.contains("\"pr_url\""));

        let fail_outcome = OutcomeType::Fail {
            reason: "compile error".to_string(),
            logs: Some("error[E0308]".to_string()),
        };
        let json = serde_json::to_string(&fail_outcome).unwrap();
        assert!(json.contains("\"type\":\"fail\""));
    }

    #[test]
    fn decision_type_serde() {
        let approved = DecisionType::Approved;
        let json = serde_json::to_string(&approved).unwrap();
        assert!(json.contains("\"decision\":\"approved\""));

        let changes = DecisionType::ChangesRequested {
            feedback: "fix tests".to_string(),
        };
        let json = serde_json::to_string(&changes).unwrap();
        assert!(json.contains("\"decision\":\"changes_requested\""));
    }

    #[test]
    fn parse_job_key_valid() {
        let (owner, repo, seq) = parse_job_key("acme.payments.57").unwrap();
        assert_eq!(owner, "acme");
        assert_eq!(repo, "payments");
        assert_eq!(seq, 57);
    }

    #[test]
    fn parse_job_key_invalid() {
        assert!(parse_job_key("nope").is_none());
        assert!(parse_job_key("one.two").is_none());
        assert!(parse_job_key("a.b.notanumber").is_none());
    }

    #[test]
    fn build_job_key() {
        assert_eq!(job_key("acme", "payments", 57), "acme.payments.57");
    }

    #[test]
    fn validate_repo_names() {
        assert!(validate_repo_name("acme", "payments").is_ok());
        assert!(validate_repo_name("a.b", "payments").is_err());
        assert!(validate_repo_name("acme", "pay.ments").is_err());
        assert!(validate_repo_name("", "payments").is_err());
    }

    #[test]
    fn validate_claude_args_allowed() {
        let flags = default_allowed_claude_flags();
        assert!(validate_claude_args("", &flags).is_ok());
        assert!(validate_claude_args("--model claude-sonnet-4-5-20250514", &flags).is_ok());
        assert!(validate_claude_args("--model opus", &flags).is_ok());
        assert!(validate_claude_args("--max-turns 10", &flags).is_ok());
        assert!(validate_claude_args("--max-budget-usd 5.50", &flags).is_ok());
        assert!(validate_claude_args("--effort high", &flags).is_ok());
        assert!(validate_claude_args("--permission-mode plan", &flags).is_ok());
        // Multiple flags combined
        assert!(
            validate_claude_args(
                "--model claude-sonnet-4-5-20250514 --max-turns 5 --effort low",
                &flags,
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_claude_args_unknown_flags() {
        let flags = default_allowed_claude_flags();
        assert!(validate_claude_args("--dangerously-skip-permissions", &flags).is_err());
        assert!(validate_claude_args("--print something", &flags).is_err());
        assert!(validate_claude_args("-p something", &flags).is_err());
        assert!(validate_claude_args("--mcp-config /tmp/evil.json", &flags).is_err());
        assert!(validate_claude_args("--output-format json", &flags).is_err());
        assert!(validate_claude_args("--allowedTools Bash", &flags).is_err());
        assert!(validate_claude_args("--verbose", &flags).is_err());
        assert!(validate_claude_args("--model opus --print sneaky", &flags).is_err());
    }

    #[test]
    fn validate_claude_args_bad_values() {
        let flags = default_allowed_claude_flags();
        // Missing value
        assert!(validate_claude_args("--model", &flags).is_err());
        // Flag as value (missing value)
        assert!(validate_claude_args("--model --max-turns", &flags).is_err());
        // Invalid integer
        assert!(validate_claude_args("--max-turns abc", &flags).is_err());
        assert!(validate_claude_args("--max-turns 0", &flags).is_err());
        assert!(validate_claude_args("--max-turns -5", &flags).is_err());
        // Invalid float
        assert!(validate_claude_args("--max-budget-usd abc", &flags).is_err());
        assert!(validate_claude_args("--max-budget-usd 0", &flags).is_err());
        // Invalid enum
        assert!(validate_claude_args("--effort mega", &flags).is_err());
        assert!(validate_claude_args("--permission-mode bypassPermissions", &flags).is_err());
        // Shell injection in value (rejected by alphanumeric check)
        assert!(validate_claude_args("--model $(whoami)", &flags).is_err());
        assert!(validate_claude_args("--model foo;rm", &flags).is_err());
        // Bare value without a flag
        assert!(validate_claude_args("opus", &flags).is_err());
    }

    #[test]
    fn validate_claude_args_custom_allowlist() {
        // Custom config that only allows --model
        let flags = vec![AllowedClaudeFlag {
            flag: "--model".into(),
            value_type: ClaudeFlagValueType::Alphanumeric,
            allowed_values: None,
        }];
        assert!(validate_claude_args("--model opus", &flags).is_ok());
        // --max-turns not in custom list
        assert!(validate_claude_args("--max-turns 10", &flags).is_err());
    }

    #[test]
    fn validate_claude_args_allowed_values() {
        let flags = vec![AllowedClaudeFlag {
            flag: "--model".into(),
            value_type: ClaudeFlagValueType::Alphanumeric,
            allowed_values: Some(vec!["opus".into(), "sonnet".into()]),
        }];
        assert!(validate_claude_args("--model opus", &flags).is_ok());
        assert!(validate_claude_args("--model sonnet", &flags).is_ok());
        // haiku not in allowed_values
        assert!(validate_claude_args("--model haiku", &flags).is_err());
    }

    #[test]
    fn validate_worker_command_args_safe() {
        // Typical work.yml command_args
        assert!(validate_worker_command_args(
            "--model opus --verbose --output-format stream-json --include-partial-messages --allowedTools Bash Read Write"
        ).is_ok());
        // Typical review.yml command_args
        assert!(validate_worker_command_args("--allowedTools Bash Read Glob Grep").is_ok());
        // Empty is fine
        assert!(validate_worker_command_args("").is_ok());
    }

    #[test]
    fn validate_worker_command_args_rejects_dangerous() {
        assert!(validate_worker_command_args("--dangerously-skip-permissions").is_err());
        assert!(
            validate_worker_command_args(
                "--verbose --output-format stream-json --dangerously-skip-permissions"
            )
            .is_err()
        );
        assert!(validate_worker_command_args("--print foo").is_err());
        assert!(validate_worker_command_args("--mcp-config /tmp/evil.json").is_err());
    }

    #[test]
    fn job_state_helpers() {
        assert!(JobState::Done.is_terminal());
        assert!(JobState::Revoked.is_terminal());
        assert!(!JobState::OnDeck.is_terminal());

        assert!(JobState::OnDeck.is_claimable());
        assert!(JobState::ChangesRequested.is_claimable());
        assert!(!JobState::Blocked.is_claimable());
    }

    #[test]
    fn create_job_request_defaults() {
        let json = r#"{
            "repo": "acme/payments",
            "title": "Test",
            "body": "Test body"
        }"#;
        let req: CreateJobRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.priority, 50);
        assert_eq!(req.timeout_secs, 3600);
        assert_eq!(req.max_retries, 3);
        assert_eq!(req.review, ReviewLevel::High);
        assert!(req.depends_on.is_empty());
        assert!(req.initial_state.is_none());
        assert!(req.claude_args.is_none());
    }

    #[test]
    fn create_job_request_with_claude_args() {
        let json = r#"{
            "repo": "acme/payments",
            "title": "Test",
            "body": "Test body",
            "claude_args": "--model claude-sonnet-4-5-20250514"
        }"#;
        let req: CreateJobRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.claude_args.as_deref(),
            Some("--model claude-sonnet-4-5-20250514")
        );
    }

    #[test]
    fn schema_generation() {
        let schema = generate_schema();
        let json = serde_json::to_string_pretty(&schema).unwrap();
        // Should contain our key types in definitions
        assert!(json.contains("JobState"));
        assert!(json.contains("ClaimState"));
        assert!(json.contains("JobTransition"));
        assert!(json.contains("ReviewDecision"));
        assert!(json.contains("JobListResponse"));
    }

    #[test]
    fn job_roundtrip() {
        let job = Job {
            key: "acme.payments.57".to_string(),
            repo: "acme/payments".to_string(),
            state: JobState::OnDeck,
            title: "Add retry logic".to_string(),
            body: "Retry with backoff".to_string(),
            priority: 80,
            capabilities: vec!["rust".to_string()],
            platform: None,
            timeout_secs: 3600,
            review: ReviewLevel::High,
            max_retries: 3,
            retry_count: 0,
            retry_after: None,
            pr_url: None,
            token_usage: vec![],
            claude_args: Some("--model claude-sonnet-4-5-20250514".to_string()),
            continuation_count: 0,
            rework_count: 0,
            ci_status: None,
            ci_check_since: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&job).unwrap();
        let back: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(back.key, job.key);
        assert_eq!(back.state, JobState::OnDeck);
        assert_eq!(back.priority, 80);
    }

    #[test]
    fn heartbeat_serde_backward_compat() {
        // Old-style heartbeat without new fields should deserialize fine
        let json = r#"{"worker_id":"action-acme.payments.57","job_key":"acme.payments.57"}"#;
        let hb: WorkerHeartbeat = serde_json::from_str(json).unwrap();
        assert_eq!(hb.worker_id, "action-acme.payments.57");
        assert!(hb.token_usage.is_none());
        assert!(hb.cost_usd.is_none());
        assert!(hb.turns.is_none());
        assert!(hb.rate_limit.is_none());
    }

    #[test]
    fn heartbeat_serde_with_token_data() {
        let hb = WorkerHeartbeat {
            worker_id: "action-acme.payments.57".to_string(),
            job_key: "acme.payments.57".to_string(),
            token_usage: Some(TokenUsage {
                input_tokens: 5000,
                output_tokens: 200,
                cache_read_tokens: 1000,
                cache_write_tokens: 500,
            }),
            cost_usd: Some(0.05),
            turns: Some(3),
            rate_limit: Some(HeartbeatRateLimit {
                resets_at: 1774458000,
                rate_limit_type: "five_hour".to_string(),
                is_using_overage: true,
            }),
        };
        let json = serde_json::to_string(&hb).unwrap();
        let back: WorkerHeartbeat = serde_json::from_str(&json).unwrap();
        assert_eq!(back.token_usage.unwrap().output_tokens, 200);
        assert!((back.cost_usd.unwrap() - 0.05).abs() < 0.001);
        assert_eq!(back.turns, Some(3));
        let rl = back.rate_limit.unwrap();
        assert_eq!(rl.resets_at, 1774458000);
        assert!(rl.is_using_overage);
    }

    #[test]
    fn heartbeat_skip_serializing_none_fields() {
        let hb = WorkerHeartbeat {
            worker_id: "w".to_string(),
            job_key: "k".to_string(),
            token_usage: None,
            cost_usd: None,
            turns: None,
            rate_limit: None,
        };
        let json = serde_json::to_string(&hb).unwrap();
        // None fields should not appear in serialized output
        assert!(!json.contains("token_usage"));
        assert!(!json.contains("cost_usd"));
        assert!(!json.contains("turns"));
        assert!(!json.contains("rate_limit"));
    }

    #[test]
    fn yield_partial_backward_compat() {
        // Old-style yield without partial field should deserialize with partial=false
        let json = r#"{"type":"yield","pr_url":"http://forgejo/acme/repo/pulls/1"}"#;
        let outcome: OutcomeType = serde_json::from_str(json).unwrap();
        match outcome {
            OutcomeType::Yield { pr_url, partial } => {
                assert_eq!(pr_url, "http://forgejo/acme/repo/pulls/1");
                assert!(!partial);
            }
            _ => panic!("expected Yield"),
        }
    }

    #[test]
    fn yield_partial_roundtrip() {
        let outcome = OutcomeType::Yield {
            pr_url: "http://forgejo/acme/repo/pulls/1".to_string(),
            partial: true,
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("\"partial\":true"));
        let back: OutcomeType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, outcome);
    }

    #[test]
    fn ci_status_serde() {
        assert_eq!(
            serde_json::to_string(&CiStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&CiStatus::Success).unwrap(),
            "\"success\""
        );
        assert_eq!(
            serde_json::from_str::<CiStatus>("\"failure\"").unwrap(),
            CiStatus::Failure
        );
    }

    #[test]
    fn parse_pr_url_index_valid() {
        assert_eq!(
            parse_pr_url_index("http://forgejo/acme/repo/pulls/42"),
            Some(42)
        );
        assert_eq!(
            parse_pr_url_index("http://forgejo:3000/org/project/pulls/1"),
            Some(1)
        );
        // GitHub uses /pull/ (singular) instead of /pulls/
        assert_eq!(
            parse_pr_url_index("https://github.com/owner/repo/pull/99"),
            Some(99)
        );
    }

    #[test]
    fn parse_pr_url_index_invalid() {
        assert_eq!(parse_pr_url_index("http://forgejo/acme/repo/pulls/"), None);
        assert_eq!(parse_pr_url_index(""), None);
    }

    #[test]
    fn job_new_fields_default() {
        // Job JSON without new fields should deserialize with defaults
        let json = r#"{
            "key": "a.b.1", "repo": "a/b", "state": "on-deck",
            "title": "t", "body": "b", "priority": 50,
            "timeout_secs": 3600, "review": "high",
            "max_retries": 3, "retry_count": 0,
            "token_usage": [],
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z"
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.continuation_count, 0);
        assert!(job.ci_status.is_none());
        assert!(job.ci_check_since.is_none());
    }
}

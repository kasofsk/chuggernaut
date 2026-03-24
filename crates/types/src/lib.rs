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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewLevel {
    Human,
    Low,
    Medium,
    High,
}

impl Default for ReviewLevel {
    fn default() -> Self {
        Self::High
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OutcomeType {
    Yield { pr_url: String },
    Fail { reason: String, logs: Option<String> },
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
        Self { name, _t: PhantomData }
    }
}

/// A typed NATS subject for request-reply patterns.
pub struct RequestSubject<Req, Resp> {
    pub name: &'static str,
    _r: PhantomData<(Req, Resp)>,
}

impl<Req, Resp> RequestSubject<Req, Resp> {
    pub const fn new(name: &'static str) -> Self {
        Self { name, _r: PhantomData }
    }
}

/// A typed parametric NATS subject (e.g., "chuggernaut.channel.{job_key}.inbox").
pub struct SubjectFn<T> {
    pub pattern: &'static str,
    _t: PhantomData<T>,
}

impl<T> SubjectFn<T> {
    pub const fn new(pattern: &'static str) -> Self {
        Self { pattern, _t: PhantomData }
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
    let mut parts = key.rsplitn(2, '.');
    let seq_str = parts.next()?;
    let rest = parts.next()?;
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&job).unwrap();
        let back: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(back.key, job.key);
        assert_eq!(back.state, JobState::OnDeck);
        assert_eq!(back.priority, 80);
    }
}

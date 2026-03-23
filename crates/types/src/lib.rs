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
    NeedsHelp,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerState {
    Idle,
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OutcomeType {
    Yield { pr_url: String },
    Fail { reason: String, logs: Option<String> },
    Abandon {},
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
    ClaimUnknownWorker,
    StaleSession,
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
    pub worker_type: Option<String>,
    pub platform: Option<String>,
    pub timeout_secs: u64,
    pub review: ReviewLevel,
    pub max_retries: u32,
    pub retry_count: u32,
    pub retry_after: Option<DateTime<Utc>>,
    pub pr_url: Option<String>,
    pub last_worker_id: Option<String>,
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
pub struct WorkerInfo {
    pub worker_id: String,
    pub state: WorkerState,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub worker_type: String,
    #[serde(default)]
    pub platform: Vec<String>,
    pub current_job: Option<String>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DepRecord {
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub depended_on_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    pub worker_id: String,
    pub session_url: String,
    pub session_name: String,
    pub mode: String,
    pub started_at: DateTime<Utc>,
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
pub struct PendingRework {
    pub worker_id: String,
    pub review_feedback: String,
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
    pub worker_type: Option<String>,
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
pub struct WorkerRegistration {
    pub worker_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub worker_type: String,
    #[serde(default)]
    pub platform: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Assignment {
    pub job: Job,
    pub claim: ClaimState,
    pub is_rework: bool,
    pub review_feedback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerOutcome {
    pub worker_id: String,
    pub job_key: String,
    pub outcome: OutcomeType,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReviewDecision {
    pub job_key: String,
    pub decision: DecisionType,
    pub pr_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HelpRequest {
    pub worker_id: String,
    pub job_key: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HelpResponse {
    pub job_key: String,
    pub message: String,
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
pub struct PreemptNotice {
    pub reason: String,
    pub new_job_key: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdleEvent {
    pub worker_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UnregisterEvent {
    pub worker_id: String,
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
pub struct WorkerListResponse {
    pub workers: Vec<WorkerInfo>,
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
// NATS subject constants
// ---------------------------------------------------------------------------

pub mod subjects {
    // Worker events (workers publish)
    pub const WORKER_REGISTER: &str = "forge2.worker.register";
    pub const WORKER_IDLE: &str = "forge2.worker.idle";
    pub const WORKER_HEARTBEAT: &str = "forge2.worker.heartbeat";
    pub const WORKER_OUTCOME: &str = "forge2.worker.outcome";
    pub const WORKER_UNREGISTER: &str = "forge2.worker.unregister";

    // Dispatch (dispatcher publishes to specific workers)
    pub fn dispatch_assign(worker_id: &str) -> String {
        format!("forge2.dispatch.assign.{worker_id}")
    }
    pub fn dispatch_preempt(worker_id: &str) -> String {
        format!("forge2.dispatch.preempt.{worker_id}")
    }

    // Transitions (dispatcher publishes per-job)
    pub fn transitions(job_key: &str) -> String {
        // job_key is already dotted: owner.repo.seq
        format!("forge2.transitions.{job_key}")
    }

    // Interaction
    pub const INTERACT_HELP: &str = "forge2.interact.help";
    pub fn interact_respond(job_key: &str) -> String {
        format!("forge2.interact.respond.{job_key}")
    }
    pub fn interact_deliver(worker_id: &str) -> String {
        format!("forge2.interact.deliver.{worker_id}")
    }
    pub fn interact_attach(worker_id: &str) -> String {
        format!("forge2.interact.attach.{worker_id}")
    }
    pub fn interact_detach(worker_id: &str) -> String {
        format!("forge2.interact.detach.{worker_id}")
    }

    // Review
    pub const REVIEW_DECISION: &str = "forge2.review.decision";

    // Activity / journal (fire-and-forget)
    pub const ACTIVITY_APPEND: &str = "forge2.activity.append";
    pub const JOURNAL_APPEND: &str = "forge2.journal.append";

    // Monitor advisories
    pub const MONITOR_LEASE_EXPIRED: &str = "forge2.monitor.lease-expired";
    pub const MONITOR_TIMEOUT: &str = "forge2.monitor.timeout";
    pub const MONITOR_ORPHAN: &str = "forge2.monitor.orphan";
    pub const MONITOR_RETRY: &str = "forge2.monitor.retry";

    // Admin (CLI → dispatcher, request-reply)
    pub const ADMIN_CREATE_JOB: &str = "forge2.admin.create-job";
    pub const ADMIN_REQUEUE: &str = "forge2.admin.requeue";
    pub const ADMIN_CLOSE_JOB: &str = "forge2.admin.close-job";
}

// ---------------------------------------------------------------------------
// KV bucket name constants
// ---------------------------------------------------------------------------

pub mod buckets {
    // NATS KV bucket names must be alphanumeric + underscores (no dots or hyphens)
    pub const JOBS: &str = "forge2_jobs";
    pub const CLAIMS: &str = "forge2_claims";
    pub const DEPS: &str = "forge2_deps";
    pub const WORKERS: &str = "forge2_workers";
    pub const COUNTERS: &str = "forge2_counters";
    pub const SESSIONS: &str = "forge2_sessions";
    pub const ACTIVITIES: &str = "forge2_activities";
    pub const PENDING_REWORKS: &str = "forge2_pending_reworks";
    pub const ABANDON_BLACKLIST: &str = "forge2_abandon_blacklist";
    pub const MERGE_QUEUE: &str = "forge2_merge_queue";
    pub const REWORK_COUNTS: &str = "forge2_rework_counts";
    pub const JOURNAL: &str = "forge2_journal";
}

// ---------------------------------------------------------------------------
// Stream name constants
// ---------------------------------------------------------------------------

pub mod streams {
    pub const TRANSITIONS: &str = "FORGE2-TRANSITIONS";
    pub const WORKER_EVENTS: &str = "FORGE2-WORKER-EVENTS";
    pub const MONITOR: &str = "FORGE2-MONITOR";
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
    generator.subschema_for::<WorkerInfo>();
    generator.subschema_for::<DepRecord>();
    generator.subschema_for::<SessionInfo>();
    generator.subschema_for::<ActivityEntry>();
    generator.subschema_for::<ActivityLog>();
    generator.subschema_for::<JournalEntry>();
    generator.subschema_for::<MergeQueueEntry>();
    generator.subschema_for::<JobTransition>();

    // Messages
    generator.subschema_for::<CreateJobRequest>();
    generator.subschema_for::<CreateJobResponse>();
    generator.subschema_for::<WorkerRegistration>();
    generator.subschema_for::<Assignment>();
    generator.subschema_for::<WorkerOutcome>();
    generator.subschema_for::<ReviewDecision>();
    generator.subschema_for::<HelpRequest>();
    generator.subschema_for::<HelpResponse>();
    generator.subschema_for::<ActivityAppend>();
    generator.subschema_for::<PreemptNotice>();
    generator.subschema_for::<RequeueRequest>();
    generator.subschema_for::<CloseJobRequest>();
    generator.subschema_for::<WorkerHeartbeat>();
    generator.subschema_for::<IdleEvent>();
    generator.subschema_for::<UnregisterEvent>();

    // HTTP API responses
    generator.subschema_for::<JobListResponse>();
    generator.subschema_for::<JobDetailResponse>();
    generator.subschema_for::<JobDepsResponse>();
    generator.subschema_for::<WorkerListResponse>();
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
            serde_json::to_string(&JobState::NeedsHelp).unwrap(),
            "\"needs-help\""
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

        let abandon = OutcomeType::Abandon {};
        let json = serde_json::to_string(&abandon).unwrap();
        assert!(json.contains("\"type\":\"abandon\""));
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
        assert!(json.contains("WorkerInfo"));
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
            worker_type: None,
            platform: None,
            timeout_secs: 3600,
            review: ReviewLevel::High,
            max_retries: 3,
            retry_count: 0,
            retry_after: None,
            pr_url: None,
            last_worker_id: None,
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

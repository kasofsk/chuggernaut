/// Provider-agnostic git hosting types.
///
/// These types represent the common interface across git providers (Forgejo, GitHub, etc.).
/// Provider implementations convert their wire types to/from these types.

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Repository {
    pub name: String,
    pub full_name: String,
    pub default_branch: String,
    pub clone_url: String,
    pub html_url: String,
    pub permissions: RepoPermissions,
}

#[derive(Debug, Clone, Default)]
pub struct RepoPermissions {
    pub admin: bool,
    pub push: bool,
    pub pull: bool,
}

// ---------------------------------------------------------------------------
// Pull Request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub merged: bool,
    pub html_url: String,
    pub head: PullRequestBranch,
    pub base: PullRequestBranch,
}

#[derive(Debug, Clone)]
pub struct PullRequestBranch {
    pub ref_name: String,
    pub sha: String,
}

#[derive(Debug, Clone)]
pub struct CreatePullRequest {
    pub title: String,
    pub body: Option<String>,
    pub head: String,
    pub base: String,
}

#[derive(Debug, Clone)]
pub enum MergeMethod {
    Merge,
    Rebase,
    Squash,
}

#[derive(Debug, Clone)]
pub struct MergePullRequest {
    pub method: MergeMethod,
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PullReview {
    pub id: u64,
    pub state: String,
    pub body: String,
    pub user: Option<ReviewUser>,
}

#[derive(Debug, Clone)]
pub struct ReviewUser {
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Debug, Clone)]
pub struct CreateReview {
    pub body: String,
    pub event: ReviewEvent,
}

// ---------------------------------------------------------------------------
// Commit Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CombinedStatus {
    pub state: String,
    pub statuses: Vec<CommitStatus>,
    pub total_count: u64,
    pub sha: String,
}

#[derive(Debug, Clone)]
pub struct CommitStatus {
    pub status: String,
    pub context: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Actions / CI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DispatchWorkflow {
    pub git_ref: String,
    pub inputs: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct DispatchWorkflowRun {
    pub id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ActionRun {
    pub id: u64,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ActionRunList {
    pub workflow_runs: Vec<ActionRun>,
    pub total_count: u64,
}

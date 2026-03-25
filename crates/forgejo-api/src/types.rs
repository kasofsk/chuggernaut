use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub default_branch: String,
    pub clone_url: String,
    pub html_url: String,
    #[serde(default)]
    pub permissions: RepoPermissions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoPermissions {
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub push: bool,
    #[serde(default)]
    pub pull: bool,
}

// ---------------------------------------------------------------------------
// Pull Request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub merged: bool,
    pub html_url: String,
    pub head: PullRequestBranch,
    pub base: PullRequestBranch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestBranch {
    #[serde(rename = "ref")]
    pub ref_field: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePullRequestOption {
    pub title: String,
    pub body: Option<String>,
    pub head: String,
    pub base: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePullRequestOption {
    #[serde(rename = "Do")]
    pub method: String, // "rebase", "merge", "squash"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_message_field: Option<String>,
}

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullReview {
    pub id: u64,
    pub state: String, // "APPROVED", "REQUEST_CHANGES", "COMMENT", "PENDING"
    pub body: String,
    pub submitted_at: Option<String>,
    pub user: Option<ReviewUser>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewUser {
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePullReviewOptions {
    pub body: String,
    pub event: String, // "APPROVED", "REQUEST_CHANGES", "COMMENT"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullReviewRequestOptions {
    pub reviewers: Vec<String>,
}

// ---------------------------------------------------------------------------
// Commit Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedStatus {
    pub state: String, // "pending", "success", "failure", "error", ""
    pub statuses: Vec<CommitStatus>,
    pub total_count: u64,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitStatus {
    pub id: u64,
    pub status: String,
    pub context: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchWorkflowOption {
    #[serde(rename = "ref")]
    pub ref_field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRun {
    pub id: u64,
    pub status: String, // "waiting", "running", "success", "failure"
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRunList {
    pub workflow_runs: Vec<ActionRun>,
    pub total_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchWorkflowRun {
    pub id: Option<u64>,
    pub jobs: Option<Vec<String>>,
    pub run_number: Option<u64>,
}

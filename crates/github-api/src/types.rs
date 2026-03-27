use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Repository
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Default, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    #[serde(default)]
    pub merged: bool,
    #[serde(default)]
    pub mergeable: Option<bool>,
    pub html_url: String,
    pub head: PullRequestBranch,
    pub base: PullRequestBranch,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestBranch {
    #[serde(rename = "ref")]
    pub ref_field: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatePullRequestOption {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub head: String,
    pub base: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergePullRequestOption {
    pub merge_method: String, // "merge", "rebase", "squash"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
}

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct PullReview {
    pub id: u64,
    pub state: String, // "APPROVED", "CHANGES_REQUESTED", "COMMENTED", "PENDING"
    #[serde(default)]
    pub body: Option<String>,
    pub submitted_at: Option<String>,
    pub user: Option<ReviewUser>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewUser {
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatePullReviewOptions {
    pub body: String,
    pub event: String, // "APPROVE", "REQUEST_CHANGES", "COMMENT"
}

#[derive(Debug, Clone, Serialize)]
pub struct PullReviewRequestOptions {
    pub reviewers: Vec<String>,
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateIssueOption {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateIssueOption {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct IssueComment {
    pub id: u64,
    #[serde(default)]
    pub body: Option<String>,
    pub user: Option<ReviewUser>,
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Commit Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CombinedStatus {
    pub state: String, // "pending", "success", "failure", "error"
    #[serde(default)]
    pub statuses: Vec<CommitStatus>,
    pub total_count: u64,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommitStatus {
    pub id: u64,
    #[serde(alias = "state")]
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

#[derive(Debug, Clone, Serialize)]
pub struct DispatchWorkflowOption {
    #[serde(rename = "ref")]
    pub ref_field: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionRun {
    pub id: u64,
    pub status: String, // "queued", "in_progress", "completed"
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionRunList {
    pub workflow_runs: Vec<ActionRun>,
    pub total_count: u64,
}

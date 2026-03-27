use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize null or missing as Default::default() (e.g. null → vec![]).
fn deserialize_null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::deserialize(deserializer)?.unwrap_or_default())
}

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
    #[serde(default)]
    pub mergeable: Option<bool>,
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
pub struct EditIssueOption {
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
    pub body: String,
    pub user: Option<ReviewUser>,
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Commit Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombinedStatus {
    pub state: String, // "pending", "success", "failure", "error", ""
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_status_null_statuses() {
        // Forgejo returns null instead of [] when no CI checks exist.
        let json = r#"{"state":"","statuses":null,"total_count":0,"sha":"abc123"}"#;
        let status: CombinedStatus = serde_json::from_str(json).unwrap();
        assert!(status.statuses.is_empty());
        assert_eq!(status.total_count, 0);
    }

    #[test]
    fn combined_status_missing_statuses() {
        // Field entirely absent.
        let json = r#"{"state":"","total_count":0,"sha":"abc123"}"#;
        let status: CombinedStatus = serde_json::from_str(json).unwrap();
        assert!(status.statuses.is_empty());
    }

    #[test]
    fn combined_status_with_statuses() {
        let json = r#"{
            "state": "success",
            "statuses": [{"id":1,"status":"success","context":"ci/build"}],
            "total_count": 1,
            "sha": "abc123"
        }"#;
        let status: CombinedStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.statuses.len(), 1);
        assert_eq!(status.statuses[0].status, "success");
    }
}

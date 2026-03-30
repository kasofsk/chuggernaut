mod types;

pub use types::*;

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("{0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait]
pub trait GitProvider: Send + Sync {
    // Repository
    async fn get_repo(&self, owner: &str, repo: &str) -> Result<Repository>;

    // Pull Requests
    async fn create_pull_request(
        &self,
        owner: &str,
        repo: &str,
        opts: CreatePullRequest,
    ) -> Result<PullRequest>;

    async fn list_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        state: Option<&str>,
    ) -> Result<Vec<PullRequest>>;

    async fn get_pull_request(&self, owner: &str, repo: &str, number: u64) -> Result<PullRequest>;

    async fn find_pr_by_head(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
    ) -> Result<Option<PullRequest>>;

    async fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        opts: MergePullRequest,
    ) -> Result<()>;

    // Reviews
    async fn list_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<PullReview>>;

    async fn submit_review(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        opts: CreateReview,
    ) -> Result<PullReview>;

    async fn add_reviewers(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        reviewers: Vec<String>,
    ) -> Result<Vec<PullReview>>;

    // Issues
    async fn create_issue(&self, owner: &str, repo: &str, opts: CreateIssue) -> Result<Issue>;

    async fn update_issue(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        opts: UpdateIssue,
    ) -> Result<Issue>;

    // Comments
    async fn list_comments(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> Result<Vec<Comment>>;

    async fn create_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> Result<Comment>;

    // Commit Status
    async fn get_combined_status(
        &self,
        owner: &str,
        repo: &str,
        ref_name: &str,
    ) -> Result<CombinedStatus>;

    // CI / Actions
    async fn dispatch_workflow(
        &self,
        owner: &str,
        repo: &str,
        workflow_file: &str,
        opts: DispatchWorkflow,
    ) -> Result<DispatchWorkflowRun>;

    async fn get_action_run(&self, owner: &str, repo: &str, run_id: u64) -> Result<ActionRun>;

    async fn list_action_runs(
        &self,
        owner: &str,
        repo: &str,
        status: Option<&str>,
    ) -> Result<ActionRunList>;
}

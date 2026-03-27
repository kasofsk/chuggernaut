mod types;

pub use types::*;

use async_trait::async_trait;
use chuggernaut_git_provider as gp;
use reqwest::Client;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Error)]
pub enum ForgejoError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ForgejoError>;

#[derive(Clone)]
pub struct ForgejoClient {
    client: Client,
    base_url: String,
    token: String,
}

impl ForgejoClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base_url)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        debug!(path, "GET");
        let resp = self
            .client
            .get(self.url(path))
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .send()
            .await?;
        self.handle_response(resp).await
    }

    async fn post<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        debug!(path, "POST");
        let resp = self
            .client
            .post(self.url(path))
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    async fn patch<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        debug!(path, "PATCH");
        let resp = self
            .client
            .patch(self.url(path))
            .header("Authorization", format!("token {}", self.token))
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    async fn handle_response<T: DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ForgejoError::Api { status, body });
        }
        let body = resp.text().await?;
        let parsed = serde_json::from_str(&body)?;
        Ok(parsed)
    }

    // -----------------------------------------------------------------------
    // Repository
    // -----------------------------------------------------------------------

    pub async fn get_repo(&self, owner: &str, repo: &str) -> Result<Repository> {
        self.get(&format!("/repos/{owner}/{repo}")).await
    }

    // -----------------------------------------------------------------------
    // Pull Requests
    // -----------------------------------------------------------------------

    pub async fn create_pull_request(
        &self,
        owner: &str,
        repo: &str,
        opts: &CreatePullRequestOption,
    ) -> Result<PullRequest> {
        self.post(&format!("/repos/{owner}/{repo}/pulls"), opts)
            .await
    }

    pub async fn list_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        state: Option<&str>,
    ) -> Result<Vec<PullRequest>> {
        let mut path = format!("/repos/{owner}/{repo}/pulls");
        if let Some(s) = state {
            path.push_str(&format!("?state={s}"));
        }
        self.get(&path).await
    }

    pub async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        index: u64,
    ) -> Result<PullRequest> {
        self.get(&format!("/repos/{owner}/{repo}/pulls/{index}"))
            .await
    }

    /// Find a PR by head branch name. Returns None if not found.
    pub async fn find_pr_by_head(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
    ) -> Result<Option<PullRequest>> {
        let prs: Vec<PullRequest> = self
            .get(&format!("/repos/{owner}/{repo}/pulls?state=open"))
            .await?;
        Ok(prs.into_iter().find(|pr| pr.head.ref_field == head))
    }

    pub async fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        index: u64,
        opts: &MergePullRequestOption,
    ) -> Result<()> {
        let path = format!("/repos/{owner}/{repo}/pulls/{index}/merge");
        debug!(path, "POST merge");
        let resp = self
            .client
            .post(self.url(&path))
            .header("Authorization", format!("token {}", self.token))
            .json(opts)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ForgejoError::Api { status, body });
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Reviews
    // -----------------------------------------------------------------------

    pub async fn list_reviews(
        &self,
        owner: &str,
        repo: &str,
        index: u64,
    ) -> Result<Vec<PullReview>> {
        self.get(&format!("/repos/{owner}/{repo}/pulls/{index}/reviews"))
            .await
    }

    pub async fn submit_review(
        &self,
        owner: &str,
        repo: &str,
        index: u64,
        opts: &CreatePullReviewOptions,
    ) -> Result<PullReview> {
        self.post(
            &format!("/repos/{owner}/{repo}/pulls/{index}/reviews"),
            opts,
        )
        .await
    }

    pub async fn add_reviewers(
        &self,
        owner: &str,
        repo: &str,
        index: u64,
        opts: &PullReviewRequestOptions,
    ) -> Result<Vec<PullReview>> {
        self.post(
            &format!("/repos/{owner}/{repo}/pulls/{index}/requested_reviewers"),
            opts,
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Commit Status
    // -----------------------------------------------------------------------

    /// Get the combined commit status for a ref (branch, tag, or SHA).
    /// Forgejo API: GET /repos/{owner}/{repo}/commits/{ref}/status
    pub async fn get_combined_status(
        &self,
        owner: &str,
        repo: &str,
        ref_name: &str,
    ) -> Result<CombinedStatus> {
        self.get(&format!("/repos/{owner}/{repo}/commits/{ref_name}/status"))
            .await
    }

    // -----------------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------------

    pub async fn dispatch_workflow(
        &self,
        owner: &str,
        repo: &str,
        workflow_file: &str,
        opts: &DispatchWorkflowOption,
    ) -> Result<DispatchWorkflowRun> {
        let path = format!("/repos/{owner}/{repo}/actions/workflows/{workflow_file}/dispatches");
        debug!(path, "POST dispatch workflow");
        let resp = self
            .client
            .post(self.url(&path))
            .header("Authorization", format!("token {}", self.token))
            .json(opts)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ForgejoError::Api { status, body });
        }
        // Some Forgejo versions return empty body (204), others return JSON
        let body = resp.text().await.unwrap_or_default();
        if body.is_empty() {
            Ok(DispatchWorkflowRun {
                id: None,
                jobs: None,
                run_number: None,
            })
        } else {
            Ok(serde_json::from_str(&body)?)
        }
    }

    pub async fn get_action_run(&self, owner: &str, repo: &str, run_id: u64) -> Result<ActionRun> {
        self.get(&format!("/repos/{owner}/{repo}/actions/runs/{run_id}"))
            .await
    }

    pub async fn list_action_runs(&self, owner: &str, repo: &str) -> Result<ActionRunList> {
        self.get(&format!("/repos/{owner}/{repo}/actions/runs"))
            .await
    }
}

// ---------------------------------------------------------------------------
// Error conversion: ForgejoError → gp::Error
// ---------------------------------------------------------------------------

impl From<ForgejoError> for gp::Error {
    fn from(e: ForgejoError) -> Self {
        match e {
            ForgejoError::Api { status, body } => gp::Error::Api { status, body },
            other => gp::Error::Other(Box::new(other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Type conversions: Forgejo wire types → provider-agnostic types
// ---------------------------------------------------------------------------

impl From<Repository> for gp::Repository {
    fn from(r: Repository) -> Self {
        Self {
            name: r.name,
            full_name: r.full_name,
            default_branch: r.default_branch,
            clone_url: r.clone_url,
            html_url: r.html_url,
            permissions: gp::RepoPermissions {
                admin: r.permissions.admin,
                push: r.permissions.push,
                pull: r.permissions.pull,
            },
        }
    }
}

impl From<PullRequest> for gp::PullRequest {
    fn from(pr: PullRequest) -> Self {
        Self {
            number: pr.number,
            title: pr.title,
            body: pr.body,
            state: pr.state,
            merged: pr.merged,
            mergeable: pr.mergeable,
            html_url: pr.html_url,
            head: gp::PullRequestBranch {
                ref_name: pr.head.ref_field,
                sha: pr.head.sha,
            },
            base: gp::PullRequestBranch {
                ref_name: pr.base.ref_field,
                sha: pr.base.sha,
            },
        }
    }
}

impl From<PullReview> for gp::PullReview {
    fn from(r: PullReview) -> Self {
        Self {
            id: r.id,
            state: r.state,
            body: r.body,
            user: r.user.map(|u| gp::ReviewUser {
                login: u.login,
                id: u.id,
            }),
            submitted_at: r.submitted_at,
        }
    }
}

impl From<CombinedStatus> for gp::CombinedStatus {
    fn from(s: CombinedStatus) -> Self {
        Self {
            state: s.state,
            statuses: s
                .statuses
                .into_iter()
                .map(|cs| gp::CommitStatus {
                    status: cs.status,
                    context: cs.context,
                    description: cs.description,
                    target_url: cs.target_url,
                })
                .collect(),
            total_count: s.total_count,
            sha: s.sha,
        }
    }
}

impl From<Issue> for gp::Issue {
    fn from(i: Issue) -> Self {
        Self {
            number: i.number,
            title: i.title,
            body: i.body,
            state: i.state,
            html_url: i.html_url,
        }
    }
}

impl From<IssueComment> for gp::Comment {
    fn from(c: IssueComment) -> Self {
        Self {
            id: c.id,
            body: c.body,
            user: c.user.map(|u| gp::CommentUser {
                login: u.login,
                id: u.id,
            }),
            created_at: c.created_at,
        }
    }
}

impl From<DispatchWorkflowRun> for gp::DispatchWorkflowRun {
    fn from(r: DispatchWorkflowRun) -> Self {
        Self { id: r.id }
    }
}

impl From<ActionRun> for gp::ActionRun {
    fn from(r: ActionRun) -> Self {
        Self {
            id: r.id,
            status: r.status,
            conclusion: r.conclusion,
            html_url: r.html_url,
        }
    }
}

impl From<ActionRunList> for gp::ActionRunList {
    fn from(l: ActionRunList) -> Self {
        Self {
            workflow_runs: l.workflow_runs.into_iter().map(Into::into).collect(),
            total_count: l.total_count,
        }
    }
}

// ---------------------------------------------------------------------------
// GitProvider trait implementation for ForgejoClient
// ---------------------------------------------------------------------------

#[async_trait]
impl gp::GitProvider for ForgejoClient {
    async fn get_repo(&self, owner: &str, repo: &str) -> gp::Result<gp::Repository> {
        let r: Repository = self.get(&format!("/repos/{owner}/{repo}")).await?;
        Ok(r.into())
    }

    async fn create_pull_request(
        &self,
        owner: &str,
        repo: &str,
        opts: gp::CreatePullRequest,
    ) -> gp::Result<gp::PullRequest> {
        let wire = CreatePullRequestOption {
            title: opts.title,
            body: opts.body,
            head: opts.head,
            base: opts.base,
        };
        let r: PullRequest = self
            .post(&format!("/repos/{owner}/{repo}/pulls"), &wire)
            .await?;
        Ok(r.into())
    }

    async fn list_pull_requests(
        &self,
        owner: &str,
        repo: &str,
        state: Option<&str>,
    ) -> gp::Result<Vec<gp::PullRequest>> {
        let mut path = format!("/repos/{owner}/{repo}/pulls");
        if let Some(s) = state {
            path.push_str(&format!("?state={s}"));
        }
        let prs: Vec<PullRequest> = self.get(&path).await?;
        Ok(prs.into_iter().map(Into::into).collect())
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> gp::Result<gp::PullRequest> {
        let r: PullRequest = self
            .get(&format!("/repos/{owner}/{repo}/pulls/{number}"))
            .await?;
        Ok(r.into())
    }

    async fn find_pr_by_head(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
    ) -> gp::Result<Option<gp::PullRequest>> {
        let prs: Vec<PullRequest> = self
            .get(&format!("/repos/{owner}/{repo}/pulls?state=open"))
            .await?;
        Ok(prs
            .into_iter()
            .find(|pr| pr.head.ref_field == head)
            .map(Into::into))
    }

    async fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        opts: gp::MergePullRequest,
    ) -> gp::Result<()> {
        let wire = MergePullRequestOption {
            method: match opts.method {
                gp::MergeMethod::Merge => "merge".to_string(),
                gp::MergeMethod::Rebase => "rebase".to_string(),
                gp::MergeMethod::Squash => "squash".to_string(),
            },
            merge_message_field: opts.message,
        };
        let path = format!("/repos/{owner}/{repo}/pulls/{number}/merge");
        debug!(path, "POST merge");
        let resp = self
            .client
            .post(self.url(&path))
            .header("Authorization", format!("token {}", self.token))
            .json(&wire)
            .send()
            .await
            .map_err(|e| gp::Error::Other(Box::new(e)))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(gp::Error::Api { status, body });
        }
        Ok(())
    }

    async fn list_reviews(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> gp::Result<Vec<gp::PullReview>> {
        let reviews: Vec<PullReview> = self
            .get(&format!("/repos/{owner}/{repo}/pulls/{pr_number}/reviews"))
            .await?;
        Ok(reviews.into_iter().map(Into::into).collect())
    }

    async fn submit_review(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        opts: gp::CreateReview,
    ) -> gp::Result<gp::PullReview> {
        let wire = CreatePullReviewOptions {
            body: opts.body,
            event: match opts.event {
                gp::ReviewEvent::Approve => "APPROVED".to_string(),
                gp::ReviewEvent::RequestChanges => "REQUEST_CHANGES".to_string(),
                gp::ReviewEvent::Comment => "COMMENT".to_string(),
            },
        };
        let r: PullReview = self
            .post(
                &format!("/repos/{owner}/{repo}/pulls/{pr_number}/reviews"),
                &wire,
            )
            .await?;
        Ok(r.into())
    }

    async fn add_reviewers(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        reviewers: Vec<String>,
    ) -> gp::Result<Vec<gp::PullReview>> {
        let wire = PullReviewRequestOptions { reviewers };
        let reviews: Vec<PullReview> = self
            .post(
                &format!("/repos/{owner}/{repo}/pulls/{pr_number}/requested_reviewers"),
                &wire,
            )
            .await?;
        Ok(reviews.into_iter().map(Into::into).collect())
    }

    async fn create_issue(
        &self,
        owner: &str,
        repo: &str,
        opts: gp::CreateIssue,
    ) -> gp::Result<gp::Issue> {
        let wire = CreateIssueOption {
            title: opts.title,
            body: opts.body,
        };
        let r: Issue = self
            .post(&format!("/repos/{owner}/{repo}/issues"), &wire)
            .await?;
        Ok(r.into())
    }

    async fn update_issue(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        opts: gp::UpdateIssue,
    ) -> gp::Result<gp::Issue> {
        let wire = EditIssueOption {
            title: opts.title,
            body: opts.body,
            state: opts.state,
        };
        let r: Issue = self
            .patch(&format!("/repos/{owner}/{repo}/issues/{number}"), &wire)
            .await?;
        Ok(r.into())
    }

    async fn list_comments(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
    ) -> gp::Result<Vec<gp::Comment>> {
        let comments: Vec<IssueComment> = self
            .get(&format!(
                "/repos/{owner}/{repo}/issues/{issue_number}/comments"
            ))
            .await?;
        Ok(comments.into_iter().map(Into::into).collect())
    }

    async fn create_comment(
        &self,
        owner: &str,
        repo: &str,
        issue_number: u64,
        body: &str,
    ) -> gp::Result<gp::Comment> {
        #[derive(serde::Serialize)]
        struct CreateComment {
            body: String,
        }
        let wire: IssueComment = self
            .post(
                &format!("/repos/{owner}/{repo}/issues/{issue_number}/comments"),
                &CreateComment {
                    body: body.to_string(),
                },
            )
            .await?;
        Ok(wire.into())
    }

    async fn get_combined_status(
        &self,
        owner: &str,
        repo: &str,
        ref_name: &str,
    ) -> gp::Result<gp::CombinedStatus> {
        let s: CombinedStatus = self
            .get(&format!("/repos/{owner}/{repo}/commits/{ref_name}/status"))
            .await?;
        Ok(s.into())
    }

    async fn dispatch_workflow(
        &self,
        owner: &str,
        repo: &str,
        workflow_file: &str,
        opts: gp::DispatchWorkflow,
    ) -> gp::Result<gp::DispatchWorkflowRun> {
        let wire = DispatchWorkflowOption {
            ref_field: opts.git_ref,
            inputs: opts.inputs,
        };
        let path = format!("/repos/{owner}/{repo}/actions/workflows/{workflow_file}/dispatches");
        debug!(path, "POST dispatch workflow");
        let resp = self
            .client
            .post(self.url(&path))
            .header("Authorization", format!("token {}", self.token))
            .json(&wire)
            .send()
            .await
            .map_err(|e| gp::Error::Other(Box::new(e)))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(gp::Error::Api { status, body });
        }
        let body = resp.text().await.unwrap_or_default();
        if body.is_empty() {
            Ok(gp::DispatchWorkflowRun::default())
        } else {
            let r: DispatchWorkflowRun =
                serde_json::from_str(&body).map_err(|e| gp::Error::Other(Box::new(e)))?;
            Ok(r.into())
        }
    }

    async fn get_action_run(
        &self,
        owner: &str,
        repo: &str,
        run_id: u64,
    ) -> gp::Result<gp::ActionRun> {
        let r: ActionRun = self
            .get(&format!("/repos/{owner}/{repo}/actions/runs/{run_id}"))
            .await?;
        Ok(r.into())
    }

    async fn list_action_runs(&self, owner: &str, repo: &str) -> gp::Result<gp::ActionRunList> {
        let l: ActionRunList = self
            .get(&format!("/repos/{owner}/{repo}/actions/runs"))
            .await?;
        Ok(l.into())
    }
}

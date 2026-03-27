mod types;

pub use types::*;

use async_trait::async_trait;
use chuggernaut_git_provider as gp;
use reqwest::Client;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Error)]
pub enum GitHubError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, GitHubError>;

#[derive(Clone)]
pub struct GitHubClient {
    client: Client,
    api_url: String,
    token: String,
}

impl GitHubClient {
    /// Create a new GitHub API client.
    ///
    /// `base_url` is the server URL (e.g. `https://github.com` or a GHES URL).
    /// The API base is derived automatically:
    /// - `https://github.com` → `https://api.github.com`
    /// - `https://github.example.com` → `https://github.example.com/api/v3`
    pub fn new(base_url: &str, token: &str) -> Self {
        let base = base_url.trim_end_matches('/');
        let api_url = if base == "https://github.com" || base == "http://github.com" {
            "https://api.github.com".to_string()
        } else {
            format!("{base}/api/v3")
        };
        Self {
            client: Client::new(),
            api_url,
            token: token.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.api_url)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        debug!(path, "GET");
        let resp = self
            .client
            .get(self.url(path))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
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
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
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
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(body)
            .send()
            .await?;
        self.handle_response(resp).await
    }

    async fn handle_response<T: DeserializeOwned>(&self, resp: reqwest::Response) -> Result<T> {
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(GitHubError::Api { status, body });
        }
        let body = resp.text().await?;
        let parsed = serde_json::from_str(&body)?;
        Ok(parsed)
    }
}

// ---------------------------------------------------------------------------
// Error conversion: GitHubError → gp::Error
// ---------------------------------------------------------------------------

impl From<GitHubError> for gp::Error {
    fn from(e: GitHubError) -> Self {
        match e {
            GitHubError::Api { status, body } => gp::Error::Api { status, body },
            other => gp::Error::Other(Box::new(other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Type conversions: GitHub wire types → provider-agnostic types
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
            body: r.body.unwrap_or_default(),
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
            body: c.body.unwrap_or_default(),
            user: c.user.map(|u| gp::CommentUser {
                login: u.login,
                id: u.id,
            }),
            created_at: c.created_at,
        }
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
// GitProvider trait implementation for GitHubClient
// ---------------------------------------------------------------------------

#[async_trait]
impl gp::GitProvider for GitHubClient {
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
        // GitHub supports filtering by head branch directly
        let path = format!("/repos/{owner}/{repo}/pulls?state=open&head={owner}:{head}");
        let prs: Vec<PullRequest> = self.get(&path).await?;
        Ok(prs.into_iter().next().map(Into::into))
    }

    async fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        opts: gp::MergePullRequest,
    ) -> gp::Result<()> {
        let wire = MergePullRequestOption {
            merge_method: match opts.method {
                gp::MergeMethod::Merge => "merge".to_string(),
                gp::MergeMethod::Rebase => "rebase".to_string(),
                gp::MergeMethod::Squash => "squash".to_string(),
            },
            commit_message: opts.message,
        };
        let path = format!("/repos/{owner}/{repo}/pulls/{number}/merge");
        debug!(path, "PUT merge");
        let resp = self
            .client
            .put(self.url(&path))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
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
                // GitHub uses "APPROVE" (no trailing D), unlike Forgejo's "APPROVED"
                gp::ReviewEvent::Approve => "APPROVE".to_string(),
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
        // GitHub's request-reviewers endpoint returns a PullRequest, not reviews.
        // We just need to confirm success; return an empty vec.
        let wire = PullReviewRequestOptions { reviewers };
        let path = format!("/repos/{owner}/{repo}/pulls/{pr_number}/requested_reviewers");
        debug!(path, "POST add reviewers");
        let resp = self
            .client
            .post(self.url(&path))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&wire)
            .send()
            .await
            .map_err(|e| gp::Error::Other(Box::new(e)))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(gp::Error::Api { status, body });
        }
        Ok(vec![])
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
        let wire = UpdateIssueOption {
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
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&wire)
            .send()
            .await
            .map_err(|e| gp::Error::Other(Box::new(e)))?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(gp::Error::Api { status, body });
        }
        // GitHub always returns 204 No Content for workflow dispatches
        Ok(gp::DispatchWorkflowRun::default())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_github_com() {
        let client = GitHubClient::new("https://github.com", "tok");
        assert_eq!(client.api_url, "https://api.github.com");
    }

    #[test]
    fn api_url_github_com_trailing_slash() {
        let client = GitHubClient::new("https://github.com/", "tok");
        assert_eq!(client.api_url, "https://api.github.com");
    }

    #[test]
    fn api_url_ghes() {
        let client = GitHubClient::new("https://github.example.com", "tok");
        assert_eq!(client.api_url, "https://github.example.com/api/v3");
    }

    #[test]
    fn combined_status_deserialize() {
        let json = r#"{
            "state": "success",
            "statuses": [{"id":1,"state":"success","context":"ci/test"}],
            "total_count": 1,
            "sha": "abc123"
        }"#;
        let status: CombinedStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.statuses.len(), 1);
        assert_eq!(status.statuses[0].status, "success");
    }

    #[test]
    fn pull_request_deserialize() {
        let json = r#"{
            "id": 1,
            "number": 42,
            "title": "Test PR",
            "body": "Description",
            "state": "open",
            "merged": false,
            "html_url": "https://github.com/owner/repo/pull/42",
            "head": {"ref": "feature-branch", "sha": "abc123"},
            "base": {"ref": "main", "sha": "def456"}
        }"#;
        let pr: PullRequest = serde_json::from_str(json).unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.head.ref_field, "feature-branch");
    }
}

mod types;

pub use types::*;

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

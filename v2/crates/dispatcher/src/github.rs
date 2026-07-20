//! Minimal GitHub REST client for the origin-release PR surface — create and
//! read pull requests, nothing else. Behind a trait so integration tests
//! script PR state instead of talking to GitHub. The PAT is resolved from
//! project secrets per call (see `origin.rs`), never held by the client.

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("github api: {0}")]
    Http(String),
    #[error("github api {status} on {context}: {message}")]
    Status {
        status: u16,
        context: String,
        message: String,
    },
}

pub type Result<T> = std::result::Result<T, GithubError>;

#[derive(Debug, Clone, PartialEq)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    /// `open` | `closed` — GitHub reports merged PRs as `closed` with
    /// `merged: true`.
    pub state: String,
    pub merged: bool,
    pub merge_commit_sha: Option<String>,
}

#[async_trait]
pub trait PullRequestApi: Send + Sync {
    /// `POST /repos/{repo}/pulls` — open `head` → `base`.
    async fn create_pr(
        &self,
        repo: &str,
        pat: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrInfo>;

    /// `GET /repos/{repo}/pulls/{number}`.
    async fn get_pr(&self, repo: &str, pat: &str, number: u64) -> Result<PrInfo>;
}

/// Subset of the GitHub pull-request resource we consume.
#[derive(Deserialize)]
struct PrResource {
    number: u64,
    html_url: String,
    state: String,
    #[serde(default)]
    merged: bool,
    merge_commit_sha: Option<String>,
}

impl From<PrResource> for PrInfo {
    fn from(r: PrResource) -> Self {
        Self {
            number: r.number,
            url: r.html_url,
            state: r.state,
            merged: r.merged,
            merge_commit_sha: r.merge_commit_sha,
        }
    }
}

pub struct GithubClient {
    client: reqwest::Client,
    /// Overridable for tests against a local HTTP stub.
    api_base: String,
}

impl GithubClient {
    pub fn new() -> Self {
        Self::with_base("https://api.github.com")
    }

    pub fn with_base(api_base: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_base: api_base.into(),
        }
    }

    fn request(&self, method: reqwest::Method, url: &str, pat: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .bearer_auth(pat)
            .header("User-Agent", "chuggernaut")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .timeout(std::time::Duration::from_secs(30))
    }

    async fn parse_pr(resp: reqwest::Response, context: &str) -> Result<PrInfo> {
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| GithubError::Http(e.to_string()))?;
        if !status.is_success() {
            // GitHub error envelopes carry a "message"; fall back to the body.
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["message"].as_str().map(String::from))
                .unwrap_or(text);
            return Err(GithubError::Status {
                status: status.as_u16(),
                context: context.to_string(),
                message,
            });
        }
        serde_json::from_str::<PrResource>(&text)
            .map(PrInfo::from)
            .map_err(|e| GithubError::Http(format!("parsing {context}: {e}")))
    }
}

impl Default for GithubClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PullRequestApi for GithubClient {
    async fn create_pr(
        &self,
        repo: &str,
        pat: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrInfo> {
        let url = format!("{}/repos/{repo}/pulls", self.api_base);
        let resp = self
            .request(reqwest::Method::POST, &url, pat)
            .json(&serde_json::json!({
                "title": title, "head": head, "base": base, "body": body,
            }))
            .send()
            .await
            .map_err(|e| GithubError::Http(e.to_string()))?;
        Self::parse_pr(resp, &format!("POST /repos/{repo}/pulls")).await
    }

    async fn get_pr(&self, repo: &str, pat: &str, number: u64) -> Result<PrInfo> {
        let url = format!("{}/repos/{repo}/pulls/{number}", self.api_base);
        let resp = self
            .request(reqwest::Method::GET, &url, pat)
            .send()
            .await
            .map_err(|e| GithubError::Http(e.to_string()))?;
        Self::parse_pr(resp, &format!("GET /repos/{repo}/pulls/{number}")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_resource_parses_github_json() {
        let json = r#"{
            "number": 41,
            "html_url": "https://github.com/acme/api/pull/41",
            "state": "closed",
            "merged": true,
            "merge_commit_sha": "6dcb09b5b57875f334f61aebed695e2e4193db5e",
            "title": "chug release 3",
            "user": {"login": "chug-bot"}
        }"#;
        let pr: PrInfo = serde_json::from_str::<PrResource>(json).unwrap().into();
        assert_eq!(pr.number, 41);
        assert_eq!(pr.state, "closed");
        assert!(pr.merged);
        assert_eq!(
            pr.merge_commit_sha.as_deref(),
            Some("6dcb09b5b57875f334f61aebed695e2e4193db5e")
        );
    }

    #[test]
    fn open_pr_without_merged_field_defaults_false() {
        let json = r#"{"number": 7, "html_url": "u", "state": "open", "merge_commit_sha": null}"#;
        let pr: PrInfo = serde_json::from_str::<PrResource>(json).unwrap().into();
        assert!(!pr.merged);
        assert_eq!(pr.state, "open");
    }
}

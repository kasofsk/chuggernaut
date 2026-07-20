//! Project record (linked-origin projects): platform-level state for a project
//! whose canonical default branch lives on an external git host (GitHub). A
//! classic self-hosted project has no record — absence means "not linked".
//!
//! For linked projects the local bare repo's HEAD points at the
//! chuggernaut-owned `integration` branch; the external default branch is
//! tracked as `refs/remotes/origin/{main_branch}` and only ever reached via
//! pull requests opened from pushed `chug/release-{n}` snapshots.

use serde::{Deserialize, Serialize};

/// `projects.{owner}.{project}` KV entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectRecord {
    /// Present iff the project is linked to an external origin.
    pub origin: Option<OriginLink>,
    /// The current (latest) origin release; `None` before the first release.
    pub release: Option<ReleaseState>,
    /// Monotonic origin-release sequence; incremented (and persisted) before
    /// the release branch is pushed so a crashed release never reuses `n`.
    pub release_counter: u64,
}

/// Immutable link configuration, mirrored from the bare repo's git config
/// (`remote.origin.url`) for API/UI consumption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OriginLink {
    /// Git URL of the external origin (`ssh://git@github.com/owner/repo.git`).
    pub url: String,
    /// The origin's default branch (usually `main`), autodetected at link time.
    pub main_branch: String,
    /// `owner/repo` for the GitHub REST API, parsed from `url` at link time.
    /// `None` for non-GitHub origins (e.g. `file://` test fixtures) — release
    /// then pushes the branch but cannot open a PR.
    pub github_repo: Option<String>,
}

/// One origin release: a frozen snapshot of `integration` pushed to the origin
/// as `chug/release-{number}` with a PR into the origin's default branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseState {
    pub number: u64,
    pub pr_number: u64,
    pub pr_url: String,
    /// `refs/remotes/origin/{main}` at PR-open time.
    pub base_main_sha: String,
    /// `integration` at PR-open time (== tip of `chug/release-{number}`).
    pub integration_sha: String,
    pub status: ReleaseStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseStatus {
    /// PR open on the origin; the project's merge queue is held.
    Open,
    /// PR merged; integration was hard-reset onto the new origin main.
    Merged,
    /// PR closed unmerged; integration kept its unreleased work.
    Closed,
}

/// Parse `owner/repo` out of a GitHub remote URL for the REST API.
/// Supports `ssh://git@github.com/owner/repo.git`, `git@github.com:owner/repo.git`,
/// and `https://github.com/owner/repo(.git)`. `None` for anything else.
pub fn github_repo_from_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("ssh://git@github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("https://github.com/"))?;
    let repo = rest.strip_suffix(".git").unwrap_or(rest).trim_end_matches('/');
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => {
            Some(format!("{owner}/{name}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_repo_parsing() {
        for url in [
            "ssh://git@github.com/acme/api.git",
            "git@github.com:acme/api.git",
            "https://github.com/acme/api",
            "https://github.com/acme/api.git",
        ] {
            assert_eq!(github_repo_from_url(url).as_deref(), Some("acme/api"), "{url}");
        }
        assert_eq!(github_repo_from_url("file:///tmp/origin.git"), None);
        assert_eq!(github_repo_from_url("ssh://git@github.com/acme"), None);
        assert_eq!(github_repo_from_url("ssh://git@github.com/a/b/c"), None);
    }

    #[test]
    fn record_roundtrip() {
        let rec = ProjectRecord {
            origin: Some(OriginLink {
                url: "ssh://git@github.com/acme/api.git".into(),
                main_branch: "main".into(),
                github_repo: Some("acme/api".into()),
            }),
            release: Some(ReleaseState {
                number: 3,
                pr_number: 41,
                pr_url: "https://github.com/acme/api/pull/41".into(),
                base_main_sha: "a".repeat(40),
                integration_sha: "b".repeat(40),
                status: ReleaseStatus::Open,
            }),
            release_counter: 3,
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert_eq!(serde_json::from_str::<ProjectRecord>(&json).unwrap(), rec);
    }
}

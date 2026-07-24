//! Linked-origin projects: the link flow (`req.projects.link`) and the origin
//! release surface (`req.origin.*`) — GitHub owns the default branch, jobs
//! land on the local `integration` branch, and work ships as PRs opened from
//! pushed `chug/release-{n}` snapshots.
//!
//! Everything here runs inside the single-writer core actor: origin git ops
//! need the age identity for the deploy key, and hold/reset/pump are actor
//! state. Origin credentials live in project secrets under reserved names and
//! are never injected into containers (see `RESERVED_SECRET_PREFIX`).
//!
//! - **Accepts:** `req.projects.link` and `req.origin.*` requests; project
//!   secrets for the deploy key.
//! - **Emits:** origin git ops (the local `integration` branch,
//!   `chug/release-{n}` snapshots) and GitHub PRs via `github`.
//! - **Guarantees:** runs inside the single-writer actor; origin credentials
//!   are never injected into containers.
//! - **Spec:** §5.3.

use crate::core::{Core, CoreError, Result};
use crate::release::ValidationError;
use store::secrets::SecretStore;
use types::{OriginLink, ProjectRecord, ReleaseState, ReleaseStatus};
use vcs::OriginEnv;

/// `req.origin.status` / `req.origin.sync` reply: the project record plus the
/// live git view (local resolves only — `sync` is the fetching variant).
#[derive(Debug, Clone, serde::Serialize)]
pub struct OriginStatusResponse {
    pub origin: Option<OriginLink>,
    pub release: Option<ReleaseState>,
    pub release_counter: u64,
    /// `refs/remotes/origin/{main}` as last fetched.
    pub origin_main_sha: Option<String>,
    /// Tip of the local `integration` branch.
    pub integration_sha: Option<String>,
    /// Commits on `integration` not reachable from origin main — the
    /// unreleased backlog.
    pub ahead_by: u64,
    /// Merge queue held by an Open release.
    pub held: bool,
}

/// OpenSSH private key (write deploy key) for fetch/push to the origin.
pub const SECRET_DEPLOY_KEY: &str = "CHUG_ORIGIN_DEPLOY_KEY";
/// Fine-grained PAT (pull requests read/write) for the GitHub REST API.
pub const SECRET_PAT: &str = "CHUG_ORIGIN_PAT";
/// Reserved secret-name prefix: dispatcher-only credentials. Job types cannot
/// declare them (release validation) and injection skips them (defense in
/// depth) — nothing inside a container ever sees an origin credential.
pub const RESERVED_SECRET_PREFIX: &str = "CHUG_";

impl Core {
    /// Decrypted project secret — age store, or raw KV in dev mode (the same
    /// fallback `container_env` uses).
    pub(crate) async fn secret_value(
        &self,
        owner: &str,
        project: &str,
        name: &str,
    ) -> Result<Option<String>> {
        match &self.secrets {
            Some(secrets) => Ok(secrets.get(owner, project, name).await?),
            None => {
                let bucket = self.store.raw_bucket(store::buckets::SECRETS).await?;
                Ok(bucket
                    .get_json::<String>(&format!("{owner}.{project}.{name}"))
                    .await?)
            }
        }
    }

    /// Build the environment for a dispatcher-side origin git op. For SSH-style
    /// origins the deploy key is decrypted into a 0600 file in a private
    /// tempdir; the returned guard must be held across the git call and drops
    /// the key from disk afterward. `file://` origins (tests) need nothing.
    pub(crate) async fn origin_git_env(
        &self,
        owner: &str,
        project: &str,
        origin_url: &str,
    ) -> Result<(OriginEnv, Option<tempfile::TempDir>)> {
        if !origin_url.starts_with("ssh://") && !origin_url.contains('@') {
            return Ok((OriginEnv::default(), None));
        }
        let key = self
            .secret_value(owner, project, SECRET_DEPLOY_KEY)
            .await?
            .ok_or_else(|| {
                CoreError::Config(format!(
                    "secret {SECRET_DEPLOY_KEY} is not set for {owner}/{project}"
                ))
            })?;
        let dir = tempfile::tempdir().map_err(CoreError::from_io("origin key tempdir"))?;
        let path = dir.path().join("deploy_key");
        // OpenSSH refuses keys without a trailing newline or with open modes.
        let mut contents = key;
        if !contents.ends_with('\n') {
            contents.push('\n');
        }
        tokio::fs::write(&path, &contents)
            .await
            .map_err(CoreError::from_io("writing origin key"))?;
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(CoreError::from_io("origin key permissions"))?;
        let env = OriginEnv {
            ssh_command: Some(format!(
                "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new",
                path.display()
            )),
        };
        Ok((env, Some(dir)))
    }

    /// `req.projects.link`: create a linked-origin project. The caller
    /// (handler) has already validated the name components; this checks
    /// existence and credentials, creates the repo fetched from the origin
    /// with `HEAD` → `integration`, installs the hook, seeds the chuggernaut
    /// config surface (skip-existing), and writes the project record.
    pub async fn link_project(
        &mut self,
        owner: &str,
        name: &str,
        origin_url: &str,
        main_branch: Option<&str>,
    ) -> Result<ProjectRecord> {
        let counters = self.store.raw_bucket(store::buckets::COUNTERS).await?;
        let counter_key = format!("{owner}.{name}");
        if counters.get_json::<u64>(&counter_key).await?.is_some() {
            return Err(CoreError::Conflict(format!(
                "project {owner}/{name} already exists"
            )));
        }

        // Credential preconditions, checked before any repo state exists so
        // the request fails clean and retryable. The deploy key is exercised
        // by the fetch below; the PAT only at first release — but requiring
        // both up front makes onboarding failures immediate. file:// origins
        // (tests, local mirrors) need neither.
        let needs_ssh = origin_url.starts_with("ssh://") || origin_url.contains('@');
        let github_repo = types::github_repo_from_url(origin_url);
        let mut missing = Vec::new();
        if needs_ssh
            && self
                .secret_value(owner, name, SECRET_DEPLOY_KEY)
                .await?
                .is_none()
        {
            missing.push(SECRET_DEPLOY_KEY);
        }
        if github_repo.is_some() && self.secret_value(owner, name, SECRET_PAT).await?.is_none() {
            missing.push(SECRET_PAT);
        }
        if !missing.is_empty() {
            return Err(CoreError::Validation(
                missing
                    .into_iter()
                    .map(|s| {
                        ValidationError::new(
                            None,
                            "secrets".to_string(),
                            format!("secret '{s}' must be set before linking (admin secret set)"),
                        )
                    })
                    .collect(),
            ));
        }

        let (env, _key_guard) = self.origin_git_env(owner, name, origin_url).await?;
        let main = self
            .repos
            .create_linked_project(owner, name, origin_url, main_branch, &env)
            .await?;

        let bin = self
            .config
            .hook_bin
            .clone()
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| "/usr/local/bin/chuggernaut".into());
        self.repos
            .install_pre_receive_hook(owner, name, &auth::ssh::pre_receive_hook_body(&bin))
            .await?;
        self.repos
            .seed_files(
                owner,
                name,
                crate::seed::CONFIG_TEMPLATE,
                "chuggernaut: seed job types and prompts",
                true,
            )
            .await?;

        let record = ProjectRecord {
            origin: Some(OriginLink {
                url: origin_url.to_string(),
                main_branch: main,
                github_repo,
            }),
            release: None,
            release_counter: 0,
        };
        self.projects.put(owner, name, &record).await?;
        counters.put_json(&counter_key, &0u64).await?;
        Ok(record)
    }

    /// The project's record + link, or the right 404/409 when absent/classic.
    async fn linked_record(
        &self,
        owner: &str,
        project: &str,
    ) -> Result<(ProjectRecord, OriginLink)> {
        let record = self.projects.get(owner, project).await?.ok_or_else(|| {
            CoreError::NotFound(format!("project {owner}/{project} is not linked"))
        })?;
        let link = record.origin.clone().ok_or_else(|| {
            CoreError::Conflict(format!("project {owner}/{project} has no linked origin"))
        })?;
        Ok((record, link))
    }

    /// `req.origin.release`: push the current `integration` to the origin as
    /// `chug/release-{n}`, open the PR, persist `ReleaseState{Open}`, and hold
    /// the merge queue.
    ///
    /// Crash-safety ordering: the counter increment is persisted before the
    /// push, so a crash mid-release burns `n` (an orphan branch on the origin
    /// is harmless) instead of ever reusing it; `ReleaseState` + hold commit
    /// last, atomically with respect to the single-writer actor.
    pub async fn origin_release(&mut self, owner: &str, project: &str) -> Result<ProjectRecord> {
        let (mut record, link) = self.linked_record(owner, project).await?;
        let slug = format!("{owner}/{project}");
        if matches!(&record.release, Some(r) if r.status == ReleaseStatus::Open) {
            let n = record.release.as_ref().map(|r| r.number).unwrap_or(0);
            return Err(CoreError::Conflict(format!(
                "origin release {n} is already open"
            )));
        }
        // A gate in flight would land a commit on integration *after* the
        // snapshot below, and the post-merge reset would silently discard it.
        if self
            .merge_gates
            .get(&slug)
            .is_some_and(|g| g.gating.is_some())
        {
            return Err(CoreError::Conflict(
                "merge gate in flight — retry shortly".into(),
            ));
        }

        let (env, _key_guard) = self.origin_git_env(owner, project, &link.url).await?;
        let base_main_sha = self.repos.fetch_origin(owner, project, &env).await?;
        let integration = self.repos.default_branch(owner, project).await?;
        if !self
            .repos
            .has_commits_beyond(owner, project, &base_main_sha, &integration)
            .await?
        {
            return Err(CoreError::Conflict(
                "nothing to release — integration is not ahead of origin main".into(),
            ));
        }

        let n = record.release_counter + 1;
        record.release_counter = n;
        self.projects.put(owner, project, &record).await?;

        let integration_sha = self.repos.resolve_ref(owner, project, &integration).await?;
        let release_branch = format!("chug/release-{n}");
        // Local pin first: after a later squash-merge reset, held jobs'
        // base_refs are only reachable through this ref (gc + provenance).
        self.repos
            .update_ref(
                owner,
                project,
                &format!("refs/chug/release-{n}"),
                &integration_sha,
            )
            .await?;
        self.repos
            .push_origin(
                owner,
                project,
                "refs/heads/integration",
                &format!("refs/heads/{release_branch}"),
                false,
                &env,
            )
            .await?;

        let (pr_number, pr_url) = match &link.github_repo {
            Some(repo) => {
                let pat = self.require_pat(owner, project).await?;
                let body = self
                    .release_pr_body(owner, project, n, &base_main_sha, &integration)
                    .await?;
                let pr = self
                    .pr_api
                    .create_pr(
                        repo,
                        &pat,
                        &release_branch,
                        &link.main_branch,
                        &format!("chug release {n}"),
                        &body,
                    )
                    .await
                    .map_err(|e| CoreError::Config(e.to_string()))?;
                (pr.number, pr.url)
            }
            // Non-GitHub origin: branch pushed, no PR. Sync resolves the
            // release by watching origin main move (see `origin_sync`).
            None => (0, String::new()),
        };

        record.release = Some(ReleaseState {
            number: n,
            pr_number,
            pr_url,
            base_main_sha,
            integration_sha,
            status: ReleaseStatus::Open,
        });
        self.projects.put(owner, project, &record).await?;
        self.release_holds.insert(slug);
        Ok(record)
    }

    async fn require_pat(&self, owner: &str, project: &str) -> Result<String> {
        self.secret_value(owner, project, SECRET_PAT)
            .await?
            .ok_or_else(|| {
                CoreError::Config(format!(
                    "secret {SECRET_PAT} is not set for {owner}/{project}"
                ))
            })
    }

    /// PR body: the squash subjects that landed on integration since the last
    /// origin main base.
    async fn release_pr_body(
        &self,
        owner: &str,
        project: &str,
        n: u64,
        base_main_sha: &str,
        integration: &str,
    ) -> Result<String> {
        let range = format!("{base_main_sha}..{integration}");
        let log = self.repos.log(owner, project, Some(&range), 200).await?;
        let mut body = format!("Chuggernaut origin release {n}.\n\nJobs in this release:\n");
        for entry in log {
            body.push_str(&format!("- {}\n", entry.message));
        }
        Ok(body)
    }

    /// `req.origin.status`: the record + live git view. When a release is
    /// Open this also runs the sync reconciliation opportunistically, so
    /// polling the status page is enough to land a merged PR.
    pub async fn origin_status(
        &mut self,
        owner: &str,
        project: &str,
    ) -> Result<OriginStatusResponse> {
        let (record, _) = self.linked_record(owner, project).await?;
        if matches!(&record.release, Some(r) if r.status == ReleaseStatus::Open) {
            return self.origin_sync(owner, project).await;
        }
        self.status_response(owner, project, record).await
    }

    /// `req.origin.sync`: fetch the origin and reconcile.
    /// - Open release, PR merged → mark Merged, hard-reset `integration` onto
    ///   the new origin main, clear the hold, pump the merge queue (held jobs
    ///   finalize against the new HEAD via the normal gate/rework paths).
    /// - Open release, PR closed unmerged → mark Closed, clear the hold, no
    ///   reset (integration keeps the unreleased work; re-release later).
    /// - No open release → fast-forward `integration` to origin main when it
    ///   has nothing unreleased; otherwise leave it (divergence surfaces as PR
    ///   conflicts at the next release — documented v1 limitation).
    pub async fn origin_sync(
        &mut self,
        owner: &str,
        project: &str,
    ) -> Result<OriginStatusResponse> {
        let (mut record, link) = self.linked_record(owner, project).await?;
        let slug = format!("{owner}/{project}");
        let (env, _key_guard) = self.origin_git_env(owner, project, &link.url).await?;
        let origin_main_sha = self.repos.fetch_origin(owner, project, &env).await?;

        match record.release.clone() {
            Some(release) if release.status == ReleaseStatus::Open => {
                let outcome = if release.pr_number != 0 {
                    let repo = link.github_repo.as_deref().ok_or_else(|| {
                        CoreError::Config("release has a PR but origin is not GitHub".into())
                    })?;
                    let pat = self.require_pat(owner, project).await?;
                    let pr = self
                        .pr_api
                        .get_pr(repo, &pat, release.pr_number)
                        .await
                        .map_err(|e| CoreError::Config(e.to_string()))?;
                    if pr.merged {
                        Some(ReleaseStatus::Merged)
                    } else if pr.state == "closed" {
                        Some(ReleaseStatus::Closed)
                    } else {
                        None
                    }
                } else {
                    // No PR to ask (non-GitHub origin): origin main moving off
                    // the release base is the only merge signal available.
                    (origin_main_sha != release.base_main_sha).then_some(ReleaseStatus::Merged)
                };

                match outcome {
                    Some(ReleaseStatus::Merged) => {
                        let integration = self.repos.default_branch(owner, project).await?;
                        self.repos
                            .reset_branch(owner, project, &integration, &origin_main_sha)
                            .await?;
                        record.release = Some(ReleaseState {
                            status: ReleaseStatus::Merged,
                            ..release
                        });
                        self.projects.put(owner, project, &record).await?;
                        self.release_holds.remove(&slug);
                        self.pump_merges(owner, project).await?;
                    }
                    Some(ReleaseStatus::Closed) => {
                        record.release = Some(ReleaseState {
                            status: ReleaseStatus::Closed,
                            ..release
                        });
                        self.projects.put(owner, project, &record).await?;
                        self.release_holds.remove(&slug);
                        self.pump_merges(owner, project).await?;
                    }
                    _ => {}
                }
            }
            _ => {
                // No open release: keep integration on origin main while it
                // has nothing unreleased (external commits flow in).
                let integration = self.repos.default_branch(owner, project).await?;
                let integration_sha = self.repos.resolve_ref(owner, project, &integration).await?;
                if integration_sha != origin_main_sha
                    && !self
                        .repos
                        .has_commits_beyond(owner, project, &origin_main_sha, &integration)
                        .await?
                {
                    self.repos
                        .reset_branch(owner, project, &integration, &origin_main_sha)
                        .await?;
                }
            }
        }
        self.status_response(owner, project, record).await
    }

    async fn status_response(
        &mut self,
        owner: &str,
        project: &str,
        record: ProjectRecord,
    ) -> Result<OriginStatusResponse> {
        // Re-read: sync paths above may have updated the record.
        let record = self.projects.get(owner, project).await?.unwrap_or(record);
        let integration = self.repos.default_branch(owner, project).await?;
        let integration_sha = self
            .repos
            .resolve_ref(owner, project, &integration)
            .await
            .ok();
        let origin_main_sha = self.repos.origin_main_sha(owner, project).await.ok();
        let ahead_by = match &origin_main_sha {
            Some(main) => self
                .repos
                .count_commits_beyond(owner, project, main, &integration)
                .await
                .unwrap_or(0),
            None => 0,
        };
        Ok(OriginStatusResponse {
            held: self.release_holds.contains(&format!("{owner}/{project}")),
            origin: record.origin,
            release: record.release,
            release_counter: record.release_counter,
            origin_main_sha,
            integration_sha,
            ahead_by,
        })
    }
}

//! Bare repo management and git operations (spec Part 5, §12.2).
//!
//! All operations shell out to the `git` CLI against bare repos at
//! `{repos_root}/{owner}/{project}.git`. There is no working tree anywhere:
//! branch operations are `update-ref`, and squash-merges use
//! `git merge-tree --write-tree` (git ≥ 2.38) + `commit-tree`. The single-writer
//! dispatcher serializes all mutations; concurrent reads are safe.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Output;
use thiserror::Error;
use tokio::process::Command;
use types::{Job, JobState};

#[derive(Debug, Error)]
pub enum VcsError {
    #[error("git {args} failed: {stderr}")]
    Git { args: String, stderr: String },
    #[error("repo already exists: {0}")]
    RepoExists(String),
    #[error("git >= 2.38 required for merge-tree --write-tree; found: {0}")]
    GitTooOld(String),
    #[error("unexpected git output from {context}: {detail}")]
    Parse {
        context: &'static str,
        detail: String,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("git {args} timed out after {seconds}s")]
    Timeout { args: String, seconds: u64 },
}

pub type Result<T> = std::result::Result<T, VcsError>;

#[derive(Debug, Clone, PartialEq)]
pub enum MergeOutcome {
    Merged {
        commit: String,
    },
    /// No commits on the job branch beyond `base_ref` (spec §5.1).
    NoOp,
    Conflict {
        files: Vec<String>,
    },
    /// The squash tree still carries conflict markers left by a prior WIP
    /// rebase commit that the agent never resolved (spec §3.2 step 12 guard).
    /// Merging would land `<<<<<<< / ======= / >>>>>>>` on the default branch,
    /// so the dispatcher escalates instead.
    UnresolvedMarkers {
        files: Vec<String>,
    },
}

/// Internal result of building a squash commit without advancing any ref.
enum SquashBuild {
    Commit { commit: String, old_head: String },
    NoOp,
    Conflict { files: Vec<String> },
    UnresolvedMarkers { files: Vec<String> },
}

/// Outcome of [`RepoManager::rebase_onto_with_conflict`] — the merge-tree
/// rebase that REUSES the merged tree instead of discarding it (spec §3.2
/// step 12 conflict / merge-gate rework). Unlike [`RebaseOutcome`], BOTH arms
/// move `job/{seq}` to a single WIP commit parented on the new base; a
/// `Conflict` leaves conflict markers in the listed files' blobs for the agent
/// to resolve in place.
#[derive(Debug, Clone, PartialEq)]
pub enum ConflictRebaseOutcome {
    /// Merged cleanly onto the new base (defensive — a real conflict is the
    /// expected reason this path runs).
    Clean,
    /// Merged onto the new base with conflict markers in the listed files.
    Conflict { files: Vec<String> },
}

/// Outcome of replaying `job/{seq}` onto a fresh base (spec §3.2 pre-eval
/// rebase). A `Conflict` leaves the branch exactly as pushed — the caller
/// evaluates on the old base and lets the wrap-up merge gate handle it.
#[derive(Debug, Clone, PartialEq)]
pub enum RebaseOutcome {
    /// Branch replayed onto the new base; its tip is now `new_head`.
    Rebased { new_head: String },
    /// A real merge conflict replaying some commit; branch untouched.
    Conflict { files: Vec<String> },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DiffResponse {
    pub files: Vec<FileStat>,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileStat {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub r#type: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlobResponse {
    pub content: String,
    pub encoding: BlobEncoding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BlobEncoding {
    #[serde(rename = "utf-8")]
    Utf8,
    #[serde(rename = "base64")]
    Base64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogEntry {
    pub hash: String,
    pub message: String,
    pub author: String,
    pub ts: DateTime<Utc>,
}

/// Environment for origin-facing git commands (fetch, push, ls-remote).
/// `ssh_command` becomes `GIT_SSH_COMMAND` — the dispatcher builds it around a
/// decrypted deploy key; `None` for `file://` origins (tests, local mirrors).
#[derive(Debug, Clone, Default)]
pub struct OriginEnv {
    pub ssh_command: Option<String>,
}

/// Bound on origin-facing git commands (fetch/push/ls-remote): these hit the
/// network from inside the single-writer dispatcher task, so a hung remote
/// must fail the operation, not wedge the actor. Local plumbing stays unbounded.
const ORIGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Commit identity for all dispatcher-authored commits (init, squash-merges).
const GIT_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "chuggernaut"),
    ("GIT_AUTHOR_EMAIL", "dispatcher@chuggernaut.local"),
    ("GIT_COMMITTER_NAME", "chuggernaut"),
    ("GIT_COMMITTER_EMAIL", "dispatcher@chuggernaut.local"),
];

pub struct RepoManager {
    repos_root: PathBuf,
}

impl RepoManager {
    pub fn new(repos_root: impl Into<PathBuf>) -> Self {
        Self {
            repos_root: repos_root.into(),
        }
    }

    pub fn repo_path(&self, owner: &str, project: &str) -> PathBuf {
        self.repos_root.join(owner).join(format!("{project}.git"))
    }

    /// Startup check: `merge-tree --write-tree` needs git ≥ 2.38.
    pub async fn check_git_version(&self) -> Result<()> {
        let out = self.exec(&self.repos_root, &["version"], None).await?;
        let text = expect_success(out, "version")?;
        let version = text
            .trim()
            .strip_prefix("git version ")
            .unwrap_or(text.trim());
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        if (major, minor) < (2, 38) {
            return Err(VcsError::GitTooOld(version.to_string()));
        }
        Ok(())
    }

    // ── Project lifecycle (spec §12.2) ──────────────────────────────────────

    /// Init a bare repo with `HEAD` → `refs/heads/{default_branch}` and an
    /// initial empty commit so HEAD is a valid ref.
    pub async fn create_project(
        &self,
        owner: &str,
        project: &str,
        default_branch: &str,
    ) -> Result<()> {
        let repo = self.repo_path(owner, project);
        if repo.exists() {
            return Err(VcsError::RepoExists(format!("{owner}/{project}")));
        }
        tokio::fs::create_dir_all(&repo).await?;
        self.run(
            &repo,
            &["init", "--bare", "--initial-branch", default_branch, "."],
        )
        .await?;
        let empty_tree = self.run_stdin(&repo, &["mktree"], b"").await?;
        let commit = self
            .run(
                &repo,
                &[
                    "commit-tree",
                    empty_tree.trim(),
                    "-m",
                    "chuggernaut: initialize repository",
                ],
            )
            .await?;
        self.run(
            &repo,
            &[
                "update-ref",
                &format!("refs/heads/{default_branch}"),
                commit.trim(),
            ],
        )
        .await?;
        self.ensure_upload_filter(owner, project).await?;
        Ok(())
    }

    /// Write `hooks/pre-receive` into a bare repo (body from `auth::ssh`,
    /// §5.2/§12.2), mode 0755.
    pub async fn install_pre_receive_hook(
        &self,
        owner: &str,
        project: &str,
        body: &str,
    ) -> Result<()> {
        let hook = self
            .repo_path(owner, project)
            .join("hooks")
            .join("pre-receive");
        tokio::fs::create_dir_all(hook.parent().unwrap()).await?;
        tokio::fs::write(&hook, body).await?;
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).await?;
        Ok(())
    }

    /// Commit a set of files onto the default branch of a bare repo, via a
    /// temporary worktree. Used to seed a fresh project with the platform
    /// starter template (§12.2). Files whose path ends in `.sh` are committed
    /// executable.
    ///
    /// With `skip_existing`, paths already present at the branch tip are left
    /// untouched (seeding config into a linked repo must never clobber user
    /// files), and the commit is skipped entirely when nothing new is staged.
    pub async fn seed_files(
        &self,
        owner: &str,
        project: &str,
        files: &[(&str, &str)],
        message: &str,
        skip_existing: bool,
    ) -> Result<()> {
        let repo = self.repo_path(owner, project);
        let branch = self.default_branch(owner, project).await?;
        let tmp = tempfile::tempdir()?;
        let wt = tmp.path().join("wt");
        let wt_str = wt.to_string_lossy().to_string();
        self.run(&repo, &["worktree", "add", &wt_str, &branch])
            .await?;
        let result: Result<()> = async {
            for (path, contents) in files {
                let dest = wt.join(path);
                if skip_existing && dest.exists() {
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&dest, contents).await?;
                if path.ends_with(".sh") {
                    use std::os::unix::fs::PermissionsExt;
                    tokio::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
                        .await?;
                }
            }
            self.run(&wt, &["add", "-A"]).await?;
            let staged = self
                .exec(&wt, &["diff", "--cached", "--quiet"], None)
                .await?;
            if !staged.status.success() {
                self.run(&wt, &["commit", "-m", message]).await?;
            }
            Ok(())
        }
        .await;
        // Always detach the worktree — a stale registration would block the
        // next seed. The temp dir itself is cleaned by its guard.
        let _ = self
            .run(&repo, &["worktree", "remove", "--force", &wt_str])
            .await;
        result
    }

    // ── Linked-origin projects ──────────────────────────────────────────────

    /// Create a project whose canonical default branch lives on an external
    /// origin: init a bare repo, register `origin` with a single-branch fetch
    /// refspec, fetch, and point `HEAD` at a local `integration` branch created
    /// from the origin's default branch. Returns the origin main branch name
    /// (autodetected via `ls-remote --symref origin HEAD` when not given).
    ///
    /// The origin branch is tracked as `refs/remotes/origin/{main}` — never a
    /// local head, so the SSH front cannot expose it and the merge machinery
    /// (which follows `HEAD`) only ever sees `integration`.
    pub async fn create_linked_project(
        &self,
        owner: &str,
        project: &str,
        origin_url: &str,
        main_branch: Option<&str>,
        env: &OriginEnv,
    ) -> Result<String> {
        let repo = self.repo_path(owner, project);
        if repo.exists() {
            return Err(VcsError::RepoExists(format!("{owner}/{project}")));
        }
        tokio::fs::create_dir_all(&repo).await?;
        self.run(&repo, &["init", "--bare", "."]).await?;

        let result: Result<String> = async {
            self.run(&repo, &["remote", "add", "origin", origin_url])
                .await?;
            let main = match main_branch {
                Some(m) => m.to_string(),
                None => self.detect_origin_head(&repo, env).await?,
            };
            self.run(
                &repo,
                &[
                    "config",
                    "remote.origin.fetch",
                    &format!("+refs/heads/{main}:refs/remotes/origin/{main}"),
                ],
            )
            .await?;
            self.run(&repo, &["config", "chuggernaut.originMain", &main])
                .await?;
            self.run_origin(&repo, &["fetch", "origin"], env).await?;
            let sha = self
                .run(
                    &repo,
                    &[
                        "rev-parse",
                        "--verify",
                        &format!("refs/remotes/origin/{main}^{{commit}}"),
                    ],
                )
                .await?;
            self.run(&repo, &["update-ref", "refs/heads/integration", sha.trim()])
                .await?;
            self.run(&repo, &["symbolic-ref", "HEAD", "refs/heads/integration"])
                .await?;
            self.ensure_upload_filter(owner, project).await?;
            Ok(main)
        }
        .await;
        if result.is_err() {
            // A half-linked repo would block a retry on the RepoExists guard.
            let _ = tokio::fs::remove_dir_all(&repo).await;
        }
        result
    }

    /// `ls-remote --symref origin HEAD` → the origin's default branch name.
    async fn detect_origin_head(&self, repo: &Path, env: &OriginEnv) -> Result<String> {
        let out = self
            .run_origin(repo, &["ls-remote", "--symref", "origin", "HEAD"], env)
            .await?;
        // First line: "ref: refs/heads/{main}\tHEAD"
        out.lines()
            .find_map(|l| {
                l.strip_prefix("ref: ")
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|r| r.strip_prefix("refs/heads/"))
            })
            .map(str::to_string)
            .ok_or_else(|| VcsError::Parse {
                context: "ls-remote --symref",
                detail: out.lines().next().unwrap_or("").to_string(),
            })
    }

    /// The origin's default branch name, recorded at link time.
    pub async fn origin_main_branch(&self, owner: &str, project: &str) -> Result<String> {
        let repo = self.repo_path(owner, project);
        Ok(self
            .run(&repo, &["config", "--get", "chuggernaut.originMain"])
            .await?
            .trim()
            .to_string())
    }

    /// `remote.origin.url`, `None` when the project has no origin (classic).
    pub async fn origin_url(&self, owner: &str, project: &str) -> Result<Option<String>> {
        let repo = self.repo_path(owner, project);
        let out = self
            .exec(&repo, &["config", "--get", "remote.origin.url"], None)
            .await?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    }

    /// Fetch the origin's default branch; returns the new
    /// `refs/remotes/origin/{main}` commit.
    pub async fn fetch_origin(
        &self,
        owner: &str,
        project: &str,
        env: &OriginEnv,
    ) -> Result<String> {
        let repo = self.repo_path(owner, project);
        self.run_origin(&repo, &["fetch", "origin"], env).await?;
        self.origin_main_sha(owner, project).await
    }

    /// Resolve `refs/remotes/origin/{main}` as last fetched (no network).
    pub async fn origin_main_sha(&self, owner: &str, project: &str) -> Result<String> {
        let main = self.origin_main_branch(owner, project).await?;
        self.resolve_ref(owner, project, &format!("refs/remotes/origin/{main}"))
            .await
    }

    /// Push `local_ref` to the origin as `remote_ref`.
    pub async fn push_origin(
        &self,
        owner: &str,
        project: &str,
        local_ref: &str,
        remote_ref: &str,
        force: bool,
        env: &OriginEnv,
    ) -> Result<()> {
        let repo = self.repo_path(owner, project);
        let refspec = format!("{local_ref}:{remote_ref}");
        let mut args = vec!["push"];
        if force {
            args.push("--force");
        }
        args.extend(["origin", &refspec]);
        self.run_origin(&repo, &args, env).await?;
        Ok(())
    }

    /// Point an arbitrary fully-qualified ref (e.g. the `refs/chug/release-{n}`
    /// history pins) at a commit. [`Self::create_branch`]/[`Self::reset_branch`]
    /// only speak `refs/heads/`.
    pub async fn update_ref(
        &self,
        owner: &str,
        project: &str,
        full_ref: &str,
        sha: &str,
    ) -> Result<()> {
        let repo = self.repo_path(owner, project);
        self.run(&repo, &["update-ref", full_ref, sha]).await?;
        Ok(())
    }

    /// Advertise partial-clone support on the bare repo. This is the server
    /// half only: `container::bootstrap_cmd` does not yet pass
    /// `--filter=blob:none`, because the SSH front speaks git protocol v0 and
    /// v0 upload-pack refuses the promisor remote's follow-up fetch (see
    /// `chuggernaut/tests/ssh_front.rs`). It does make direct/`file://` partial
    /// clones work, and is the prerequisite for enabling the flag once the
    /// front carries protocol v2.
    ///
    /// Idempotent — safe to call on repos created before this landed.
    pub async fn ensure_upload_filter(&self, owner: &str, project: &str) -> Result<()> {
        let repo = self.repo_path(owner, project);
        self.run(&repo, &["config", "uploadpack.allowFilter", "true"])
            .await?;
        Ok(())
    }

    /// Read the default branch from the `HEAD` symref (spec §5.1) — there is no
    /// separate KV entry.
    pub async fn default_branch(&self, owner: &str, project: &str) -> Result<String> {
        let repo = self.repo_path(owner, project);
        let full = self.run(&repo, &["symbolic-ref", "HEAD"]).await?;
        Ok(full
            .trim()
            .strip_prefix("refs/heads/")
            .unwrap_or(full.trim())
            .to_string())
    }

    pub async fn resolve_ref(&self, owner: &str, project: &str, reference: &str) -> Result<String> {
        let repo = self.repo_path(owner, project);
        let sha = self
            .run(
                &repo,
                &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
            )
            .await?;
        Ok(sha.trim().to_string())
    }

    // ── Branch operations (spec §3.2, §5.1) ─────────────────────────────────

    pub async fn create_branch(
        &self,
        owner: &str,
        project: &str,
        branch: &str,
        sha: &str,
    ) -> Result<()> {
        let repo = self.repo_path(owner, project);
        self.run(&repo, &["update-ref", &format!("refs/heads/{branch}"), sha])
            .await?;
        Ok(())
    }

    /// Hard-reset in a bare repo is just moving the ref pointer.
    pub async fn reset_branch(
        &self,
        owner: &str,
        project: &str,
        branch: &str,
        sha: &str,
    ) -> Result<()> {
        self.create_branch(owner, project, branch, sha).await
    }

    pub async fn delete_branch(&self, owner: &str, project: &str, branch: &str) -> Result<()> {
        let repo = self.repo_path(owner, project);
        self.run(
            &repo,
            &["update-ref", "-d", &format!("refs/heads/{branch}")],
        )
        .await?;
        Ok(())
    }

    /// Replay `branch`'s commits (`old_base..branch`) onto `new_base` so the
    /// stack tests exactly what would merge (spec §3.2 pre-eval rebase). Runs in
    /// a throwaway detached worktree — no ref moves until the whole replay
    /// succeeds, so a conflict leaves the branch byte-for-byte as pushed.
    ///
    /// Each commit is cherry-picked individually to preserve its author
    /// (cherry-pick keeps it automatically) and committer (overridden per commit
    /// via `GIT_COMMITTER_*`). `--keep-redundant-commits` (git ≥ 2.39) keeps a
    /// commit whose change already landed on `new_base` as an empty commit
    /// rather than stopping — that is not a conflict. Only a real content
    /// conflict (`--diff-filter=U`) yields [`RebaseOutcome::Conflict`].
    pub async fn rebase_branch(
        &self,
        owner: &str,
        project: &str,
        branch: &str,
        old_base: &str,
        new_base: &str,
    ) -> Result<RebaseOutcome> {
        let repo = self.repo_path(owner, project);
        let list = self
            .run(
                &repo,
                &["rev-list", "--reverse", &format!("{old_base}..{branch}")],
            )
            .await?;
        let commits: Vec<String> = list
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        // No commits of our own: the branch collapses onto the new base.
        if commits.is_empty() {
            self.create_branch(owner, project, branch, new_base).await?;
            return Ok(RebaseOutcome::Rebased {
                new_head: new_base.to_string(),
            });
        }
        let branch_tip = self.resolve_ref(owner, project, branch).await?;

        let tmp = tempfile::tempdir()?;
        let wt = tmp.path().join("wt");
        let wt_str = wt.to_string_lossy().to_string();
        self.run(&repo, &["worktree", "add", "--detach", &wt_str, new_base])
            .await?;

        let result: Result<RebaseOutcome> = async {
            for sha in &commits {
                let meta = self
                    .run(&wt, &["show", "-s", "--format=%cn%x1f%ce%x1f%cI", sha])
                    .await?;
                let fields: Vec<&str> = meta.trim().splitn(3, '\x1f').collect();
                let [cn, ce, cd] = fields[..] else {
                    return Err(VcsError::Parse {
                        context: "rebase committer",
                        detail: meta.clone(),
                    });
                };
                let out = self
                    .exec_env(
                        &wt,
                        &["cherry-pick", "--keep-redundant-commits", sha],
                        None,
                        &[
                            ("GIT_COMMITTER_NAME", cn),
                            ("GIT_COMMITTER_EMAIL", ce),
                            ("GIT_COMMITTER_DATE", cd),
                        ],
                    )
                    .await?;
                if !out.status.success() {
                    // A real conflict leaves unmerged (`U`) paths; anything else
                    // (e.g. a merge commit) is a genuine git error, not a
                    // conflict to route through rework.
                    let conflicted = self
                        .run(&wt, &["diff", "--name-only", "--diff-filter=U"])
                        .await?;
                    let files: Vec<String> = conflicted
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect();
                    let _ = self.exec(&wt, &["cherry-pick", "--abort"], None).await;
                    if files.is_empty() {
                        return Err(VcsError::Git {
                            args: format!("cherry-pick {sha}"),
                            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                        });
                    }
                    return Ok(RebaseOutcome::Conflict { files });
                }
            }
            let new_head = self
                .run(&wt, &["rev-parse", "HEAD"])
                .await?
                .trim()
                .to_string();
            // CAS on the branch tip: the single-writer dispatcher makes a race
            // impossible, so a surprise is a logic bug, not a silent stomp.
            self.run(
                &repo,
                &[
                    "update-ref",
                    &format!("refs/heads/{branch}"),
                    &new_head,
                    &branch_tip,
                ],
            )
            .await?;
            Ok(RebaseOutcome::Rebased { new_head })
        }
        .await;

        // Always detach the worktree — a stale registration would block reuse.
        let _ = self
            .run(&repo, &["worktree", "remove", "--force", &wt_str])
            .await;
        result
    }

    // ── Content reads (spec §2.2, §3.2, §6.2) ───────────────────────────────

    /// Job type and prompt resolution at `base_ref`. None if the path does not
    /// exist at that ref.
    pub async fn read_file_at(
        &self,
        owner: &str,
        project: &str,
        reference: &str,
        path: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .blob(owner, project, reference, path)
            .await?
            .filter(|b| b.encoding == BlobEncoding::Utf8)
            .map(|b| b.content))
    }

    pub async fn blob(
        &self,
        owner: &str,
        project: &str,
        reference: &str,
        path: &str,
    ) -> Result<Option<BlobResponse>> {
        let repo = self.repo_path(owner, project);
        let out = self
            .exec(
                &repo,
                &["cat-file", "blob", &format!("{reference}:{path}")],
                None,
            )
            .await?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(match String::from_utf8(out.stdout.clone()) {
            Ok(content) => BlobResponse {
                content,
                encoding: BlobEncoding::Utf8,
            },
            Err(_) => BlobResponse {
                content: BASE64.encode(&out.stdout),
                encoding: BlobEncoding::Base64,
            },
        }))
    }

    pub async fn tree(
        &self,
        owner: &str,
        project: &str,
        reference: &str,
    ) -> Result<Vec<TreeEntry>> {
        let repo = self.repo_path(owner, project);
        let out = self
            .run(&repo, &["ls-tree", "-r", "-t", "-l", reference])
            .await?;
        out.lines()
            .map(|line| {
                // "<mode> <type> <oid> <size>\t<path>" — size is "-" for trees.
                let (meta, path) = line.split_once('\t').ok_or_else(|| VcsError::Parse {
                    context: "ls-tree",
                    detail: line.to_string(),
                })?;
                let fields: Vec<&str> = meta.split_whitespace().collect();
                let [_, r#type, _, size] = fields[..] else {
                    return Err(VcsError::Parse {
                        context: "ls-tree",
                        detail: line.to_string(),
                    });
                };
                Ok(TreeEntry {
                    path: path.to_string(),
                    r#type: r#type.to_string(),
                    size: size.parse().ok(),
                })
            })
            .collect()
    }

    pub async fn log(
        &self,
        owner: &str,
        project: &str,
        reference: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LogEntry>> {
        let repo = self.repo_path(owner, project);
        let reference = match reference {
            Some(r) => r.to_string(),
            None => self.default_branch(owner, project).await?,
        };
        let out = self
            .run(
                &repo,
                &[
                    "log",
                    "--format=%H%x1f%s%x1f%an%x1f%aI",
                    "-n",
                    &limit.to_string(),
                    &reference,
                ],
            )
            .await?;
        out.lines()
            .map(|line| {
                let [hash, message, author, ts] = line.splitn(4, '\x1f').collect::<Vec<_>>()[..]
                else {
                    return Err(VcsError::Parse {
                        context: "log",
                        detail: line.to_string(),
                    });
                };
                let ts = DateTime::parse_from_rfc3339(ts)
                    .map_err(|e| VcsError::Parse {
                        context: "log ts",
                        detail: e.to_string(),
                    })?
                    .with_timezone(&Utc);
                Ok(LogEntry {
                    hash: hash.to_string(),
                    message: message.to_string(),
                    author: author.to_string(),
                    ts,
                })
            })
            .collect()
    }

    // ── Squash-merge (spec §3.2 step 12, §5.1) ──────────────────────────────

    pub async fn has_commits_beyond(
        &self,
        owner: &str,
        project: &str,
        base_ref: &str,
        branch: &str,
    ) -> Result<bool> {
        Ok(self
            .count_commits_beyond(owner, project, base_ref, branch)
            .await?
            != 0)
    }

    /// `rev-list --count {base_ref}..{branch}` — ahead-by counts for the
    /// origin status surface.
    pub async fn count_commits_beyond(
        &self,
        owner: &str,
        project: &str,
        base_ref: &str,
        branch: &str,
    ) -> Result<u64> {
        let repo = self.repo_path(owner, project);
        let count = self
            .run(
                &repo,
                &["rev-list", "--count", &format!("{base_ref}..{branch}")],
            )
            .await?;
        count.trim().parse().map_err(|_| VcsError::Parse {
            context: "rev-list --count",
            detail: count.trim().to_string(),
        })
    }

    /// Build the squash commit for `job/{seq}` onto the current default head —
    /// `merge-tree --write-tree` + `commit-tree`, no ref updated. Shared by
    /// [`Self::squash_merge`] (advance immediately) and
    /// [`Self::create_squash_candidate`] (park on `merge-gate/{seq}` for the
    /// merge gate, spec §3.3).
    async fn build_squash_commit(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        base_ref: &str,
        job_type: &str,
        summary: Option<&str>,
    ) -> Result<SquashBuild> {
        let repo = self.repo_path(owner, project);
        let branch = format!("job/{seq}");
        if !self
            .has_commits_beyond(owner, project, base_ref, &branch)
            .await?
        {
            return Ok(SquashBuild::NoOp);
        }
        let default = self.default_branch(owner, project).await?;
        let old_head = self.resolve_ref(owner, project, &default).await?;

        let out = self
            .exec(
                &repo,
                &[
                    "merge-tree",
                    "--write-tree",
                    "--name-only",
                    &default,
                    &branch,
                ],
                None,
            )
            .await?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        match out.status.code() {
            Some(0) => {
                let tree = stdout.trim().to_string();
                // §3.2 step 12 guard: a clean merge-tree can still carry conflict
                // markers if the branch holds an UNRESOLVED WIP-rebase commit
                // (merge-base == new base → the branch's blob, markers and all,
                // is taken verbatim). A no-evaluator job would otherwise squash
                // them straight onto the default branch.
                let unresolved = self
                    .residual_conflict_markers(owner, project, base_ref, &branch, &tree)
                    .await?;
                if !unresolved.is_empty() {
                    return Ok(SquashBuild::UnresolvedMarkers { files: unresolved });
                }
                let subject = format!("job/{seq}: {job_type}");
                let mut args = vec!["commit-tree", &tree, "-p", &old_head, "-m", &subject];
                if let Some(s) = summary {
                    args.extend(["-m", s]);
                }
                let commit = self.run(&repo, &args).await?;
                Ok(SquashBuild::Commit {
                    commit: commit.trim().to_string(),
                    old_head,
                })
            }
            Some(1) => {
                // Line 1: toplevel tree OID; then conflicted file names until a
                // blank line separates the informational messages.
                let mut files: Vec<String> = stdout
                    .lines()
                    .skip(1)
                    .take_while(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect();
                files.dedup();
                Ok(SquashBuild::Conflict { files })
            }
            _ => Err(VcsError::Git {
                args: "merge-tree --write-tree".into(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            }),
        }
    }

    /// Rebase `job/{seq}` onto `new_base` by REUSING the tree `git merge-tree`
    /// already writes (spec §3.2 step 12 conflict / merge-gate rework). Unlike
    /// [`Self::rebase_branch`] this spins up no worktree and never fails on a
    /// conflict: it commits the 3-way-merged tree — conflict markers and all —
    /// as a single WIP commit parented on `new_base`, collapsing the job's prior
    /// commits into it. `merge-tree` computes the merge base automatically (the
    /// old base, the common ancestor of `new_base` and the branch), so the
    /// merged tree carries both sides' changes with `<<<<<<< / ======= />>>>>>>`
    /// markers in the conflicting hunks only. The agent resolves the markers in
    /// place and commits; the next squash's merge-base is then `new_base`
    /// (degenerate 3-way), so the resolved tree lands exactly.
    ///
    /// On conflict the WIP commit records the conflicted paths as
    /// `Conflicted-file:` trailers so [`Self::residual_conflict_markers`] can
    /// find and scan them later without re-deriving the merge.
    pub async fn rebase_onto_with_conflict(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        new_base: &str,
    ) -> Result<ConflictRebaseOutcome> {
        let repo = self.repo_path(owner, project);
        let branch = format!("job/{seq}");
        let branch_tip = self.resolve_ref(owner, project, &branch).await?;
        let out = self
            .exec(
                &repo,
                &[
                    "merge-tree",
                    "--write-tree",
                    "--name-only",
                    new_base,
                    &branch,
                ],
                None,
            )
            .await?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        // Line 0 is the merged tree OID in BOTH exit 0 and exit 1.
        let tree = stdout.lines().next().unwrap_or_default().trim().to_string();
        let short = &new_base[..new_base.len().min(7)];

        // Build the commit message, then commit the merged tree as one commit
        // parented on the new base and move the branch ref onto it (CAS on the
        // old tip: the single-writer dispatcher makes a race impossible, so a
        // surprise is a logic bug).
        let (msg, result) = match out.status.code() {
            Some(0) => (
                format!("job/{seq}: rebased onto {short}"),
                ConflictRebaseOutcome::Clean,
            ),
            Some(1) => {
                let mut files: Vec<String> = stdout
                    .lines()
                    .skip(1)
                    .take_while(|l| !l.trim().is_empty())
                    .map(str::to_string)
                    .collect();
                files.dedup();
                let mut msg = format!(
                    "WIP: unresolved merge conflict - resolve markers and commit (job/{seq})\n"
                );
                for f in &files {
                    msg.push_str(&format!("\nConflicted-file: {f}"));
                }
                (msg, ConflictRebaseOutcome::Conflict { files })
            }
            _ => {
                return Err(VcsError::Git {
                    args: "merge-tree --write-tree".into(),
                    stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                });
            }
        };

        let commit = self
            .run(&repo, &["commit-tree", &tree, "-p", new_base, "-m", &msg])
            .await?;
        self.run(
            &repo,
            &[
                "update-ref",
                &format!("refs/heads/{branch}"),
                commit.trim(),
                &branch_tip,
            ],
        )
        .await?;
        Ok(result)
    }

    /// Scan for residual conflict markers a WIP-rebase commit
    /// ([`Self::rebase_onto_with_conflict`]) left behind (spec §3.2 step 12
    /// guard). Returns the flagged files whose blob in `tree` STILL carries
    /// conflict markers — i.e. the agent never resolved them. Only files a WIP
    /// commit recorded (`Conflicted-file:` trailers in `base_ref..branch`) are
    /// scanned, so the cost is bounded to the originally-conflicted set. No WIP
    /// commit on the branch → nothing to scan → empty.
    async fn residual_conflict_markers(
        &self,
        owner: &str,
        project: &str,
        base_ref: &str,
        branch: &str,
        tree: &str,
    ) -> Result<Vec<String>> {
        let repo = self.repo_path(owner, project);
        // %B (full body) of each commit unique to the branch, record-separated.
        let bodies = self
            .run(
                &repo,
                &["log", "--format=%B%x1e", &format!("{base_ref}..{branch}")],
            )
            .await?;
        let mut flagged: Vec<String> = Vec::new();
        for body in bodies.split('\x1e') {
            let body = body.trim_start_matches(['\n', '\r']);
            if !body.starts_with("WIP: unresolved merge conflict") {
                continue;
            }
            for line in body.lines() {
                if let Some(f) = line.strip_prefix("Conflicted-file: ") {
                    flagged.push(f.trim().to_string());
                }
            }
        }
        if flagged.is_empty() {
            return Ok(Vec::new());
        }
        flagged.sort();
        flagged.dedup();
        let mut unresolved = Vec::new();
        for f in flagged {
            let out = self
                .exec(&repo, &["cat-file", "blob", &format!("{tree}:{f}")], None)
                .await?;
            if !out.status.success() {
                continue; // path gone from the merged tree — nothing to land
            }
            let content = String::from_utf8_lossy(&out.stdout);
            if content.contains("<<<<<<< ") && content.contains(">>>>>>> ") {
                unresolved.push(f);
            }
        }
        Ok(unresolved)
    }

    /// Squash-merge `job/{seq}` into the default branch. A squash-merge is by
    /// definition one new commit with one parent — built by
    /// [`Self::build_squash_commit`], then `update-ref` (CAS on the old head).
    /// No working tree involved.
    pub async fn squash_merge(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        base_ref: &str,
        job_type: &str,
        summary: Option<&str>,
    ) -> Result<MergeOutcome> {
        match self
            .build_squash_commit(owner, project, seq, base_ref, job_type, summary)
            .await?
        {
            SquashBuild::NoOp => Ok(MergeOutcome::NoOp),
            SquashBuild::Conflict { files } => Ok(MergeOutcome::Conflict { files }),
            SquashBuild::UnresolvedMarkers { files } => {
                Ok(MergeOutcome::UnresolvedMarkers { files })
            }
            SquashBuild::Commit { commit, old_head } => {
                self.advance_default(owner, project, &commit, &old_head)
                    .await?;
                Ok(MergeOutcome::Merged { commit })
            }
        }
    }

    /// Build the candidate squash commit and park it on `merge-gate/{seq}`
    /// without touching the default branch (spec §3.3 Merge Gate). On gate
    /// pass, [`Self::advance_default`] promotes the same commit — the
    /// candidate IS the merge, never re-merged.
    pub async fn create_squash_candidate(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        base_ref: &str,
        job_type: &str,
        summary: Option<&str>,
    ) -> Result<MergeOutcome> {
        match self
            .build_squash_commit(owner, project, seq, base_ref, job_type, summary)
            .await?
        {
            SquashBuild::NoOp => Ok(MergeOutcome::NoOp),
            SquashBuild::Conflict { files } => Ok(MergeOutcome::Conflict { files }),
            SquashBuild::UnresolvedMarkers { files } => {
                Ok(MergeOutcome::UnresolvedMarkers { files })
            }
            SquashBuild::Commit { commit, .. } => {
                self.create_branch(owner, project, &format!("merge-gate/{seq}"), &commit)
                    .await?;
                Ok(MergeOutcome::Merged { commit })
            }
        }
    }

    /// Advance the default branch to `commit`, CAS on `expected_old_head`. The
    /// single-writer dispatcher makes a race impossible; the CAS turns a logic
    /// bug into an error instead of a silent overwrite.
    pub async fn advance_default(
        &self,
        owner: &str,
        project: &str,
        commit: &str,
        expected_old_head: &str,
    ) -> Result<()> {
        let repo = self.repo_path(owner, project);
        let default = self.default_branch(owner, project).await?;
        self.run(
            &repo,
            &[
                "update-ref",
                &format!("refs/heads/{default}"),
                commit,
                expected_old_head,
            ],
        )
        .await?;
        Ok(())
    }

    /// Human-readable conflict context block (spec §4.3): conflicting files,
    /// commits landed on the default branch since the old base, diff summary.
    pub async fn conflict_context(
        &self,
        owner: &str,
        project: &str,
        old_base_ref: &str,
        new_base_ref: &str,
        conflicting_files: &[String],
    ) -> Result<String> {
        let repo = self.repo_path(owner, project);
        let default = self.default_branch(owner, project).await?;
        let range = format!("{old_base_ref}..{new_base_ref}");
        let log = self.run(&repo, &["log", "--oneline", &range]).await?;
        let stat = self.run(&repo, &["diff", "--stat", &range]).await?;
        let short_base = &old_base_ref[..old_base_ref.len().min(7)];

        let mut ctx = String::from("Conflicting files:\n");
        for f in conflicting_files {
            ctx.push_str(&format!("  {f}\n"));
        }
        ctx.push_str(&format!(
            "\nChanges on {default} since base commit {short_base}:\n"
        ));
        for l in log.lines() {
            ctx.push_str(&format!("  {l}\n"));
        }
        ctx.push('\n');
        for l in stat.lines() {
            ctx.push_str(&format!("  {}\n", l.trim_start()));
        }
        Ok(ctx)
    }

    // ── Diff API (spec §6.2) ────────────────────────────────────────────────

    /// Diff behavior by job state (spec §6.2). `Done` recovers the squash-merge
    /// commit via an anchored `--grep` on the default branch.
    pub async fn diff_for_job(&self, job: &Job) -> Result<DiffResponse> {
        let Some((owner, project)) = job.project.split_once('/') else {
            return Err(VcsError::Parse {
                context: "job.project",
                detail: job.project.clone(),
            });
        };
        match job.state {
            // Draft/Stalled/Batched are pre-work: no branch of their own to
            // diff (§1.2, §2.1) — a batch member's changes live on the batch
            // branch, and the batch itself diffs under Work/Evaluation/etc.
            JobState::Draft
            | JobState::Frozen
            | JobState::Batched
            | JobState::Blocked
            | JobState::Ready
            | JobState::Stalled
            | JobState::Revoked => Ok(DiffResponse::default()),
            JobState::Work | JobState::Evaluation | JobState::WrapUp | JobState::Escalated => {
                let Some(base_ref) = job.base_ref.as_deref() else {
                    return Ok(DiffResponse::default());
                };
                self.diff_range(owner, project, base_ref, &job.branch).await
            }
            JobState::Done => {
                let repo = self.repo_path(owner, project);
                let default = self.default_branch(owner, project).await?;
                let grep = format!("^job/{}: ", job.id);
                let sha = self
                    .run(
                        &repo,
                        &["log", "-1", "--format=%H", "--grep", &grep, &default],
                    )
                    .await?;
                let sha = sha.trim();
                if sha.is_empty() {
                    return Ok(DiffResponse::default());
                }
                self.diff_range(owner, project, &format!("{sha}^"), sha)
                    .await
            }
        }
    }

    async fn diff_range(
        &self,
        owner: &str,
        project: &str,
        from: &str,
        to: &str,
    ) -> Result<DiffResponse> {
        let repo = self.repo_path(owner, project);
        let range = format!("{from}..{to}");
        let numstat = self.run(&repo, &["diff", "--numstat", &range]).await?;
        let files = numstat
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\t');
                let (a, d, path) = (parts.next()?, parts.next()?, parts.next()?);
                Some(FileStat {
                    path: path.to_string(),
                    // "-" for binary files
                    additions: a.parse().unwrap_or(0),
                    deletions: d.parse().unwrap_or(0),
                })
            })
            .collect();
        let diff = self.run(&repo, &["diff", &range]).await?;
        Ok(DiffResponse { files, diff })
    }

    /// Public two-ref diff `{from}..{to}` — the delta a re-review focuses on
    /// (spec §3.3, job #155). Same shape as [`diff_for_job`], but between two
    /// arbitrary refs the caller supplies (e.g. the last-reviewed tip and the
    /// branch HEAD).
    pub async fn diff_between(
        &self,
        owner: &str,
        project: &str,
        from: &str,
        to: &str,
    ) -> Result<DiffResponse> {
        self.diff_range(owner, project, from, to).await
    }

    /// `git merge-base --is-ancestor {ancestor} {descendant}` — true when
    /// `ancestor` is reachable from `descendant`. Used by re-review to tell a
    /// linear delta (`last_reviewed_tip..HEAD` is meaningful) from a rebase
    /// (the branch was replayed onto a moved base, so the delta is bogus and the
    /// full diff is shown instead — job #155). A missing ref is treated as "not
    /// an ancestor" (fall back to the full diff) rather than an error.
    pub async fn is_ancestor(
        &self,
        owner: &str,
        project: &str,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool> {
        let repo = self.repo_path(owner, project);
        let out = self
            .exec(
                &repo,
                &["merge-base", "--is-ancestor", ancestor, descendant],
                None,
            )
            .await?;
        // Exit 0 = ancestor, exit 1 = not an ancestor; anything else (e.g. a bad
        // rev) is also treated as "not an ancestor" so re-review degrades to the
        // full diff rather than failing the whole eval launch.
        Ok(out.status.success())
    }

    // ── git plumbing ────────────────────────────────────────────────────────

    async fn run(&self, repo: &Path, args: &[&str]) -> Result<String> {
        let out = self.exec(repo, args, None).await?;
        expect_success(out, &args.join(" "))
    }

    /// Origin-facing command: per-call `GIT_SSH_COMMAND` and a hard timeout —
    /// a hung remote fails the operation instead of wedging the single-writer
    /// dispatcher (the process is killed on drop via `kill_on_drop`).
    async fn run_origin(&self, repo: &Path, args: &[&str], env: &OriginEnv) -> Result<String> {
        let fut = self.exec_with(repo, args, None, env.ssh_command.as_deref(), &[]);
        match tokio::time::timeout(ORIGIN_TIMEOUT, fut).await {
            Ok(out) => expect_success(out?, &args.join(" ")),
            Err(_) => Err(VcsError::Timeout {
                args: args.join(" "),
                seconds: ORIGIN_TIMEOUT.as_secs(),
            }),
        }
    }

    async fn run_stdin(&self, repo: &Path, args: &[&str], stdin: &[u8]) -> Result<String> {
        let out = self.exec(repo, args, Some(stdin)).await?;
        expect_success(out, &args.join(" "))
    }

    async fn exec(&self, repo: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Output> {
        self.exec_with(repo, args, stdin, None, &[]).await
    }

    /// `exec` with per-call env overrides layered over [`GIT_IDENTITY`] — used
    /// by the rebase replay to preserve each commit's committer identity.
    async fn exec_env(
        &self,
        repo: &Path,
        args: &[&str],
        stdin: Option<&[u8]>,
        env: &[(&str, &str)],
    ) -> Result<Output> {
        self.exec_with(repo, args, stdin, None, env).await
    }

    async fn exec_with(
        &self,
        repo: &Path,
        args: &[&str],
        stdin: Option<&[u8]>,
        ssh_command: Option<&str>,
        extra_env: &[(&str, &str)],
    ) -> Result<Output> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(repo).args(args);
        // Deterministic identity; ignore host-level git config (gpg signing,
        // init.defaultBranch, etc.).
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        for (k, v) in GIT_IDENTITY {
            cmd.env(k, v);
        }
        // Layered last so callers can override the default identity per commit.
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        if let Some(ssh) = ssh_command {
            cmd.env("GIT_SSH_COMMAND", ssh);
        }
        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;
        if let Some(bytes) = stdin {
            use tokio::io::AsyncWriteExt;
            let mut pipe = child.stdin.take().expect("stdin piped");
            pipe.write_all(bytes).await?;
        } else {
            drop(child.stdin.take());
        }
        Ok(child.wait_with_output().await?)
    }
}

fn expect_success(out: Output, args: &str) -> Result<String> {
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(VcsError::Git {
            args: args.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        })
    }
}

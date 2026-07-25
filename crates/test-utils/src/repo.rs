//! Temp bare-repo harness (testing.md tier 2).
//!
//! Repos are built programmatically per test — no checked-in fixture repos.
//! [`TempRepo`] goes through `RepoManager::create_project`, so the init path is
//! covered for free; [`WorkClone`] simulates an agent container: a real
//! `git clone` of the bare repo that commits and pushes to a job branch.

use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tokio::process::Command;
use vcs::RepoManager;

pub struct TempRepo {
    _root: TempDir,
    pub manager: RepoManager,
    pub owner: String,
    pub project: String,
}

impl TempRepo {
    /// Create a `{owner}/{project}` bare repo with default branch `main` in a
    /// fresh temp dir.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::expect_used)]
    pub async fn create(owner: &str, project: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let manager = RepoManager::new(root.path());
        manager
            .create_project(owner, project, "main")
            .await
            .expect("create_project");
        Self {
            _root: root,
            manager,
            owner: owner.into(),
            project: project.into(),
        }
    }

    pub fn bare_path(&self) -> PathBuf {
        self.manager.repo_path(&self.owner, &self.project)
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::expect_used)]
    pub async fn head(&self) -> String {
        self.manager
            .resolve_ref(&self.owner, &self.project, "main")
            .await
            .expect("resolve main")
    }

    /// Create `job/{seq}` at the given sha (as the dispatcher does on Ready→Work).
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::expect_used)]
    pub async fn create_job_branch(&self, seq: u64, sha: &str) {
        self.manager
            .create_branch(&self.owner, &self.project, &format!("job/{seq}"), sha)
            .await
            .expect("create branch");
    }

    /// Clone the bare repo checked out at `branch` — the same path a real agent
    /// container takes via the workspace bootstrap.
    pub async fn clone_branch(&self, branch: &str) -> WorkClone {
        clone_branch_from(&self.bare_path(), branch).await
    }
}

/// Standalone form of [`TempRepo::clone_branch`] for contexts that only hold a
/// bare-repo path — e.g. a `FakeProvider` run hook standing in for an agent
/// container committing to its job branch.
// TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
#[allow(clippy::expect_used, clippy::unwrap_used)]
pub async fn clone_branch_from(bare_path: &Path, branch: &str) -> WorkClone {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("workspace");
    git(
        dir.path(),
        &[
            "clone",
            "--branch",
            branch,
            bare_path.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    )
    .await;
    WorkClone {
        _dir: dir,
        path: target,
    }
}

/// A working clone standing in for an agent container's `/workspace`.
pub struct WorkClone {
    _dir: TempDir,
    path: PathBuf,
}

impl WorkClone {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write (or overwrite) a file and commit it.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::expect_used)]
    pub async fn commit_file(&self, rel_path: &str, contents: &[u8], message: &str) {
        let file = self.path.join(rel_path);
        if let Some(parent) = file.parent() {
            tokio::fs::create_dir_all(parent).await.expect("mkdir");
        }
        tokio::fs::write(&file, contents).await.expect("write");
        git(&self.path, &["add", "."]).await;
        git(&self.path, &["commit", "-m", message]).await;
    }

    /// Push HEAD to the given branch on the bare repo.
    pub async fn push(&self, branch: &str) {
        git(
            &self.path,
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        )
        .await;
    }
}

/// A local bare repo standing in for a GitHub origin (linked-origin tests):
/// linked via its `file://` URL, mutated through clones to simulate external
/// pushes and PR merges (merge-commit and squash variants).
pub struct FakeOrigin {
    _dir: TempDir,
    pub path: PathBuf,
}

impl FakeOrigin {
    /// Bare repo with default branch `main` and one initial commit.
    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::expect_used)]
    pub async fn create() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("upstream.git");
        tokio::fs::create_dir_all(&path).await.expect("mkdir");
        git(&path, &["init", "--bare", "--initial-branch", "main", "."]).await;
        let origin = Self { _dir: dir, path };
        // Seed via a clone: an empty bare repo has no refs to clone from, so
        // build the first commit with plumbing.
        let empty = git_out(&origin.path, &["mktree"], Some(b"")).await;
        let commit = git_out(
            &origin.path,
            &["commit-tree", empty.trim(), "-m", "upstream: initial"],
            None,
        )
        .await;
        git(
            &origin.path,
            &["update-ref", "refs/heads/main", commit.trim()],
        )
        .await;
        origin
    }

    /// `file://` URL for linking.
    pub fn url(&self) -> String {
        format!("file://{}", self.path.display())
    }

    pub async fn main_sha(&self) -> String {
        git_out(&self.path, &["rev-parse", "refs/heads/main"], None)
            .await
            .trim()
            .to_string()
    }

    /// Clone at `main`, commit a file, push — an external commit landing on
    /// the origin's default branch.
    pub async fn commit_to_main(&self, rel_path: &str, contents: &[u8], message: &str) {
        let clone = clone_branch_from(&self.path, "main").await;
        clone.commit_file(rel_path, contents, message).await;
        clone.push("main").await;
    }

    /// Simulate merging a release branch into main the way GitHub does.
    /// `squash: true` = "squash and merge" (one new commit, release history
    /// discarded); `false` = "create a merge commit".
    pub async fn merge_branch_to_main(&self, branch: &str, squash: bool) {
        let clone = clone_branch_from(&self.path, "main").await;
        if squash {
            git(
                clone.path(),
                &["merge", "--squash", &format!("origin/{branch}")],
            )
            .await;
            git(
                clone.path(),
                &["commit", "-m", &format!("squash-merge {branch}")],
            )
            .await;
        } else {
            git(
                clone.path(),
                &[
                    "merge",
                    "--no-ff",
                    "-m",
                    &format!("merge {branch}"),
                    &format!("origin/{branch}"),
                ],
            )
            .await;
        }
        clone.push("main").await;
    }

    // TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
    #[allow(clippy::expect_used)]
    pub async fn branch_exists(&self, branch: &str) -> bool {
        Command::new("git")
            .current_dir(&self.path)
            .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
            .output()
            .await
            .expect("spawn git")
            .status
            .success()
    }
}

async fn git(cwd: &Path, args: &[&str]) {
    git_out(cwd, args, None).await;
}

// TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
#[allow(clippy::expect_used)]
async fn git_out(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fake-agent")
        .env("GIT_AUTHOR_EMAIL", "agent@test.local")
        .env("GIT_COMMITTER_NAME", "fake-agent")
        .env("GIT_COMMITTER_EMAIL", "agent@test.local");
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn git");
    if let Some(bytes) = stdin {
        use tokio::io::AsyncWriteExt;
        let mut pipe = child.stdin.take().expect("stdin piped");
        pipe.write_all(bytes).await.expect("write stdin");
        drop(pipe);
    }
    let out = child.wait_with_output().await.expect("git output");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

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

    pub async fn head(&self) -> String {
        self.manager
            .resolve_ref(&self.owner, &self.project, "main")
            .await
            .expect("resolve main")
    }

    /// Create `job/{seq}` at the given sha (as the dispatcher does on Ready→Work).
    pub async fn create_job_branch(&self, seq: u64, sha: &str) {
        self.manager
            .create_branch(&self.owner, &self.project, &format!("job/{seq}"), sha)
            .await
            .expect("create branch");
    }

    /// Clone the bare repo checked out at `branch` — the same path a real agent
    /// container takes via the workspace bootstrap.
    pub async fn clone_branch(&self, branch: &str) -> WorkClone {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("workspace");
        git(
            dir.path(),
            &[
                "clone",
                "--branch",
                branch,
                self.bare_path().to_str().unwrap(),
                target.to_str().unwrap(),
            ],
        )
        .await;
        WorkClone {
            _dir: dir,
            path: target,
        }
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

async fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fake-agent")
        .env("GIT_AUTHOR_EMAIL", "agent@test.local")
        .env("GIT_COMMITTER_NAME", "fake-agent")
        .env("GIT_COMMITTER_EMAIL", "agent@test.local")
        .output()
        .await
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

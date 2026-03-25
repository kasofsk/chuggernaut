use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, info};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git command failed: {0}")]
    CommandFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, GitError>;

/// Clone a repo into a work directory. Returns the path to the cloned repo.
pub fn clone_repo(forgejo_url: &str, repo: &str, token: &str, workdir: &Path) -> Result<PathBuf> {
    let repo_dir = workdir.join(repo.replace('/', "_"));
    if repo_dir.exists() {
        debug!(?repo_dir, "repo already cloned, pulling");
        run_git(&repo_dir, &["pull", "--ff-only"])?;
        return Ok(repo_dir);
    }

    let clone_url = format!(
        "{}://chuggernaut-worker:{token}@{}/{repo}.git",
        if forgejo_url.starts_with("https") {
            "https"
        } else {
            "http"
        },
        forgejo_url
            .trim_start_matches("http://")
            .trim_start_matches("https://"),
    );

    info!(repo, "cloning");
    std::fs::create_dir_all(&repo_dir)?;
    let output = Command::new("git")
        .args(["clone", &clone_url, repo_dir.to_str().unwrap()])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::CommandFailed(format!("git clone: {stderr}")));
    }

    // Configure a credential helper that returns our token, overriding any
    // global helper (e.g. one set by actions/checkout) that might interfere.
    let helper_script = format!(
        "!f() {{ echo \"username=chuggernaut-worker\"; echo \"password={token}\"; }}; f"
    );
    run_git(&repo_dir, &[
        "config", "credential.helper", &helper_script,
    ])?;

    Ok(repo_dir)
}

/// Create and checkout a work branch, or checkout if it already exists on remote.
pub fn checkout_branch(repo_dir: &Path, branch: &str) -> Result<()> {
    // Fetch to see if the branch exists remotely
    run_git(repo_dir, &["fetch", "origin"])?;

    // Check if branch exists on remote
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["ls-remote", "--heads", "origin", branch])
        .output()?;

    let exists_remote = !output.stdout.is_empty();

    if exists_remote {
        debug!(branch, "checking out existing remote branch");
        // Checkout and track the remote branch
        let result = run_git(repo_dir, &["checkout", "-B", branch, &format!("origin/{branch}")]);
        if result.is_err() {
            // Might already be on this branch
            run_git(repo_dir, &["checkout", branch])?;
            run_git(repo_dir, &["pull", "--ff-only"])?;
        }
    } else {
        debug!(branch, "creating new branch");
        run_git(repo_dir, &["checkout", "-b", branch])?;
    }

    Ok(())
}

/// Commit all changes with the given message.
pub fn commit_all(repo_dir: &Path, message: &str) -> Result<()> {
    run_git(repo_dir, &["add", "-A"])?;

    // Check if there's anything to commit
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["status", "--porcelain"])
        .output()?;

    if output.stdout.is_empty() {
        debug!("nothing to commit");
        return Ok(());
    }

    run_git(repo_dir, &["commit", "-m", message])?;
    Ok(())
}

/// Push the current branch to origin.
pub fn push(repo_dir: &Path, branch: &str) -> Result<()> {
    run_git(repo_dir, &["push", "-u", "origin", branch])?;
    Ok(())
}

/// Get the default branch name from origin.
pub fn default_branch(repo_dir: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()?;

    if output.status.success() {
        let full_ref = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // refs/remotes/origin/main -> main
        Ok(full_ref
            .rsplit('/')
            .next()
            .unwrap_or("main")
            .to_string())
    } else {
        Ok("main".to_string())
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").current_dir(dir).args(args).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::CommandFailed(format!(
            "git {}: {stderr}",
            args.join(" ")
        )));
    }
    Ok(())
}

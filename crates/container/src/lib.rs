//! Container execution backends (spec §3.1, §4.1).
//!
//! The dispatcher launches all work and eval containers through [`ContainerBackend`].
//! No CI or workflow engine sits in between — Docker socket in dev, the Kubernetes
//! Jobs API in production.

pub mod docker;
pub mod k8s;

use async_trait::async_trait;
use std::collections::HashMap;
use thiserror::Error;

pub type ContainerId = String;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("container not found: {0}")]
    NotFound(ContainerId),
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    #[error("launch failed: {0}")]
    Launch(String),
    #[error("backend error: {0}")]
    Other(String),
}

#[async_trait]
pub trait ContainerBackend: Send + Sync {
    /// Launch a container; returns an opaque ID used for subsequent calls.
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError>;
    /// Block until the container exits; returns its exit code.
    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError>;
    /// Kill a running container (SIGKILL).
    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError>;
    /// Query current container status; None if container is not found.
    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError>;
    /// Copy a single file out of the container filesystem; None if not found.
    async fn copy_file(
        &self,
        id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError>;
    /// Captured stdout and stderr. Read after exit; this does not follow.
    /// Call before [`remove`](ContainerBackend::remove): the container's logs
    /// and filesystem vanish with it.
    ///
    /// Order is preserved *within* each stream, but not across them: Docker
    /// orders frames by timestamp, so writes to stdout and stderr in the same
    /// millisecond can come back either way round (measured, not assumed).
    async fn logs(&self, id: &ContainerId) -> Result<Vec<u8>, BackendError>;
    /// Remove an exited container, reclaiming its writable overlay layer (spec
    /// §3.1: the container lifecycle ends in removal). `force=false` — callers
    /// remove only after `wait`/`logs`/`copy_file` have captured everything the
    /// dispatcher needs. Idempotent: an already-removed container is `Ok(())`.
    ///
    /// This is the leak fix — a cargo-building job leaves 5–10 GB per task in
    /// its overlay, so leaving exited containers around fills the host disk.
    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError>;
    /// IDs of managed containers that have exited, across every node. Used by
    /// the dispatcher's startup sweep (spec §3.6) to reclaim overlays orphaned
    /// by crashes or restarts, which never went through the exit path.
    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError>;
}

#[derive(Debug, Clone)]
pub struct ContainerLaunchConfig {
    pub image: String,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    /// Written into the created container before start (MCP binaries, prompt,
    /// event batch).
    pub files: Vec<InjectedFile>,
    /// Fractional CPUs.
    pub cpu_limit: Option<f64>,
    /// e.g. "4Gi".
    pub memory_limit: Option<String>,
}

/// Injected via the backend's file API (Docker put-archive / k8s equivalent)
/// after create, before start. No host bind-mounts — works identically on
/// remote fleet nodes (spec §3.1).
#[derive(Debug, Clone)]
pub struct InjectedFile {
    pub container_path: String,
    pub contents: Vec<u8>,
    /// e.g. 0o755 for the MCP binaries.
    pub mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerStatus {
    Running,
    Exited { exit_code: i32 },
}

/// Wrap a container CMD with the standard workspace bootstrap (spec §4.1):
/// clone the job branch to /workspace, cd, exec the original command.
/// Images must provide `git` and an SSH client; they never clone themselves.
///
/// `--single-branch` skips every other in-flight `job/*` and `merge-gate/*`
/// ref; the container only ever works on its own branch, and all merging is
/// server-side. This is the big one — without it each task also drags in every
/// concurrent job's unmerged work.
///
/// `--filter=blob:none` skips historical blobs while keeping the commit graph,
/// so `git log`/`git blame` still work as agent context.
///
/// Both depend on server-side setup, and both fail *silently-ish* without it:
/// - `uploadpack.allowFilter` on the bare repo (`RepoManager::create_project`),
///   else the filter is ignored and the full history ships anyway.
/// - git protocol **v2** through the SSH front (`AcceptEnv GIT_PROTOCOL` in
///   sshd_config). On v0, upload-pack refuses the promisor remote's follow-up
///   fetch for unadvertised blobs: the clone "succeeds" and checkout leaves an
///   empty workspace. git supplies the client half itself.
pub fn bootstrap_cmd(original: &[String]) -> Vec<String> {
    let joined = original
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "git clone --single-branch --filter=blob:none --branch \"$JOB_BRANCH\" \"$REPO_URL\" /workspace && cd /workspace && exec {joined}"
        ),
    ]
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./=:".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_wraps_and_quotes() {
        let cmd = bootstrap_cmd(&["claude".into(), "-p".into(), "do the thing".into()]);
        assert_eq!(cmd[0], "sh");
        assert_eq!(cmd[1], "-c");
        assert!(cmd[2].starts_with("git clone "));
        assert!(cmd[2].ends_with("exec claude -p 'do the thing'"));
    }

    /// The clone must stay narrow: every task in a job re-clones, so the flags
    /// are the whole cost story.
    #[test]
    fn bootstrap_clone_is_narrow() {
        let cmd = bootstrap_cmd(&["true".into()]);
        assert!(cmd[2].contains("--single-branch"), "{}", cmd[2]);
        assert!(cmd[2].contains("--filter=blob:none"), "{}", cmd[2]);
        // The branch and URL stay shell-quoted env refs, cloned to /workspace.
        assert!(
            cmd[2].contains("--branch \"$JOB_BRANCH\" \"$REPO_URL\" /workspace"),
            "{}",
            cmd[2]
        );
    }
}

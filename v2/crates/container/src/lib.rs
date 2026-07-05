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
            "git clone --branch \"$JOB_BRANCH\" \"$REPO_URL\" /workspace && cd /workspace && exec {joined}"
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
        assert!(cmd[2].starts_with("git clone --branch"));
        assert!(cmd[2].ends_with("exec claude -p 'do the thing'"));
    }
}

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
    /// Placement found no free slot on any eligible node (spec §3.1). Distinct
    /// from [`Launch`](BackendError::Launch) because it is transient — a slot
    /// frees when a running container exits — so the dispatcher queues the
    /// launch and retries rather than failing the task (§3.5). The message is
    /// carried verbatim (e.g. `no free slots on any node`).
    #[error("{0}")]
    NoCapacity(String),
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
    /// Bounded, non-following read of a container's captured stdout+stderr from
    /// byte cursor `since` — usable while the container is still **running**,
    /// unlike [`logs`](ContainerBackend::logs) (which is documented as an
    /// after-exit read). `follow: false` throughout, so it returns promptly
    /// with whatever has been captured so far and never hangs. Routes to the
    /// owning node like every other op, so it works for containers on a remote
    /// worker.
    ///
    /// Byte offsets are stable — container logs are append-only — so a poller
    /// advances monotonically by passing back the returned `offset`. The chunk
    /// is capped at [`MAX_LOG_TAIL`] (`offset` is where the returned bytes end,
    /// so a caller still advances across the cap); a `since` at/past the end
    /// yields empty `data` and the unchanged length. The same offsets address
    /// the harvested `stdout.log` after exit, so a live-then-artifact poller
    /// never loses the tail when the container is removed.
    async fn logs_tail(&self, id: &ContainerId, since: u64) -> Result<LogTail, BackendError>;
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
    /// Running `chuggernaut.managed` containers across every node, each tagged
    /// with the `(project, job, task)` it was launched for. Used by the §3.6
    /// fleet sweep to reap containers no live task owns — a crash-restart can
    /// fail a task while its container keeps running and holding a fleet slot.
    /// Best-effort per node: a node that cannot be listed is logged and skipped
    /// rather than failing the whole sweep, so one unreachable node never blocks
    /// the others' reap.
    async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError>;
    /// Live per-node fleet status for the platform config snapshot (spec §3.1):
    /// health and last-reported build version per node. Empty by default —
    /// backends without fleet-health tracking (e.g. the test fake) report
    /// nothing; Docker fills health, the worker fleet fills both. The dispatcher
    /// republishes this each scan so the UI sees live fleet state and deploy
    /// drift.
    fn fleet_status(&self) -> Vec<NodeStatus> {
        Vec::new()
    }
}

/// One fleet node's live health and build version for the platform config
/// snapshot (spec §3.1). `version` is `None` for docker-endpoint nodes (they
/// carry no chuggernaut version) and for workers that have not answered yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
}

/// A running managed container tagged with the task it serves (spec §3.6 fleet
/// sweep). The identity is read back from the labels stamped at launch; a field
/// is `None` for a container launched before those labels existed, which the
/// sweep treats as unmatchable — exactly the orphan it must reap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningContainer {
    /// Full `{node}/{docker_id}` id — the handle for `kill`/`remove`.
    pub id: ContainerId,
    /// `owner/project` slug, from the `chuggernaut.project` label.
    pub project: Option<String>,
    /// Job sequence, from the `chuggernaut.job` label.
    pub job: Option<u64>,
    /// Task id, from the `chuggernaut.task` label.
    pub task: Option<u64>,
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
    /// Optional placement pin (spec §3.1): the fleet node name this container
    /// must launch on. `None` = the default most-free placement. A pinned node
    /// that is full or unknown fails the launch rather than spilling over.
    pub node: Option<String>,
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
    /// Static-artifact name (e.g. `"channel"`) when this file is provisioned
    /// node-locally on worker nodes (spec §3.1): a worker-proxying backend
    /// sends the name instead of `contents` and the worker substitutes its
    /// local copy. Docker/k8s backends ignore it and inject `contents`.
    pub artifact: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerStatus {
    Running,
    Exited { exit_code: i32 },
}

/// Chunk cap for [`ContainerBackend::logs_tail`]. Bounds each poll's reply so a
/// worker-proxied tail fits NATS's 1MB `max_payload` even after base64 + JSON
/// overhead, and keeps a single request cheap. A busy build keeps producing
/// output, so the poller just advances across several capped chunks.
pub const MAX_LOG_TAIL: usize = 512 * 1024;

/// A cursor-paged slice of a container's captured logs (see
/// [`ContainerBackend::logs_tail`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTail {
    /// Where the returned bytes end in the full log — the next `since` cursor.
    pub offset: u64,
    /// The captured bytes from the requested `since` up to `offset`.
    pub data: Vec<u8>,
}

impl LogTail {
    /// Slice a full captured-log buffer into the response for cursor `since`,
    /// capping the returned chunk at [`MAX_LOG_TAIL`]. `offset` is where the
    /// returned bytes end (not necessarily the buffer end), so a caller
    /// advances monotonically even when the tail is capped across polls.
    pub fn slice(full: &[u8], since: u64) -> Self {
        let start = (since as usize).min(full.len());
        let end = start.saturating_add(MAX_LOG_TAIL).min(full.len());
        LogTail {
            offset: end as u64,
            data: full[start..end].to_vec(),
        }
    }
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

    /// The cursor slice underpinning live output: monotonic across polls, empty
    /// at/past the end, and capped so a worker-proxied reply stays bounded.
    #[test]
    fn log_tail_slice_is_monotonic_and_capped() {
        let full = b"line-0\nline-1\nline-2\n";
        // From the start: the whole buffer, cursor at its end.
        let t = LogTail::slice(full, 0);
        assert_eq!(t.offset, full.len() as u64);
        assert_eq!(t.data, full);
        // From a mid cursor: only the remainder, cursor unchanged at the end.
        let t = LogTail::slice(full, 7);
        assert_eq!(t.data, b"line-1\nline-2\n");
        assert_eq!(t.offset, full.len() as u64);
        // At the end: empty, offset holds — a caught-up poll makes no progress.
        let t = LogTail::slice(full, full.len() as u64);
        assert!(t.data.is_empty());
        assert_eq!(t.offset, full.len() as u64);
        // Past the end (a truncated/rotated log): clamped, never panics.
        let t = LogTail::slice(full, 9_999);
        assert!(t.data.is_empty());
        assert_eq!(t.offset, full.len() as u64);

        // Capped: a chunk larger than MAX_LOG_TAIL returns exactly the cap and
        // an offset that still advances the caller past it.
        let big = vec![b'x'; MAX_LOG_TAIL + 4096];
        let t = LogTail::slice(&big, 0);
        assert_eq!(t.data.len(), MAX_LOG_TAIL);
        assert_eq!(t.offset, MAX_LOG_TAIL as u64);
        let t2 = LogTail::slice(&big, t.offset);
        assert_eq!(t2.data.len(), 4096);
        assert_eq!(t2.offset, big.len() as u64);
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

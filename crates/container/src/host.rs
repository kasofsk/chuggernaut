//! Host-process backend — design #309 P0, the prototype (spec §3.1).
//!
//! A task is a **process group rooted in a per-task directory** under the
//! node's host root, holding `meta.json` (pid, pgid, process start time,
//! identity labels), `output.log` and `exit_code`. That directory *is* the
//! container: it is what makes `inspect`, the two listings and `remove`
//! implementable at all.
//!
//! P0 takes #309 §2's option **(iii)** — one host task per node, `slots: 1` —
//! so [`HOST_WORKSPACE`] is unambiguous without rebasing the path out of
//! `bootstrap_cmd`, `dispatcher::launch_queue` and `agent::transcript_path`.
//! Rebasing (option i) is the durable answer and is not P0's; option (ii),
//! per-task mount namespaces, is Linux-only and macOS is the point. The
//! exclusion is **enforced here**, not assumed: a launch arriving while another
//! task is live is refused as `NoCapacity`.
//!
//! The declared `image` is ignored. #309 P0 calls that "deliberately a lie that
//! must never leave the prototype node", so every launch says so in the log.

use crate::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
    InjectedFile, LogTail, MAX_LOG_TAIL, RunningContainer,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Where a host task's clone lands, because that is the path `bootstrap_cmd`
/// hardcodes (`git clone … /workspace && cd /workspace`) and the path
/// `agent::transcript_path`'s measured `-workspace` slug is derived from.
/// Unambiguous only under the one-task-per-node rule this backend enforces.
pub const HOST_WORKSPACE: &str = "/workspace";

/// The node's host root when `WORKER_HOST_ROOT` is unset — worker-writable node
/// state beside the other `/var/lib/chuggernaut` leaves, never a nix store path.
pub const HOST_ROOT_DEFAULT: &str = "/var/lib/chuggernaut/host-tasks";

/// Merged stdout+stderr, one fd, append-only. One fd means true cross-stream
/// ordering, which the trait's Docker-shaped caveat on
/// [`logs`](ContainerBackend::logs) explicitly does not promise.
const OUTPUT_LOG: &str = "output.log";

const META_JSON: &str = "meta.json";
const EXIT_CODE: &str = "exit_code";
const EXIT_CODE_TMP: &str = "exit_code.tmp";

/// Prefix for a task tree detached by [`ContainerBackend::remove`] and not yet
/// deleted. The leading dot is what makes [`is_task_id`] reject it, so a
/// half-removed tree is never mistaken for a task.
const REMOVING_PREFIX: &str = ".removing-";

/// Names the task currently holding [`HOST_WORKSPACE`], so a `remove` arriving
/// after the next launch has already claimed it deletes only its own task
/// directory. Survives a daemon restart because it is a file, not a field.
const WORKSPACE_OWNER: &str = "workspace-owner";

/// Where the task's own wrapper writes its exit status, handed over as
/// environment so no path is ever quoted into a shell command.
const EXIT_TMP_VAR: &str = "CHUG_HOST_EXIT_TMP";
const EXIT_VAR: &str = "CHUG_HOST_EXIT";

/// How long a killed process group gets on SIGTERM before the escalation, the
/// same shape `signal_refresh_build` uses for the refresh script's build.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

/// Poll interval for [`ContainerBackend::wait`], which is trait-completeness
/// only — §3.1 polls dispatcher-side and the daemon never calls it.
const WAIT_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// One host task's durable identity, written once at launch. The start time is
/// what makes a recycled pid readable as *gone* across a daemon restart (#309
/// §2(b)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMeta {
    pub pid: i32,
    /// Process **group**, so `kill` reaches the whole tree the way a container
    /// runtime reaches a whole cgroup.
    pub pgid: i32,
    /// Field 22 of `/proc/<pid>/stat` on Linux, `ps -o lstart=` elsewhere;
    /// `None` when the process was already gone at launch.
    pub start_time: Option<String>,
    pub project: Option<String>,
    pub job: Option<u64>,
    pub task: Option<u64>,
    /// Absolute paths this launch materialized outside the task directory, so
    /// `remove` reclaims exactly what it wrote and nothing beside it.
    pub files: Vec<String>,
}

/// Host-process execution on one node (design #309 P0). Serves every launch
/// routed to the node — P0 has no per-request mode selector, which is why the
/// declared `image` is ignored rather than honored.
pub struct HostBackend {
    node: String,
    root: PathBuf,
    workspace: PathBuf,
    /// Serializes the whole of [`ContainerBackend::launch`], so the
    /// one-task-per-node check and the task directory it publishes cannot race
    /// a second concurrent launch on the daemon's op semaphore.
    launching: tokio::sync::Mutex<()>,
    /// Task ids this daemon spawned and has not yet written an `exit_code` for.
    /// Authoritative while the daemon lives: it holds the `Child`, so no pid
    /// recycling is possible and no post-exit window can read as gone.
    live: Arc<Mutex<HashSet<String>>>,
    counter: AtomicU64,
}

impl HostBackend {
    /// The node's host backend, rooted at `root` and cloning into
    /// [`HOST_WORKSPACE`]. The root is created if absent — it is worker-owned
    /// node state, not an operator precondition.
    pub fn new(node: impl Into<String>, root: impl Into<PathBuf>) -> Result<Self, BackendError> {
        Self::with_workspace(node, root, HOST_WORKSPACE)
    }

    /// [`Self::new`] with the workspace path named explicitly. A real node only
    /// ever passes [`HOST_WORKSPACE`], because that is the literal
    /// `bootstrap_cmd` emits; the parameter exists so the round-trip test does
    /// not touch the machine's real `/workspace`.
    pub fn with_workspace(
        node: impl Into<String>,
        root: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
    ) -> Result<Self, BackendError> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| BackendError::Unavailable(format!("host root {}: {e}", root.display())))?;
        sweep_detached(&root);
        Ok(Self {
            node: node.into(),
            root,
            workspace: workspace.into(),
            launching: tokio::sync::Mutex::new(()),
            live: Arc::new(Mutex::new(HashSet::new())),
            counter: AtomicU64::new(0),
        })
    }

    /// The task directory a `{node}/{task_id}` id names. A foreign node, an
    /// empty segment or any path traversal is [`BackendError::NotFound`] rather
    /// than a read outside the root.
    fn task_dir(&self, id: &ContainerId) -> Result<PathBuf, BackendError> {
        let (node, task) = id
            .split_once('/')
            .ok_or_else(|| BackendError::NotFound(id.clone()))?;
        if node != self.node || !is_task_id(task) {
            return Err(BackendError::NotFound(id.clone()));
        }
        Ok(self.root.join(task))
    }

    fn id_of(&self, task: &str) -> ContainerId {
        format!("{}/{task}", self.node)
    }

    /// Every task directory under the root, oldest name first. A root that
    /// cannot be read is an error, never an empty fleet — job/181's shape.
    fn task_ids(&self) -> Result<Vec<String>, BackendError> {
        let mut ids = Vec::new();
        let entries = std::fs::read_dir(&self.root)
            .map_err(|e| BackendError::Unavailable(format!("host root: {e}")))?;
        for entry in entries {
            let entry = entry.map_err(|e| BackendError::Unavailable(format!("host root: {e}")))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if is_task_id(&name) && entry.path().is_dir() {
                ids.push(name);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// One task's status, or `None` when its directory is gone. Reads the
    /// written `exit_code` first, then this daemon's own live set, and only
    /// then falls back to the pid-identity rule that covers a daemon restart.
    fn status(&self, task: &str) -> Option<ContainerStatus> {
        let dir = self.root.join(task);
        if !dir.is_dir() {
            return None;
        }
        if let Some(code) = read_exit_code(&dir) {
            return Some(ContainerStatus::Exited { exit_code: code });
        }
        if self.live.lock().is_ok_and(|l| l.contains(task)) {
            return Some(ContainerStatus::Running);
        }
        let meta = read_meta(&dir);
        let observed = meta.as_ref().and_then(|m| process_start_time(m.pid));
        Some(status_after_restart(meta.as_ref(), observed.as_deref()))
    }

    /// The task currently holding [`Self::workspace`], if any.
    fn workspace_owner(&self) -> Option<String> {
        std::fs::read_to_string(self.root.join(WORKSPACE_OWNER))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Whether this node may take the launch, and the two things P0 is lying
    /// about if it does. The exclusion is #309 §2 option (iii) **enforced**:
    /// `NoCapacity` is transient, so the dispatcher queues and retries (§3.5)
    /// rather than spending the job's retry budget.
    async fn admit(&self, config: &ContainerLaunchConfig) -> Result<(), BackendError> {
        if let Some(held) = self.list_managed_running().await?.first() {
            return Err(BackendError::NoCapacity(format!(
                "host node {} runs one task at a time (#309 P0 takes §2 option iii, which is \
                 what makes {} unambiguous); {} holds it",
                self.node,
                self.workspace.display(),
                held.id
            )));
        }
        tracing::warn!(
            node = %self.node,
            image = %config.image,
            "host launch IGNORES the declared image (#309 P0: deliberately a lie that must never \
             leave the prototype node)"
        );
        if config.cpu_limit.is_some() || config.memory_limit.is_some() {
            tracing::warn!(
                node = %self.node,
                cpu = ?config.cpu_limit,
                memory = ?config.memory_limit,
                "host node cannot enforce resources.cpu/memory (#309 §7) — task_timeout still \
                 bounds this task in time"
            );
        }
        Ok(())
    }

    /// Reclaim a leftover workspace before a launch claims it. Only ever called
    /// with no managed task live, which is what makes deleting a path outside
    /// the root defensible at all.
    fn reclaim_workspace(&self) -> Result<(), BackendError> {
        if !self.workspace.exists() {
            return Ok(());
        }
        tracing::warn!(
            workspace = %self.workspace.display(),
            "host node: reclaiming an orphaned workspace before launch — no managed task holds it"
        );
        std::fs::remove_dir_all(&self.workspace).map_err(|e| {
            BackendError::Launch(format!(
                "reclaiming {}: {e} — a host node cannot launch into a workspace it cannot clear",
                self.workspace.display()
            ))
        })
    }
}

/// Task ids are minted by [`HostBackend`] alone and address a directory, so the
/// charset is checked rather than trusted.
fn is_task_id(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The pid-identity rule (#309 §2(b)): a live pid is this task's only while the
/// recorded start time still matches. A recycled pid, an unrecorded start time
/// and a vanished process all read as **gone**, because reporting a dead task
/// as running is what §3.6 hears as "re-attach" and hangs until `task_timeout`.
fn status_after_restart(meta: Option<&TaskMeta>, observed: Option<&str>) -> ContainerStatus {
    let matched = match (meta.and_then(|m| m.start_time.as_deref()), observed) {
        (Some(recorded), Some(seen)) => recorded == seen,
        _ => false,
    };
    if matched {
        ContainerStatus::Running
    } else {
        ContainerStatus::Exited { exit_code: -1 }
    }
}

/// Field 22 (`starttime`) out of a `/proc/<pid>/stat` body. Parsed after the
/// last `)` because the comm field is parenthesized and may itself hold spaces
/// and parens.
fn proc_stat_start_time(stat: &str) -> Option<&str> {
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    rest.split_whitespace().nth(19)
}

/// A live process's start time as the platform reports it, `None` when the pid
/// is gone. Linux reads `/proc/<pid>/stat`; elsewhere `ps -o lstart=`, which is
/// what #309 §2(b) names for macOS.
fn process_start_time(pid: i32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        proc_stat_start_time(&stat).map(str::to_string)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let seen = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!seen.is_empty()).then_some(seen)
    }
}

/// Wrap the launch command so the **task itself** records its exit status. The
/// daemon's own supervisor is only a backstop: a task must survive the daemon
/// being swapped under it (spec §3.1 drain guarantee), and a `wait` this
/// process performed would not.
fn supervised_cmd(cmd: &[String]) -> Result<Vec<String>, BackendError> {
    if cmd.is_empty() {
        return Err(BackendError::Launch(
            "host launch has no command to run".into(),
        ));
    }
    let mut wrapped = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "\"$@\"; s=$?; printf %s \"$s\" > \"${EXIT_TMP_VAR}\" && mv \"${EXIT_TMP_VAR}\" \
             \"${EXIT_VAR}\"; exit $s"
        ),
        "sh".to_string(),
    ];
    wrapped.extend(cmd.iter().cloned());
    Ok(wrapped)
}

fn read_meta(dir: &Path) -> Option<TaskMeta> {
    let raw = std::fs::read(dir.join(META_JSON)).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn read_exit_code(dir: &Path) -> Option<i32> {
    std::fs::read_to_string(dir.join(EXIT_CODE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Write the exit status the way the task's own wrapper does — temp file then
/// rename — so a reader never observes a half-written code.
fn write_exit_code(dir: &Path, code: i32) -> std::io::Result<()> {
    let tmp = dir.join(EXIT_CODE_TMP);
    std::fs::write(&tmp, code.to_string())?;
    std::fs::rename(tmp, dir.join(EXIT_CODE))
}

/// Materialize one injected file at its literal absolute path. P0 option (iii)
/// makes the node's own filesystem the container's, so `/chuggernaut/prompt.md`
/// means exactly that — and [`ContainerBackend::remove`] reclaims each path this
/// returns.
fn materialize(file: &InjectedFile) -> Result<String, BackendError> {
    let path = PathBuf::from(&file.container_path);
    if !path.is_absolute() {
        return Err(BackendError::Launch(format!(
            "injected file {:?} is not an absolute path",
            file.container_path
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| BackendError::Launch(format!("{}: {e}", parent.display())))?;
    }
    std::fs::write(&path, &file.contents)
        .map_err(|e| BackendError::Launch(format!("{}: {e}", path.display())))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(file.mode))
        .map_err(|e| BackendError::Launch(format!("{}: {e}", path.display())))?;
    Ok(file.container_path.clone())
}

/// Materialize the injected files, open the merged log and spawn the task into
/// its own process group. The group is the unit `kill` signals, the way a
/// container runtime kills a whole cgroup (#309 §2).
fn spawn_task(
    dir: &Path,
    config: &ContainerLaunchConfig,
) -> Result<(std::process::Child, TaskMeta), BackendError> {
    let mut files = Vec::with_capacity(config.files.len());
    for file in &config.files {
        files.push(materialize(file)?);
    }
    let log = std::fs::File::create(dir.join(OUTPUT_LOG))
        .map_err(|e| BackendError::Launch(format!("{OUTPUT_LOG}: {e}")))?;
    let errors = log
        .try_clone()
        .map_err(|e| BackendError::Launch(format!("{OUTPUT_LOG}: {e}")))?;

    let wrapped = supervised_cmd(&config.cmd)?;
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(&wrapped[0]);
    command
        .args(&wrapped[1..])
        .current_dir(dir)
        .envs(&config.env)
        .env(EXIT_TMP_VAR, dir.join(EXIT_CODE_TMP))
        .env(EXIT_VAR, dir.join(EXIT_CODE))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(errors))
        .process_group(0);
    let child = command
        .spawn()
        .map_err(|e| BackendError::Launch(format!("spawning host task: {e}")))?;

    let pid = i32::try_from(child.id()).unwrap_or(-1);
    let meta = TaskMeta {
        pid,
        pgid: pid,
        start_time: process_start_time(pid),
        project: config.env.get("JOB_PROJECT").cloned(),
        job: config.env.get("JOB_ID").and_then(|v| v.parse().ok()),
        task: config.env.get("CHUG_TASK_ID").and_then(|v| v.parse().ok()),
        files,
    };
    Ok((child, meta))
}

/// Where a task tree goes for the instant between detaching it and deleting
/// it. Stamped, so a leftover from an earlier crashed remove is never the
/// rename's target.
fn detached_path(dir: &Path) -> PathBuf {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    dir.with_file_name(format!("{REMOVING_PREFIX}{name}-{stamp:x}"))
}

/// Detach the task tree with an atomic rename, then delete it. Deleting in
/// place races the task's own reaper still writing `exit_code` — every writer
/// addresses the old path, so the rename is what makes the delete race-free.
fn detach_and_remove(dir: &Path) -> std::io::Result<()> {
    match std::fs::rename(dir, detached_path(dir)) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return std::fs::remove_dir_all(dir),
    }
    sweep_detached_in(dir.parent().unwrap_or(dir))
}

/// Delete every tree a `remove` detached but did not finish deleting. A crash
/// in that window is the one way a host node leaks a task tree, and nothing
/// else on the node reclaims it (#309 §2(c)).
fn sweep_detached(root: &Path) {
    if let Err(e) = sweep_detached_in(root) {
        tracing::error!(root = %root.display(), "host root: a detached task tree is unreclaimable: {e}");
    }
}

fn sweep_detached_in(root: &Path) -> std::io::Result<()> {
    let mut failure = None;
    for entry in std::fs::read_dir(root)?.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(REMOVING_PREFIX)
        {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(entry.path()) {
            failure.get_or_insert(e);
        }
    }
    failure.map_or(Ok(()), Err)
}

/// A reclaim's failure, or `None` when it succeeded or the path was already
/// gone. Idempotence is the trait's documented contract for `remove`.
fn reclaim_failure(result: std::io::Result<()>, what: &str) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!("{what}: {e}")),
    }
}

/// Signal a process group, negating the pgid exactly as
/// `daemon::kill_process_group` does. A zero or negative pgid is refused rather
/// than sent — `kill(0, …)` would reach the daemon's own group.
fn signal_group(pgid: i32, sig: i32) {
    if pgid <= 0 {
        tracing::error!(pgid, "host kill: refusing to signal a non-positive group");
        return;
    }
    // SAFETY: `kill` is async-signal-safe and takes no pointers; its only failure mode is ESRCH — the group already exited, which is a no-op for a signal whose point is that the group stops.
    let rc = unsafe { libc::kill(-pgid, sig) };
    if rc != 0 {
        tracing::info!(pgid, sig, "host kill: process group already gone");
    }
}

#[async_trait]
impl ContainerBackend for HostBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        let _serialized = self.launching.lock().await;
        self.admit(&config).await?;
        self.reclaim_workspace()?;

        let task = format!(
            "host-{:x}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let dir = self.root.join(&task);
        std::fs::create_dir_all(&dir)
            .map_err(|e| BackendError::Launch(format!("{}: {e}", dir.display())))?;

        let (child, meta) = spawn_task(&dir, &config)?;
        let encoded = serde_json::to_vec(&meta)
            .map_err(|e| BackendError::Launch(format!("{META_JSON}: {e}")))?;
        std::fs::write(dir.join(META_JSON), encoded)
            .map_err(|e| BackendError::Launch(format!("{META_JSON}: {e}")))?;
        std::fs::write(self.root.join(WORKSPACE_OWNER), &task)
            .map_err(|e| BackendError::Launch(format!("{WORKSPACE_OWNER}: {e}")))?;
        if let Ok(mut live) = self.live.lock() {
            live.insert(task.clone());
        }
        spawn_reaper(child, dir, task.clone(), self.live.clone());
        Ok(self.id_of(&task))
    }

    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError> {
        loop {
            match self.inspect(id).await? {
                Some(ContainerStatus::Exited { exit_code }) => return Ok(exit_code),
                Some(ContainerStatus::Running) => tokio::time::sleep(WAIT_POLL).await,
                None => return Err(BackendError::NotFound(id.clone())),
            }
        }
    }

    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError> {
        let dir = self.task_dir(id)?;
        if !dir.is_dir() {
            return Err(BackendError::NotFound(id.clone()));
        }
        let Some(meta) = read_meta(&dir) else {
            return Ok(());
        };
        if read_exit_code(&dir).is_some() {
            return Ok(());
        }
        tracing::warn!(node = %self.node, id = %id, pgid = meta.pgid, "host kill: SIGTERM to the process group");
        signal_group(meta.pgid, libc::SIGTERM);
        let pgid = meta.pgid;
        let dir = dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(KILL_GRACE).await;
            if read_exit_code(&dir).is_none() {
                tracing::warn!(pgid, "host kill: group ignored SIGTERM — SIGKILL");
                signal_group(pgid, libc::SIGKILL);
            }
        });
        Ok(())
    }

    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError> {
        let dir = self.task_dir(id)?;
        let Some(task) = dir.file_name().map(|n| n.to_string_lossy().to_string()) else {
            return Ok(None);
        };
        Ok(self.status(&task))
    }

    async fn copy_file(
        &self,
        id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        self.task_dir(id)?;
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(BackendError::Other(format!("{path}: {e}"))),
        }
    }

    async fn logs(&self, id: &ContainerId) -> Result<Vec<u8>, BackendError> {
        let dir = self.task_dir(id)?;
        match std::fs::read(dir.join(OUTPUT_LOG)) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(BackendError::Other(format!("{OUTPUT_LOG}: {e}"))),
        }
    }

    /// A seek into an append-only file, so the trait's "byte offsets are
    /// stable" is definitional here rather than a Docker property being relied
    /// on, and no read is larger than [`MAX_LOG_TAIL`].
    async fn logs_tail(&self, id: &ContainerId, since: u64) -> Result<LogTail, BackendError> {
        let dir = self.task_dir(id)?;
        let mut file = match std::fs::File::open(dir.join(OUTPUT_LOG)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LogTail {
                    offset: 0,
                    data: Vec::new(),
                });
            }
            Err(e) => return Err(BackendError::Other(format!("{OUTPUT_LOG}: {e}"))),
        };
        let len = file
            .metadata()
            .map_err(|e| BackendError::Other(format!("{OUTPUT_LOG}: {e}")))?
            .len();
        let start = since.min(len);
        let want = usize::try_from(len - start)
            .unwrap_or(MAX_LOG_TAIL)
            .min(MAX_LOG_TAIL);
        file.seek(SeekFrom::Start(start))
            .map_err(|e| BackendError::Other(format!("{OUTPUT_LOG}: {e}")))?;
        let mut data = vec![0u8; want];
        let read = read_fully(&mut file, &mut data)
            .map_err(|e| BackendError::Other(format!("{OUTPUT_LOG}: {e}")))?;
        data.truncate(read);
        Ok(LogTail {
            offset: start + read as u64,
            data,
        })
    }

    /// Delete the task directory, every path the launch wrote outside it, and
    /// the workspace when this task still owns it. Nothing else on the node
    /// reclaims a 5–10 GB `target/`, so a failure is an error **and** an
    /// `error!` rather than a silent leak (#309 §2(c)).
    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        let dir = self.task_dir(id)?;
        if !dir.exists() {
            return Ok(());
        }
        let meta = read_meta(&dir);
        let mut failed: Vec<String> = Vec::new();
        for path in meta.iter().flat_map(|m| m.files.iter()) {
            failed.extend(reclaim_failure(std::fs::remove_file(path), path));
        }
        let task = dir.file_name().map(|n| n.to_string_lossy().to_string());
        if task.is_some() && task == self.workspace_owner() {
            let workspace = self.workspace.display().to_string();
            failed.extend(reclaim_failure(
                std::fs::remove_dir_all(&self.workspace),
                &workspace,
            ));
            let _ = std::fs::remove_file(self.root.join(WORKSPACE_OWNER));
        }
        failed.extend(reclaim_failure(
            detach_and_remove(&dir),
            &dir.display().to_string(),
        ));
        if failed.is_empty() {
            return Ok(());
        }
        let detail = failed.join("; ");
        tracing::error!(
            node = %self.node,
            id = %id,
            "host remove LEAKED disk nothing else reclaims: {detail}"
        );
        Err(BackendError::Other(format!(
            "host remove {id} left state behind: {detail}"
        )))
    }

    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
        Ok(self
            .task_ids()?
            .into_iter()
            .filter(|t| matches!(self.status(t), Some(ContainerStatus::Exited { .. })))
            .map(|t| self.id_of(&t))
            .collect())
    }

    async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError> {
        let mut out = Vec::new();
        for task in self.task_ids()? {
            if !matches!(self.status(&task), Some(ContainerStatus::Running)) {
                continue;
            }
            let meta = read_meta(&self.root.join(&task));
            out.push(RunningContainer {
                id: self.id_of(&task),
                project: meta.as_ref().and_then(|m| m.project.clone()),
                job: meta.as_ref().and_then(|m| m.job),
                task: meta.as_ref().and_then(|m| m.task),
            });
        }
        Ok(out)
    }
}

/// Read until the buffer is full or the file ends, returning how many bytes
/// landed. `read_exact` would fail on a log the task truncated between the
/// metadata read and the seek.
fn read_fully(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Reap the spawned child and, only if its own wrapper never got to it, write
/// the exit code. Leaving the live set **last** is what closes the window in
/// which a just-exited task would otherwise read as gone.
fn spawn_reaper(
    mut child: std::process::Child,
    dir: PathBuf,
    task: String,
    live: Arc<Mutex<HashSet<String>>>,
) {
    tokio::task::spawn_blocking(move || {
        let status = child.wait();
        if dir.is_dir() && read_exit_code(&dir).is_none() {
            let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
            if let Err(e) = write_exit_code(&dir, code) {
                tracing::error!(task = %task, "host task exit code unwritable: {e}");
            }
        }
        if let Ok(mut live) = live.lock() {
            live.remove(&task);
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The pid-identity rule (#309 §2(b)) — the assertion the design asks for by
    /// name. Same pid, DIFFERENT start time is the recycled-pid case a bare pgid
    /// check gets wrong, and it must read as gone.
    #[test]
    fn a_recycled_pid_reads_as_gone() {
        let meta = TaskMeta {
            pid: 4242,
            pgid: 4242,
            start_time: Some("918273".into()),
            project: None,
            job: None,
            task: None,
            files: Vec::new(),
        };
        assert_eq!(
            status_after_restart(Some(&meta), Some("918273")),
            ContainerStatus::Running,
            "the same process is still this task's"
        );
        for observed in [Some("918274"), Some(""), None] {
            assert_eq!(
                status_after_restart(Some(&meta), observed),
                ContainerStatus::Exited { exit_code: -1 },
                "a pid that is not ours must never read as running: {observed:?}"
            );
        }
        let unrecorded = TaskMeta {
            start_time: None,
            ..meta.clone()
        };
        assert_eq!(
            status_after_restart(Some(&unrecorded), Some("918273")),
            ContainerStatus::Exited { exit_code: -1 },
            "an unrecorded start time cannot confirm identity"
        );
        assert_eq!(
            status_after_restart(None, Some("918273")),
            ContainerStatus::Exited { exit_code: -1 },
            "no meta at all is gone, never running"
        );
    }

    /// Field 22 out of a real `/proc/<pid>/stat` shape, including the comm
    /// fields that break every naive `split_whitespace().nth(21)`.
    #[test]
    fn proc_stat_start_time_survives_a_hostile_comm() {
        let fields: Vec<String> = (3..=52).map(|f| f.to_string()).collect();
        let stat = format!("1234 (bash) {}\n", fields.join(" "));
        assert_eq!(proc_stat_start_time(&stat), Some("22"));

        let hostile = format!("1234 (my proc (x) y) {}\n", fields.join(" "));
        assert_eq!(
            proc_stat_start_time(&hostile),
            Some("22"),
            "the comm field may hold spaces and parens"
        );
        assert_eq!(proc_stat_start_time("garbage"), None);
        assert_eq!(proc_stat_start_time("1234 (sh) S 1 2"), None);
    }

    /// The task's own wrapper records the exit status, so a daemon swap under a
    /// running task (spec §3.1 drain guarantee) does not lose it.
    #[test]
    fn the_task_records_its_own_exit_status() {
        let cmd = vec!["sh".to_string(), "-c".to_string(), "exit 7".to_string()];
        let wrapped = supervised_cmd(&cmd).unwrap();
        assert_eq!(wrapped[0], "sh");
        assert_eq!(wrapped[3], "sh", "$0 is consumed before the real argv");
        assert_eq!(&wrapped[4..], &cmd[..]);
        assert!(wrapped[2].contains(EXIT_VAR), "{}", wrapped[2]);
        assert!(wrapped[2].contains("exit $s"), "{}", wrapped[2]);
        assert!(supervised_cmd(&[]).is_err(), "an empty command is refused");

        let dir = std::env::temp_dir().join(format!("chug-host-wrap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = std::process::Command::new(&wrapped[0])
            .args(&wrapped[1..])
            .env(EXIT_TMP_VAR, dir.join(EXIT_CODE_TMP))
            .env(EXIT_VAR, dir.join(EXIT_CODE))
            .status()
            .unwrap();
        assert_eq!(out.code(), Some(7), "the wrapper preserves the exit status");
        assert_eq!(read_exit_code(&dir), Some(7));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `remove` detaches the task tree before deleting it, so the reaper still
    /// writing this task's `exit_code` cannot repopulate a directory
    /// `remove_dir_all` is walking — which fails with `ENOTEMPTY`.
    #[test]
    fn a_removed_task_tree_is_detached_before_it_is_deleted() {
        let root = std::env::temp_dir().join(format!("chug-host-detach-{}", std::process::id()));
        let dir = root.join("host-1-0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OUTPUT_LOG), b"x").unwrap();

        let detached = detached_path(&dir);
        let name = detached.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with(REMOVING_PREFIX));
        assert!(
            !is_task_id(&name),
            "a detached tree must never read as a task: {name}"
        );
        assert_eq!(
            detached.parent(),
            dir.parent(),
            "it stays on the same mount"
        );

        detach_and_remove(&dir).unwrap();
        assert!(!dir.exists());
        assert!(
            write_exit_code(&dir, 0).is_err(),
            "a late reaper write must not resurrect the tree"
        );
        assert!(!dir.exists());
        assert!(detach_and_remove(&dir).is_ok(), "remove stays idempotent");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A remove that died between the rename and the delete leaks a tree
    /// nothing else on the node reclaims (#309 §2(c)), so the next boot does.
    #[test]
    fn a_crashed_remove_is_reclaimed_at_the_next_boot() {
        let root = std::env::temp_dir().join(format!("chug-host-sweep-{}", std::process::id()));
        let leftover = root.join(format!("{REMOVING_PREFIX}host-1-0-beef"));
        std::fs::create_dir_all(leftover.join("nested")).unwrap();
        std::fs::write(leftover.join("nested").join("big"), b"target/").unwrap();
        std::fs::create_dir_all(root.join("host-2-0")).unwrap();

        let backend = HostBackend::with_workspace("w1", &root, root.join("ws")).unwrap();
        assert!(!leftover.exists(), "a detached tree is reclaimed at boot");
        assert_eq!(
            backend.task_ids().unwrap(),
            vec!["host-2-0".to_string()],
            "and a live task directory is not"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// An id addresses a directory, so a foreign node and every traversal shape
    /// are refused rather than read from somewhere outside the root.
    #[test]
    fn ids_outside_the_root_are_not_found() {
        let root = std::env::temp_dir().join(format!("chug-host-ids-{}", std::process::id()));
        let backend = HostBackend::with_workspace("w1", &root, root.join("ws")).unwrap();
        assert_eq!(
            backend.task_dir(&"w1/host-1-0".to_string()).unwrap(),
            root.join("host-1-0")
        );
        for bad in [
            "w2/host-1-0",
            "host-1-0",
            "w1/",
            "w1/..",
            "w1/a/b",
            "w1/../x",
        ] {
            assert!(
                backend.task_dir(&bad.to_string()).is_err(),
                "must refuse {bad:?}"
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}

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
//! A launch **declaring an image is refused** (#309 §1, P1): the image's absence
//! is what selects this backend, so one that carries an image was misrouted and
//! saying so is the only answer that cannot run a container task against the
//! machine's own toolchain.
//!
//! Each task is launched into its own **supervision unit** ([`Supervision`],
//! design #440 D3) rather than the daemon's, so the restart that swaps the
//! daemon leaves in-flight work running (spec §3.1). Which systemd manager that
//! unit lives in is the daemon's privilege ([`ScopeManager`]), not a preference:
//! polkit refuses an unprivileged process a system scope. A node that cannot
//! create one must not advertise `host` at all — [`probe_supervision`] measures
//! it, in the environment a launch actually gets, and [`host_refusal`] says why.
//! The `systemd-run` invocation is a **client** of the manager's bus and is
//! given the two variables that locate it (`BUS_VARS`); the task inside the
//! scope sheds them, so its own environment stays exactly what #309 §10 says.

use crate::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
    InjectedFile, LogTail, MAX_LOG_TAIL, RunningContainer,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
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

/// Prefix every host task id carries, so a node serving both runtimes can tell
/// which of its two backends owns an id without asking either (see
/// [`names_host_task`]). A docker id is hex and can never collide with it.
pub const TASK_PREFIX: &str = "host-";

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

/// The only two variables a host task takes from the daemon, and it takes them
/// **by name** (design #440 D8). `PATH` because a host node's toolchain is
/// machine configuration rather than an image's (#309 §9) and the clone needs
/// `git` and `ssh`; `HOME` because docker gives every container one and the
/// per-user state of `git`, `ssh` and the agent harness hangs off it.
const INHERITED: [&str; 2] = [PATH_VAR, HOME_VAR];

const PATH_VAR: &str = "PATH";
const HOME_VAR: &str = "HOME";

/// The only names `sd-bus` reads to find a `systemd --user` manager's bus, and
/// therefore what the `systemd-run` **client** needs to reach one. They address
/// that invocation, never the task inside the scope (#309 §10).
const BUS_VARS: [&str; 2] = [RUNTIME_DIR_VAR, BUS_ADDRESS_VAR];

const RUNTIME_DIR_VAR: &str = "XDG_RUNTIME_DIR";
const BUS_ADDRESS_VAR: &str = "DBUS_SESSION_BUS_ADDRESS";

/// How `sd-bus` opens every failure to reach a bus at all, which is the one
/// failure operator provisioning fixes. The phrase and not the errno it carries:
/// `No such file or directory` is also how a missing `/bin/sh` fails to exec.
const BUS_UNREACHABLE: &str = "failed to connect";

/// `PATH` for a daemon that carries none — the value docker gives a container
/// whose image declares none. Without it the task falls back to whatever
/// default the shell was compiled with, which is the undocumented version of
/// the same hardcoding.
const PATH_FALLBACK: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// How long a killed process group gets on SIGTERM before the escalation, the
/// same shape `signal_refresh_build` uses for the refresh script's build.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(20);

const SYSTEMD_RUN: &str = "systemd-run";
const SYSTEMCTL: &str = "systemctl";

/// How long either `systemd` call gets to answer, because both wait on the
/// manager's own job queue and a wedged bus would otherwise hang the daemon's
/// boot silently. Expiry is a named refusal or a logged failure, never a pass.
const SYSTEMD_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// Unit-name prefix for a task's transient scope, so every unit a host node
/// creates is greppable and none can collide with a machine's own.
const UNIT_PREFIX: &str = "chug-task-";

/// What keeps the **client's** own `${VARIABLE}` substitution and `$$` collapse
/// out of the task's command, which `systemd-run --scope` performs itself and
/// turns on by default from systemd v258. Without it a shell command's `$$`
/// reaches the task as a bare `$` and a `${VAR}` as the daemon's value (design
/// #440 D8, job #462).
const EXPAND_ENV_OFF: &str = "--expand-environment=no";

/// Poll interval for [`ContainerBackend::wait`], which is trait-completeness
/// only — §3.1 polls dispatcher-side and the daemon never calls it.
const WAIT_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// How a host task is held **outside** the daemon's own supervision unit
/// (design #440 D3), which is what lets the daemon be restarted under running
/// work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervision {
    /// A transient systemd scope per task, in the named manager. A scope is its
    /// own cgroup, so the restart that swaps the daemon's unit reaches the
    /// daemon and not the task (#309 §6), and a cgroup kill also reaches a
    /// `setsid()` escapee (#440 D8).
    Scope(ScopeManager),
    /// The process group [`spawn_task`] creates, which is the unit `launchd`
    /// tears down by. **Unproven** — the assertion is an operator procedure,
    /// `docs/reference/runbooks/macos-host-supervision-proof.md`.
    ProcessGroup,
}

/// Which systemd manager a node's transient scopes are created in (design #440
/// D3). It follows the daemon's privilege rather than a preference, because
/// polkit refuses an unprivileged process a **system** scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeManager {
    /// `systemd --system`, which is what the root daemon #440 D2 declares gets,
    /// and whose scopes outlive every user session.
    System,
    /// The invoking user's own `systemd --user`, whose scopes outlive the daemon
    /// unit but **not** that user's manager.
    User,
}

impl ScopeManager {
    /// The flag that addresses this manager, which both `systemd-run` and
    /// `systemctl` take ahead of the verb.
    fn flag(self) -> Option<&'static str> {
        matches!(self, Self::User).then_some("--user")
    }

    /// How a refusal names what was asked for, so "polkit denied a system scope"
    /// and "there is no user manager" are never the same line.
    fn asked(self) -> String {
        match self.flag() {
            Some(flag) => format!("{SYSTEMD_RUN} {flag} --scope"),
            None => format!("{SYSTEMD_RUN} --scope"),
        }
    }

    /// What an operator would have to provision, read out of the failure rather
    /// than assumed from the manager: only a bus that answered nothing is the
    /// missing user manager `loginctl enable-linger` creates.
    fn precondition(self, stderr: &str) -> &'static str {
        let unreachable = stderr.to_ascii_lowercase().contains(BUS_UNREACHABLE);
        match (self, unreachable) {
            (Self::System, _) => "",
            (Self::User, true) => {
                " — the user bus was addressed and nothing answered, so this uid has no running \
                 systemd --user manager: a live session or `loginctl enable-linger` for it is the \
                 operator's, #440 slice 7"
            }
            (Self::User, false) => {
                " — the user bus was reached, so the failure above is the whole finding and not a \
                 missing systemd --user manager: no linger and no session would change it"
            }
        }
    }
}

/// One `systemd` invocation's argv, addressed at the manager the scope lives in.
fn addressed(manager: ScopeManager, args: impl IntoIterator<Item = String>) -> Vec<String> {
    manager
        .flag()
        .map(String::from)
        .into_iter()
        .chain(args)
        .collect()
}

/// The manager this process can actually create a scope in. An unprivileged
/// `systemd-run --scope` is answered by polkit with `Access denied … requires
/// interactive authentication`, measured on `gumbo-nuc-0` (#440, job #451).
fn manager_for(euid: u32) -> ScopeManager {
    if euid == 0 {
        ScopeManager::System
    } else {
        ScopeManager::User
    }
}

fn scope_manager() -> ScopeManager {
    // SAFETY: `geteuid` takes no arguments, cannot fail and returns a plain integer — it is unsafe only because it is a foreign function.
    manager_for(unsafe { libc::geteuid() })
}

/// Ask this node to create one transient scope, in the environment a launch
/// actually gets plus the bus variables the client itself needs, so "this node
/// can supervise a host task" is measured rather than assumed. The macOS answer
/// is [`Supervision::ProcessGroup`] without a probe, because there is nothing to
/// create.
pub async fn probe_supervision() -> Result<Supervision, String> {
    if !cfg!(target_os = "linux") {
        return Ok(Supervision::ProcessGroup);
    }
    let manager = scope_manager();
    let bus = borrowed_bus(Supervision::Scope(manager), &daemon_bus(), &BTreeMap::new());
    if let Some(reason) = bus_refusal(manager, &bus) {
        return Err(reason);
    }
    let unit = format!(
        "chug-probe-{}-{:x}.scope",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    let mut probe = tokio::process::Command::new(SYSTEMD_RUN);
    probe
        .args(scope_args(manager, &unit))
        .args(["/bin/sh", "-c", ":"])
        .env_clear()
        .envs(probe_env(&daemon_floor(), &bus))
        .kill_on_drop(true);
    scope_verdict(
        manager,
        tokio::time::timeout(SYSTEMD_BOUND, probe.output())
            .await
            .ok(),
    )
}

/// The probe's verdict, taken as data so the refusal is testable on a machine
/// that has no systemd to fail against. `None` is the bound expiring, which is a
/// refusal like any other rather than a boot that hangs.
fn scope_verdict(
    manager: ScopeManager,
    outcome: Option<std::io::Result<std::process::Output>>,
) -> Result<Supervision, String> {
    let asked = manager.asked();
    match outcome {
        Some(Ok(out)) if out.status.success() => Ok(Supervision::Scope(manager)),
        Some(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(format!(
                "{asked} exited {}: {}{}",
                out.status.code().unwrap_or(-1),
                first_line(&out.stderr),
                expand_precondition(&stderr).unwrap_or_else(|| manager.precondition(&stderr))
            ))
        }
        Some(Err(e)) => Err(format!("{SYSTEMD_RUN} is unusable on this node: {e}")),
        None => Err(format!(
            "{asked} did not answer within {}s — this node's systemd cannot start a unit promptly \
             enough to supervise a task",
            SYSTEMD_BOUND.as_secs()
        )),
    }
}

/// What an operator reads when the refusal is this node's `systemd-run` not
/// knowing [`EXPAND_ENV_OFF`], which systemd added in v254. It takes precedence
/// over the bus preconditions because the client refused the option and never
/// addressed a manager at all.
fn expand_precondition(stderr: &str) -> Option<&'static str> {
    stderr.contains(EXPAND_ENV_OFF).then_some(
        " — this node's systemd-run does not know --expand-environment=no, which systemd added in \
         v254: without it a v258 client substitutes ${VARIABLE} and collapses $$ in the task's own \
         command before exec'ing it, so a host task's shell would be handed a command the \
         dispatcher never wrote (#440 D8)",
    )
}

/// Why a node may not advertise `host` when [`probe_supervision`] refused. A
/// node that fell back to daemon-parented tasks would lose every in-flight task
/// to the next daemon restart while still claiming the mode — the silent lie
/// #309 §7 rejects.
pub fn host_refusal(node: &str, reason: &str) -> String {
    format!(
        "node {node} cannot create a supervision unit for a host task, so it must not advertise \
         WORKER_MODES=host: {reason} — design #440 D3 puts each host task in its own transient \
         scope because a task in the daemon's own unit is killed by the restart that swaps the \
         daemon (#309 §6)"
    )
}

/// The flags every transient scope this backend creates carries. `--collect` is
/// what reclaims the unit when the task's last process exits, and
/// [`EXPAND_ENV_OFF`] is what stops the client rewriting the task's own command.
fn scope_args(manager: ScopeManager, unit: &str) -> Vec<String> {
    addressed(
        manager,
        [
            "--scope".to_string(),
            "--quiet".to_string(),
            "--collect".to_string(),
            EXPAND_ENV_OFF.to_string(),
            format!("--unit={unit}"),
            "--".to_string(),
        ],
    )
}

/// The transient unit one task runs in.
fn unit_name(task: &str) -> String {
    format!("{UNIT_PREFIX}{task}.scope")
}

/// The argv a launch becomes under this node's mechanism (design #440 D3).
/// `--scope` runs the command from `systemd-run` itself, so the composed
/// environment, the cwd and the log fds are inherited exactly as they are
/// without it.
pub fn supervised_launch(supervision: Supervision, unit: &str, cmd: Vec<String>) -> Vec<String> {
    match supervision {
        Supervision::ProcessGroup => cmd,
        Supervision::Scope(manager) => {
            let mut argv = vec![SYSTEMD_RUN.to_string()];
            argv.extend(scope_args(manager, unit));
            argv.extend(cmd);
            argv
        }
    }
}

/// The first line of a tool's stderr, so a failure reaches a log line without
/// carrying a paragraph into it.
fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

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
    /// The transient scope this task runs in (#440 D3), `None` on a node whose
    /// mechanism is the process group. `kill` reaches through it to a `setsid()`
    /// escapee the group no longer holds.
    #[serde(default)]
    pub unit: Option<String>,
    /// Which manager holds [`Self::unit`], so a `kill` after a daemon restart
    /// signals the one the scope was created in. `None` is the process group,
    /// and a `meta.json` written before the manager was selectable.
    #[serde(default)]
    pub scope: Option<ScopeManager>,
}

/// Host-process execution on one node (design #309 P0). Serves the launches
/// that carry no image and refuses the rest, so a node offering both runtimes
/// routes to it per launch ([`names_host_task`]).
pub struct HostBackend {
    node: String,
    root: PathBuf,
    workspace: PathBuf,
    /// The mechanism every launch goes into (#440 D3), named by the caller
    /// rather than probed here — the daemon probes once at boot and refuses to
    /// serve `host` at all when the node has none.
    supervision: Supervision,
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
    pub fn new(
        node: impl Into<String>,
        root: impl Into<PathBuf>,
        supervision: Supervision,
    ) -> Result<Self, BackendError> {
        Self::with_workspace(node, root, HOST_WORKSPACE, supervision)
    }

    /// [`Self::new`] with the workspace path named explicitly. A real node only
    /// ever passes [`HOST_WORKSPACE`], because that is the literal
    /// `bootstrap_cmd` emits; the parameter exists so the round-trip test does
    /// not touch the machine's real `/workspace`.
    pub fn with_workspace(
        node: impl Into<String>,
        root: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
        supervision: Supervision,
    ) -> Result<Self, BackendError> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| BackendError::Unavailable(format!("host root {}: {e}", root.display())))?;
        sweep_detached(&root);
        Ok(Self {
            node: node.into(),
            root,
            workspace: workspace.into(),
            supervision,
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

    /// Whether this node may take the launch: it must be a host launch, and it
    /// must be the only one. The exclusion is #309 §2 option (iii) **enforced**:
    /// `NoCapacity` is transient, so the dispatcher queues and retries (§3.5)
    /// rather than spending the job's retry budget.
    async fn admit(&self, config: &ContainerLaunchConfig) -> Result<(), BackendError> {
        if let Some(image) = config.image.as_deref() {
            return Err(BackendError::Launch(format!(
                "node {} serves host mode and this launch declares image {image:?} — a container \
                 task routed to a host backend is a placement bug (design #309 §1: mode is the \
                 image's presence), refused rather than run against the machine's own toolchain",
                self.node
            )));
        }
        if let Some(held) = self.list_managed_running().await?.first() {
            return Err(BackendError::NoCapacity(format!(
                "host node {} runs one task at a time (#309 P0 takes §2 option iii, which is \
                 what makes {} unambiguous); {} holds it",
                self.node,
                self.workspace.display(),
                held.id
            )));
        }
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

/// Whether a `{node}/{task}` id names a host task rather than a container
/// (design #309 §1). The routing question a dual-mode node asks of every id it
/// is handed after the launch that minted it.
pub fn names_host_task(id: &ContainerId) -> bool {
    id.split_once('/')
        .is_some_and(|(_, task)| task.starts_with(TASK_PREFIX) && is_task_id(task))
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

/// Wrap the launch command so the **task itself** records its exit status, and
/// sheds whatever the `systemd-run` client borrowed to find its bus. The
/// daemon's own supervisor is only a backstop: a task must survive the daemon
/// being swapped under it (spec §3.1 drain guarantee), and a `wait` this
/// process performed would not.
fn supervised_cmd(
    cmd: &[String],
    borrowed: &BTreeMap<String, OsString>,
) -> Result<Vec<String>, BackendError> {
    if cmd.is_empty() {
        return Err(BackendError::Launch(
            "host launch has no command to run".into(),
        ));
    }
    let shed = shed_borrowed(borrowed);
    let mut wrapped = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!(
            "{shed}\"$@\"; s=$?; printf %s \"$s\" > \"${EXIT_TMP_VAR}\" && mv \"${EXIT_TMP_VAR}\" \
             \"${EXIT_VAR}\"; exit $s"
        ),
        "sh".to_string(),
    ];
    wrapped.extend(cmd.iter().cloned());
    Ok(wrapped)
}

/// The floor's values as the daemon holds them, read **by name** so no other
/// variable of the daemon's is ever copied, and with the non-panicking API so a
/// non-UTF-8 value drops to the fallback instead of killing the launch.
fn daemon_floor() -> HashMap<String, String> {
    INHERITED
        .iter()
        .filter_map(|name| {
            let value = std::env::var_os(name)?.into_string().ok()?;
            Some(((*name).to_string(), value))
        })
        .collect()
}

/// The floor alone: the two names carried from the daemon, with `PATH` always
/// holding a value. It is the whole of what a launch inherits, and the base
/// [`probe_env`] composes the client's bus variables onto.
fn floor_env(daemon: &HashMap<String, String>) -> BTreeMap<String, OsString> {
    let mut env = BTreeMap::new();
    for name in INHERITED {
        if let Some(value) = daemon.get(name) {
            env.insert(name.to_string(), OsString::from(value));
        }
    }
    env.entry(PATH_VAR.to_string())
        .or_insert_with(|| OsString::from(PATH_FALLBACK));
    env
}

/// The `PATH` a launch composes when the config declares none — the daemon's, or
/// [`PATH_FALLBACK`] when the daemon carries none. A caller staging a tool for a
/// task resolves it through this, so the guard follows the floor rather than its
/// own environment.
pub fn task_path() -> OsString {
    floor_env(&daemon_floor())
        .remove(PATH_VAR)
        .unwrap_or_else(|| OsString::from(PATH_FALLBACK))
}

/// The bus variables as the daemon holds them, read **by name** the way the
/// floor is so nothing else of the daemon's is copied. They are the client's,
/// which is why they are read here and not in [`daemon_floor`].
fn daemon_bus() -> HashMap<String, OsString> {
    BUS_VARS
        .iter()
        .filter_map(|name| Some(((*name).to_string(), std::env::var_os(name)?)))
        .collect()
}

/// What the `systemd-run` client borrows from the daemon to reach the manager it
/// is addressed at, minus every name the task's own environment already defines.
/// Only a **user** manager needs locating; the system bus is a fixed socket path.
fn borrowed_bus(
    supervision: Supervision,
    daemon: &HashMap<String, OsString>,
    task: &BTreeMap<String, OsString>,
) -> BTreeMap<String, OsString> {
    if supervision != Supervision::Scope(ScopeManager::User) {
        return BTreeMap::new();
    }
    BUS_VARS
        .iter()
        .filter(|name| !task.contains_key(**name))
        .filter_map(|name| Some(((*name).to_string(), daemon.get(*name)?.clone())))
        .collect()
}

/// Why a `--user` scope is refused before it is attempted: the daemon holds
/// neither bus variable, so nothing it spawns can locate a manager. `None`
/// wherever the answer can be measured instead of predicted.
fn bus_refusal(manager: ScopeManager, borrowed: &BTreeMap<String, OsString>) -> Option<String> {
    (manager == ScopeManager::User && borrowed.is_empty()).then(|| {
        format!(
            "{} cannot address a bus: this daemon's environment holds neither {RUNTIME_DIR_VAR} \
             nor {BUS_ADDRESS_VAR}, so no systemd --user manager can be located from it — a live \
             session, or `loginctl enable-linger` for this uid and {RUNTIME_DIR_VAR}=/run/user/\
             $UID in the daemon's environment, is the operator's, #440 slice 7",
            manager.asked()
        )
    })
}

/// The environment [`probe_supervision`] runs `systemd-run` in: the floor a
/// launch gets, plus exactly what the client borrows to find its bus. The task's
/// own environment is [`task_env`] and is not this.
fn probe_env(
    floor: &HashMap<String, String>,
    borrowed: &BTreeMap<String, OsString>,
) -> BTreeMap<String, OsString> {
    let mut env = floor_env(floor);
    env.extend(borrowed.clone());
    env
}

/// The environment a **launch's** `systemd-run` client runs in: the task's own,
/// plus what the client borrows to find its bus. It is [`probe_env`]'s superset
/// by construction, so a launch cannot fall back where the probe succeeded.
fn launch_env(
    task: &BTreeMap<String, OsString>,
    borrowed: &BTreeMap<String, OsString>,
) -> BTreeMap<String, OsString> {
    let mut env = task.clone();
    env.extend(borrowed.clone());
    env
}

/// The `unset` a task's wrapper opens with, so a variable the client borrowed to
/// find its bus does not survive into the task (#309 §10). Empty for every
/// launch that borrowed nothing.
fn shed_borrowed(borrowed: &BTreeMap<String, OsString>) -> String {
    if borrowed.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = borrowed.keys().map(String::as_str).collect();
    format!("unset {}; ", names.join(" "))
}

/// Everything a host task's environment holds: the floor carried from the
/// daemon by name, the dispatcher's launch env, and the two exit-status paths
/// the wrapper writes through. Composed rather than inherited, so it is
/// exhaustive — [`spawn_task`] clears the daemon's environment first.
fn task_env(
    daemon: &HashMap<String, String>,
    launch: &HashMap<String, String>,
    dir: &Path,
) -> BTreeMap<String, OsString> {
    let mut env = floor_env(daemon);
    for (name, value) in launch {
        env.insert(name.clone(), OsString::from(value));
    }
    env.insert(
        EXIT_TMP_VAR.to_string(),
        dir.join(EXIT_CODE_TMP).into_os_string(),
    );
    env.insert(EXIT_VAR.to_string(), dir.join(EXIT_CODE).into_os_string());
    env
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
/// its own supervision unit and its own process group (#440 D3). Both are units
/// `kill` signals, the way a container runtime kills a whole cgroup (#309 §2).
fn spawn_task(
    dir: &Path,
    config: &ContainerLaunchConfig,
    supervision: Supervision,
    unit: &str,
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

    let env = task_env(&daemon_floor(), &config.env, dir);
    let borrowed = borrowed_bus(supervision, &daemon_bus(), &env);
    let wrapped = supervised_launch(supervision, unit, supervised_cmd(&config.cmd, &borrowed)?);
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(&wrapped[0]);
    command
        .args(&wrapped[1..])
        .current_dir(dir)
        .env_clear()
        .envs(launch_env(&env, &borrowed))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(errors))
        .process_group(0);
    let child = command
        .spawn()
        .map_err(|e| BackendError::Launch(format!("spawning host task: {e}")))?;

    let pid = i32::try_from(child.id()).unwrap_or(-1);
    let scope = match supervision {
        Supervision::Scope(manager) => Some(manager),
        Supervision::ProcessGroup => None,
    };
    let meta = TaskMeta {
        pid,
        pgid: pid,
        start_time: process_start_time(pid),
        project: config.env.get("JOB_PROJECT").cloned(),
        job: config.env.get("JOB_ID").and_then(|v| v.parse().ok()),
        task: config.env.get("CHUG_TASK_ID").and_then(|v| v.parse().ok()),
        files,
        unit: scope.map(|_| unit.to_string()),
        scope,
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

/// The argv one signal to a task's scope becomes, factored out of
/// [`signal_unit`] the way [`supervised_launch`] is, so the D8 half is asserted
/// on a machine with no systemd to send it to.
fn kill_unit_args(manager: ScopeManager, unit: &str, signal: &str) -> Vec<String> {
    addressed(
        manager,
        [
            "kill".to_string(),
            format!("--signal={signal}"),
            unit.to_string(),
        ],
    )
}

/// Signal every process in a task's transient scope, bounded by
/// [`SYSTEMD_BOUND`]. This is the half that reaches a `setsid()` escapee (#309
/// §2, #440 D8): the escapee leaves the process group and stays in the cgroup.
async fn signal_unit(manager: ScopeManager, unit: &str, signal: &str) {
    let mut kill = tokio::process::Command::new(SYSTEMCTL);
    kill.args(kill_unit_args(manager, unit, signal))
        .kill_on_drop(true);
    match tokio::time::timeout(SYSTEMD_BOUND, kill.output()).await {
        Ok(Ok(out)) if out.status.success() => (),
        Ok(Ok(out)) => tracing::info!(
            unit,
            signal,
            "host kill: scope already gone: {}",
            first_line(&out.stderr)
        ),
        Ok(Err(e)) => tracing::error!(unit, signal, "host kill: {SYSTEMCTL} is unusable: {e}"),
        Err(_) => tracing::error!(
            unit,
            signal,
            "host kill: {SYSTEMCTL} did not answer within {}s — only the process-group signal \
             reached this task, so a setsid() escapee is still running",
            SYSTEMD_BOUND.as_secs()
        ),
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
            "{TASK_PREFIX}{:x}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let dir = self.root.join(&task);
        std::fs::create_dir_all(&dir)
            .map_err(|e| BackendError::Launch(format!("{}: {e}", dir.display())))?;

        let (child, meta) = spawn_task(&dir, &config, self.supervision, &unit_name(&task))?;
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
        let manager = meta.scope.unwrap_or(ScopeManager::System);
        tracing::warn!(node = %self.node, id = %id, pgid = meta.pgid, unit = ?meta.unit, scope = ?manager, "host kill: SIGTERM to the process group and the scope");
        signal_group(meta.pgid, libc::SIGTERM);
        if let Some(unit) = meta.unit.as_deref() {
            signal_unit(manager, unit, "SIGTERM").await;
        }
        let pgid = meta.pgid;
        let unit = meta.unit.clone();
        let dir = dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(KILL_GRACE).await;
            if read_exit_code(&dir).is_none() {
                tracing::warn!(pgid, "host kill: group ignored SIGTERM — SIGKILL");
                signal_group(pgid, libc::SIGKILL);
                if let Some(unit) = unit.as_deref() {
                    signal_unit(manager, unit, "SIGKILL").await;
                }
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
            unit: None,
            scope: None,
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
        let wrapped = supervised_cmd(&cmd, &BTreeMap::new()).unwrap();
        assert_eq!(wrapped[0], "sh");
        assert_eq!(wrapped[3], "sh", "$0 is consumed before the real argv");
        assert_eq!(&wrapped[4..], &cmd[..]);
        assert!(wrapped[2].contains(EXIT_VAR), "{}", wrapped[2]);
        assert!(wrapped[2].contains("exit $s"), "{}", wrapped[2]);
        assert!(
            supervised_cmd(&[], &BTreeMap::new()).is_err(),
            "an empty command is refused"
        );

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

    fn daemon_env() -> HashMap<String, String> {
        HashMap::from([
            ("PATH".to_string(), "/run/current-system/sw/bin".to_string()),
            ("HOME".to_string(), "/var/root".to_string()),
            ("NATS_CREDS".to_string(), "the daemon's own".to_string()),
            (
                "DOCKER_HOST".to_string(),
                "unix:///var/run/docker.sock".to_string(),
            ),
            ("WORKER_NODE".to_string(), "gumbo-nuc-0".to_string()),
        ])
    }

    /// The launch environment is **composed, not inherited** (design #440 slice
    /// 1): a variable the daemon holds and the launch config does not — here a
    /// reachable docker socket and the daemon's own NATS credential — never
    /// reaches the task.
    #[test]
    fn a_host_task_inherits_nothing_the_dispatcher_did_not_declare() {
        let dir = Path::new("/var/lib/chuggernaut/host-tasks/host-1-0");
        let launch = HashMap::from([
            ("JOB_ID".to_string(), "440".to_string()),
            ("NATS_CREDS".to_string(), "the dispatcher's".to_string()),
        ]);
        let env = task_env(&daemon_env(), &launch, dir);

        assert_eq!(
            env.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                EXIT_VAR,
                EXIT_TMP_VAR,
                "HOME",
                "JOB_ID",
                "NATS_CREDS",
                "PATH"
            ],
            "the launch env, the floor and the two exit paths — and nothing else"
        );
        for leaked in ["DOCKER_HOST", "WORKER_NODE"] {
            assert!(
                !env.contains_key(leaked),
                "{leaked} is the daemon's, never the task's"
            );
        }
        assert_eq!(
            env["NATS_CREDS"], "the dispatcher's",
            "a declared name takes the dispatcher's value, never the daemon's"
        );
        assert_eq!(env["JOB_ID"], "440");
        assert_eq!(env["PATH"], "/run/current-system/sw/bin");
        assert_eq!(env["HOME"], "/var/root");
        assert_eq!(env[EXIT_TMP_VAR], dir.join(EXIT_CODE_TMP).into_os_string());
        assert_eq!(env[EXIT_VAR], dir.join(EXIT_CODE).into_os_string());
    }

    /// The floor is exactly `PATH` and `HOME`, and `PATH` has a value even on a
    /// daemon carrying none — a task left to the shell's compiled-in default is
    /// the undocumented version of the hardcoding this replaces.
    #[test]
    fn the_floor_is_two_names_and_path_always_has_a_value() {
        let dir = Path::new("/tmp/host-1-0");
        assert_eq!(INHERITED, ["PATH", "HOME"]);

        for name in daemon_floor().keys() {
            assert!(
                INHERITED.contains(&name.as_str()),
                "{name} is not the floor, and nothing else is read from the daemon"
            );
        }

        assert_eq!(
            floor_env(&daemon_env()).keys().collect::<Vec<_>>(),
            ["HOME", "PATH"],
            "the launch's own environment is the floor and nothing else — the client's bus \
             variables are composed beside it, never into it"
        );

        assert_eq!(
            task_path(),
            task_env(&daemon_floor(), &HashMap::new(), dir)[PATH_VAR],
            "a tool staged for a task is resolved through the PATH a launch composes, so a floor \
             that stopped carrying the daemon's would break the staging guard and not only the task"
        );

        let bare = task_env(&HashMap::new(), &HashMap::new(), dir);
        assert_eq!(bare["PATH"], PATH_FALLBACK);
        assert!(
            !bare.contains_key("HOME"),
            "no daemon HOME means no invented one — the tools fall back to the passwd entry"
        );
        assert_eq!(bare.len(), 3, "PATH and the two exit paths: {bare:?}");

        let declared = HashMap::from([("PATH".to_string(), "/nix/store/x/bin".to_string())]);
        assert_eq!(
            task_env(&daemon_env(), &declared, dir)["PATH"],
            "/nix/store/x/bin",
            "a declared PATH wins over the floor, as a container env wins over its image's"
        );
    }

    /// A host task is launched into its **own** transient scope (design #440
    /// D3), which is what a `systemctl restart` of the daemon's unit does not
    /// reach — and the wrapped argv is the task's own, unaltered.
    #[test]
    fn a_host_task_is_launched_into_its_own_supervision_unit() {
        let cmd: Vec<String> = ["sh", "-c", "cargo build"].map(String::from).to_vec();
        let unit = unit_name("host-1a2b-0");
        assert_eq!(unit, "chug-task-host-1a2b-0.scope");
        assert!(
            unit.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "a unit name systemd would refuse: {unit}"
        );

        let scoped =
            supervised_launch(Supervision::Scope(ScopeManager::System), &unit, cmd.clone());
        assert_eq!(
            &scoped[..7],
            &[
                "systemd-run".to_string(),
                "--scope".to_string(),
                "--quiet".to_string(),
                "--collect".to_string(),
                EXPAND_ENV_OFF.to_string(),
                format!("--unit={unit}"),
                "--".to_string(),
            ],
            "the scope is transient, silent on the task's own log, and reclaimed"
        );
        assert_eq!(&scoped[7..], &cmd[..], "the task's argv is passed through");

        assert_eq!(
            supervised_launch(Supervision::ProcessGroup, &unit, cmd.clone()),
            cmd,
            "macOS has no unit to create — the process group is the mechanism"
        );
    }

    /// The one line design #440 D8's escapee turned on (job #462): a scope
    /// launch must hand the task the command the dispatcher wrote, and
    /// `systemd-run --scope` substitutes `${VARIABLE}` and collapses `$$` in
    /// that argv itself unless it is told not to.
    #[test]
    fn a_scope_hands_the_task_the_dollars_the_dispatcher_wrote() {
        let unit = unit_name("host-1a2b-0");
        let cmd: Vec<String> = ["sh", "-c", "printf %s \"$$\" > \"${HOME}/pid\""]
            .map(String::from)
            .to_vec();

        for manager in [ScopeManager::System, ScopeManager::User] {
            let scoped = supervised_launch(Supervision::Scope(manager), &unit, cmd.clone());
            let flag = scoped.iter().position(|a| a == EXPAND_ENV_OFF);
            let sep = scoped.iter().position(|a| a == "--");
            assert!(
                matches!((flag, sep), (Some(f), Some(s)) if f < s),
                "without {EXPAND_ENV_OFF} ahead of the separator the client expands the task's \
                 own command, and a client flag after it is argv: {scoped:?}"
            );
            assert_eq!(
                &scoped[sep.unwrap() + 1..],
                &cmd[..],
                "the task's command is passed through byte for byte"
            );
            assert!(
                scope_args(manager, &unit).contains(&EXPAND_ENV_OFF.to_string()),
                "the probe creates its scope through this same argv, so a client too old to be \
                 told it is refused at boot and not at the first launch"
            );
        }
    }

    /// A `setsid()` escapee leaves the process group and stays in the cgroup, so
    /// `kill` addresses the scope by name at both stages (design #440 D8) — the
    /// group signal alone misses it by construction.
    #[test]
    fn a_killed_task_is_signalled_through_its_scope_at_both_stages() {
        let unit = unit_name("host-1a2b-0");
        for signal in ["SIGTERM", "SIGKILL"] {
            let args = kill_unit_args(ScopeManager::System, &unit, signal);
            assert_eq!(
                args,
                [
                    "kill".to_string(),
                    format!("--signal={signal}"),
                    unit.clone()
                ],
                "systemctl kill takes the signal as a flag and the unit as the pattern"
            );
        }
        assert_ne!(
            kill_unit_args(ScopeManager::System, &unit, "SIGTERM"),
            kill_unit_args(ScopeManager::System, &unit, "SIGKILL"),
            "the escalation is a different signal to the same unit, not a repeat"
        );
        assert_eq!(
            supervised_launch(Supervision::ProcessGroup, &unit, vec!["true".to_string()]),
            vec!["true".to_string()],
            "a node with no scope records no unit, so kill signals only the group"
        );
    }

    /// A node that cannot create a supervision unit must not advertise `host`
    /// (design #440 D3): the refusal names the node and carries the probe's own
    /// reason, because a silent fall back to daemon-parented tasks is the lie
    /// #309 §7 rejects.
    #[test]
    fn a_node_that_cannot_create_a_unit_may_not_advertise_host() {
        use std::os::unix::process::ExitStatusExt;
        let failed = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: b"Failed to connect to bus: No such file or directory\nmore\n".to_vec(),
        };
        let reason = scope_verdict(ScopeManager::System, Some(Ok(failed))).unwrap_err();
        assert!(reason.contains("systemd-run"), "{reason}");
        assert!(reason.contains("Failed to connect to bus"), "{reason}");
        assert!(
            !reason.contains("more"),
            "one line, not a paragraph: {reason}"
        );

        let missing = scope_verdict(
            ScopeManager::System,
            Some(Err(std::io::Error::from(std::io::ErrorKind::NotFound))),
        )
        .unwrap_err();
        assert!(missing.contains("systemd-run"), "{missing}");

        let expired = scope_verdict(ScopeManager::System, None).unwrap_err();
        assert!(
            expired.contains(&format!("{}s", SYSTEMD_BOUND.as_secs())),
            "a wedged bus refuses the boot by name rather than hanging it: {expired}"
        );
        assert!(
            host_refusal("gumbo-nuc-0", &expired).contains(&expired),
            "the bound's expiry is a refusal like any other"
        );

        let refusal = host_refusal("gumbo-nuc-0", &reason);
        assert!(refusal.contains("gumbo-nuc-0"), "{refusal}");
        assert!(refusal.contains("WORKER_MODES=host"), "{refusal}");
        assert!(
            refusal.contains(&reason),
            "the probe's own reason: {refusal}"
        );

        let ok = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(
            scope_verdict(ScopeManager::System, Some(Ok(ok))),
            Ok(Supervision::Scope(ScopeManager::System))
        );
    }

    /// A `systemd-run` predating [`EXPAND_ENV_OFF`] refuses the **option**, not
    /// the scope, so the refusal names the version rather than sending the
    /// operator after a bus that was never addressed (design #440 D8, job #462).
    #[test]
    fn a_client_too_old_to_leave_the_command_alone_is_named_as_such() {
        use std::os::unix::process::ExitStatusExt;
        let old = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: Vec::new(),
            stderr: format!("systemd-run: unrecognized option '{EXPAND_ENV_OFF}'\n").into_bytes(),
        };
        let reason = scope_verdict(ScopeManager::User, Some(Ok(old))).unwrap_err();
        assert!(reason.contains(EXPAND_ENV_OFF), "{reason}");
        assert!(reason.contains("v254"), "{reason}");
        assert!(
            !reason.contains("enable-linger"),
            "the client never addressed a bus, so linger is the wrong advice: {reason}"
        );

        let bus = "Failed to start transient scope unit: Access denied";
        assert!(
            expand_precondition(bus).is_none(),
            "every other refusal keeps the bus preconditions"
        );
        assert!(
            scope_verdict(
                ScopeManager::User,
                Some(Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(1 << 8),
                    stdout: Vec::new(),
                    stderr: bus.as_bytes().to_vec(),
                }))
            )
            .unwrap_err()
            .contains("the user bus was reached"),
            "a manager that answered is still the finding it was"
        );
    }

    /// The scope an **unprivileged** daemon asks for is the one polkit grants:
    /// measured on `gumbo-nuc-0`, a system scope is `Access denied` to a normal
    /// user and a `--user` scope is exit 0 (design #440 D3, job #451).
    #[test]
    fn an_unprivileged_daemon_asks_for_the_scope_it_can_create() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            manager_for(0),
            ScopeManager::System,
            "root gets the machine"
        );
        for euid in [1u32, 1000, 65534] {
            assert_eq!(
                manager_for(euid),
                ScopeManager::User,
                "polkit denies uid {euid} a system scope without interactive authentication"
            );
        }

        let unit = unit_name("host-1a2b-0");
        let cmd = vec!["true".to_string()];
        let user = supervised_launch(Supervision::Scope(ScopeManager::User), &unit, cmd.clone());
        assert_eq!(
            &user[..3],
            &[
                "systemd-run".to_string(),
                "--user".to_string(),
                "--scope".to_string()
            ],
            "the manager is addressed before the verb"
        );
        assert_eq!(&user[user.len() - 1..], &cmd[..]);
        assert_ne!(
            user,
            supervised_launch(Supervision::Scope(ScopeManager::System), &unit, cmd),
            "the two managers are different units, not one unit named twice"
        );
        assert_eq!(
            kill_unit_args(ScopeManager::User, &unit, "SIGKILL")[0],
            "--user",
            "a scope in the user manager is invisible to a systemctl without it, so the escapee \
             would outlive the kill"
        );

        let denied = std::process::Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: b"Failed to start transient scope unit: Access denied\n".to_vec(),
            stderr: b"Failed to start transient scope unit: Access denied\nas the requested \
                      operation requires interactive authentication.\n"
                .to_vec(),
        };
        let reason = scope_verdict(ScopeManager::User, Some(Ok(denied))).unwrap_err();
        assert!(reason.contains("systemd-run --user --scope"), "{reason}");
        assert!(
            !reason.contains("enable-linger"),
            "a manager that answered and refused is not provisioning, and naming linger here \
             costs an operator a node change for nothing: {reason}"
        );
    }

    /// The refusal distinguishes the three things that keep a `--user` scope
    /// from existing, because they need three different actions: no bus to
    /// address, a bus nobody answers, and a manager that answered and refused.
    #[test]
    fn a_refusal_names_the_bus_it_could_not_reach() {
        use std::os::unix::process::ExitStatusExt;

        let none = bus_refusal(ScopeManager::User, &BTreeMap::new()).expect("no bus to address");
        assert!(
            none.contains(RUNTIME_DIR_VAR) && none.contains(BUS_ADDRESS_VAR),
            "{none}"
        );
        assert!(
            none.contains("slice 7"),
            "the provisioning is named: {none}"
        );
        assert_eq!(
            bus_refusal(ScopeManager::System, &BTreeMap::new()),
            None,
            "the system bus is a fixed socket path and needs no variable"
        );
        assert_eq!(
            bus_refusal(
                ScopeManager::User,
                &BTreeMap::from([(
                    RUNTIME_DIR_VAR.to_string(),
                    OsString::from("/run/user/1000")
                )])
            ),
            None,
            "a bus that can be addressed is measured, not predicted"
        );

        let failed = |stderr: &str| {
            scope_verdict(
                ScopeManager::User,
                Some(Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(1 << 8),
                    stdout: Vec::new(),
                    stderr: stderr.as_bytes().to_vec(),
                })),
            )
            .unwrap_err()
        };

        let unreachable = failed(
            "Failed to connect to user scope bus via local transport: No such file or directory\n",
        );
        assert!(
            unreachable.contains("enable-linger") && unreachable.contains("nothing answered"),
            "a bus that answered nothing is the uid's missing manager: {unreachable}"
        );
        let refused = failed("Failed to start transient scope unit: Access denied\n");
        assert!(
            !refused.contains("enable-linger") && refused.contains("was reached"),
            "{refused}"
        );

        let exec = failed("Failed to execute /bin/sh: No such file or directory\n");
        assert!(
            !exec.contains("enable-linger") && exec.contains("was reached"),
            "`--scope` execs the command from systemd-run itself, so a missing binary is a \
             manager that answered: reading its errno as a missing manager is the wrong advice \
             this classification exists to stop giving: {exec}"
        );
    }

    /// The bus variables reach the `systemd-run` **client** and stop there: the
    /// task's own environment is the floor, the launch config and the two exit
    /// paths, exactly as #309 §10 and slice 1 leave it.
    #[test]
    fn the_client_gets_the_bus_and_the_task_never_sees_it() {
        let dir = Path::new("/var/lib/chuggernaut/host-tasks/host-1-0");
        let bus = HashMap::from([
            (
                RUNTIME_DIR_VAR.to_string(),
                OsString::from("/run/user/1000"),
            ),
            (
                BUS_ADDRESS_VAR.to_string(),
                OsString::from("unix:path=/run/user/1000/bus"),
            ),
        ]);
        let task = task_env(&daemon_env(), &HashMap::new(), dir);
        for name in BUS_VARS {
            assert!(
                !task.contains_key(name),
                "{name} is the client's, never the task's"
            );
        }

        let user = borrowed_bus(Supervision::Scope(ScopeManager::User), &bus, &task);
        assert_eq!(
            user.keys().map(String::as_str).collect::<Vec<_>>(),
            [BUS_ADDRESS_VAR, RUNTIME_DIR_VAR],
            "a --user scope cannot be reached without them"
        );
        assert_eq!(
            probe_env(&daemon_env(), &user).keys().collect::<Vec<_>>(),
            [BUS_ADDRESS_VAR, "HOME", "PATH", RUNTIME_DIR_VAR],
            "the probe measures the launch's floor plus what the client itself needs"
        );

        assert!(
            borrowed_bus(
                Supervision::Scope(ScopeManager::User),
                &HashMap::new(),
                &task
            )
            .is_empty(),
            "a daemon holding neither carries neither"
        );
        assert_eq!(
            probe_env(&daemon_env(), &BTreeMap::new())
                .keys()
                .collect::<Vec<_>>(),
            ["HOME", "PATH"],
            "and the probe is then exactly the launch floor"
        );
        assert!(
            borrowed_bus(Supervision::Scope(ScopeManager::System), &bus, &task).is_empty(),
            "the system bus is a fixed socket path"
        );
        assert!(
            borrowed_bus(Supervision::ProcessGroup, &bus, &task).is_empty(),
            "macOS creates no unit, so there is no client to address"
        );

        let declared = task_env(
            &daemon_env(),
            &HashMap::from([(RUNTIME_DIR_VAR.to_string(), "the dispatcher's".to_string())]),
            dir,
        );
        assert_eq!(
            borrowed_bus(Supervision::Scope(ScopeManager::User), &bus, &declared)
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [BUS_ADDRESS_VAR],
            "a declared name keeps the launch config's value, which the daemon's never overwrites \
             and the shed never removes"
        );
    }

    /// The **launch's** client is handed everything the probe measured with, so
    /// a node whose `probe_supervision` returned a scope cannot silently fall
    /// back to the daemon's own cgroup at launch (job #455's first suspect).
    #[test]
    fn the_launch_client_gets_everything_the_probe_measured() {
        let dir = Path::new("/var/lib/chuggernaut/host-tasks/host-1-0");
        let bus = HashMap::from([
            (
                RUNTIME_DIR_VAR.to_string(),
                OsString::from("/run/user/1000"),
            ),
            (
                BUS_ADDRESS_VAR.to_string(),
                OsString::from("unix:path=/run/user/1000/bus"),
            ),
        ]);
        let launch = HashMap::from([("JOB_ID".to_string(), "455".to_string())]);
        let task = task_env(&daemon_env(), &launch, dir);
        let borrowed = borrowed_bus(Supervision::Scope(ScopeManager::User), &bus, &task);
        let client = launch_env(&task, &borrowed);

        for (name, value) in probe_env(&daemon_env(), &borrowed) {
            assert_eq!(
                client.get(&name),
                Some(&value),
                "the probe measured with {name} and the launch's client does not carry it, so the \
                 launch would refuse where the probe passed"
            );
        }
        for name in BUS_VARS {
            assert!(client.contains_key(name), "{name} reaches the client");
            assert!(!task.contains_key(name), "{name} never reaches the task");
        }
        assert_eq!(client["JOB_ID"], OsString::from("455"));
    }

    /// The shed runs **inside** the scope: `systemd-run` reads the bus variables
    /// and the task started under it does not, which is the ordering the whole
    /// fix turns on.
    #[test]
    fn the_task_sheds_what_the_client_borrowed() {
        let dir = std::env::temp_dir().join(format!("chug-host-shed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let seen = dir.join("env");
        let borrowed = BTreeMap::from([(
            RUNTIME_DIR_VAR.to_string(),
            OsString::from("/run/user/1000"),
        )]);

        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("printenv > {}", seen.display()),
        ];
        let wrapped = supervised_cmd(&cmd, &borrowed).unwrap();
        assert!(
            wrapped[2].starts_with(&format!("unset {RUNTIME_DIR_VAR}; ")),
            "{}",
            wrapped[2]
        );
        assert!(
            !supervised_cmd(&cmd, &BTreeMap::new()).unwrap()[2].contains("unset"),
            "a launch that borrowed nothing sheds nothing"
        );

        let status = std::process::Command::new(&wrapped[0])
            .args(&wrapped[1..])
            .env_clear()
            .env("PATH", PATH_FALLBACK)
            .env(EXIT_TMP_VAR, dir.join(EXIT_CODE_TMP))
            .env(EXIT_VAR, dir.join(EXIT_CODE))
            .envs(&borrowed)
            .status()
            .unwrap();
        assert!(status.success());
        let env = std::fs::read_to_string(&seen).unwrap();
        assert!(
            !env.contains(RUNTIME_DIR_VAR),
            "the client had it and the task must not: {env}"
        );
        assert!(
            env.contains(EXIT_VAR),
            "the task's own environment is intact"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `kill` after a daemon restart reads the manager out of `meta.json`, so
    /// the spelling is asserted here rather than only where a scope can be
    /// created — and an older meta keeps the system manager's meaning.
    #[test]
    fn the_scopes_manager_survives_a_daemon_restart() {
        let meta = TaskMeta {
            pid: 7,
            pgid: 7,
            start_time: None,
            project: None,
            job: None,
            task: None,
            files: Vec::new(),
            unit: Some(unit_name("host-1a2b-0")),
            scope: Some(ScopeManager::User),
        };
        let encoded = serde_json::to_value(&meta).unwrap();
        assert_eq!(encoded["scope"], "user");
        let restored: TaskMeta = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored.scope, Some(ScopeManager::User));

        let older: TaskMeta = serde_json::from_str(
            r#"{"pid":7,"pgid":7,"start_time":null,"project":null,"job":null,"task":null,
                "files":[],"unit":"chug-task-host-1a2b-0.scope"}"#,
        )
        .unwrap();
        assert_eq!(
            older.scope.unwrap_or(ScopeManager::System),
            ScopeManager::System,
            "a meta written before the manager was selectable is a system scope"
        );
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

        let backend =
            HostBackend::with_workspace("w1", &root, root.join("ws"), Supervision::ProcessGroup)
                .unwrap();
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
        let backend =
            HostBackend::with_workspace("w1", &root, root.join("ws"), Supervision::ProcessGroup)
                .unwrap();
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

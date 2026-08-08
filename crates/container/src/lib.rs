//! Container execution backends (spec §3.1, §4.1).
//!
//! The dispatcher launches all work and eval containers through [`ContainerBackend`].
//! No CI or workflow engine sits in between — Docker socket in dev, the Kubernetes
//! Jobs API in production.

pub mod docker;
pub mod host;
pub mod k8s;

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use thiserror::Error;
use types::job_type::RuntimeMode;

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

/// Platform-level fleet placement policy (spec §3.1), set once for the whole
/// fleet by `PLACEMENT_POLICY`. Per-job-type `placement.node` pinning overrides
/// placement entirely and is unaffected by the policy. Kept as a single enum so
/// the choice lives in one place ([`choose_placement`]) and future policies
/// (weighted, cache-affinity) drop in beside these two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlacementPolicy {
    /// Fewest running jobs wins; ties broken by most free slots, then by name.
    /// An idle node always beats a busy one regardless of slot counts. The
    /// default — on an asymmetric fleet the idle small node takes the next job
    /// rather than always trailing the big one.
    #[default]
    Busyness,
    /// Most free slots (`slots − running`) wins; ties broken by name. The
    /// original rule — maximizes absolute headroom for burst absorption.
    Headroom,
}

impl PlacementPolicy {
    /// Parse the `PLACEMENT_POLICY` env value. `Err` names the accepted values
    /// so an unknown setting fails config load loudly (spec §12.4).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "busyness" => Ok(Self::Busyness),
            "headroom" => Ok(Self::Headroom),
            other => Err(format!(
                "unknown placement policy {other:?} (expected busyness | headroom)"
            )),
        }
    }

    /// The canonical string form, for the config snapshot and diagnostics.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Busyness => "busyness",
            Self::Headroom => "headroom",
        }
    }
}

/// A fleet node's live load for placement (spec §3.1): running managed
/// containers and free slots (`slots − running`). Both policies read from this
/// same ping-provided pair — no policy needs a new RPC.
#[derive(Debug, Clone, Copy)]
pub struct NodeLoad {
    pub running: i64,
    pub free: i64,
}

/// The modes a node serves when it has advertised nothing (design #309 §4):
/// container only. The fail-closed reading, and every docker-endpoint node's.
pub const CONTAINER_ONLY_MODES: &[RuntimeMode] = &[RuntimeMode::Container];

/// A probed node for the placement decision. `load` is `None` when the node is
/// out of service (unreachable / failed its ping), which excludes it from
/// unpinned placement and fails a pin onto it.
pub struct PlacementCandidate<'a> {
    pub index: usize,
    pub name: &'a str,
    pub load: Option<NodeLoad>,
    /// The execution modes the node advertises (design #309 §4/§5a), as of the
    /// same probe that produced `load`. A node advertising nothing reads as
    /// [`CONTAINER_ONLY_MODES`].
    pub modes: &'a [RuntimeMode],
}

impl PlacementCandidate<'_> {
    /// Whether this node serves `mode` — the design #309 §5a capability
    /// predicate placement filters candidates on.
    pub fn serves(&self, mode: RuntimeMode) -> bool {
        self.modes.contains(&mode)
    }
}

fn mode_list(modes: &[RuntimeMode]) -> String {
    modes
        .iter()
        .map(RuntimeMode::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn fleet_modes(candidates: &[PlacementCandidate<'_>]) -> String {
    candidates
        .iter()
        .map(|c| format!("{} serves {}", c.name, mode_list(c.modes)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The spec §3.1 placement decision — pure, so both policies and the design
/// #309 §5a capability predicate are unit-tested without a daemon. Its
/// postcondition is `docs/implementation-notes.md`'s `fn choose_placement`
/// entries; the returned `usize` is the chosen candidate's `index`.
#[allow(
    clippy::expect_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
pub fn choose_placement(
    policy: PlacementPolicy,
    candidates: &[PlacementCandidate<'_>],
    pin: Option<&str>,
    required: RuntimeMode,
) -> Result<usize, BackendError> {
    if let Some(name) = pin {
        let Some(c) = candidates.iter().find(|c| c.name == name) else {
            let known: Vec<&str> = candidates.iter().map(|c| c.name).collect();
            return Err(BackendError::Launch(format!(
                "placement pinned to unknown node {name:?}; known nodes: {}",
                known.join(", ")
            )));
        };
        return match c.load {
            Some(_) if !c.serves(required) => Err(BackendError::Launch(format!(
                "placement pinned to node {name}, which serves {} and not {} mode; a pin never \
                 falls back, so this needs a config change on the node or the job type",
                mode_list(c.modes),
                required.as_str()
            ))),
            Some(load) if load.free > 0 => Ok(c.index),
            _ => Err(BackendError::NoCapacity(format!(
                "no free slots on node {name}"
            ))),
        };
    }
    if !candidates.is_empty() && !candidates.iter().any(|c| c.serves(required)) {
        return Err(BackendError::NoCapacity(format!(
            "no node advertises {} mode: no fleet node serves it, so this launch cannot be placed \
             until one declares it in WORKER_MODES ({})",
            required.as_str(),
            fleet_modes(candidates)
        )));
    }
    let mut best: Option<&PlacementCandidate> = None;
    for c in candidates {
        if !c.serves(required) {
            continue;
        }
        let Some(load) = c.load else { continue };
        if load.free <= 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => {
                let bl = b.load.expect("best always has load");
                match policy {
                    PlacementPolicy::Headroom => {
                        load.free > bl.free || (load.free == bl.free && c.name < b.name)
                    }
                    PlacementPolicy::Busyness => {
                        load.running < bl.running
                            || (load.running == bl.running && load.free > bl.free)
                            || (load.running == bl.running
                                && load.free == bl.free
                                && c.name < b.name)
                    }
                }
            }
        };
        if better {
            best = Some(c);
        }
    }
    match best {
        Some(c) => Ok(c.index),
        None => Err(BackendError::NoCapacity("no free slots on any node".into())),
    }
}

/// Bounded cadence for the fleet-wide unadvertised-mode warning, following
/// design #293 §8's never-observed warning: one line per mode per interval.
pub const MODE_WARN_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Is the fleet-wide unadvertised-mode warning due? Pure over its inputs, so
/// the cadence is unit-tested without a fleet or a fifteen-minute wait.
pub fn mode_warning_due(last_warned: Option<Instant>, now: Instant) -> bool {
    match last_warned {
        None => true,
        Some(at) => now.duration_since(at) >= MODE_WARN_INTERVAL,
    }
}

/// A placing backend's record of when it last warned that work requires a mode
/// no node advertises (design #309 §5a). Held beside the fleet, because the
/// finding is fleet-wide and the trigger is one launch.
#[derive(Default)]
pub struct ModeWarnings(Mutex<Option<(RuntimeMode, Instant)>>);

impl ModeWarnings {
    /// Warn — at most once per [`MODE_WARN_INTERVAL`] per mode — when a launch
    /// requires a mode no candidate advertises. A mode nothing asks for is
    /// never warned about, which is what keeps this off a container-only fleet.
    pub fn observe(&self, candidates: &[PlacementCandidate<'_>], required: RuntimeMode) {
        if candidates.is_empty() || candidates.iter().any(|c| c.serves(required)) {
            return;
        }
        let now = Instant::now();
        let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let previous = last.filter(|(mode, _)| *mode == required).map(|(_, at)| at);
        if !mode_warning_due(previous, now) {
            return;
        }
        *last = Some((required, now));
        tracing::warn!(
            mode = required.as_str(),
            "a job type requires {} mode but no fleet node advertises it — a configuration \
             error, not a busy fleet: its launches queue until a node declares the mode in \
             WORKER_MODES (design #309 §5a). Fleet: {}",
            required.as_str(),
            fleet_modes(candidates)
        );
    }
}

/// How many matches [`ContainerBackend::find_file`] returns before it refuses
/// (design #490 D1a, `docs/reference/style.md` Tier 2 rule 3). Small because the
/// caller wants exactly one file and D1b already refuses "several": the count
/// only has to be wide enough to report a small ambiguity as a count.
pub const FIND_FILE_MATCHES_MAX: usize = 8;

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
    /// A worker-proxied node bounds the reply to `store::worker::MAX_COPY_FILE_BYTES`
    /// and errors above it (spec §3.1); a Docker-endpoint node reads in-process
    /// and is unbounded.
    async fn copy_file(
        &self,
        id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError>;
    /// Copy a file out of the container in bounded slices, so an output archive
    /// past one worker RPC reply still travels (spec §3.1); a file over
    /// `max_bytes` is refused with [`types::worker::COPY_FILE_TOO_LARGE`]
    /// rather than truncated.
    ///
    /// The default reads in one shot, correct for any in-process backend; a
    /// worker-proxying backend overrides it with a chunked read.
    async fn copy_file_chunked(
        &self,
        id: &ContainerId,
        path: &str,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        match self.copy_file(id, path).await? {
            Some(bytes) if bytes.len() > max_bytes => Err(BackendError::Other(
                types::worker::copy_file_too_large(path, bytes.len(), max_bytes),
            )),
            found => Ok(found),
        }
    }
    /// The wire paths of every file under `dir` whose file name is `name`
    /// (design #490 D1a), so a caller resolves a file it knows the name of
    /// rather than computing a path it has to guess the directory of.
    ///
    /// A **list**, since "several" must be distinguishable from "one", bounded
    /// by [`FIND_FILE_MATCHES_MAX`] — past which the scan refuses with
    /// [`types::worker::FIND_FILE_TOO_MANY`] — and node-local in both
    /// deployments, so only the resolved paths cross the wire and the bytes
    /// ride [`copy_file_chunked`](ContainerBackend::copy_file_chunked).
    async fn find_file(
        &self,
        id: &ContainerId,
        dir: &str,
        name: &str,
    ) -> Result<Vec<String>, BackendError>;
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
    /// How many managed containers are running across every node — the count a
    /// worker's `ping` reports (spec §3.1 slot source). Derived from
    /// [`list_managed_running`](ContainerBackend::list_managed_running) so the
    /// count and the listing can never disagree; a backend whose runtime answers
    /// it more cheaply overrides this.
    async fn managed_running_total(&self) -> Result<u32, BackendError> {
        Ok(self.list_managed_running().await?.len() as u32)
    }

    /// Live per-node fleet status for the platform config snapshot (spec §3.1):
    /// health and last-reported build version per node. Empty by default —
    /// backends without fleet-health tracking (e.g. the test fake) report
    /// nothing; Docker fills health, the worker fleet fills both. The dispatcher
    /// republishes this each scan so the UI sees live fleet state and deploy
    /// drift.
    fn fleet_status(&self) -> Vec<NodeStatus> {
        Vec::new()
    }

    /// Apply a worker's announce heartbeat (spec §3.1 dynamic registration):
    /// add it to the live fleet, or update the slot count / build version of an
    /// existing entry. The node's own report wins over a static `DOCKER_NODES`
    /// seed of the same name — but only when the observation's
    /// `(capacity_epoch, capacity_generation)` pair is at least the node's
    /// watermark, so a stale in-flight announce cannot undo a fresher
    /// observation (`types::capacity_applies`). Returns whether this *changed*
    /// fleet membership or capacity (a new node, or a slot-count change) — so
    /// the caller can log a join and only re-drain the launch queue when new
    /// capacity appeared.
    ///
    /// Default no-op returning `false`: only the worker fleet backend acts on
    /// announcements; single-node Docker, k8s, and the test fake ignore them.
    /// Called only from the dispatcher's single-writer actor, so implementations
    /// are the fleet's sole writer even though the method takes `&self`.
    fn register_worker(
        &self,
        _name: &str,
        _capacity: types::CapacityObservation,
        _version: Option<String>,
        _capabilities: Option<types::worker::NodeCapabilities>,
    ) -> bool {
        false
    }

    /// Whether this backend acts on worker announcements at all (spec §3.1
    /// dynamic registration). Only the worker fleet backend routes to announced
    /// nodes; single-node/multi-node Docker, k8s, and the test fake cannot, so
    /// they return `false` (the default) and the dispatcher drops stray announces
    /// rather than inserting a phantom node into the fleet roster that nothing can
    /// ever route to. Distinct from [`Self::register_worker`]'s bool, which
    /// conflates "no-op backend" with "membership unchanged".
    fn supports_dynamic_workers(&self) -> bool {
        false
    }

    /// Command one worker node's slot count (spec §3.1 operator capacity
    /// control): relay the operator's desired value to the daemon over
    /// `req.worker.{node}.set_slots`. The node is the authority — a value above
    /// its `slots_max` comes back `accepted: false` with a reason, which is a
    /// *reply*, not an error, and is terminal for that value.
    ///
    /// Never called on the dispatcher's actor turn: the caller spawns it, because
    /// state management is single-threaded by design and must not block on a node
    /// RPC. Not a placement input in either direction — this pushes intent out, it
    /// never reads capacity in.
    ///
    /// Default: `Unavailable`. Only the worker fleet backend has a daemon to
    /// command; a docker-endpoint node's capacity is static `DOCKER_NODES` config
    /// (design #293 §7), which is why a capacity edit against one is refused
    /// upstream rather than silently dropped here.
    async fn set_node_slots(
        &self,
        node: &str,
        _slots: u32,
    ) -> Result<types::worker::SetSlotsOk, BackendError> {
        Err(BackendError::Unavailable(format!(
            "node {node} has no worker daemon to command"
        )))
    }

    /// Mark an announced worker unschedulable after its heartbeat lapses (spec
    /// §3.1): placement skips it, but its already-running containers stay
    /// routable — they keep running and the poll-based `wait` re-attaches (spec
    /// §3.1 semantics unchanged). A later announce re-admits it. Default no-op.
    fn mark_worker_unschedulable(&self, _name: &str) {}

    /// Nodes whose most recent [`Self::list_managed_running`] call could not
    /// enumerate their containers (spec §3.1 occupancy). Unlike the §3.6 reap —
    /// which tolerates a node it can't list by skipping it — the *occupancy*
    /// snapshot must not present such a node as idle: `occupied: 0, available:
    /// true` is indistinguishable from a genuinely empty node and silently hid a
    /// prod outage (job/181) where a worker daemon reported no managed containers
    /// while two were live. Returning the node here lets the dispatcher show it
    /// out-of-service instead. Default empty: backends whose listing is
    /// all-or-nothing (single-node Docker, the test fake) surface a total failure
    /// through `list_managed_running`'s `Err` and never partially blank a node.
    fn occupancy_unavailable_nodes(&self) -> Vec<String> {
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
    /// The node's last self-refresh outcome (ticket #187), last reported by its
    /// ping. `None` for docker-endpoint nodes and workers that have not
    /// refreshed. Carried through to the platform snapshot so a failed refresh
    /// is durable, queryable fleet state.
    pub refresh_outcome: Option<types::worker::RefreshOutcome>,
    /// The slot count the scheduler is actually using for this node (spec §3.1
    /// slot source). `None` for a docker-endpoint node, whose capacity is static
    /// `DOCKER_NODES` config the roster already carries. For a worker node this
    /// is the observed number once the node has reported over either transport,
    /// and the boot seed until then — which is exactly what
    /// [`Self::capacity_source`] distinguishes.
    pub slots: Option<u32>,
    /// Provenance of [`Self::slots`] (design #293 §7/§8): the observation
    /// watermark, the ceiling the node named, and when it last reported —
    /// `ObservedCapacity::source()` turns the last of those into the
    /// `node` | `seed` chip the fleet view shows. `None` for a docker-endpoint
    /// node. A reachable worker whose `observed_at` is `None` is the
    /// denied-publish signature of the 2026-07-26 incident.
    pub capacity: Option<types::worker::ObservedCapacity>,
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
    /// The image a container task runs, and the mode selector every backend
    /// routes on (design #309 §1): `None` is a host task, which has no image.
    /// A backend that cannot serve the mode it selects refuses the launch —
    /// silently serving the other one would hide a placement bug.
    pub image: Option<String>,
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
    /// The job type's declared `runtime.env` (spec §1.1, design #373 P2): the
    /// toolchain the node realises and mounts beside the `image`'s userland.
    /// `None` for every job type that declares none, which launches exactly as
    /// it does today.
    pub runtime_env: Option<String>,
}

impl ContainerLaunchConfig {
    /// The execution mode this launch requires (design #309 §5a), read off the
    /// same `image` selector every backend routes on. One selector, so
    /// placement and the node it places onto cannot disagree.
    pub fn required_mode(&self) -> RuntimeMode {
        match self.image {
            Some(_) => RuntimeMode::Container,
            None => RuntimeMode::Host,
        }
    }
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
/// clone the job branch to `${CHUG_WORKSPACE:-/workspace}` ([`WORKSPACE_VAR`]),
/// cd, exec the original command. The clone's narrowing flags, their
/// server-side preconditions and the indirection are in
/// `docs/implementation-notes.md`.
pub fn bootstrap_cmd(original: &[String], runtime_env: Option<&str>) -> Vec<String> {
    let joined = original
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    let prelude = runtime_env.map(runtime_env_prelude).unwrap_or_default();
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "{prelude}WS=\"${{{WORKSPACE_VAR}:-{WIRE_WORKSPACE}}}\"; git clone --single-branch --filter=blob:none --branch \"$JOB_BRANCH\" \"$REPO_URL\" \"$WS\" && cd \"$WS\" && {{ git config core.hooksPath .githooks || true; }} && exec {joined}"
        ),
    ]
}

/// Where the bootstrap clones when nothing redirects it, and the first of the
/// two prefixes a task's paths are addressed by on the wire (design #322 §2).
/// A container backend takes it literally; [`host::HostBackend`] maps it.
pub const WIRE_WORKSPACE: &str = "/workspace";

/// The second wire prefix: injected credential files and node-local artifacts
/// (`InjectedFile::container_path`, spec §4.1).
pub const WIRE_CHUGGERNAUT: &str = "/chuggernaut";

/// Task-side variable naming the clone destination, set by a backend that maps
/// [`WIRE_WORKSPACE`] elsewhere (design #322 §2). One name, so the injection
/// site and the script consuming it cannot drift.
pub const WORKSPACE_VAR: &str = "CHUG_WORKSPACE";

/// Task-side variable naming the environment root the node resolved for a
/// launch's `runtime.env`: the realised store path for `nix:` (design #373 P2),
/// the selected `Developer/usr` for `xcode:` (design #322 §3). One name, so the
/// injection site and the script consuming it cannot drift.
pub const RUNTIME_ENV_PATH_VAR: &str = "CHUG_ENV_PATH";

/// The bootstrap's toolchain half: refuse the task when the declared
/// environment is absent, and otherwise prepend its `bin` to `PATH`. The
/// refusal is what makes a node that dropped `runtime_env` — an N-1 worker
/// (spec §14.1) — or handed over a path it never mounted loud rather than a
/// build against whatever the image carries.
fn runtime_env_prelude(env_ref: &str) -> String {
    let unrealised = shell_quote(&format!(
        "chuggernaut: this job type declares runtime.env {env_ref:?} and this node realised \
         none — refusing to run against the image's toolchain instead (design #373 P2)"
    ));
    let unmounted = shell_quote(&format!(
        "chuggernaut: this job type declares runtime.env {env_ref:?} and this node realised it \
         somewhere this container cannot see — refusing to run against the image's toolchain \
         instead (design #373 P2)"
    ));
    format!(
        "if [ -z \"${{{RUNTIME_ENV_PATH_VAR}:-}}\" ]; then echo {unrealised} >&2; exit 1; fi; \
         if [ ! -d \"${RUNTIME_ENV_PATH_VAR}\" ]; then echo {unmounted} >&2; exit 1; fi; \
         PATH=\"${RUNTIME_ENV_PATH_VAR}/bin:$PATH\"; export PATH; "
    )
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The clone the bootstrap emits, so a test that wants to run the rest of
    /// the script can stand something else in its place.
    const CLONE: &str = "git clone --single-branch --filter=blob:none --branch \"$JOB_BRANCH\" \"$REPO_URL\" \"$WS\"";

    fn cand<'a>(name: &'a str, running: i64, free: i64, index: usize) -> PlacementCandidate<'a> {
        PlacementCandidate {
            index,
            name,
            load: Some(NodeLoad { running, free }),
            modes: CONTAINER_ONLY_MODES,
        }
    }

    const HOST_AND_CONTAINER: &[RuntimeMode] = &[RuntimeMode::Container, RuntimeMode::Host];

    fn host_cand<'a>(
        name: &'a str,
        running: i64,
        free: i64,
        index: usize,
    ) -> PlacementCandidate<'a> {
        PlacementCandidate {
            modes: HOST_AND_CONTAINER,
            ..cand(name, running, free, index)
        }
    }

    /// A host launch takes the one node advertising the mode, even where the
    /// policy would otherwise prefer an incapable node (design #309 §5a) — and
    /// the same fleet places container work by load alone.
    #[test]
    fn capable_node_wins_over_an_incapable_one() {
        for policy in [PlacementPolicy::Busyness, PlacementPolicy::Headroom] {
            let nodes = [cand("air", 0, 4, 0), host_cand("nuc", 1, 1, 1)];
            assert_eq!(
                choose_placement(policy, &nodes, None, RuntimeMode::Host).unwrap(),
                1,
                "{policy:?}"
            );
            assert_eq!(
                choose_placement(policy, &nodes, None, RuntimeMode::Container).unwrap(),
                0,
                "{policy:?}"
            );
        }
    }

    /// The two `NoCapacity` messages are distinct (#309 §5a): a capable fleet
    /// with no free slot is the transient one, and a fleet no node of which
    /// serves the mode reads as the configuration error it is.
    #[test]
    fn full_capable_fleet_and_no_capable_node_read_differently() {
        let full = [cand("air", 4, 0, 0), host_cand("nuc", 1, 0, 1)];
        let err = choose_placement(PlacementPolicy::Busyness, &full, None, RuntimeMode::Host)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "no free slots on any node");

        let incapable = [cand("air", 0, 4, 0), cand("mini", 0, 2, 1)];
        let err = choose_placement(
            PlacementPolicy::Busyness,
            &incapable,
            None,
            RuntimeMode::Host,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.starts_with("no node advertises host mode")
                && err.contains("air serves container")
                && !err.contains("no free slots"),
            "{err}"
        );
    }

    /// An empty fleet is still the transient "no free slots" answer, never the
    /// configuration error: zero nodes is a fleet that has not registered yet
    /// (spec §3.1 zero-seed boot), not one that refuses the mode.
    #[test]
    fn empty_fleet_is_not_a_capability_error() {
        let err = choose_placement(PlacementPolicy::Busyness, &[], None, RuntimeMode::Host)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "no free slots on any node");
    }

    /// A pin onto a live node that cannot serve the mode is a hard `Launch`
    /// error whether or not it has a free slot, because only a config change
    /// clears it; a pin onto a node that is merely down or full stays transient.
    #[test]
    fn pin_to_an_incapable_live_node_is_a_hard_error() {
        let by = PlacementPolicy::Busyness;
        let nodes = [cand("air", 0, 4, 0), host_cand("nuc", 0, 1, 1)];
        let err = choose_placement(by, &nodes, Some("air"), RuntimeMode::Host)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("launch failed: placement pinned to node air")
                && err.contains("serves container and not host mode"),
            "{err}"
        );
        assert_eq!(
            choose_placement(by, &nodes, Some("nuc"), RuntimeMode::Host).unwrap(),
            1
        );

        let full = [cand("air", 4, 0, 0)];
        let err = choose_placement(by, &full, Some("air"), RuntimeMode::Host)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("launch failed: placement pinned to node air")
                && err.contains("serves container and not host mode"),
            "{err}"
        );
        assert_eq!(
            choose_placement(by, &full, Some("air"), RuntimeMode::Container)
                .unwrap_err()
                .to_string(),
            "no free slots on node air"
        );

        let down = [PlacementCandidate {
            index: 0,
            name: "air",
            load: None,
            modes: CONTAINER_ONLY_MODES,
        }];
        let err = choose_placement(by, &down, Some("air"), RuntimeMode::Host)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "no free slots on node air");
    }

    /// The fleet-wide warning is bounded to one line per interval, and the
    /// first observation always fires (design #309 §5a, #293 §8's shape).
    #[test]
    fn mode_warning_is_bounded_to_one_per_interval() {
        let now = Instant::now();
        assert!(mode_warning_due(None, now));
        assert!(!mode_warning_due(Some(now), now));
        assert!(mode_warning_due(now.checked_sub(MODE_WARN_INTERVAL), now));
    }

    /// The warning is silent on a fleet that serves the mode and on one with no
    /// nodes at all — only a fleet that answers and cannot serve is a finding.
    #[test]
    fn mode_warning_observes_only_an_incapable_fleet() {
        let warnings = ModeWarnings::default();
        warnings.observe(&[], RuntimeMode::Host);
        assert!(warnings.0.lock().unwrap().is_none());

        let capable = [host_cand("nuc", 0, 1, 0)];
        warnings.observe(&capable, RuntimeMode::Host);
        assert!(warnings.0.lock().unwrap().is_none());

        let incapable = [cand("air", 0, 4, 0)];
        warnings.observe(&incapable, RuntimeMode::Host);
        assert_eq!(
            warnings.0.lock().unwrap().map(|(mode, _)| mode),
            Some(RuntimeMode::Host)
        );
    }

    /// A container launch requires container mode and a host launch requires
    /// host mode, read off `image` — the selector the backends route on (#479).
    #[test]
    fn required_mode_follows_the_image_selector() {
        let mut config = ContainerLaunchConfig {
            image: Some("img".into()),
            cmd: vec!["run".into()],
            env: HashMap::new(),
            files: vec![],
            cpu_limit: None,
            memory_limit: None,
            node: None,
            runtime_env: None,
        };
        assert_eq!(config.required_mode(), RuntimeMode::Container);
        config.image = None;
        assert_eq!(config.required_mode(), RuntimeMode::Host);
    }

    /// Busyness (the default, §3.1): fewest running wins outright — an idle
    /// small node beats a busy big node regardless of slot counts. The air/nuc
    /// scenario from #153: air (1 running / 3 free) vs nuc (0 running / 2 free)
    /// → nuc, even though air has more free slots.
    #[test]
    fn busyness_idle_small_beats_busy_big() {
        let by = PlacementPolicy::Busyness;
        let nodes = [cand("air", 1, 3, 0), cand("nuc", 0, 2, 1)];
        assert_eq!(
            choose_placement(by, &nodes, None, RuntimeMode::Container).unwrap(),
            1
        );
        assert_eq!(
            choose_placement(
                PlacementPolicy::Headroom,
                &nodes,
                None,
                RuntimeMode::Container
            )
            .unwrap(),
            0
        );
    }

    /// Busyness tie-break chain: equal running → most free wins → then name.
    #[test]
    fn busyness_ties_break_by_free_then_name() {
        let by = PlacementPolicy::Busyness;
        let nodes = [cand("air", 2, 2, 0), cand("nuc", 2, 4, 1)];
        assert_eq!(
            choose_placement(by, &nodes, None, RuntimeMode::Container).unwrap(),
            1
        );
        let nodes = [cand("nuc", 2, 3, 0), cand("air", 2, 3, 1)];
        assert_eq!(
            choose_placement(by, &nodes, None, RuntimeMode::Container).unwrap(),
            1
        );
    }

    /// The #60 rule holds under both policies: a 0-slot (or full) node is never
    /// chosen even when it has the fewest running jobs, and an out-of-service
    /// node (`load: None`) is skipped.
    #[test]
    fn zero_slot_and_out_of_service_skipped_under_both_policies() {
        for policy in [PlacementPolicy::Busyness, PlacementPolicy::Headroom] {
            let nodes = [cand("zero", 0, 0, 0), cand("nuc", 3, 1, 1)];
            assert_eq!(
                choose_placement(policy, &nodes, None, RuntimeMode::Container).unwrap(),
                1,
                "{policy:?}"
            );

            let nodes = [
                PlacementCandidate {
                    index: 0,
                    name: "down",
                    load: None,
                    modes: CONTAINER_ONLY_MODES,
                },
                cand("nuc", 1, 1, 1),
            ];
            assert_eq!(
                choose_placement(policy, &nodes, None, RuntimeMode::Container).unwrap(),
                1,
                "{policy:?}"
            );

            let nodes = [cand("full", 4, 0, 0)];
            let err = choose_placement(policy, &nodes, None, RuntimeMode::Container)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("no free slots on any node"),
                "{policy:?}: {err}"
            );
        }
    }

    /// A runtime that answers exactly the two ops these tests reach — the
    /// managed-container listing and a single-file read — so each exercises what
    /// the trait *derives* from them. Every other op is out of reach.
    struct StubBackend {
        listing: Result<Vec<RunningContainer>, String>,
        file: Option<Vec<u8>>,
    }

    impl StubBackend {
        fn listing(listing: Result<Vec<RunningContainer>, String>) -> Self {
            Self {
                listing,
                file: None,
            }
        }

        fn file(file: Option<Vec<u8>>) -> Self {
            Self {
                listing: Ok(Vec::new()),
                file,
            }
        }
    }

    #[async_trait]
    impl ContainerBackend for StubBackend {
        async fn launch(&self, _: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
            unimplemented!()
        }
        async fn wait(&self, _: &ContainerId) -> Result<i32, BackendError> {
            unimplemented!()
        }
        async fn kill(&self, _: &ContainerId) -> Result<(), BackendError> {
            unimplemented!()
        }
        async fn inspect(&self, _: &ContainerId) -> Result<Option<ContainerStatus>, BackendError> {
            unimplemented!()
        }
        async fn copy_file(
            &self,
            _: &ContainerId,
            _: &str,
        ) -> Result<Option<Vec<u8>>, BackendError> {
            Ok(self.file.clone())
        }
        async fn find_file(
            &self,
            _: &ContainerId,
            _: &str,
            _: &str,
        ) -> Result<Vec<String>, BackendError> {
            unimplemented!()
        }
        async fn logs(&self, _: &ContainerId) -> Result<Vec<u8>, BackendError> {
            unimplemented!()
        }
        async fn logs_tail(&self, _: &ContainerId, _: u64) -> Result<LogTail, BackendError> {
            unimplemented!()
        }
        async fn remove(&self, _: &ContainerId) -> Result<(), BackendError> {
            unimplemented!()
        }
        async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
            unimplemented!()
        }
        async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError> {
            self.listing
                .clone()
                .map_err(|e| BackendError::Unavailable(e.clone()))
        }
    }

    fn running(task: u64) -> RunningContainer {
        RunningContainer {
            id: format!("w1/c{task}"),
            project: Some("acme/chug".into()),
            job: Some(7),
            task: Some(task),
        }
    }

    /// The trait's derived running count (design #322 W1): a backend that
    /// implements only the listing gets a total that cannot disagree with it,
    /// and a listing failure stays a failure rather than reading as an idle node
    /// (job/181).
    #[tokio::test]
    async fn managed_running_total_defaults_to_the_listing() {
        let empty = StubBackend::listing(Ok(Vec::new()));
        assert_eq!(empty.managed_running_total().await.unwrap(), 0);

        let busy = StubBackend::listing(Ok(vec![running(1), running(2), running(3)]));
        assert_eq!(
            busy.managed_running_total().await.unwrap(),
            busy.list_managed_running().await.unwrap().len() as u32
        );

        let blind = StubBackend::listing(Err("node w1 unreachable".into()));
        let err = blind.managed_running_total().await.unwrap_err();
        assert!(err.to_string().contains("unreachable"), "{err}");
    }

    /// The chunked read's ceiling is a refusal at the boundary, never a cut
    /// (design #362): a partial archive carries nothing, so an over-band file
    /// comes back as the named error and no bytes at all.
    #[tokio::test]
    async fn copy_file_chunked_refuses_past_the_cap_and_never_truncates() {
        let id = ContainerId::from("w1/c1");
        let path = "/workspace/chug-output.tar.gz";
        let cap = 1024;

        for len in [cap - 1, cap] {
            let got = StubBackend::file(Some(vec![7u8; len]))
                .copy_file_chunked(&id, path, cap)
                .await
                .unwrap();
            assert_eq!(got, Some(vec![7u8; len]), "{len} bytes must survive whole");
        }

        let err = StubBackend::file(Some(vec![7u8; cap + 1]))
            .copy_file_chunked(&id, path, cap)
            .await
            .unwrap_err()
            .to_string();
        for expected in [types::worker::COPY_FILE_TOO_LARGE, path, "1025"] {
            assert!(err.contains(expected), "missing {expected}: {err}");
        }
    }

    /// No archive is the ordinary case — a work container that produced none is
    /// silent, not an error, so nothing anywhere reads it as a failure.
    #[tokio::test]
    async fn copy_file_chunked_absent_is_not_an_error() {
        let got = StubBackend::file(None)
            .copy_file_chunked(&ContainerId::from("w1/c1"), "/workspace/nope.tar.gz", 1024)
            .await;
        assert_eq!(got.unwrap(), None);
    }

    #[test]
    fn bootstrap_wraps_and_quotes() {
        let cmd = bootstrap_cmd(&["claude".into(), "-p".into(), "do the thing".into()], None);
        assert_eq!(cmd[0], "sh");
        assert_eq!(cmd[1], "-c");
        assert!(cmd[2].starts_with(&format!(
            "WS=\"${{{WORKSPACE_VAR}:-{WIRE_WORKSPACE}}}\"; git clone "
        )));
        assert!(cmd[2].ends_with("exec claude -p 'do the thing'"));
        assert!(
            cmd[2].contains("git config core.hooksPath .githooks"),
            "{}",
            cmd[2]
        );
    }

    /// The wire path is resolved by the task's own shell (design #322 §2), and
    /// **unset is today's behaviour**: the clone lands at [`WIRE_WORKSPACE`] and
    /// the cwd is the same path, so no container task moves.
    #[test]
    fn bootstrap_clones_where_the_workspace_variable_says() {
        let script = bootstrap_cmd(&["pwd".into()], None)[2]
            .replace(CLONE, "printf '%s ' \"$WS\"")
            .replace("cd \"$WS\"", "cd /");
        let destination = |env: &str| {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{env}{script}"))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(
            destination(""),
            WIRE_WORKSPACE,
            "an unset {WORKSPACE_VAR} is byte-for-byte today's clone destination"
        );
        assert_eq!(
            destination(&format!(
                "{WORKSPACE_VAR}=/var/tasks/host-7/workspace; export {WORKSPACE_VAR}; "
            )),
            "/var/tasks/host-7/workspace",
            "a backend that maps the wire path redirects both the clone and the cwd"
        );
    }

    /// A job type declaring `runtime.env` (design #373 P2) runs against the
    /// realised environment or NOT AT ALL: with the node's injection the env's
    /// `bin` leads `PATH`, and without it the task refuses instead of silently
    /// building against whatever the image carries.
    #[test]
    fn bootstrap_puts_a_declared_env_on_path_and_refuses_without_one() {
        let task: Vec<String> = vec!["sh".into(), "-c".into(), "echo $PATH".into()];
        let cmd = bootstrap_cmd(&task, Some("nix:.#chug-mobile"));
        assert!(cmd[2].contains("nix:.#chug-mobile"), "{}", cmd[2]);
        assert!(cmd[2].contains(RUNTIME_ENV_PATH_VAR), "{}", cmd[2]);

        let undeclared = bootstrap_cmd(&task, None);
        assert!(
            undeclared[2].starts_with("WS=") && !undeclared[2].contains(RUNTIME_ENV_PATH_VAR),
            "a job type declaring no env is bootstrapped exactly as it is today: {}",
            undeclared[2]
        );

        let script = cmd[2].clone();
        let run = |env: &str| {
            let body = script.replace(CLONE, "true").replace("cd \"$WS\"", "cd /");
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{env}{body}"))
                .output()
                .unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )
        };

        let realised = std::env::temp_dir().join(format!("chug-env-{}", std::process::id()));
        std::fs::create_dir_all(&realised).unwrap();
        let (ok, out, _) = run(&format!(
            "{RUNTIME_ENV_PATH_VAR}={}; export {RUNTIME_ENV_PATH_VAR}; ",
            realised.display()
        ));
        assert!(ok, "a realised env runs the task");
        assert!(
            out.starts_with(&format!("{}/bin:", realised.display())),
            "the env leads PATH: {out}"
        );
        let _ = std::fs::remove_dir_all(&realised);

        let (ok, out, err) = run("");
        assert!(!ok, "a missing env refuses the task");
        assert_eq!(out, "", "nothing ran");
        assert!(err.contains("nix:.#chug-mobile"), "{err}");

        let (ok, out, err) = run(&format!(
            "{RUNTIME_ENV_PATH_VAR}=/nix/store/no-such-closure; export {RUNTIME_ENV_PATH_VAR}; "
        ));
        assert!(!ok, "a path this container cannot see refuses the task");
        assert_eq!(out, "", "nothing ran");
        assert!(err.contains("cannot see"), "{err}");
    }

    /// The `.githooks` opt-in must be non-fatal while the clone stays fatal:
    /// losing the hook costs feedback, but exec'ing outside a clean /workspace
    /// costs the attempt (ticket A6).
    #[test]
    fn bootstrap_hooks_path_failure_still_execs_but_clone_failure_does_not() {
        let script = bootstrap_cmd(&["echo".into(), "started".into()], None)[2].clone();
        let run = |s: String| {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&s)
                .output()
                .unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )
        };

        let hooks_fail = script
            .replace(CLONE, "true")
            .replace("cd \"$WS\"", "cd /")
            .replace("git config core.hooksPath .githooks", "false");
        assert_eq!(run(hooks_fail), (true, "started".into()));

        let clone_fail = script.replace(CLONE, "false").replace("cd \"$WS\"", "cd /");
        assert_eq!(run(clone_fail), (false, String::new()));
    }

    /// The cursor slice underpinning live output: monotonic across polls, empty
    /// at/past the end, and capped so a worker-proxied reply stays bounded.
    #[test]
    fn log_tail_slice_is_monotonic_and_capped() {
        let full = b"line-0\nline-1\nline-2\n";
        let t = LogTail::slice(full, 0);
        assert_eq!(t.offset, full.len() as u64);
        assert_eq!(t.data, full);
        let t = LogTail::slice(full, 7);
        assert_eq!(t.data, b"line-1\nline-2\n");
        assert_eq!(t.offset, full.len() as u64);
        let t = LogTail::slice(full, full.len() as u64);
        assert!(t.data.is_empty());
        assert_eq!(t.offset, full.len() as u64);
        let t = LogTail::slice(full, 9_999);
        assert!(t.data.is_empty());
        assert_eq!(t.offset, full.len() as u64);

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
        let cmd = bootstrap_cmd(&["true".into()], None);
        assert!(cmd[2].contains("--single-branch"), "{}", cmd[2]);
        assert!(cmd[2].contains("--filter=blob:none"), "{}", cmd[2]);
        assert!(
            cmd[2].contains("--branch \"$JOB_BRANCH\" \"$REPO_URL\" \"$WS\""),
            "{}",
            cmd[2]
        );
    }
}

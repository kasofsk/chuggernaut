//! The worker daemon: subscribe `req.worker.{node}.>`, execute each op
//! against the local Docker daemon, reply with the `types::worker` envelope.
//!
//! One worker per node name is the deployment contract. Overlap during a
//! restart is harmless — request-reply takes the first reply and every op is
//! idempotent from the dispatcher's view (`kill` swallows already-exited,
//! `inspect`/`logs` are reads, double `launch` cannot happen because the
//! dispatcher sends each launch once).
//!
//! Containers survive daemon restarts: the dispatcher's poll-based `wait`
//! (fleet backend) re-attaches via `inspect` on the existing container id.

use crate::agent_cli::AgentCli;
use crate::capacity::Capacity;
use crate::config::{WorkerConfig, WorkerMode};
use crate::nix::{NIX_ENV_PREFIX, NixRoots, REAP_AGE_MIN, Realised, flake_installable};
use crate::xcode::{DEVELOPER_DIR_VAR, XCODE_ENV_PREFIX, XcodeInstall, Xcodes};
use container::docker::{DockerBackend, DockerNodeConfig, KvmGrant};
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
    InjectedFile, RunningContainer,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use store::worker::{
    MAX_COPY_FILE_BYTES, copy_file_over_bound, copy_file_over_size, encode_reply, op_from_subject,
};
use store::{NatsStore, StoreError};
use types::worker::{
    ContainerRef, CopyFileChunkOk, CopyFileChunkRequest, CopyFileOk, CopyFileRequest, FileSource,
    FindFileOk, FindFileRequest, InspectOk, LaunchOk, LogsOk, LogsTailOk, LogsTailRequest, PingOk,
    REFRESH_STAGE_CANCELLED, RefreshCancelOk, RefreshCancelRequest, RefreshOk, RefreshOutcome,
    RefreshProgress, RefreshRequest, RefreshResult, SetSlotsOk, SetSlotsRequest, WireStatus,
    WorkerError, WorkerLaunchRequest, WorkerReply, b64_decode, b64_encode,
};

/// Logs are tailed to fit the reply under NATS's 1MB max_payload after
/// base64 + JSON overhead.
const LOGS_CAP: usize = 700 * 1024;

/// Concurrent op handlers — a slow launch must not starve inspect polls.
const MAX_INFLIGHT: usize = 16;

/// How long the swap step waits for in-flight launches to finish before it
/// gives up and aborts (spec §3.1 drain guarantee): a launch turns into a
/// running container in seconds, so this only ever waits out the ones already
/// in flight when quiescing began.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of `worker-refresh.sh`'s combined stdout+stderr the daemon keeps as
/// the failure tail (deploy #212): the last N lines, then the last M bytes of
/// those. Bounds both the RPC failure report and the durable job record so a
/// huge build log cannot bloat the ping reply (NATS 1MB) or the deploy leg.
const REFRESH_TAIL_LINES: usize = 50;
const REFRESH_TAIL_BYTES: usize = 4096;

/// The line prefix `worker-refresh.sh` stamps on a phase transition (ticket
/// #253). The script names its own phases — the daemon only reads them — so the
/// deploy log's per-phase story stays in one place, next to the work it
/// describes.
const REFRESH_PHASE_MARKER: &str = "worker-refresh: phase ";

/// How many recent script lines the daemon carries in `ping.refresh_progress`
/// (ticket #253). Far smaller than the failure tail: this rides EVERY ping of an
/// in-flight refresh (one every few seconds while the deploy waits), so it is
/// sized to answer "what was it last doing" and nothing more.
const REFRESH_PROGRESS_LINES: usize = 5;
const REFRESH_PROGRESS_LINE_BYTES: usize = 300;

/// How long a cancelled build (ticket #254) gets to die on SIGTERM before the
/// daemon escalates to SIGKILL. Generous enough for `worker-refresh.sh`'s EXIT
/// trap to drop the staged tags and prune, short enough that the deploy's own
/// cancel drain does not wait on it.
const REFRESH_CANCEL_GRACE: Duration = Duration::from_secs(20);

/// Guards the daemon self-refresh swap window (spec §3.1 drain guarantee).
/// Refreshing must never interrupt in-flight job containers, and the daemon
/// must not replace itself *between accepting a launch and the container
/// existing* — so once the swap window opens, new launches are refused
/// (retryably) and the swap waits for accepted-but-not-yet-created launches to
/// finish. Job containers themselves are untouched by the daemon replace: the
/// dispatcher's poll-based `wait` re-attaches (spec §3.1).
#[derive(Default)]
struct RefreshGate {
    /// Set for the swap window; refuses new launches. `SeqCst` so it orders
    /// against `inflight` in the launch/drain handshake below.
    quiescing: AtomicBool,
    /// Launches accepted whose container may not yet exist.
    inflight: AtomicUsize,
    /// One refresh at a time.
    refreshing: AtomicBool,
    /// A cancel landed for the refresh in flight (ticket #254). Checked at every
    /// step boundary of `run_refresh`, so a cancelled refresh stops at the next
    /// one instead of only when its build process notices the signal.
    cancelled: AtomicBool,
    /// The swap window has opened: the daemon is being replaced and the node
    /// WILL come up on the new images. Past this point a cancel is refused —
    /// nothing can un-swap a node, and saying otherwise would put a lie in the
    /// deploy leg.
    swapping: AtomicBool,
}

/// Held for the accept→container-exists window of one launch; decrements the
/// in-flight count on drop so the swap can tell when it is safe to proceed.
struct LaunchPermit<'a> {
    gate: &'a RefreshGate,
}

impl Drop for LaunchPermit<'_> {
    fn drop(&mut self) {
        self.gate.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl RefreshGate {
    /// Reserve a launch slot, or `None` when the node is quiescing for a swap.
    /// Increment-then-recheck closes the race with [`Self::quiesce`] +
    /// [`Self::drained`]: either drain observes this increment and waits, or
    /// this observes the flag and backs off — the swap never races a launch.
    fn try_launch(&self) -> Option<LaunchPermit<'_>> {
        if self.quiescing.load(Ordering::SeqCst) {
            return None;
        }
        self.inflight.fetch_add(1, Ordering::SeqCst);
        if self.quiescing.load(Ordering::SeqCst) {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(LaunchPermit { gate: self })
    }

    /// Claim the sole refresh slot; `false` if one is already running.
    fn begin_refresh(&self) -> bool {
        !self.refreshing.swap(true, Ordering::SeqCst)
    }

    /// Open the swap window: refuse new launches.
    fn quiesce(&self) {
        self.quiescing.store(true, Ordering::SeqCst);
    }

    /// Mark the in-flight refresh cancelled (ticket #254). Returns whether the
    /// cancel is authoritative — `false` means the swap window had already
    /// opened, so the node is going onto the new images regardless.
    ///
    /// The store-then-read pairs with [`Self::begin_swap`]'s read-then-store: at
    /// most one of the two can win, and this one only claims a win when it can
    /// prove the swap had not started. The error is therefore always in the safe
    /// direction — a cancel may under-report, never over-report.
    fn cancel(&self) -> bool {
        self.cancelled.store(true, Ordering::SeqCst);
        !self.swapping.load(Ordering::SeqCst)
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Open the swap window unless a cancel already landed; `false` ⇒ the
    /// refresh was cancelled and must not swap.
    fn begin_swap(&self) -> bool {
        if self.cancelled.load(Ordering::SeqCst) {
            return false;
        }
        self.swapping.store(true, Ordering::SeqCst);
        if self.cancelled.load(Ordering::SeqCst) {
            self.swapping.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Abort a refresh that failed before the swap: reopen launches. Clears the
    /// cancel flags too — they belong to the refresh that just ended, and a
    /// leftover `cancelled` would poison the node's NEXT refresh.
    fn abort(&self) {
        self.quiescing.store(false, Ordering::SeqCst);
        self.cancelled.store(false, Ordering::SeqCst);
        self.swapping.store(false, Ordering::SeqCst);
        self.refreshing.store(false, Ordering::SeqCst);
    }

    fn drained(&self) -> bool {
        self.inflight.load(Ordering::SeqCst) == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerRunError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("backend: {0}")]
    Backend(#[from] BackendError),
    #[error("config: {0}")]
    Config(String),
}

struct WorkerState {
    node: String,
    /// The node's execution backend behind the trait (design #322 W1), so every
    /// op handler below routes through one interface and a second runtime is a
    /// construction-time choice rather than a branch per op.
    backend: Arc<dyn ContainerBackend>,
    /// The node's live capacity (spec §3.1 slot source): the one number reported
    /// over both transports, and the ceiling `set_slots` is validated against.
    /// Shared with the announce loop — one owner, two transports.
    capacity: Arc<Capacity>,
    /// Notified when an adopted `set_slots` must be announced NOW rather than at
    /// the next [`ANNOUNCE_INTERVAL`] tick: an operator's capacity change belongs
    /// in the fleet view immediately, not up to 15s later.
    announce_now: Arc<tokio::sync::Notify>,
    /// name → bytes, loaded once at startup.
    artifacts: HashMap<String, Vec<u8>>,
    /// name → sha256 hex, reported in ping.
    artifact_hashes: HashMap<String, String>,
    version: String,
    /// Node-local build caching is on (`WORKER_CACHE_DIR` was set). When true,
    /// the backend bind-mounts the cache and every launch gets sccache env.
    cache_enabled: bool,
    /// The node's KVM grant (design #367 A1) when passthrough is on, so the
    /// launch env is injected for exactly the launches the backend gives the
    /// device and mounts to — one allow-list decision, two consumers.
    kvm: Option<KvmGrant>,
    /// The node's nix realise and per-task GC roots (design #373 P1); `None`
    /// (`WORKER_NIX_GCROOTS_DIR` unset) ⇒ neither, and today's behavior. Shared
    /// with the reaper task, which is the crash backstop for the roots this
    /// daemon holds.
    nix: Option<Arc<NixRoots>>,
    /// Which task each rooted container belongs to, so `remove` — the op
    /// `platform-ops`'s `dispose` drives — drops exactly that task's root.
    /// Bounded by [`NIX_ROOTED_MAX`]; the reaper covers anything a crash loses.
    nix_rooted: std::sync::Mutex<HashMap<ContainerId, String>>,
    /// This node can serve a launch as a host process (`WORKER_MODES`, #309
    /// §1), which is what makes such a task die with the daemon a refresh
    /// replaces (design #440 D4). False ⇒ a container-only node, whose work
    /// survives the swap by construction and whose refresh path is untouched.
    host_mode: bool,
    /// The Xcodes this node discovered at boot (design #322 W4), which a host
    /// launch's `xcode:<version>` resolves against. Empty on every node that
    /// serves no host mode, and on a Mac with none installed.
    xcodes: Xcodes,
    /// What this node advertises it can do (design #309 §4), derived once from
    /// `WORKER_MODES` and the discovery above at start. Reported on both
    /// transports, so the pull and the push halves cannot disagree.
    capabilities: types::worker::NodeCapabilities,
    /// Node-local build+swap script for self-refresh (spec §3.1); `None` ⇒
    /// refresh requests are rejected as unconfigured.
    refresh_script: Option<PathBuf>,
    /// Git URL the refresh fetches its build context from (`WORKER_REFRESH_GIT_URL`);
    /// `None` (unset/empty) ⇒ no git credential.
    refresh_git_url: Option<String>,
    /// The node's git private key (`WORKER_GIT_KEY`); its absence is the other
    /// half of "no git credential".
    refresh_git_key: PathBuf,
    /// Drain guarantee for the self-refresh swap window.
    refresh: RefreshGate,
    /// The node's last self-refresh outcome (ticket #187), reported in `ping` so
    /// a failed refresh is durable platform state instead of a node-local
    /// `tracing::error`. A successful refresh swaps this daemon away, so what a
    /// surviving daemon reports is the failure story.
    refresh_outcome: std::sync::Mutex<Option<RefreshOutcome>>,
    /// Live progress of the refresh currently running (ticket #253), reported in
    /// `ping` so the deploy's wait loop relays per-phase progress instead of
    /// sitting silent for the whole build window. `None` between refreshes.
    refresh_progress: std::sync::Mutex<Option<RefreshProgressState>>,
    /// Process GROUP of the refresh script currently running (ticket #254), so a
    /// `refresh_cancel` can signal the whole build — the shell AND the `docker
    /// build` it is blocked on — rather than orphaning the build under a dead
    /// shell. `None` whenever no script child is running.
    refresh_pgid: std::sync::Mutex<Option<i32>>,
}

/// The daemon-side half of [`RefreshProgress`]: same story, but holding the
/// phase start as a local `Instant` so the reported "seconds in phase" is
/// measured against this node's own monotonic clock and carries no cross-host
/// skew. Converted to the wire type at ping time.
struct RefreshProgressState {
    to_sha: String,
    phase: String,
    phase_since: Instant,
    /// Last [`REFRESH_PROGRESS_LINES`] script lines, oldest first.
    recent: std::collections::VecDeque<String>,
}

impl RefreshProgressState {
    fn new(to_sha: &str, phase: &str) -> Self {
        Self {
            to_sha: to_sha.to_string(),
            phase: phase.to_string(),
            phase_since: Instant::now(),
            recent: std::collections::VecDeque::with_capacity(REFRESH_PROGRESS_LINES),
        }
    }

    fn wire(&self) -> RefreshProgress {
        RefreshProgress {
            to_sha: self.to_sha.clone(),
            phase: self.phase.clone(),
            phase_secs: self.phase_since.elapsed().as_secs(),
            recent: self.recent.iter().cloned().collect(),
        }
    }
}

/// The phase name in a `worker-refresh: phase <name>` marker line, if this line
/// is one. Pure over its input so the marker contract between the script and the
/// daemon is unit-tested without a subprocess.
fn refresh_phase_marker(line: &str) -> Option<&str> {
    let phase = line.trim().strip_prefix(REFRESH_PHASE_MARKER)?.trim();
    (!phase.is_empty()).then_some(phase)
}

/// Fold one line of script output into the live progress: it always joins the
/// bounded recent-lines window, and a phase marker additionally advances the
/// phase (restarting the in-phase clock). No-op when no refresh is in flight.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
fn refresh_progress_note(slot: &std::sync::Mutex<Option<RefreshProgressState>>, line: &str) {
    let mut guard = slot.lock().unwrap();
    let Some(progress) = guard.as_mut() else {
        return;
    };
    if let Some(phase) = refresh_phase_marker(line) {
        progress.phase = phase.to_string();
        progress.phase_since = Instant::now();
    }
    if progress.recent.len() == REFRESH_PROGRESS_LINES {
        progress.recent.pop_front();
    }
    progress
        .recent
        .push_back(line.chars().take(REFRESH_PROGRESS_LINE_BYTES).collect());
}

/// Advance the phase from the DAEMON's own side of the refresh (the drain and
/// swap stages it runs between script invocations), so the deploy log never
/// shows a stale build phase while the daemon is quiescing.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
fn refresh_progress_phase(slot: &std::sync::Mutex<Option<RefreshProgressState>>, phase: &str) {
    let mut guard = slot.lock().unwrap();
    if let Some(progress) = guard.as_mut() {
        progress.phase = phase.to_string();
        progress.phase_since = Instant::now();
    }
}

/// Why a refresh cannot even be *attempted*: the node has no git credential to
/// fetch the build context (spec §3.1). Returns `Some(reason)` when the fetch
/// would fail before building — `WORKER_REFRESH_GIT_URL` unset, or its key file
/// missing — so the daemon reports the skip in the RPC reply instead of
/// accepting and no-oping silently in the background (the #114 failure: a deploy
/// that "succeeds" in 41s having refreshed nothing). Pure over its inputs so the
/// skip decision is unit-tested without a backend, NATS, or Docker.
fn refresh_skip_reason(git_url: Option<&str>, git_key: &Path) -> Option<String> {
    if git_url.is_none() {
        return Some("no git credential: WORKER_REFRESH_GIT_URL is unset".to_string());
    }
    if !git_key.exists() {
        return Some(format!(
            "no git credential: key {} does not exist",
            git_key.display()
        ));
    }
    None
}

/// Why a refresh must not proceed: this node is serving host work that the
/// daemon swap would kill (design #440 D4). Pure over the listing so the
/// `refresh` precondition and the swap-boundary re-check refuse in the same
/// words, and so the id an operator has to act on is always named.
fn host_work_refusal(node: &str, live: &[RunningContainer]) -> Option<String> {
    if live.is_empty() {
        return None;
    }
    let tasks = live
        .iter()
        .map(|c| match (c.job, c.task) {
            (Some(job), Some(task)) => format!("{} (job {job} task {task})", c.id),
            _ => c.id.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "node {node} is running host work that would not survive the daemon swap: {tasks} — a \
         host task is not a container and the drain guarantee (spec §3.1) does not cover it, so \
         the refresh is refused rather than silently killing it (design #440 D4); retry once the \
         task finishes, or drain the node first"
    ))
}

/// The reason this node must refuse a refresh right now, or `None` (design #440
/// D4). Asked at accept and again at the swap boundary, because a host task can
/// be launched while the build between them runs.
async fn host_work_check(state: &WorkerState) -> Option<String> {
    if !state.host_mode {
        return None;
    }
    match state.backend.list_managed_running().await {
        Ok(live) => host_work_refusal(&state.node, &live),
        Err(e) => Some(format!(
            "node {} cannot list its host tasks ({e}), so it cannot tell whether a swap would \
             kill one — refusing the refresh is the only answer that cannot lose work (design \
             #440 D4)",
            state.node
        )),
    }
}

/// Run the daemon until ctrl-c. Containers it launched keep running after
/// shutdown; the dispatcher re-attaches.
#[allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
pub async fn run(config: WorkerConfig) -> Result<(), WorkerRunError> {
    let store = match &config.nats_creds {
        Some(path) => {
            let creds = tokio::fs::read_to_string(path)
                .await
                .map_err(|e| WorkerRunError::Config(format!("reading {}: {e}", path.display())))?;
            NatsStore::connect_with_creds(&config.nats_url, &creds).await?
        }
        None => NatsStore::connect(&config.nats_url).await?,
    };

    let agent_cli = discover_agent_cli(&config.node, &config.modes);
    let backend = local_backend(&config, &agent_cli).await?;
    let cache_enabled = config.cache_dir.is_some();
    let kvm = kvm_grant(&config);
    let nix = nix_roots(&config)?;

    let mut artifacts = HashMap::new();
    match tokio::fs::read(&config.channel_binary).await {
        Ok(bytes) => {
            artifacts.insert(types::worker::ARTIFACT_CHANNEL.to_string(), bytes);
        }
        Err(e) => tracing::warn!(
            path = %config.channel_binary.display(),
            "channel binary unavailable — launches referencing it will fail: {e}"
        ),
    }
    let artifact_hashes = artifacts
        .iter()
        .map(|(k, v)| (k.clone(), format!("{:x}", Sha256::digest(v))))
        .collect();

    let capacity = run_capacity(&config);
    let announce_now = Arc::new(tokio::sync::Notify::new());
    let xcodes = discover_xcodes(&config.node, &config.modes);

    let state = Arc::new(WorkerState {
        node: config.node.clone(),
        backend,
        capacity: capacity.clone(),
        announce_now: announce_now.clone(),
        artifacts,
        artifact_hashes,
        version: version_string(),
        cache_enabled,
        kvm,
        nix: nix.clone(),
        nix_rooted: std::sync::Mutex::new(HashMap::new()),
        host_mode: serves_host(&config.modes),
        capabilities: node_capabilities(&config.modes, &xcodes, &agent_cli),
        xcodes,
        refresh_script: config.refresh_script.clone(),
        refresh_git_url: config.refresh_git_url.clone(),
        refresh_git_key: config.refresh_git_key.clone(),
        refresh: RefreshGate::default(),
        refresh_outcome: std::sync::Mutex::new(None),
        refresh_progress: std::sync::Mutex::new(None),
        refresh_pgid: std::sync::Mutex::new(None),
    });
    if state.refresh_script.is_none() {
        tracing::warn!(
            "WORKER_REFRESH_SCRIPT unwired — this node will reject deploy self-refresh requests \
             (worker images stay at the deployed SHA until refreshed by hand)"
        );
    } else if let Some(reason) =
        refresh_skip_reason(state.refresh_git_url.as_deref(), &state.refresh_git_key)
    {
        tracing::warn!(
            "self-refresh script wired but {reason} — deploy refresh requests will be SKIPPED \
             (enroll: `chuggernaut admin worker-git-key`, then set WORKER_REFRESH_GIT_URL)"
        );
    }

    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(&config.node))
        .await?;
    let report = capacity.report();
    tracing::info!(node = %config.node, nats = %config.nats_url, version = %state.version, slots = report.slots, slots_max = report.slots_max, capacity_epoch = report.epoch_ms, "worker up");

    spawn_announce(
        store.clone(),
        config.node.clone(),
        capacity,
        announce_now,
        state.version.clone(),
        state.capabilities.clone(),
    );
    spawn_nix_reaper(state.clone());

    let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_INFLIGHT));
    let tasks = tokio::task::JoinSet::new();
    let mut tasks = tasks;
    loop {
        tokio::select! {
            req = sub.next() => {
                let Some(req) = req else { break };
                let permit = semaphore.clone().acquire_owned().await.expect("semaphore open");
                let state = state.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let body = handle(&state, &req.subject, &req.payload).await;
                    req.respond(body).await;
                });
                while tasks.try_join_next().is_some() {}
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down — waiting for in-flight ops");
                break;
            }
        }
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    Ok(())
}

/// Whether this node offers host execution at all (design #309 §1). Not a
/// choice of backend: a node naming both runtimes constructs both and routes
/// each launch ([`crate::route::RoutedBackend`]).
fn serves_host(modes: &[WorkerMode]) -> bool {
    modes.contains(&WorkerMode::Host)
}

/// Whether this node offers container execution. A node declaring nothing
/// serves containers, which is what every node in the fleet does today.
fn serves_container(modes: &[WorkerMode]) -> bool {
    modes.contains(&WorkerMode::Container) || !serves_host(modes)
}

/// This node's capability advertisement (design #309 §4), resolved from its
/// `WORKER_MODES` and its discovered environments. `resources_enforced` follows
/// container capability: the Docker `HostConfig` is what enforces
/// `resources.cpu`/`memory`, and a host-only node has none.
fn node_capabilities(
    modes: &[WorkerMode],
    xcodes: &Xcodes,
    agent_cli: &AgentCli,
) -> types::worker::NodeCapabilities {
    let mut served = Vec::new();
    if serves_container(modes) {
        served.push(types::job_type::RuntimeMode::Container);
    }
    if serves_host(modes) {
        served.push(types::job_type::RuntimeMode::Host);
    }
    let envs = xcodes.envs();
    debug_assert!(
        envs.is_empty() || serves_host(modes),
        "only a host-capable node discovers an environment"
    );
    debug_assert!(
        !agent_cli.present() || serves_host(modes),
        "only a host-capable node probes for the agent CLI"
    );
    types::worker::NodeCapabilities {
        modes: served,
        platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        resources_enforced: serves_container(modes),
        leases: Vec::new(),
        envs,
        agent_cli: agent_cli.present(),
    }
}

/// The agent CLI a host-capable node serves agent work with, probed once at boot
/// against the daemon's own `PATH` (design #490 D3). Finding none is a warning
/// rather than a refused boot, for the reason D5 gives: the refusal belongs at
/// the launch that asked, so a Mac without a CLI keeps every container and
/// command-host slot it has.
fn discover_agent_cli(node: &str, modes: &[WorkerMode]) -> AgentCli {
    if !serves_host(modes) {
        return AgentCli::default();
    }
    let cli = AgentCli::discover();
    match cli.path() {
        Some(path) => {
            tracing::info!(node = %node, path = %path.display(), "discovered the agent CLI")
        }
        None => tracing::warn!(
            node = %node,
            bin = %crate::agent_cli::AGENT_CLI_BIN,
            "host mode is on and this node discovered no agent CLI on the daemon's own PATH — an \
             agent-shaped host launch will be refused by name (design #490 D3)"
        ),
    }
    cli
}

/// The Xcodes a host-capable node serves, scanned once at boot because the
/// installed bundles are the fact (design #322 W4). Finding **none** is a
/// warning rather than a refused boot, for the reason `docs/spec.md` §3.1 gives:
/// the refusal belongs at the launch that asked.
fn discover_xcodes(node: &str, modes: &[WorkerMode]) -> Xcodes {
    if !serves_host(modes) {
        return Xcodes::default();
    }
    let xcodes = Xcodes::discover();
    let envs = xcodes.envs();
    if envs.is_empty() {
        tracing::warn!(
            node = %node,
            root = %crate::xcode::INSTALL_ROOT,
            "host mode is on and this node installs no Xcode it can serve — a launch declaring \
             runtime.env xcode:<version> will be refused (design #322 W4)"
        );
    } else {
        tracing::info!(node = %node, envs = ?envs, "discovered Xcode environments");
    }
    xcodes
}

/// Option (iii) — one host task per node — is **enforced** here rather than
/// left to an operator's `WORKER_SLOTS` or a runtime `set_slots` raise. Why it
/// outlived the `/workspace` collision that motivated it, and why it stays
/// node-wide on a dual-mode node, is in `docs/implementation-notes.md`.
fn enforce_host_capacity(slots: u32, slots_max: u32) -> Result<(), WorkerRunError> {
    if slots == 1 && slots_max == 1 {
        return Ok(());
    }
    Err(WorkerRunError::Config(format!(
        "WORKER_MODES names host, which needs WORKER_SLOTS=1 and WORKER_SLOTS_MAX=1 (got {slots} \
         and {slots_max}): #309 §2 option (iii), one host task per node, kept for phase 1 by the \
         machine-global simulator state design #322 §5 names"
    )))
}

/// The mechanism this node puts host tasks in, or a refused boot (design #440
/// D3). Probed here for the same reason [`enforce_host_capacity`] is checked
/// here: a node that advertised `host` and then parented its tasks to the daemon
/// would lose every one of them to the restart that swaps the daemon.
async fn enforce_host_supervision(
    node: &str,
) -> Result<container::host::Supervision, WorkerRunError> {
    container::host::probe_supervision()
        .await
        .map_err(|reason| WorkerRunError::Config(container::host::host_refusal(node, &reason)))
}

/// Build exactly the backends the node's runtimes declare (design #322 W1, #309
/// §1), so a container-only node never touches [`container::host`] and a
/// host-only node never needs a docker daemon. A node declaring both gets
/// [`crate::route::RoutedBackend`] over the two, so the mode resolves per
/// launched task rather than per node.
async fn local_backend(
    config: &WorkerConfig,
    agent_cli: &AgentCli,
) -> Result<Arc<dyn ContainerBackend>, WorkerRunError> {
    let host: Option<Arc<dyn ContainerBackend>> = if serves_host(&config.modes) {
        Some(host_backend(config, agent_cli).await?)
    } else {
        None
    };
    if !serves_container(&config.modes) {
        return host.ok_or_else(|| {
            WorkerRunError::Config(format!("node {} declares no runtime", config.node))
        });
    }
    let container = docker_backend(config).await?;
    Ok(match host {
        Some(host) => Arc::new(crate::route::RoutedBackend::new(container, host)),
        None => container,
    })
}

/// The node's host backend, and the two boot-time refusals a node advertising
/// `host` has to survive first: the capacity rule #309 §2 option (iii) needs,
/// and the supervision #440 D3 needs. What it can serve an **agent** launch
/// with is not one of them — design #490 D5 refuses that at the launch that
/// asked, so a Mac missing a capability keeps every other slot it has.
async fn host_backend(
    config: &WorkerConfig,
    agent_cli: &AgentCli,
) -> Result<Arc<dyn ContainerBackend>, WorkerRunError> {
    enforce_host_capacity(config.slots, config.slots_max)?;
    let supervision = enforce_host_supervision(&config.node).await?;
    tracing::info!(
        node = %config.node,
        host_root = %config.host_root.display(),
        wire_paths = %format!("{} and {} map into each task directory", container::WIRE_WORKSPACE, container::WIRE_CHUGGERNAUT),
        supervision = ?supervision,
        modes = ?config.modes,
        host_channel = %container::CHANNEL_PATH_HOST,
        agent_cli = ?agent_cli.path(),
        "host execution enabled — launches carrying no image run as host processes here"
    );
    Ok(Arc::new(container::host::HostBackend::new(
        config.node.clone(),
        config.host_root.clone(),
        supervision,
        agent_capability(&config.node, agent_cli, container::CHANNEL_PATH_HOST),
    )?))
}

/// What this node can serve an agent host launch with (design #490 D5), as the
/// backend is told it: the daemon discovers, and hands over both the answer and
/// the refusal to give when it is a no.
fn agent_capability(
    node: &str,
    agent_cli: &AgentCli,
    host_channel: impl Into<PathBuf>,
) -> container::host::AgentCapability {
    let cli_absent = (!agent_cli.present()).then(|| agent_cli.missing(node));
    container::host::AgentCapability::new(cli_absent, host_channel)
}

/// The node's local docker backend, wired with everything the node's own config
/// turns on (cache, KVM grant, nix store) and pinged before it is used.
async fn docker_backend(
    config: &WorkerConfig,
) -> Result<Arc<dyn ContainerBackend>, WorkerRunError> {
    let mut backend = DockerBackend::new(vec![DockerNodeConfig {
        name: config.node.clone(),
        endpoint: config.docker_endpoint.clone(),
        slots: u32::MAX,
    }])?;
    if let Some(dir) = &config.cache_dir {
        std::fs::create_dir_all(dir).map_err(|e| {
            WorkerRunError::Config(format!("creating WORKER_CACHE_DIR {}: {e}", dir.display()))
        })?;
        backend = backend.with_cache_dir(dir.clone());
        tracing::info!(cache_dir = %dir.display(), "node-local build cache enabled (sccache)");
    }
    if let Some(grant) = kvm_grant(config) {
        if !grant.device.exists() {
            return Err(WorkerRunError::Config(format!(
                "WORKER_KVM names {}, which this node does not have — a node never advertises a \
                 capability it cannot serve (design #367 §2.3)",
                grant.device.display()
            )));
        }
        if grant.projects.is_empty() {
            tracing::warn!(
                "WORKER_KVM is on but WORKER_KVM_PROJECTS is empty — no launch will receive the \
                 device or the toolchain mounts (design #367 §2.3: empty grants nobody)"
            );
        }
        tracing::info!(
            device = %grant.device.display(),
            android_sdk_dir = %grant.android_sdk_dir.display(),
            flutter_dir = ?grant.flutter_dir,
            jdk_dir = ?grant.jdk_dir,
            projects = ?grant.projects,
            "KVM passthrough enabled for the allow-listed projects"
        );
        backend = backend.with_kvm(grant);
    }
    if config.nix_gcroots_dir.is_some() {
        backend = backend.with_nix_store(config.nix_store_dir.clone());
    }
    backend.ping_all().await?;
    Ok(Arc::new(backend))
}

/// The node's KVM grant (design #367 A1), or `None` when `WORKER_KVM` is unset.
/// The one place the node's KVM settings become one grant, so the device and
/// the read-only mounts can only ever be enabled together.
fn kvm_grant(config: &WorkerConfig) -> Option<KvmGrant> {
    config.kvm_device.as_ref().map(|device| KvmGrant {
        device: device.clone(),
        android_sdk_dir: config.android_sdk_dir.clone(),
        flutter_dir: config.flutter_dir.clone(),
        jdk_dir: config.jdk_dir.clone(),
        projects: config.kvm_projects.clone(),
    })
}

/// The node's nix realise and GC roots (design #373 P1), or `None` when
/// `WORKER_NIX_GCROOTS_DIR` is unset. A declared roots directory that is absent
/// from the daemon's own view refuses the boot, the same posture a declared KVM
/// device gets — never a silent skip that leaves a task's closure collectable.
fn nix_roots(config: &WorkerConfig) -> Result<Option<Arc<NixRoots>>, WorkerRunError> {
    let Some(gcroots_dir) = config.nix_gcroots_dir.clone() else {
        return Ok(None);
    };
    let roots = NixRoots {
        client: config.nix_client.clone(),
        flake_client: config.nix_flake_client.clone(),
        projects: config.nix_projects.clone(),
        git_key: config
            .refresh_git_key
            .exists()
            .then(|| config.refresh_git_key.clone()),
        gcroots_dir,
        daemon_socket: config.nix_daemon_socket.clone(),
        store_dir: config.nix_store_dir.clone(),
        realise_timeout: Duration::from_secs(config.nix_realise_timeout_secs),
    };
    let realise_target = config
        .kvm_device
        .is_some()
        .then_some(config.android_sdk_dir.as_path());
    roots
        .check(realise_target)
        .map_err(WorkerRunError::Config)?;
    if config.kvm_device.is_none() && roots.projects.is_empty() {
        tracing::warn!(
            "WORKER_NIX_GCROOTS_DIR is set but this node passes no toolchain through \
             (WORKER_KVM unset) and allow-lists no project toolchains \
             (WORKER_NIX_PROJECTS empty) — nothing hands a task store paths, so no root is \
             ever taken"
        );
    }
    tracing::info!(
        gcroots_dir = %roots.gcroots_dir.display(),
        client = %roots.client.display(),
        flake_client = %roots.flake_client.display(),
        projects = ?roots.projects,
        realise_timeout_secs = config.nix_realise_timeout_secs,
        "per-task nix GC roots enabled; the allow-listed projects may have their declared \
         runtime.env realised HERE, which evaluates their flake inside chug-worker \
         (design #373 P2, 3b)"
    );
    Ok(Some(Arc::new(roots)))
}

/// How often the stale-root reaper runs (design #373 Decision 4). Roots are
/// released at task exit, so this is the crash backstop rather than the primary
/// path — an hour between passes is ample, and the interval's immediate first
/// tick is deliberately spent doing nothing, because a daemon still booting its
/// docker connection knows least about what is live.
const NIX_REAP_INTERVAL: Duration = Duration::from_secs(3600);

/// How many rooted containers the daemon tracks at once (docs/reference/style.md Tier 2 rule
/// 3). A node runs a handful of slots, so reaching this means containers are
/// never being removed — the reaper is what recovers the roots either way.
const NIX_ROOTED_MAX: usize = 512;

/// Reap roots whose task is long gone, on a bounded cadence. Detached and
/// best-effort: a pass that cannot see the node's containers is SKIPPED rather
/// than reaping on incomplete knowledge, which would pull a root out from under
/// a live task.
fn spawn_nix_reaper(state: Arc<WorkerState>) {
    let Some(roots) = state.nix.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(NIX_REAP_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(live) = reap_live_tasks(&state).await else {
                continue;
            };
            let removed = roots.reap(&live, REAP_AGE_MIN);
            if removed > 0 {
                tracing::warn!(
                    removed,
                    "reaped stale nix GC roots — a worker died holding them"
                );
            }
        }
    });
}

/// The tasks one reaper pass must spare, or `None` when the node's containers
/// cannot be seen. The count is asked for FIRST because it is the call that
/// propagates an unreachable docker endpoint — the listing degrades an
/// unreachable node to an empty set, which would read as "nothing is live".
async fn reap_live_tasks(state: &WorkerState) -> Option<std::collections::HashSet<String>> {
    if let Err(e) = state.backend.managed_running_total().await {
        tracing::warn!("nix GC root reaper skipped this pass — cannot count containers: {e}");
        return None;
    }
    let containers = match state.backend.list_managed_running().await {
        Ok(containers) => containers,
        Err(e) => {
            tracing::warn!("nix GC root reaper skipped this pass: {e}");
            return None;
        }
    };
    let mut live: std::collections::HashSet<String> = containers
        .into_iter()
        .filter_map(|c| c.task.map(|t| t.to_string()))
        .collect();
    live.extend(nix_rooted_tasks(state));
    Some(live)
}

/// Every task this daemon currently holds a root for, so the reaper spares a
/// launch whose container the node cannot list yet.
fn nix_rooted_tasks(state: &WorkerState) -> Vec<String> {
    state
        .nix_rooted
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .cloned()
        .collect()
}

/// The node's own capacity cell, stamped with this process's epoch (spec §3.1):
/// the boot value is `WORKER_SLOTS`, clamped to the ceiling. A boot value above
/// the ceiling is a misconfigured node, so it is worth a line in the log rather
/// than a silent clamp.
fn run_capacity(config: &WorkerConfig) -> Arc<Capacity> {
    if config.slots > config.slots_max {
        tracing::warn!(
            slots = config.slots,
            slots_max = config.slots_max,
            "WORKER_SLOTS exceeds WORKER_SLOTS_MAX — booting at the ceiling instead \
             (raise WORKER_SLOTS_MAX if this node really can serve more)"
        );
    }
    Arc::new(Capacity::new(
        config.slots,
        config.slots_max,
        crate::capacity::now_epoch_ms(),
    ))
}

/// How often the daemon re-announces itself (spec §3.1 dynamic registration).
/// Comfortably shorter than the dispatcher's heartbeat timeout so an occasional
/// dropped publish never trips a spurious deregistration.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(15);

/// Publish the announce heartbeat immediately, then on every [`ANNOUNCE_INTERVAL`]
/// tick — or as soon as `announce_now` fires — for the life of the daemon.
/// Detached: the daemon keeps serving RPCs regardless, and a transient publish
/// failure just logs and waits for the next tick.
///
/// Every announce re-reads `capacity`, so an adopted `set_slots` is reported
/// from the same owner as the ping reply, with the same ordering key.
fn spawn_announce(
    store: NatsStore,
    node: String,
    capacity: Arc<Capacity>,
    announce_now: Arc<tokio::sync::Notify>,
    version: String,
    capabilities: types::worker::NodeCapabilities,
) {
    tokio::spawn(async move {
        let subject = store::subjects::worker_announce();
        let mut interval = tokio::time::interval(ANNOUNCE_INTERVAL);
        loop {
            let report = capacity.report();
            let announce = types::worker::WorkerAnnounce {
                node: node.clone(),
                slots: report.slots,
                slots_max: Some(report.slots_max),
                capacity_epoch: Some(report.epoch_ms),
                capacity_generation: Some(report.generation),
                version: version.clone(),
                capabilities: Some(capabilities.clone()),
            };
            match serde_json::to_vec(&announce) {
                Ok(bytes) => {
                    if let Err(e) = store.publish(&subject, &bytes).await {
                        tracing::warn!(node = %node, "worker announce publish failed: {e}");
                    }
                }
                Err(e) => tracing::warn!(node = %node, "worker announce serialize failed: {e}"),
            }
            tokio::select! {
                _ = interval.tick() => {}
                () = announce_now.notified() => {}
            }
        }
    });
}

fn version_string() -> String {
    match option_env!("CHUG_GIT_SHA") {
        Some(sha) => format!("{}+{}", env!("CARGO_PKG_VERSION"), sha),
        None => env!("CARGO_PKG_VERSION").to_string(),
    }
}

async fn handle(state: &Arc<WorkerState>, subject: &str, payload: &[u8]) -> Vec<u8> {
    match op_from_subject(subject) {
        Some("launch") => encode_reply(&launch(state, payload).await),
        Some("kill") => encode_reply(&kill(state, payload).await),
        Some("inspect") => encode_reply(&inspect(state, payload).await),
        Some("copy_file") => encode_reply(&copy_file(state, payload).await),
        Some("copy_file_chunk") => encode_reply(&copy_file_chunk(state, payload).await),
        Some("find_file") => encode_reply(&find_file(state, payload).await),
        Some("logs") => encode_reply(&logs(state, payload).await),
        Some("logs_tail") => encode_reply(&logs_tail(state, payload).await),
        Some("ping") => encode_reply(&ping(state).await),
        Some("set_slots") => encode_reply(&set_slots(state, payload)),
        Some("remove") => encode_reply(&remove(state, payload).await),
        Some("list_exited") => encode_reply(&list_exited(state).await),
        Some("list_running") => encode_reply(&list_running(state).await),
        Some("refresh") => encode_reply(&refresh(state, payload).await),
        Some("refresh_cancel") => encode_reply(&refresh_cancel(state, payload).await),
        other => encode_reply::<()>(&WorkerReply::Err {
            error: WorkerError::Other {
                message: unknown_op(other, subject),
            },
        }),
    }
}

/// How this daemon answers an op it does not know (spec §14.1): a **reply**,
/// naming the op, opened with the marker its caller degrades on. That degrade
/// is what lets an additive op ship without a `WORKER_RPC_VERSION` bump.
fn unknown_op(op: Option<&str>, subject: &str) -> String {
    format!("{} {op:?} on {subject}", types::worker::UNKNOWN_OP)
}

fn parse<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, WorkerError> {
    serde_json::from_slice(payload).map_err(|e| WorkerError::Other {
        message: format!("bad request payload: {e}"),
    })
}

fn backend_err(e: BackendError) -> WorkerError {
    match e {
        BackendError::NotFound(id) => WorkerError::NotFound { id },
        BackendError::Unavailable(m) => WorkerError::Unavailable { message: m },
        BackendError::Launch(m) => WorkerError::Launch { message: m },
        BackendError::NoCapacity(m) => WorkerError::NoCapacity { message: m },
        BackendError::Other(m) => WorkerError::Other { message: m },
    }
}

fn reply<T>(r: Result<T, WorkerError>) -> WorkerReply<T> {
    match r {
        Ok(value) => WorkerReply::Ok { value },
        Err(error) => WorkerReply::Err { error },
    }
}

async fn launch(state: &WorkerState, payload: &[u8]) -> WorkerReply<LaunchOk> {
    reply(
        async {
            let _permit = state
                .refresh
                .try_launch()
                .ok_or_else(|| WorkerError::NoCapacity {
                    message: format!("node {} is refreshing — launch will be retried", state.node),
                })?;
            let req: WorkerLaunchRequest = parse(payload)?;
            let files = launch_files(state, req.files)?;
            let mut env = req.env;
            inject_cache_env(&mut env, state.cache_enabled);
            inject_toolchain_env(&mut env, state.kvm.as_ref());
            let rooted = realise_for_launch(state, &mut env, req.runtime_env.as_deref()).await?;
            let launched = state
                .backend
                .launch(ContainerLaunchConfig {
                    image: req.image,
                    cmd: req.cmd,
                    env,
                    files,
                    cpu_limit: req.cpu_limit,
                    memory_limit: req.memory_limit,
                    node: None,
                    runtime_env: req.runtime_env,
                })
                .await;
            let id = match launched {
                Ok(id) => id,
                Err(e) => {
                    release_root_for_task(state, rooted.as_deref());
                    return Err(backend_err(e));
                }
            };
            remember_root(state, &id, rooted);
            Ok(LaunchOk { id })
        }
        .await,
    )
}

/// The files a launch injects, with each `LocalArtifact` reference substituted
/// for the node's own copy. An artifact this node does not hold refuses the
/// launch naming the ones it does, rather than launching a task missing a file
/// it was promised.
fn launch_files(
    state: &WorkerState,
    files: Vec<types::worker::WireFile>,
) -> Result<Vec<InjectedFile>, WorkerError> {
    let mut injected = Vec::with_capacity(files.len());
    for f in files {
        let contents =
            match f.source {
                FileSource::Inline { data_b64 } => {
                    b64_decode(&data_b64).map_err(|e| WorkerError::Launch { message: e })?
                }
                FileSource::LocalArtifact { name } => state
                    .artifacts
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| WorkerError::Launch {
                        message: format!(
                            "unknown local artifact {name:?} on node {} (have: {:?})",
                            state.node,
                            state.artifacts.keys().collect::<Vec<_>>()
                        ),
                    })?,
            };
        injected.push(InjectedFile {
            container_path: f.container_path,
            contents,
            mode: f.mode,
            artifact: None,
        });
    }
    Ok(injected)
}

/// Inject the three cache variables spec §3.1 ("Node-local build caching")
/// names, from the node's own config — the launch message never mentions the
/// cache. `SCCACHE_DIR` is [`container::docker::CACHE_MOUNT_PATH`] so the env
/// matches the bind the backend adds, with no path drift.
fn inject_cache_env(env: &mut HashMap<String, String>, cache_enabled: bool) {
    if cache_enabled {
        env.insert("RUSTC_WRAPPER".into(), "sccache".into());
        env.insert(
            "SCCACHE_DIR".into(),
            container::docker::CACHE_MOUNT_PATH.into(),
        );
        env.insert("CARGO_INCREMENTAL".into(), "0".into());
    }
}

/// Point an allow-listed launch at the toolchains the backend mounts for it
/// (design #367 A1) — the launch message never mentions them, exactly as it
/// never mentions the cache. `HOME` is set alongside because the emulator writes
/// `$HOME/.android` even with `ANDROID_USER_HOME` set, and the read-only mounts
/// must never be that target.
fn inject_toolchain_env(env: &mut HashMap<String, String>, kvm: Option<&KvmGrant>) {
    let Some(grant) = kvm.filter(|grant| grant.admits(env)) else {
        return;
    };
    let sdk = container::docker::ANDROID_SDK_MOUNT_PATH;
    let home = container::docker::KVM_HOME_PATH;
    env.insert("ANDROID_SDK_ROOT".into(), sdk.into());
    env.insert("ANDROID_HOME".into(), sdk.into());
    env.insert("ANDROID_USER_HOME".into(), format!("{home}/.android"));
    env.insert("HOME".into(), home.into());
    if grant.flutter_dir.is_some() {
        env.insert(
            "FLUTTER_ROOT".into(),
            container::docker::FLUTTER_MOUNT_PATH.into(),
        );
    }
    if grant.jdk_dir.is_some() {
        env.insert("JAVA_HOME".into(), container::docker::JDK_MOUNT_PATH.into());
    }
    debug_assert!(
        grant.flutter_dir.is_none()
            || env.get("FLUTTER_ROOT").map(String::as_str)
                == Some(container::docker::FLUTTER_MOUNT_PATH),
        "FLUTTER_ROOT names the mount the backend adds, never a host path"
    );
    debug_assert!(
        grant.jdk_dir.is_none()
            || env.get("JAVA_HOME").map(String::as_str) == Some(container::docker::JDK_MOUNT_PATH),
        "JAVA_HOME names the mount the backend adds, never a host path"
    );
}

/// Realise the toolchain this launch is about to be given, and hold a GC root
/// over it for the task's lifetime (design #373 P1, P2). A launch declaring
/// `runtime.env` takes its own root; one that declares none keeps P1's behavior
/// — the node's own toolchain, on the [`KvmGrant::admits`] decision the mounts
/// already turn on.
async fn realise_for_launch(
    state: &WorkerState,
    env: &mut HashMap<String, String>,
    runtime_env: Option<&str>,
) -> Result<Option<String>, WorkerError> {
    if let Some(env_ref) = runtime_env {
        return serve_declared_env(state, env, env_ref).await;
    }
    let Some(roots) = state.nix.as_ref() else {
        return Ok(None);
    };
    let Some(grant) = state.kvm.as_ref() else {
        return Ok(None);
    };
    let Some(task_id) = rooted_task_id(Some(grant), env)? else {
        return Ok(None);
    };
    roots
        .realise(&task_id, &grant.android_sdk_dir)
        .await
        .map_err(launch_refused)?;
    Ok(Some(task_id))
}

/// Serve the environment the job type declared, by its **scheme** (design #322
/// §3): `nix:` is realised and rooted, `xcode:` is resolved against the node's
/// discovered installs, and a scheme this node cannot serve is refused naming
/// the ones it can.
async fn serve_declared_env(
    state: &WorkerState,
    env: &mut HashMap<String, String>,
    env_ref: &str,
) -> Result<Option<String>, WorkerError> {
    match declared_scheme(env_ref) {
        Some(DeclaredScheme::Nix) => realise_declared_env(state, env, env_ref).await,
        Some(DeclaredScheme::Xcode(version)) => {
            let install = resolve_xcode_env(&state.node, state.host_mode, &state.xcodes, version)?;
            tracing::info!(
                node = %state.node,
                version = %install.version,
                build = %install.build,
                developer_dir = %install.developer_dir.display(),
                "resolved a declared xcode runtime.env"
            );
            inject_xcode_env(env, install);
            Ok(None)
        }
        None => Err(unservable_scheme(&state.node, env_ref)),
    }
}

/// Which node-side path a declared `runtime.env` takes, by its scheme (design
/// #322 §3). Pure, so the fork a launch turns on is asserted without a nix
/// daemon or a Mac.
#[derive(Debug, PartialEq, Eq)]
enum DeclaredScheme<'a> {
    /// `nix:<flake-ref>#<attr>` — realised and rooted, in either mode (design
    /// #373 P2).
    Nix,
    /// `xcode:<version>` — resolved against the node's discovered installs, in
    /// host mode only.
    Xcode(&'a str),
}

/// The scheme this node serves for `env_ref`, or `None` for one no scheme claims.
fn declared_scheme(env_ref: &str) -> Option<DeclaredScheme<'_>> {
    if env_ref.starts_with(NIX_ENV_PREFIX) {
        return Some(DeclaredScheme::Nix);
    }
    env_ref
        .strip_prefix(XCODE_ENV_PREFIX)
        .map(DeclaredScheme::Xcode)
}

/// The refusal for a scheme registered nowhere, naming the ones that are. A
/// launch whose environment cannot be resolved is refused, never run against the
/// machine's own toolchain.
fn unservable_scheme(node: &str, env_ref: &str) -> WorkerError {
    launch_refused(format!(
        "launch declares runtime.env {env_ref:?}, whose scheme node {node} serves in no mode — \
         this node resolves {NIX_ENV_PREFIX} (a flake reference) and {XCODE_ENV_PREFIX} (an \
         installed Xcode, host mode only) and refuses everything else rather than running against \
         the machine's own toolchain (design #322 §3)"
    ))
}

/// The Xcode a host launch's `xcode:<version>` names, or the refusal. Held to
/// the node's **mode** first: `xcode:` is host-only, so a node serving no host
/// mode refuses it by name rather than injecting a path no container can see.
fn resolve_xcode_env<'a>(
    node: &str,
    host_mode: bool,
    xcodes: &'a Xcodes,
    version: &str,
) -> Result<&'a XcodeInstall, WorkerError> {
    if !host_mode {
        return Err(launch_refused(format!(
            "launch declares runtime.env {XCODE_ENV_PREFIX}{version} and node {node} serves no \
             host mode (WORKER_MODES) — Xcode cannot be containerized, so this is a placement bug \
             refused rather than run (design #322 §3)"
        )));
    }
    xcodes.resolve(node, version).map_err(launch_refused)
}

/// Point the task at the Xcode that was resolved: `DEVELOPER_DIR` selects the
/// toolchain per process — never `xcode-select -s`, which mutates a machine-global
/// symlink two tasks would fight over — and `CHUG_ENV_PATH` is what the §4.1
/// bootstrap guard checks and puts on `PATH`.
fn inject_xcode_env(env: &mut HashMap<String, String>, install: &XcodeInstall) {
    env.insert(
        DEVELOPER_DIR_VAR.to_string(),
        install.developer_dir.display().to_string(),
    );
    env.insert(
        container::RUNTIME_ENV_PATH_VAR.to_string(),
        install.env_path().display().to_string(),
    );
    debug_assert!(
        env[container::RUNTIME_ENV_PATH_VAR].starts_with(&env[DEVELOPER_DIR_VAR]),
        "the PATH entry comes out of the selected developer directory"
    );
}

/// Realise the environment the job type declared, for an allow-listed project
/// only (design #373 P2), and point the task at what was realised. Every path
/// out of here is either that environment or a **named refusal** — never a
/// container running against whatever the image happens to carry.
async fn realise_declared_env(
    state: &WorkerState,
    env: &mut HashMap<String, String>,
    env_ref: &str,
) -> Result<Option<String>, WorkerError> {
    let (roots, task_id, installable) =
        declared_env_plan(state.nix.as_deref(), &state.node, env, env_ref)?;
    let realised = roots
        .realise_env(&task_id, &installable)
        .await
        .map_err(launch_refused)?;
    tracing::info!(task = %task_id, installable = %installable, path = %realised.path.display(), "realised a project-declared runtime.env");
    inject_runtime_env_path(env, &realised);
    Ok(Some(task_id))
}

/// The decision half of [`realise_declared_env`]: what this node would realise
/// for a launch, or the named refusal. Pure, so every refusal is asserted
/// without a nix daemon.
fn declared_env_plan<'a>(
    roots: Option<&'a NixRoots>,
    node: &str,
    env: &HashMap<String, String>,
    env_ref: &str,
) -> Result<(&'a NixRoots, String, String), WorkerError> {
    let roots = roots.ok_or_else(|| {
        launch_refused(format!(
            "launch declares runtime.env {env_ref:?} and node {node} realises no environments \
             (WORKER_NIX_GCROOTS_DIR unset) — refused rather than run without the declared \
             toolchain (design #373 P2)"
        ))
    })?;
    if !roots.admits(env) {
        return Err(launch_refused(format!(
            "launch declares runtime.env {env_ref:?} for project {:?}, which node {node} does not \
             allow-list (WORKER_NIX_PROJECTS grants {:?}; empty grants nobody) — a job type asks \
             for an environment, never for a privilege (design #373 Decision 2 rule 3)",
            env.get("JOB_PROJECT").map_or("<unset>", String::as_str),
            roots.projects
        )));
    }
    let task_id = rooted_task_id_required(env)?;
    let installable = flake_installable(
        env_ref,
        env.get("REPO_URL").map_or("", String::as_str),
        env.get("JOB_BRANCH").map_or("", String::as_str),
        env.get("JOB_SHA").map(String::as_str),
    )
    .map_err(launch_refused)?;
    Ok((roots, task_id, installable))
}

/// Point the task at what was realised, under the one name the
/// dispatcher-built bootstrap guard reads before it runs anything (design #373
/// P2, C7).
fn inject_runtime_env_path(env: &mut HashMap<String, String>, realised: &Realised) {
    env.insert(
        container::RUNTIME_ENV_PATH_VAR.to_string(),
        realised.path.display().to_string(),
    );
}

/// Which task a launch takes a GC root for: `None` when the launch gets no
/// toolchain — the same [`KvmGrant::admits`] decision the mounts turn on — and a
/// refusal when an admitted launch names no task. Pure, so the fork is tested
/// without a backend.
fn rooted_task_id(
    kvm: Option<&KvmGrant>,
    env: &HashMap<String, String>,
) -> Result<Option<String>, WorkerError> {
    if !kvm.is_some_and(|grant| grant.admits(env)) {
        return Ok(None);
    }
    rooted_task_id_required(env).map(Some)
}

/// The task a root is named by, or the refusal for a launch that names none. A
/// root the node cannot name is a closure nothing releases.
fn rooted_task_id_required(env: &HashMap<String, String>) -> Result<String, WorkerError> {
    env.get("CHUG_TASK_ID").cloned().ok_or_else(|| {
        launch_refused(
            "this node roots a launch's toolchain per task, but the launch carries no \
             CHUG_TASK_ID — refused rather than run against a collectable closure (design \
             #373 Decision 4)"
                .to_string(),
        )
    })
}

/// A realise that fails REFUSES the launch: `Launch`, never `NoCapacity`. A
/// realise that broke the node's bound will not get faster by being requeued as
/// capacity (design #373 3c).
fn launch_refused(message: String) -> WorkerError {
    WorkerError::Launch { message }
}

/// Remember which task a launched container's root names, so `remove` can drop
/// exactly that one. A tracking table at its bound stops growing and leans on
/// the reaper — it never refuses a launch that has already been realised.
fn remember_root(state: &WorkerState, id: &ContainerId, task_id: Option<String>) {
    let Some(task_id) = task_id else {
        return;
    };
    let mut rooted = state
        .nix_rooted
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if rooted.len() >= NIX_ROOTED_MAX {
        tracing::warn!(
            tracked = rooted.len(),
            "nix GC root tracking is at its bound — this task's root falls to the reaper"
        );
        return;
    }
    rooted.insert(id.clone(), task_id);
    debug_assert!(rooted.len() <= NIX_ROOTED_MAX, "the table stays bounded");
}

/// Drop the root a container's task holds, at the container's removal — the
/// lifecycle `platform-ops`'s `dispose` drives. Best-effort in both halves: an
/// untracked container simply has nothing to release.
fn release_root_for_container(state: &WorkerState, id: &ContainerId) {
    let task_id = state
        .nix_rooted
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(id);
    release_root_for_task(state, task_id.as_deref());
}

/// Drop one task's root, if this node holds any.
fn release_root_for_task(state: &WorkerState, task_id: Option<&str>) {
    if let (Some(roots), Some(task_id)) = (state.nix.as_ref(), task_id) {
        roots.release(task_id);
    }
}

async fn kill(state: &WorkerState, payload: &[u8]) -> WorkerReply<serde_json::Value> {
    reply(
        async {
            let req: ContainerRef = parse(payload)?;
            state.backend.kill(&req.id).await.map_err(backend_err)?;
            Ok(serde_json::json!({}))
        }
        .await,
    )
}

async fn inspect(state: &WorkerState, payload: &[u8]) -> WorkerReply<InspectOk> {
    reply(
        async {
            let req: ContainerRef = parse(payload)?;
            let status = state.backend.inspect(&req.id).await.map_err(backend_err)?;
            Ok(InspectOk {
                status: status.map(|s| match s {
                    ContainerStatus::Running => WireStatus::Running,
                    ContainerStatus::Exited { exit_code } => WireStatus::Exited { exit_code },
                }),
            })
        }
        .await,
    )
}

/// Copy one file out of a container, bounded by [`copy_file_over_bound`]. A
/// file whose reply could not be published comes back as a named error instead
/// of a reply the caller waits out its op timeout for.
async fn copy_file(state: &WorkerState, payload: &[u8]) -> WorkerReply<CopyFileOk> {
    reply(
        async {
            let req: CopyFileRequest = parse(payload)?;
            let data = state
                .backend
                .copy_file(&req.id, &req.path)
                .await
                .map_err(backend_err)?;
            if let Some(e) = copy_file_over_bound(&req.path, data.as_ref().map_or(0, Vec::len)) {
                return Err(e);
            }
            Ok(CopyFileOk {
                data_b64: data.map(|d| b64_encode(&d)),
            })
        }
        .await,
    )
}

/// One bounded slice of a container file (design #362 S1), so an output archive
/// past [`copy_file`]'s single-reply bound still travels. The whole file is
/// measured first: one over the caller's `max_bytes` is refused with the same
/// named error rather than sliced, since a partial archive carries nothing.
async fn copy_file_chunk(state: &WorkerState, payload: &[u8]) -> WorkerReply<CopyFileChunkOk> {
    reply(
        async {
            let req: CopyFileChunkRequest = parse(payload)?;
            let Some(data) = state
                .backend
                .copy_file(&req.id, &req.path)
                .await
                .map_err(backend_err)?
            else {
                return Ok(CopyFileChunkOk {
                    data_b64: None,
                    total_len: 0,
                });
            };
            if let Some(e) = copy_file_over_size(&req.path, data.len(), req.max_bytes as usize) {
                return Err(e);
            }
            let start = (req.offset as usize).min(data.len());
            let end = start.saturating_add(MAX_COPY_FILE_BYTES).min(data.len());
            Ok(CopyFileChunkOk {
                data_b64: Some(b64_encode(&data[start..end])),
                total_len: data.len() as u64,
            })
        }
        .await,
    )
}

/// Resolve a file by name under a directory (design #490 D1a), so the caller
/// never computes the agent CLI's directory slug. The scan runs node-local and
/// only the resolved wire paths cross the wire.
async fn find_file(state: &WorkerState, payload: &[u8]) -> WorkerReply<FindFileOk> {
    reply(
        async {
            let req: FindFileRequest = parse(payload)?;
            let paths = state
                .backend
                .find_file(&req.id, &req.dir, &req.name)
                .await
                .map_err(backend_err)?;
            Ok(FindFileOk { paths })
        }
        .await,
    )
}

async fn logs(state: &WorkerState, payload: &[u8]) -> WorkerReply<LogsOk> {
    reply(
        async {
            let req: ContainerRef = parse(payload)?;
            let mut data = state.backend.logs(&req.id).await.map_err(backend_err)?;
            let truncated = data.len() > LOGS_CAP;
            if truncated {
                data = data.split_off(data.len() - LOGS_CAP);
            }
            Ok(LogsOk {
                data_b64: b64_encode(&data),
                truncated,
            })
        }
        .await,
    )
}

async fn logs_tail(state: &WorkerState, payload: &[u8]) -> WorkerReply<LogsTailOk> {
    reply(
        async {
            let req: LogsTailRequest = parse(payload)?;
            let tail = state
                .backend
                .logs_tail(&req.id, req.since)
                .await
                .map_err(backend_err)?;
            Ok(LogsTailOk {
                offset: tail.offset,
                data_b64: b64_encode(&tail.data),
            })
        }
        .await,
    )
}

/// Remove a finished container and release the GC root its task held (design
/// #373 Decision 4). The root goes whatever the removal did: the task is over
/// either way, and a root outliving it is the disk leak the reaper exists for.
async fn remove(state: &WorkerState, payload: &[u8]) -> WorkerReply<serde_json::Value> {
    reply(
        async {
            let req: ContainerRef = parse(payload)?;
            let removed = state.backend.remove(&req.id).await;
            release_root_for_container(state, &req.id);
            removed.map_err(backend_err)?;
            Ok(serde_json::json!({}))
        }
        .await,
    )
}

async fn list_exited(state: &WorkerState) -> WorkerReply<types::worker::ListExitedOk> {
    reply(
        async {
            let ids = state
                .backend
                .list_managed_exited()
                .await
                .map_err(backend_err)?;
            Ok(types::worker::ListExitedOk { ids })
        }
        .await,
    )
}

async fn list_running(state: &WorkerState) -> WorkerReply<types::worker::ListRunningOk> {
    reply(
        async {
            let containers = state
                .backend
                .list_managed_running()
                .await
                .map_err(backend_err)?
                .into_iter()
                .map(|c| types::worker::WireRunningContainer {
                    id: c.id,
                    project: c.project,
                    job: c.job,
                    task: c.task,
                })
                .collect();
            Ok(types::worker::ListRunningOk { containers })
        }
        .await,
    )
}

#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
async fn ping(state: &WorkerState) -> WorkerReply<PingOk> {
    reply(
        async {
            let running = state
                .backend
                .managed_running_total()
                .await
                .map_err(backend_err)?;
            let capacity = state.capacity.report();
            Ok(PingOk {
                running,
                slots: Some(capacity.slots),
                slots_max: Some(capacity.slots_max),
                capacity_epoch: Some(capacity.epoch_ms),
                capacity_generation: Some(capacity.generation),
                version: state.version.clone(),
                artifacts: state.artifact_hashes.clone(),
                refresh_outcome: state.refresh_outcome.lock().unwrap().clone(),
                refresh_progress: state
                    .refresh_progress
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(RefreshProgressState::wire),
                capabilities: Some(state.capabilities.clone()),
            })
        }
        .await,
    )
}

/// `set_slots` (spec §3.1 operator capacity control): adopt the operator's
/// desired slot count when this node can serve it, and refuse it — with a reason
/// the caller surfaces — when it is above `slots_max`. The node is the authority
/// on its own capacity, so this is the single enforcement point.
///
/// An adoption re-announces immediately rather than waiting out the ~15s
/// heartbeat: the operator is watching the fleet view for their change to land.
/// Nothing here touches running containers — lowering below occupancy drains
/// (free slots go non-positive and placement skips the node), it never kills.
fn set_slots(state: &WorkerState, payload: &[u8]) -> WorkerReply<SetSlotsOk> {
    reply(set_slots_decide(state, payload))
}

/// The decision half of [`set_slots`]: parse, decide against the node's ceiling,
/// and trigger the immediate re-announce on an adoption. Split out so the op
/// keeps the file's `reply(..)` envelope shape.
fn set_slots_decide(state: &WorkerState, payload: &[u8]) -> Result<SetSlotsOk, WorkerError> {
    let req: SetSlotsRequest = parse(payload)?;
    let outcome = state.capacity.set_slots(req.slots);
    if outcome.accepted {
        tracing::info!(
            node = %state.node,
            slots = outcome.slots,
            capacity_generation = outcome.capacity_generation,
            "capacity adopted — re-announcing now"
        );
        state.announce_now.notify_one();
    } else {
        tracing::warn!(
            node = %state.node,
            requested = req.slots,
            slots_max = outcome.slots_max,
            "capacity request REFUSED — node stays at {}",
            outcome.slots
        );
    }
    Ok(outcome)
}

/// Self-refresh (spec §3.1): accept fast, then rebuild and swap in the
/// background. The build runs while launches continue (it takes minutes);
/// only the brief swap window quiesces launches. Returns the version we are
/// refreshing away from — the new one shows up on a later `ping`, clearing the
/// dispatcher's version-drift warning.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
async fn refresh(state: &Arc<WorkerState>, payload: &[u8]) -> WorkerReply<RefreshOk> {
    reply(
        async {
            let req: RefreshRequest = parse(payload)?;
            let Some(script) = state.refresh_script.clone() else {
                return Err(WorkerError::Other {
                    message: format!(
                        "node {} has no self-refresh script (WORKER_REFRESH_SCRIPT unset)",
                        state.node
                    ),
                });
            };
            let from_version = state.version.clone();
            if let Some(reason) =
                refresh_skip_reason(state.refresh_git_url.as_deref(), &state.refresh_git_key)
            {
                tracing::warn!(node = %state.node, "worker refresh SKIPPED — {reason}");
                return Ok(RefreshOk {
                    accepted: false,
                    skipped: Some(reason),
                    from_version,
                });
            }
            if let Some(reason) = host_work_check(state).await {
                tracing::warn!(node = %state.node, "worker refresh REFUSED — {reason}");
                return Ok(RefreshOk {
                    accepted: false,
                    skipped: Some(reason),
                    from_version,
                });
            }
            if !state.refresh.begin_refresh() {
                return Ok(RefreshOk {
                    accepted: false,
                    skipped: None,
                    from_version,
                });
            }
            *state.refresh_outcome.lock().unwrap() = Some(RefreshOutcome {
                accepted_at: chrono::Utc::now(),
                finished_at: None,
                result: RefreshResult::InProgress,
                from_sha: from_version.clone(),
                to_sha: req.sha.clone(),
            });
            *state.refresh_progress.lock().unwrap() =
                Some(RefreshProgressState::new(&req.sha, "accepted"));
            let st = state.clone();
            tokio::spawn(async move { run_refresh(st, script, req).await });
            Ok(RefreshOk {
                accepted: true,
                skipped: None,
                from_version,
            })
        }
        .await,
    )
}

/// The background refresh sequence: build the images at the SHA (launches keep
/// flowing), then quiesce + drain in-flight launches and swap the daemon. Any
/// failure before the swap reopens launches and leaves the old daemon running,
/// so version drift stays surfaced (via ping) rather than turning into an
/// outage.
async fn run_refresh(state: Arc<WorkerState>, script: PathBuf, req: RefreshRequest) {
    tracing::info!(node = %state.node, sha = %req.sha, tag = %req.tag, "worker refresh: building images");
    let progress = &state.refresh_progress;
    let built = run_script(
        &script,
        &["build", &req.sha, &req.tag],
        Some(progress),
        Some(&state.refresh_pgid),
    )
    .await;
    if state.refresh.is_cancelled() {
        refresh_end_cancelled(&state, "build");
        return;
    }
    if let Err(e) = built {
        tracing::error!(node = %state.node, "worker refresh: build failed, aborting: {e}");
        record_refresh_failure(&state, "build", &e);
        state.refresh.abort();
        return;
    }

    refresh_progress_phase(progress, "drain");
    state.refresh.quiesce();
    if !drain(&state.refresh, DRAIN_TIMEOUT).await {
        let e = format!("in-flight launches did not drain in {DRAIN_TIMEOUT:?}");
        tracing::error!(node = %state.node, "worker refresh: {e}; aborting swap");
        record_refresh_failure(&state, "drain", &e);
        state.refresh.abort();
        return;
    }
    if let Some(e) = host_work_check(&state).await {
        tracing::error!(node = %state.node, "worker refresh: {e}; aborting swap");
        record_refresh_failure(&state, "drain", &e);
        state.refresh.abort();
        return;
    }

    if !state.refresh.begin_swap() {
        refresh_end_cancelled(&state, "drain");
        return;
    }
    tracing::info!(node = %state.node, "worker refresh: swapping daemon (job containers survive)");
    refresh_progress_phase(progress, "daemon-swap");
    if let Err(e) = run_script(&script, &["swap", &req.tag], Some(progress), None).await {
        tracing::error!(node = %state.node, "worker refresh: swap failed: {e}");
        record_refresh_failure(&state, "swap", &e);
        state.refresh.abort();
    }
}

/// Stamp the in-flight refresh outcome (ticket #187) as failed at `stage`, so
/// the next `ping` reports it and the failure becomes durable, queryable fleet
/// state rather than only this node's log. The error tail is trimmed for the
/// operator; the swapped-in daemon (on success) never reaches here.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
fn record_refresh_failure(state: &WorkerState, stage: &str, error: &str) {
    let error_tail = bounded_tail(error, REFRESH_TAIL_LINES, REFRESH_TAIL_BYTES);
    let mut slot = state.refresh_outcome.lock().unwrap();
    if let Some(outcome) = slot.as_mut() {
        outcome.finished_at = Some(chrono::Utc::now());
        outcome.result = RefreshResult::Failed {
            stage: stage.to_string(),
            error_tail,
        };
    }
}

/// End a refresh the deploy cancelled (ticket #254): record the terminal
/// verdict under the shared `cancelled` stage — so the deploy reads back "this
/// node was stopped", never "this node's build broke" — and reopen launches. The
/// node keeps its old images and its old daemon, exactly as on any other
/// pre-swap failure.
fn refresh_end_cancelled(state: &WorkerState, during: &str) {
    tracing::warn!(node = %state.node, "worker refresh: cancelled during {during} (old images kept)");
    record_refresh_failure(
        state,
        REFRESH_STAGE_CANCELLED,
        &format!("cancelled by the deploy during {during}; node stays on its old images"),
    );
    state.refresh.abort();
}

/// `refresh_cancel` (spec §3.1, ticket #254): abort the in-flight refresh to
/// `sha`. Soft by construction — a cancel races the build it aborts, so "no
/// refresh in flight", "a different SHA", and "already swapping" are reported
/// outcomes, not errors.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
async fn refresh_cancel(state: &Arc<WorkerState>, payload: &[u8]) -> WorkerReply<RefreshCancelOk> {
    reply(
        async {
            let req: RefreshCancelRequest = parse(payload)?;
            let target = state
                .refresh_outcome
                .lock()
                .unwrap()
                .as_ref()
                .filter(|o| o.result == RefreshResult::InProgress)
                .map(|o| o.to_sha.clone());
            let note = match target {
                None => "no refresh in flight".to_string(),
                Some(sha) if sha != req.sha => {
                    format!("node is refreshing to {sha}, not {}", req.sha)
                }
                Some(_) if !state.refresh.cancel() => {
                    "refresh already past the swap — node stays on the new images".to_string()
                }
                Some(_) => {
                    let pgid = *state.refresh_pgid.lock().unwrap();
                    signal_refresh_build(state, pgid);
                    return Ok(RefreshCancelOk {
                        cancelled: true,
                        note: String::new(),
                    });
                }
            };
            tracing::info!(node = %state.node, sha = %req.sha, "worker refresh cancel declined: {note}");
            Ok(RefreshCancelOk {
                cancelled: false,
                note,
            })
        }
        .await,
    )
}

/// Stop the refresh script's build (ticket #254): SIGTERM the whole process
/// GROUP, then SIGKILL what survives the grace window.
///
/// The group, not the child: the script blocks in `docker build`, so killing
/// only the shell would leave a ten-minute build running against a deploy that
/// is already failing — and skip the script's cleanup. TERM first because
/// `worker-refresh.sh` traps it and exits through its EXIT trap, which is what
/// drops the staged `-refresh` tags and prunes the partial generation (#248).
/// The SIGKILL escalation is the bound: a build that ignores TERM still dies.
///
/// A missing pgid is normal — the cancel landed between scripts (during the
/// drain); the `cancelled` flag alone stops the refresh at the next checkpoint.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
fn signal_refresh_build(state: &Arc<WorkerState>, pgid: Option<i32>) {
    let Some(pgid) = pgid else {
        tracing::info!(node = %state.node, "worker refresh cancel: no build process running");
        return;
    };
    tracing::warn!(node = %state.node, pgid, "worker refresh cancel: signalling the build process group");
    kill_process_group(pgid, libc::SIGTERM);
    let st = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(REFRESH_CANCEL_GRACE).await;
        if *st.refresh_pgid.lock().unwrap() == Some(pgid) {
            tracing::warn!(node = %st.node, pgid, "worker refresh cancel: build ignored SIGTERM — SIGKILL");
            kill_process_group(pgid, libc::SIGKILL);
        }
    });
}

/// Signal a process group. The daemon spawns refresh scripts into their own
/// group (`process_group(0)` in [`run_script`]), so the negated pid can never
/// reach the daemon's own group — the one thing that would turn a cancel into
/// an outage.
fn kill_process_group(pgid: i32, signal: i32) {
    debug_assert!(pgid > 1, "pgid {pgid} must be a real process group");
    // SAFETY: `kill` is async-signal-safe and takes no pointers; its only failure mode is ESRCH — the group already exited, which the branch below treats as a no-op.
    let rc = unsafe { libc::kill(-pgid, signal) };
    if rc != 0 {
        tracing::info!(
            pgid,
            signal,
            "worker refresh cancel: process group already gone"
        );
    }
}

/// Wait until no launches are in flight, or the timeout elapses. Returns
/// whether the drain completed.
async fn drain(gate: &RefreshGate, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !gate.drained() {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    true
}

/// Run the node-local refresh script; non-zero exit is an error. The script
/// reads its own ssh-front / git coordinates from the daemon's environment
/// (spec §3.1) — the daemon passes only the phase, SHA, and tag.
///
/// The script's combined stdout+stderr is streamed line-by-line to the daemon's
/// own stdout as it runs (each line stamped by the daemon's tracing subscriber),
/// so `docker logs chug-worker` works as a live/post-mortem view instead of one
/// buffered lump. The tail of that output is kept and, on non-zero exit, folded
/// into the error string so the refresh failure report carries the real cause
/// (deploy #212) rather than only the exit status.
///
/// Every line is ALSO folded into `progress` (ticket #253) as it arrives, so a
/// concurrent `ping` reports what the script is doing right now and the deploy's
/// wait loop can relay it. `None` disables that (nothing to report into).
///
/// `pgid_slot` (ticket #254) publishes the child's process GROUP while it runs,
/// so `refresh_cancel` can stop the whole build. The child is spawned into its
/// OWN group for exactly that reason — the daemon must never be able to signal
/// itself — and the slot is cleared before this returns, so a cancel arriving
/// after the script exits signals nothing.
#[allow(
    clippy::unwrap_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
async fn run_script(
    script: &Path,
    args: &[&str],
    progress: Option<&std::sync::Mutex<Option<RefreshProgressState>>>,
    pgid_slot: Option<&std::sync::Mutex<Option<i32>>>,
) -> Result<(), String> {
    use std::process::Stdio;

    let phase = args.first().copied().unwrap_or("");
    let mut child = tokio::process::Command::new(script)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("spawning {}: {e}", script.display()))?;
    let pgid = child.id().map(|pid| pid as i32);
    if let Some(slot) = pgid_slot {
        *slot.lock().unwrap() = pgid;
    }

    let tail = run_script_stream(&mut child, phase, progress).await;
    let status = child.wait().await;
    if let Some(slot) = pgid_slot {
        *slot.lock().unwrap() = None;
    }
    let status = status.map_err(|e| format!("waiting on {}: {e}", script.display()))?;
    if status.success() {
        return Ok(());
    }
    let head = format!("{} {:?} exited {status}", script.display(), args);
    let tail = bounded_tail(
        &tail.iter().cloned().collect::<Vec<_>>().join("\n"),
        REFRESH_TAIL_LINES,
        REFRESH_TAIL_BYTES,
    );
    if tail.is_empty() {
        Err(head)
    } else {
        Err(format!("{head}\n{tail}"))
    }
}

/// Pump the child's stdout+stderr until both close, streaming every line to the
/// daemon's log and into `progress`, and return the bounded tail. Split out of
/// [`run_script`] so each stays one readable unit.
#[allow(
    clippy::expect_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
async fn run_script_stream(
    child: &mut tokio::process::Child,
    phase: &str,
    progress: Option<&std::sync::Mutex<Option<RefreshProgressState>>>,
) -> std::collections::VecDeque<String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let tx_err = tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx.send(line);
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = tx_err.send(line);
        }
    });

    let mut tail: std::collections::VecDeque<String> =
        std::collections::VecDeque::with_capacity(REFRESH_TAIL_LINES);
    while let Some(line) = rx.recv().await {
        tracing::info!(phase = %phase, "worker-refresh: {line}");
        if let Some(slot) = progress {
            refresh_progress_note(slot, &line);
        }
        if tail.len() == REFRESH_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    tail
}

/// Bound a captured script tail: keep at most the last `max_lines` lines, then
/// trim the joined text to its last `max_bytes` on a UTF-8 boundary (keeping the
/// most recent bytes). Pure so the truncation is unit-tested without a
/// subprocess. Bounds the RPC failure report and the durable job record.
fn bounded_tail(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    let joined = lines[start..].join("\n");
    if joined.len() <= max_bytes {
        return joined;
    }
    let mut cut = joined.len() - max_bytes;
    while cut < joined.len() && !joined.is_char_boundary(cut) {
        cut += 1;
    }
    joined[cut..].to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// An op this build does not know is a **reply** carrying the marker the
    /// caller degrades on (design #490 D1a): the reason `find_file` needs no
    /// `WORKER_RPC_VERSION` bump is that an N-1 daemon answers this, and the
    /// caller then falls back rather than failing.
    #[test]
    fn an_unknown_op_is_answered_with_the_marker_the_caller_reads() {
        let message = unknown_op(Some("find_file"), "req.worker.w1.find_file");
        assert!(message.starts_with(types::worker::UNKNOWN_OP), "{message}");
        assert!(message.contains("find_file"), "{message}");
    }

    /// The golden assertion for #309 §1: a node that does not name `host`
    /// constructs the docker backend and nothing else, and no host rule can
    /// reach it — a container-only daemon is what ships today, byte for byte.
    #[test]
    fn a_container_only_node_is_unchanged() {
        for modes in [vec![], vec![WorkerMode::Container]] {
            assert!(serves_container(&modes), "{modes:?}");
            assert!(!serves_host(&modes), "{modes:?}");
        }
        assert!(
            serves_container(&crate::config::default_modes())
                && !serves_host(&crate::config::default_modes()),
            "the WORKER_MODES default and the backends constructed cannot drift"
        );
    }

    /// #309 §1's routing rule at the construction site: a node naming both
    /// serves both, and one naming only `host` constructs no docker backend —
    /// a Mac without Docker must not be made to need one.
    #[test]
    fn a_declared_mode_is_a_backend_constructed() {
        let both = vec![WorkerMode::Container, WorkerMode::Host];
        assert!(serves_container(&both) && serves_host(&both));
        assert!(serves_container(&[WorkerMode::Host, WorkerMode::Container]));

        assert!(!serves_container(&[WorkerMode::Host]));
        assert!(serves_host(&[WorkerMode::Host]));
    }

    /// What this node advertises (design #309 §4) is what it constructs: the
    /// `WORKER_MODES` default reads container-only with limits enforced, a
    /// dual-mode node names both in canonical order, and a host-only node
    /// reports it cannot enforce `resources.cpu`/`memory`.
    #[test]
    fn advertised_capabilities_follow_worker_modes() {
        use types::job_type::RuntimeMode;

        let none = Xcodes::default();
        let no_cli = AgentCli::default();
        let default = node_capabilities(&crate::config::default_modes(), &none, &no_cli);
        assert_eq!(default.modes, vec![RuntimeMode::Container]);
        assert!(default.resources_enforced);
        assert_eq!(
            default,
            node_capabilities(&[], &none, &no_cli),
            "declaring nothing is the same node"
        );
        assert_eq!(
            types::worker::NodeCapabilities {
                platform: types::worker::PLATFORM_UNKNOWN.into(),
                ..default.clone()
            },
            types::worker::NodeCapabilities::absent(),
            "only the platform separates a container-only node from the absent reading"
        );

        let both = node_capabilities(&[WorkerMode::Host, WorkerMode::Container], &none, &no_cli);
        assert_eq!(both.modes, vec![RuntimeMode::Container, RuntimeMode::Host]);
        assert!(both.resources_enforced, "it still has a docker daemon");

        let host_only = node_capabilities(&[WorkerMode::Host], &none, &no_cli);
        assert_eq!(host_only.modes, vec![RuntimeMode::Host]);
        assert!(
            !host_only.resources_enforced,
            "no HostConfig means no cpu/memory enforcement (design #309 §7)"
        );
        assert!(host_only.leases.is_empty(), "device leases are #309 P4");
        assert!(
            host_only.platform.contains('/')
                && host_only.platform != types::worker::PLATFORM_UNKNOWN,
            "a live node names its platform: {}",
            host_only.platform
        );
        assert!(
            !default.agent_cli && !both.agent_cli && !host_only.agent_cli,
            "a node that discovered no CLI advertises none (design #490 D3)"
        );
    }

    /// A fixture `PATH` with the agent CLI on it, so every assertion about the
    /// probe says the same thing on the Linux evaluator as it would on the air.
    fn fixture_agent_cli(name: &str) -> (PathBuf, AgentCli) {
        let root =
            std::env::temp_dir().join(format!("chug-daemon-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dir = crate::agent_cli::path_fixture(&root, "local-bin", Some(0o755));
        let cli = AgentCli::discover_on(Some(&crate::agent_cli::fixture_path(&[dir])));
        assert!(cli.present(), "the fixture PATH carries a runnable CLI");
        (root, cli)
    }

    /// Discovery is scoped to the nodes that can use it (design #490 D3): a
    /// container-only node probes nothing and advertises nothing — its CLI comes
    /// out of the agent image — and a host node that found one says so.
    #[test]
    fn only_a_host_capable_node_advertises_an_agent_cli() {
        assert!(
            !discover_agent_cli("nuc", &crate::config::default_modes()).present(),
            "a container-only node never reads the daemon's PATH for a CLI"
        );
        let (root, cli) = fixture_agent_cli("advertise");
        let capable = node_capabilities(
            &[WorkerMode::Container, WorkerMode::Host],
            &Xcodes::default(),
            &cli,
        );
        assert!(capable.agent_cli);
        assert!(
            !node_capabilities(
                &[WorkerMode::Container],
                &Xcodes::default(),
                &AgentCli::default()
            )
            .agent_cli,
            "a container-only node's advertisement is unchanged"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The capability the daemon hands the host backend carries the refusal a
    /// node with no CLI owes an agent launch, BY NAME (design #490 D5) — the
    /// daemon discovers, and the backend is told both the answer and the words.
    #[test]
    fn the_capability_a_node_without_a_cli_hands_over_refuses_naming_it() {
        let (root, cli) = fixture_agent_cli("capability");
        let channel = crate::agent_cli::path_fixture(&root, "lib", Some(0o755))
            .join(crate::agent_cli::AGENT_CLI_BIN);

        let message = agent_capability("air", &AgentCli::default(), &channel)
            .refusal("air")
            .expect("a node that discovered no CLI cannot serve an agent launch");
        assert!(
            message.contains(crate::agent_cli::AGENT_CLI_BIN),
            "{message}"
        );
        assert!(message.contains("air"), "{message}");

        assert!(
            agent_capability("air", &cli, &channel)
                .refusal("air")
                .is_none(),
            "a node holding both halves refuses nothing"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A node offering `host` — alone or beside `container` — whose capacity is
    /// not exactly 1 refuses to boot, which is #309 §2 option (iii) enforced
    /// rather than assumed, and stays node-wide on a dual-mode node.
    #[test]
    fn host_mode_is_one_slot_or_no_boot() {
        for modes in [
            vec![WorkerMode::Host],
            vec![WorkerMode::Container, WorkerMode::Host],
            vec![WorkerMode::Host, WorkerMode::Container],
        ] {
            assert!(serves_host(&modes), "{modes:?}");
        }

        assert!(enforce_host_capacity(1, 1).is_ok());
        for (slots, max) in [(2, 2), (1, 4), (4, 1), (0, 1)] {
            let err = enforce_host_capacity(slots, max).unwrap_err().to_string();
            assert!(err.contains("WORKER_SLOTS=1"), "{slots}/{max}: {err}");
            assert!(
                err.contains("one host task per node"),
                "the refusal names the rule it enforces: {err}"
            );
        }
    }

    /// The swap window refuses new launches (retryable NoCapacity), and each
    /// permit is released on drop so the drain can complete — the core of the
    /// spec §3.1 drain guarantee, exercised without Docker or NATS.
    #[test]
    fn refresh_gate_quiesce_and_drain() {
        let gate = RefreshGate::default();

        let p1 = gate.try_launch().expect("admitted before quiescing");
        assert!(!gate.drained(), "a live launch means not drained");

        assert!(gate.begin_refresh());
        assert!(!gate.begin_refresh(), "only one refresh at a time");

        gate.quiesce();
        assert!(
            gate.try_launch().is_none(),
            "launches refused while quiescing"
        );

        assert!(!gate.drained());
        drop(p1);
        assert!(gate.drained(), "drained once the in-flight launch releases");

        gate.abort();
        assert!(gate.try_launch().is_some(), "launches admitted after abort");
        assert!(gate.begin_refresh(), "refresh slot freed after abort");
    }

    /// A cancel that lands BEFORE the swap window stops the refresh: it is
    /// authoritative, the swap is refused, and the abort leaves the node clean
    /// for its next refresh (ticket #254 — the deploy fan-out cancels the nodes
    /// still building the moment one node fails).
    #[test]
    fn refresh_gate_cancel_before_swap_stops_the_swap() {
        let gate = RefreshGate::default();
        assert!(gate.begin_refresh());

        assert!(gate.cancel(), "a pre-swap cancel is authoritative");
        assert!(gate.is_cancelled());
        assert!(
            !gate.begin_swap(),
            "a cancelled refresh must never swap the daemon"
        );

        gate.abort();
        assert!(!gate.is_cancelled(), "abort clears the cancel");
        assert!(gate.begin_refresh(), "refresh slot freed after abort");
        assert!(gate.begin_swap(), "the next refresh may swap normally");
    }

    /// Once the swap window is open the node IS going onto the new images:
    /// the cancel is refused rather than reported as a stop that never happened.
    /// That honesty is what lets the deploy leg say "this node stayed swapped".
    #[test]
    fn refresh_gate_cancel_after_swap_is_declined() {
        let gate = RefreshGate::default();
        assert!(gate.begin_refresh());
        assert!(gate.begin_swap());

        assert!(
            !gate.cancel(),
            "a cancel arriving after the swap window opened cannot un-swap the node"
        );
    }

    /// Caching on ⇒ the launch env gains sccache wiring, with `SCCACHE_DIR`
    /// pinned to the same mount path the backend binds and incremental
    /// compilation off (sccache does not cache incremental units, deploy #347).
    #[test]
    fn cache_env_injected_when_enabled() {
        let mut env = HashMap::from([("JOB_ID".to_string(), "7".to_string())]);
        inject_cache_env(&mut env, true);
        assert_eq!(
            env.get("RUSTC_WRAPPER").map(String::as_str),
            Some("sccache")
        );
        assert_eq!(
            env.get("SCCACHE_DIR").map(String::as_str),
            Some(container::docker::CACHE_MOUNT_PATH)
        );
        assert_eq!(env.get("CARGO_INCREMENTAL").map(String::as_str), Some("0"));
        assert_eq!(env.get("JOB_ID").map(String::as_str), Some("7"));
    }

    fn kvm_grant_for(projects: &[&str]) -> KvmGrant {
        KvmGrant {
            device: PathBuf::from(container::docker::KVM_DEVICE_PATH),
            android_sdk_dir: PathBuf::from("/var/lib/chuggernaut/android-sdk"),
            flutter_dir: None,
            jdk_dir: None,
            projects: projects.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    /// An allow-listed launch on a KVM node is pointed at the SDK the backend
    /// mounts for it — a fixed container path carrying no store hash — with a
    /// writable `HOME` for the `$HOME/.android` the emulator writes anyway
    /// (design #367 A1).
    #[test]
    fn android_env_injected_for_an_allow_listed_project() {
        let mut env = HashMap::from([("JOB_PROJECT".to_string(), "acme/beacon".to_string())]);
        inject_toolchain_env(&mut env, Some(&kvm_grant_for(&["acme/beacon"])));
        let sdk = container::docker::ANDROID_SDK_MOUNT_PATH;
        assert_eq!(env.get("ANDROID_SDK_ROOT").map(String::as_str), Some(sdk));
        assert_eq!(env.get("ANDROID_HOME").map(String::as_str), Some(sdk));
        assert_eq!(
            env.get("ANDROID_USER_HOME").map(String::as_str),
            Some("/root/.android")
        );
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(container::docker::KVM_HOME_PATH)
        );
        assert!(
            !env.values().any(|v| v.starts_with("/nix/store/")),
            "no store path may reach the launch env: {env:?}"
        );
        assert!(
            !env.contains_key("FLUTTER_ROOT"),
            "a node that provisions no Flutter names none: {env:?}"
        );
        assert!(
            !env.contains_key("JAVA_HOME"),
            "a node that provisions no JDK names none: {env:?}"
        );
    }

    /// A Flutter-provisioned node points the same admitted launch at its own
    /// mount too, leaving the Android variables on the Android mount — each
    /// toolchain resolves to its own leaf, and neither carries a store path.
    #[test]
    fn flutter_env_injected_only_when_the_node_provisions_it() {
        let grant = KvmGrant {
            flutter_dir: Some(PathBuf::from("/var/lib/chuggernaut/toolchain/flutter")),
            ..kvm_grant_for(&["acme/beacon"])
        };
        let mut env = HashMap::from([("JOB_PROJECT".to_string(), "acme/beacon".to_string())]);
        inject_toolchain_env(&mut env, Some(&grant));
        assert_eq!(
            env.get("FLUTTER_ROOT").map(String::as_str),
            Some(container::docker::FLUTTER_MOUNT_PATH)
        );
        assert_eq!(
            env.get("ANDROID_SDK_ROOT").map(String::as_str),
            Some(container::docker::ANDROID_SDK_MOUNT_PATH)
        );
        assert_eq!(
            env.get("ANDROID_HOME").map(String::as_str),
            Some(container::docker::ANDROID_SDK_MOUNT_PATH)
        );
        assert_ne!(
            env.get("FLUTTER_ROOT"),
            env.get("ANDROID_SDK_ROOT"),
            "the two toolchains are separate leaves: {env:?}"
        );
        assert!(
            !env.values().any(|v| v.starts_with("/nix/store/")),
            "no store path may reach the launch env: {env:?}"
        );

        let mut unlisted = HashMap::from([("JOB_PROJECT".to_string(), "acme/api".to_string())]);
        inject_toolchain_env(&mut unlisted, Some(&grant));
        assert_eq!(unlisted.len(), 1, "unlisted launch got env: {unlisted:?}");
    }

    /// A JDK-provisioned node names its own leaf in `JAVA_HOME` — the variable
    /// gradle needs, since it is not a nix wrapper and cannot resolve a JDK out
    /// of the store (design #367 correction 14) — leaving the other toolchain
    /// variables on their own mounts, each distinct and none a store path.
    #[test]
    fn java_home_injected_only_when_the_node_provisions_a_jdk() {
        let grant = KvmGrant {
            flutter_dir: Some(PathBuf::from("/var/lib/chuggernaut/toolchain/flutter")),
            jdk_dir: Some(PathBuf::from("/var/lib/chuggernaut/toolchain/jdk")),
            ..kvm_grant_for(&["acme/beacon"])
        };
        let mut env = HashMap::from([("JOB_PROJECT".to_string(), "acme/beacon".to_string())]);
        inject_toolchain_env(&mut env, Some(&grant));
        assert_eq!(
            env.get("JAVA_HOME").map(String::as_str),
            Some(container::docker::JDK_MOUNT_PATH)
        );
        assert_eq!(
            env.get("FLUTTER_ROOT").map(String::as_str),
            Some(container::docker::FLUTTER_MOUNT_PATH)
        );
        assert_eq!(
            env.get("ANDROID_SDK_ROOT").map(String::as_str),
            Some(container::docker::ANDROID_SDK_MOUNT_PATH)
        );
        let leaves = [
            env.get("JAVA_HOME"),
            env.get("FLUTTER_ROOT"),
            env.get("ANDROID_SDK_ROOT"),
        ];
        assert_eq!(
            leaves
                .iter()
                .flatten()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "the three toolchains are separate leaves: {env:?}"
        );
        assert!(
            !env.values().any(|v| v.starts_with("/nix/store/")),
            "no store path may reach the launch env: {env:?}"
        );

        let mut jdk_only = HashMap::from([("JOB_PROJECT".to_string(), "acme/beacon".to_string())]);
        inject_toolchain_env(
            &mut jdk_only,
            Some(&KvmGrant {
                flutter_dir: None,
                jdk_dir: grant.jdk_dir.clone(),
                ..kvm_grant_for(&["acme/beacon"])
            }),
        );
        assert_eq!(
            jdk_only.get("JAVA_HOME").map(String::as_str),
            Some(container::docker::JDK_MOUNT_PATH),
            "a JDK leaf is independent of the Flutter one: {jdk_only:?}"
        );
        assert!(!jdk_only.contains_key("FLUTTER_ROOT"));
    }

    /// The negative space: the same node injects nothing for a project it did
    /// not allow-list, for a launch with no project, or when KVM is off — the
    /// same allow-list decision the backend applies to the device and mounts.
    #[test]
    fn no_toolchain_env_without_the_grant() {
        let grant = kvm_grant_for(&["acme/beacon"]);
        for (env, kvm) in [
            (
                HashMap::from([("JOB_PROJECT".into(), "acme/api".into())]),
                Some(&grant),
            ),
            (HashMap::new(), Some(&grant)),
            (
                HashMap::from([("JOB_PROJECT".into(), "acme/beacon".into())]),
                None,
            ),
        ] {
            let mut env: HashMap<String, String> = env;
            let before = env.len();
            inject_toolchain_env(&mut env, kvm);
            assert_eq!(env.len(), before, "unexpected android env: {env:?}");
        }

        let mut env = HashMap::from([("JOB_PROJECT".to_string(), "acme/beacon".to_string())]);
        inject_toolchain_env(&mut env, Some(&kvm_grant_for(&[])));
        assert_eq!(env.len(), 1, "an empty allow-list grants nobody: {env:?}");
    }

    /// A launch takes a root exactly when it takes the toolchain mounts, by the
    /// same allow-list decision (design #373 P1): a project the node did not
    /// grant, and a node with no grant at all, root nothing.
    #[test]
    fn only_an_admitted_launch_takes_a_root() {
        let grant = kvm_grant_for(&["acme/beacon"]);
        let admitted = HashMap::from([
            ("JOB_PROJECT".to_string(), "acme/beacon".to_string()),
            ("CHUG_TASK_ID".to_string(), "42".to_string()),
        ]);
        assert_eq!(
            rooted_task_id(Some(&grant), &admitted).unwrap(),
            Some("42".to_string())
        );

        let other = HashMap::from([("JOB_PROJECT".to_string(), "acme/api".to_string())]);
        assert_eq!(rooted_task_id(Some(&grant), &other).unwrap(), None);
        assert_eq!(rooted_task_id(None, &admitted).unwrap(), None);
        assert_eq!(rooted_task_id(Some(&grant), &HashMap::new()).unwrap(), None);
    }

    /// Every refusal on the realise path is `Launch`, NEVER `NoCapacity`
    /// (design #373 3c): a `NoCapacity` would requeue the task onto the same
    /// node to break the same bound again, forever, instead of failing loudly.
    #[test]
    fn a_realise_refusal_is_a_launch_failure_not_no_capacity() {
        let admitted = HashMap::from([("JOB_PROJECT".to_string(), "acme/beacon".to_string())]);
        let err = rooted_task_id(Some(&kvm_grant_for(&["acme/beacon"])), &admitted)
            .expect_err("an admitted launch with no task id is refused");
        match &err {
            WorkerError::Launch { message } => assert!(message.contains("CHUG_TASK_ID"), "{err:?}"),
            other => panic!("a realise refusal must be Launch, got {other:?}"),
        }
        assert!(matches!(
            launch_refused("realise exceeded WORKER_NIX_REALISE_TIMEOUT_SECS=600s".into()),
            WorkerError::Launch { .. }
        ));
    }

    fn nix_roots_for(projects: &[&str]) -> NixRoots {
        NixRoots {
            projects: projects.iter().map(|p| (*p).to_string()).collect(),
            gcroots_dir: PathBuf::from("/var/lib/chuggernaut/gcroots"),
            client: PathBuf::from("/nix/var/nix/profiles/system/sw/bin/nix-store"),
            flake_client: PathBuf::from("/nix/var/nix/profiles/system/sw/bin/nix"),
            git_key: None,
            daemon_socket: PathBuf::from("/nix/var/nix/daemon-socket/socket"),
            store_dir: PathBuf::from("/nix/store"),
            realise_timeout: Duration::from_secs(45),
        }
    }

    fn declared_env_launch(project: &str) -> HashMap<String, String> {
        HashMap::from([
            ("JOB_PROJECT".to_string(), project.to_string()),
            ("CHUG_TASK_ID".to_string(), "42".to_string()),
            (
                "REPO_URL".to_string(),
                "ssh://git@front:2222/acme/beacon.git".to_string(),
            ),
            ("JOB_BRANCH".to_string(), "job/403".to_string()),
        ])
    }

    /// Every refusal on the project-declared path is a NAMED `Launch` failure
    /// (design #373 P2), never `NoCapacity` and never a fall-through to a
    /// container without the toolchain: a node that realises nothing, a project
    /// the node does not allow-list, an empty allow-list, and a launch that
    /// names no task.
    #[test]
    fn a_declared_env_is_refused_by_name_before_anything_is_realised() {
        let granted = nix_roots_for(&["acme/beacon"]);
        let refusal =
            |roots: Option<&NixRoots>, env: &HashMap<String, String>| match declared_env_plan(
                roots,
                "nuc",
                env,
                "nix:.#chug-mobile",
            ) {
                Err(WorkerError::Launch { message }) => message,
                other => panic!("a declared-env refusal must be Launch, got {other:?}"),
            };

        let unrealising = refusal(None, &declared_env_launch("acme/beacon"));
        assert!(
            unrealising.contains("WORKER_NIX_GCROOTS_DIR"),
            "{unrealising}"
        );

        let ungranted = refusal(Some(&granted), &declared_env_launch("acme/api"));
        assert!(ungranted.contains("WORKER_NIX_PROJECTS"), "{ungranted}");
        assert!(ungranted.contains("acme/api"), "{ungranted}");

        let nobody = refusal(
            Some(&nix_roots_for(&[])),
            &declared_env_launch("acme/beacon"),
        );
        assert!(nobody.contains("WORKER_NIX_PROJECTS"), "{nobody}");

        let mut anonymous = declared_env_launch("acme/beacon");
        anonymous.remove("CHUG_TASK_ID");
        assert!(
            refusal(Some(&granted), &anonymous).contains("CHUG_TASK_ID"),
            "a root the node cannot name is a closure nothing releases"
        );
    }

    /// An admitted launch resolves its relative ref against the job branch AT
    /// the commit the task will check out, and what was realised reaches the
    /// container under the one variable the bootstrap guard reads (design #373
    /// 3a, C7).
    #[test]
    fn an_admitted_declared_env_resolves_against_the_branch_and_reaches_the_container() {
        let granted = nix_roots_for(&["acme/beacon"]);
        let sha = "4b84d2596f0e2b1c0a9a7d3e2f1c0b9a8d7e6f5a";
        let mut env = declared_env_launch("acme/beacon");
        env.insert("JOB_SHA".to_string(), sha.to_string());

        let (roots, task_id, installable) =
            declared_env_plan(Some(&granted), "nuc", &env, "nix:.#chug-mobile")
                .expect("an allow-listed project's launch is planned");
        assert_eq!(roots.projects, vec!["acme/beacon".to_string()]);
        assert_eq!(task_id, "42");
        assert_eq!(
            installable,
            format!("git+ssh://git@front:2222/acme/beacon.git?ref=job/403&rev={sha}#chug-mobile")
        );

        inject_runtime_env_path(
            &mut env,
            &Realised {
                root: PathBuf::from("/var/lib/chuggernaut/gcroots/task-42"),
                path: PathBuf::from("/nix/store/aaaa-env"),
            },
        );
        assert_eq!(
            env.get(container::RUNTIME_ENV_PATH_VAR).map(String::as_str),
            Some("/nix/store/aaaa-env"),
            "the realised path is what the task is pointed at: {env:?}"
        );
    }

    /// A fixture Mac: one Xcode under a temp root, so every assertion about the
    /// scheme says the same thing on the Linux evaluator as it would on the air.
    fn fixture_xcodes(name: &str) -> (PathBuf, Xcodes) {
        let root =
            std::env::temp_dir().join(format!("chug-daemon-xcode-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        crate::xcode::install_fixture(&root, "Xcode.app", "26.5", "17F42");
        let xcodes = Xcodes::discover_in(&root);
        (root, xcodes)
    }

    /// The fork a declared `runtime.env` takes is its SCHEME (design #322 §3):
    /// `nix:` keeps design #373 P2's path whole — which is what makes a nix env
    /// on a Linux node identical to today — `xcode:` takes the node-interpreted
    /// one, and a scheme registered nowhere is refused naming both.
    #[test]
    fn a_declared_env_forks_on_its_scheme() {
        assert_eq!(
            declared_scheme("nix:.#chug-mobile"),
            Some(DeclaredScheme::Nix)
        );
        assert_eq!(
            declared_scheme("nix:"),
            Some(DeclaredScheme::Nix),
            "an empty flake ref is still nix's refusal to make, not a scheme miss"
        );
        assert_eq!(
            declared_scheme("xcode:26.5"),
            Some(DeclaredScheme::Xcode("26.5"))
        );

        for unservable in ["", "brew:llvm", "26.5", "/nix/store/aaaa-env", "XCODE:26.5"] {
            assert_eq!(declared_scheme(unservable), None, "{unservable:?}");
        }
        match unservable_scheme("air", "brew:llvm") {
            WorkerError::Launch { message } => {
                assert!(message.contains("brew:llvm"), "{message}");
                assert!(
                    message.contains("nix:") && message.contains("xcode:"),
                    "the refusal names the schemes this node DOES serve: {message}"
                );
            }
            other => panic!("an unservable scheme must be Launch, got {other:?}"),
        }
    }

    /// An `xcode:<version>` a host node installs selects that toolchain PER TASK
    /// (design #322 §3): `DEVELOPER_DIR` rather than a machine-global
    /// `xcode-select -s`, plus the one variable the §4.1 bootstrap guard reads,
    /// pointed at the directory whose `bin` holds `xcodebuild`.
    #[test]
    fn a_declared_xcode_env_points_the_task_at_that_developer_dir() {
        let (root, xcodes) = fixture_xcodes("resolve");
        let developer_dir = root.join("Xcode.app").join("Contents").join("Developer");

        let install = resolve_xcode_env("air", true, &xcodes, "26.5").expect("an installed Xcode");
        assert_eq!(install.developer_dir, developer_dir);

        let mut env = HashMap::from([("JOB_PROJECT".to_string(), "acme/beacon".to_string())]);
        inject_xcode_env(&mut env, install);
        assert_eq!(
            env.get(DEVELOPER_DIR_VAR).map(String::as_str),
            Some(developer_dir.display().to_string().as_str())
        );
        assert_eq!(
            env.get(container::RUNTIME_ENV_PATH_VAR).map(String::as_str),
            Some(developer_dir.join("usr").display().to_string().as_str()),
            "the bootstrap guard's PATH entry is Developer/usr/bin: {env:?}"
        );
        assert_eq!(
            xcodes.envs(),
            vec!["xcode:26.5"],
            "what it resolves is what it advertises"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Every node that cannot serve an `xcode:` reference refuses it BY NAME
    /// (design #322 §3), never falling through to whatever `xcode-select` points
    /// at: a version the node does not install, a node that serves no host mode,
    /// and a host node with no Xcode at all.
    #[test]
    fn an_xcode_env_is_refused_by_name_wherever_it_cannot_be_served() {
        let (root, xcodes) = fixture_xcodes("refusals");
        let refusal = |host_mode: bool, xcodes: &Xcodes, version: &str| match resolve_xcode_env(
            "air", host_mode, xcodes, version,
        ) {
            Err(WorkerError::Launch { message }) => message,
            other => panic!("an xcode refusal must be Launch, got {other:?}"),
        };

        let unknown = refusal(true, &xcodes, "16.4");
        assert!(unknown.contains("xcode:16.4"), "{unknown}");
        assert!(
            unknown.contains("xcode:26.5"),
            "the refusal names what IS installed: {unknown}"
        );

        let container_only = refusal(false, &xcodes, "26.5");
        assert!(container_only.contains("WORKER_MODES"), "{container_only}");
        assert!(container_only.contains("containerized"), "{container_only}");

        let bare = Xcodes::default();
        assert!(bare.envs().is_empty(), "a node with none promises none");
        assert!(
            refusal(true, &bare, "26.5").contains("no Xcode at all"),
            "a host node with neither scheme's environment refuses by name"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Discovery is scoped to the nodes that can use it (design #322 W4): a
    /// container-only node scans nothing and advertises nothing, and finding no
    /// Xcode is an empty advertisement rather than a refused boot — a dual-mode
    /// Mac must keep serving containers.
    #[test]
    fn only_a_host_capable_node_discovers_and_none_is_not_a_boot_refusal() {
        assert!(
            discover_xcodes("nuc", &crate::config::default_modes())
                .envs()
                .is_empty(),
            "a container-only node never reads /Applications"
        );
        let host = discover_xcodes("air", &[WorkerMode::Container, WorkerMode::Host]);
        assert!(
            node_capabilities(
                &[WorkerMode::Container, WorkerMode::Host],
                &host,
                &AgentCli::default()
            )
            .modes
            .contains(&types::job_type::RuntimeMode::Host),
            "an Xcode-less host node still boots and still serves both modes"
        );
    }

    /// What a node advertises is what it discovered (design #322 §3): the
    /// `envs` list is the resolvable set, and a `nix:` reference is never in it
    /// — it names a build, not a node fact.
    #[test]
    fn advertised_envs_are_the_discovered_set() {
        let (root, xcodes) = fixture_xcodes("advertise");
        let capabilities = node_capabilities(&[WorkerMode::Host], &xcodes, &AgentCli::default());
        assert_eq!(capabilities.envs, vec!["xcode:26.5".to_string()]);
        assert_eq!(
            node_capabilities(
                &[WorkerMode::Container],
                &Xcodes::default(),
                &AgentCli::default()
            )
            .envs,
            Vec::<String>::new(),
            "a nix node advertises no environment: a flake ref is not a node fact"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The refresh skip decision (spec §3.1 / #114): a node with no git URL, or
    /// a missing key file, is reported as skipped; a fully-credentialed node is
    /// not. This is the guard that turns a silent background no-op into a loud
    /// skip in the RPC reply.
    #[test]
    fn refresh_skips_without_git_credential() {
        let reason = refresh_skip_reason(None, Path::new("/data/keys/worker_git"));
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("WORKER_REFRESH_GIT_URL")),
            "unexpected: {reason:?}"
        );

        let missing = Path::new("/definitely/not/a/real/worker_git");
        let reason = refresh_skip_reason(Some("ssh://git@front:2222/acme/chug.git"), missing);
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("does not exist")),
            "unexpected: {reason:?}"
        );

        let present = std::env::current_exe().unwrap();
        assert_eq!(
            refresh_skip_reason(Some("ssh://git@front:2222/acme/chug.git"), &present),
            None
        );
    }

    /// Caching off (the dispatcher's construction never sets a cache dir) ⇒ no
    /// cache env is added. Regression guard: the uncached path is unchanged.
    #[test]
    fn no_cache_env_when_disabled() {
        let mut env = HashMap::from([("JOB_ID".to_string(), "7".to_string())]);
        inject_cache_env(&mut env, false);
        assert!(!env.contains_key("RUSTC_WRAPPER"));
        assert!(!env.contains_key("SCCACHE_DIR"));
        assert!(!env.contains_key("CARGO_INCREMENTAL"));
        assert_eq!(env.len(), 1);
    }

    /// `bounded_tail` keeps the LAST `max_lines` lines and then trims to the last
    /// `max_bytes` — so a huge build log collapses to a small, recent tail
    /// (deploy #212). Both caps and the char-boundary guard are exercised.
    #[test]
    fn bounded_tail_caps_lines_then_bytes() {
        let big = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = bounded_tail(&big, 50, 4096);
        assert!(
            tail.starts_with("line 150"),
            "kept the wrong window: {tail:?}"
        );
        assert!(tail.ends_with("line 199"));
        assert_eq!(tail.lines().count(), 50);

        let long = "x".repeat(10_000);
        let tail = bounded_tail(&long, 50, 100);
        assert_eq!(tail.len(), 100);
        assert!(long.ends_with(&tail));

        let unicode = "😀".repeat(100);
        let tail = bounded_tail(&unicode, 50, 10);
        assert!(tail.len() <= 10);
        assert!(tail.chars().all(|c| c == '😀'));

        assert_eq!(bounded_tail("boom", 50, 4096), "boom");
        assert_eq!(bounded_tail("", 50, 4096), "");
    }

    /// A failing refresh script's combined stdout+stderr is captured and folded
    /// into the error, so the failure report carries the real cause (deploy
    /// #212), not just the exit status. Runs a tiny local script — no Docker,
    /// no NATS.
    #[tokio::test]
    async fn run_script_folds_output_tail_into_error() {
        let dir = std::env::temp_dir().join(format!("chug-refresh-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fail.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho building images\necho 'docker: no space left on device' >&2\nexit 1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = run_script(&script, &["build", "abc", "prod"], None, None)
            .await
            .expect_err("non-zero exit is an error");
        assert!(err.contains("exited"), "keeps the exit summary: {err:?}");
        assert!(
            err.contains("docker: no space left on device"),
            "captures the script's stderr tail: {err:?}"
        );
        assert!(
            err.contains("building images"),
            "captures stdout too: {err:?}"
        );

        let ok = dir.join("ok.sh");
        std::fs::write(&ok, "#!/bin/sh\necho fine\nexit 0\n").unwrap();
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(run_script(&ok, &["swap", "prod"], None, None).await.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The script→daemon phase-marker contract (ticket #253): only a
    /// `worker-refresh: phase <name>` line names a phase, and it is the phase
    /// name alone — everything else is ordinary output.
    #[test]
    fn refresh_phase_marker_reads_only_marker_lines() {
        assert_eq!(
            refresh_phase_marker("worker-refresh: phase build-image 3/3 agent-rust"),
            Some("build-image 3/3 agent-rust")
        );
        assert_eq!(
            refresh_phase_marker("  worker-refresh: phase swap-daemon\r"),
            Some("swap-daemon")
        );
        assert_eq!(
            refresh_phase_marker("worker-refresh: pruned after a successful refresh"),
            None
        );
        assert_eq!(refresh_phase_marker("worker-refresh: phase   "), None);
        assert_eq!(
            refresh_phase_marker("docker: no space left on device"),
            None
        );
    }

    /// A refresh's live progress tracks the script line by line: phase markers
    /// advance the phase, every line joins a BOUNDED recent window, and with no
    /// refresh in flight the whole thing is a no-op (ticket #253).
    #[test]
    fn refresh_progress_tracks_phases_and_bounds_recent_lines() {
        let slot = std::sync::Mutex::new(None);

        refresh_progress_note(&slot, "worker-refresh: phase build-image 1/3 worker");
        assert!(slot.lock().unwrap().is_none());

        *slot.lock().unwrap() = Some(RefreshProgressState::new("target", "accepted"));
        refresh_progress_note(&slot, "worker-refresh: disk pre-flight: 41.2GB free on /");
        refresh_progress_note(&slot, "worker-refresh: phase build-image 1/3 worker");
        let wire = slot.lock().unwrap().as_ref().unwrap().wire();
        assert_eq!(wire.to_sha, "target");
        assert_eq!(wire.phase, "build-image 1/3 worker");
        assert_eq!(wire.recent.len(), 2, "both lines kept: {:?}", wire.recent);

        for i in 0..50 {
            refresh_progress_note(&slot, &format!("build line {i}"));
        }
        let wire = slot.lock().unwrap().as_ref().unwrap().wire();
        assert_eq!(wire.recent.len(), REFRESH_PROGRESS_LINES);
        assert_eq!(
            wire.recent.last().map(String::as_str),
            Some("build line 49")
        );
        refresh_progress_phase(&slot, "drain");
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().wire().phase, "drain");

        refresh_progress_note(&slot, &"x".repeat(10_000));
        let wire = slot.lock().unwrap().as_ref().unwrap().wire();
        assert_eq!(
            wire.recent.last().map(String::len),
            Some(REFRESH_PROGRESS_LINE_BYTES)
        );
    }

    /// The one host task a host node may run (#309 §2 option iii), as the
    /// backend reports it.
    fn a_running_host_task() -> RunningContainer {
        RunningContainer {
            id: "w1/host-19f2c-0".into(),
            project: Some("acme/chug".into()),
            job: Some(440),
            task: Some(3),
        }
    }

    /// A directory this test alone owns, holding its stand-in refresh script and
    /// that script's handshake files.
    fn refresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chug-d440-{name}-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in `worker-refresh.sh` recording every phase it is called with.
    /// `hold` makes the build block until the test releases it, so "a task
    /// appeared during the build" is a handshake rather than a timing bet.
    fn stub_refresh_script(dir: &Path, hold: bool) -> PathBuf {
        let script = dir.join("refresh.sh");
        let waiter = if hold {
            format!(
                "  : > \"{started}\"\n  i=0\n  while [ ! -e \"{release}\" ] && [ $i -lt 600 ]; do \
                 sleep 0.05; i=$((i+1)); done\n",
                started = dir.join("build-started").display(),
                release = dir.join("release").display(),
            )
        } else {
            "  :\n".to_string()
        };
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = build ]; then\n{waiter}fi\necho \"$1\" >> \"{phases}\"\nexit 0\n",
                phases = dir.join("phases").display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script
    }

    /// The phases the stub script has been called with so far.
    fn phases_run(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("phases")).unwrap_or_default()
    }

    /// A daemon state wired for a refresh: a git credential that exists (so the
    /// #114 skip does not fire first) and the stand-in script.
    fn refreshable_state(
        backend: Arc<dyn ContainerBackend>,
        host_mode: bool,
        dir: &Path,
        script: PathBuf,
    ) -> Arc<WorkerState> {
        let git_key = dir.join("worker_git");
        std::fs::write(&git_key, b"stub").unwrap();
        Arc::new(WorkerState {
            node: "w1".into(),
            backend,
            capacity: Arc::new(Capacity::new(1, 1, crate::capacity::now_epoch_ms())),
            announce_now: Arc::new(tokio::sync::Notify::new()),
            artifacts: HashMap::new(),
            artifact_hashes: HashMap::new(),
            version: "0ldc0de".into(),
            cache_enabled: false,
            kvm: None,
            nix: None,
            nix_rooted: std::sync::Mutex::new(HashMap::new()),
            host_mode,
            capabilities: node_capabilities(
                if host_mode {
                    &[WorkerMode::Host]
                } else {
                    &[WorkerMode::Container]
                },
                &Xcodes::default(),
                &AgentCli::default(),
            ),
            xcodes: Xcodes::default(),
            refresh_script: Some(script),
            refresh_git_url: Some("git@example.invalid:acme/chug.git".into()),
            refresh_git_key: git_key,
            refresh: RefreshGate::default(),
            refresh_outcome: std::sync::Mutex::new(None),
            refresh_progress: std::sync::Mutex::new(None),
            refresh_pgid: std::sync::Mutex::new(None),
        })
    }

    async fn ask_refresh(state: &Arc<WorkerState>) -> RefreshOk {
        let payload = serde_json::to_vec(&RefreshRequest {
            sha: "cafef00d".into(),
            tag: "prod".into(),
        })
        .unwrap();
        match refresh(state, &payload).await {
            WorkerReply::Ok { value } => value,
            WorkerReply::Err { error } => panic!("refresh errored: {error:?}"),
        }
    }

    /// The refusal names the task, because a refusal an operator cannot act on
    /// is barely better than the silent kill it replaces (design #440 D4).
    #[test]
    fn a_host_work_refusal_names_the_task() {
        assert_eq!(host_work_refusal("w1", &[]), None);
        let reason = host_work_refusal("w1", &[a_running_host_task()]).unwrap();
        assert!(reason.contains("w1/host-19f2c-0"), "{reason}");
        assert!(
            reason.contains("job 440") && reason.contains("task 3"),
            "{reason}"
        );
        assert!(
            reason.contains("#440"),
            "the reason cites its design: {reason}"
        );
    }

    /// The accept-time precondition (design #440 D4): a host node running a task
    /// refuses the refresh in the reply, names the task, and never starts the
    /// build.
    #[tokio::test]
    async fn a_live_host_task_refuses_the_refresh_at_accept() {
        let dir = refresh_dir("accept");
        let backend = Arc::new(test_utils::FakeBackend::new());
        backend.set_managed_running([a_running_host_task()]);
        let script = stub_refresh_script(&dir, false);
        let state = refreshable_state(backend, true, &dir, script);

        let ok = ask_refresh(&state).await;
        assert!(!ok.accepted, "a host node with live work must not accept");
        let reason = ok.skipped.expect("the refusal rides in the reply");
        assert!(reason.contains("w1/host-19f2c-0"), "{reason}");
        assert_eq!(phases_run(&dir), "", "no build may start: {reason}");
        assert!(
            state.refresh.begin_refresh(),
            "a refusal must not consume the node's refresh slot"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The check that is actually load-bearing (design #440 D4): the accept-time
    /// one passed on an idle node, a task landed while the build ran, and the
    /// swap-boundary re-check refused it at the `drain` stage instead of killing
    /// it.
    #[tokio::test]
    async fn a_host_task_started_during_the_build_is_refused_at_the_swap_boundary() {
        let dir = refresh_dir("boundary");
        let backend = Arc::new(test_utils::FakeBackend::new());
        let script = stub_refresh_script(&dir, true);
        let state = refreshable_state(backend.clone(), true, &dir, script);

        let ok = ask_refresh(&state).await;
        assert!(ok.accepted, "an idle host node accepts: {ok:?}");

        let started = dir.join("build-started");
        test_utils::wait::poll_default("the stub build to start", || {
            started.exists().then_some(())
        })
        .await;
        backend.set_managed_running([a_running_host_task()]);
        std::fs::write(dir.join("release"), b"go").unwrap();

        let (stage, tail) =
            test_utils::wait::poll_default("the refresh to reach its terminal verdict", || {
                match state
                    .refresh_outcome
                    .lock()
                    .unwrap()
                    .as_ref()?
                    .result
                    .clone()
                {
                    RefreshResult::Failed { stage, error_tail } => Some((stage, error_tail)),
                    _ => None,
                }
            })
            .await;
        assert_eq!(
            stage, "drain",
            "the boundary refusal fails at the drain stage"
        );
        assert!(tail.contains("w1/host-19f2c-0"), "{tail}");
        assert!(
            phases_run(&dir).contains("build") && !phases_run(&dir).contains("swap"),
            "the build ran and the swap did not: {:?}",
            phases_run(&dir)
        );
        assert!(
            state.refresh.try_launch().is_some(),
            "an aborted refresh reopens launches"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A container-only node is untouched by design #440 D4: its work survives
    /// the swap by construction (spec §3.1), so the daemon never even asks what
    /// is running — a backend that fails that listing still refreshes to swap.
    #[tokio::test]
    async fn a_container_node_refreshes_without_asking_what_is_running() {
        let dir = refresh_dir("container");
        let backend = Arc::new(test_utils::FakeBackend::new());
        backend.set_managed_running([a_running_host_task()]);
        backend.fail_list_managed_running("a container node must never ask this");
        let script = stub_refresh_script(&dir, false);
        let state = refreshable_state(backend, false, &dir, script);

        assert!(ask_refresh(&state).await.accepted);
        test_utils::wait::poll_default("the refresh to reach the swap", || {
            phases_run(&dir).contains("swap").then_some(())
        })
        .await;
        assert!(
            state
                .refresh_outcome
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .result
                == RefreshResult::InProgress,
            "nothing failed on the way to the swap"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

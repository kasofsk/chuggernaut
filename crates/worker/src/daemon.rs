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

use crate::config::WorkerConfig;
use container::docker::{DockerBackend, DockerNodeConfig};
use container::{
    BackendError, ContainerBackend, ContainerLaunchConfig, ContainerStatus, InjectedFile,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use store::worker::{encode_reply, op_from_subject};
use store::{NatsStore, StoreError};
use types::worker::{
    ContainerRef, CopyFileOk, CopyFileRequest, FileSource, InspectOk, LaunchOk, LogsOk, LogsTailOk,
    LogsTailRequest, PingOk, REFRESH_STAGE_CANCELLED, RefreshCancelOk, RefreshCancelRequest,
    RefreshOk, RefreshOutcome, RefreshProgress, RefreshRequest, RefreshResult, WireStatus,
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
    backend: DockerBackend,
    /// name → bytes, loaded once at startup.
    artifacts: HashMap<String, Vec<u8>>,
    /// name → sha256 hex, reported in ping.
    artifact_hashes: HashMap<String, String>,
    version: String,
    /// Node-local build caching is on (`WORKER_CACHE_DIR` was set). When true,
    /// the backend bind-mounts the cache and every launch gets sccache env.
    cache_enabled: bool,
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
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
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
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
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

/// Run the daemon until ctrl-c. Containers it launched keep running after
/// shutdown; the dispatcher re-attaches.
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::expect_used, clippy::too_many_lines)]
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

    // Single-node backend named after this node so returned container ids are
    // already `{node}/{docker_id}` — the fleet backend routes on that prefix.
    let mut backend = DockerBackend::new(vec![DockerNodeConfig {
        name: config.node.clone(),
        endpoint: config.docker_endpoint.clone(),
        // The dispatcher owns slot policy; the worker only reports usage.
        slots: u32::MAX,
    }])?;
    // Node-local build cache: a worker-side property, added here from the
    // worker's own config — never from the launch message (spec §3.1). The
    // daemon owns creating/owning the host dir; concurrent containers share it,
    // which sccache handles by locking.
    let cache_enabled = config.cache_dir.is_some();
    if let Some(dir) = &config.cache_dir {
        std::fs::create_dir_all(dir).map_err(|e| {
            WorkerRunError::Config(format!("creating WORKER_CACHE_DIR {}: {e}", dir.display()))
        })?;
        backend = backend.with_cache_dir(dir.clone());
        tracing::info!(cache_dir = %dir.display(), "node-local build cache enabled (sccache)");
    }
    backend.ping_all().await?;

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

    let state = Arc::new(WorkerState {
        node: config.node.clone(),
        backend,
        artifacts,
        artifact_hashes,
        version: version_string(),
        cache_enabled,
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
    tracing::info!(node = %config.node, nats = %config.nats_url, version = %state.version, slots = config.slots, "worker up");

    // Announce heartbeat (spec §3.1 dynamic registration): tell the dispatcher
    // this node is live — name, self-advertised slots, build version — so it
    // joins the live fleet with no dispatcher restart. Fire-and-forget on a
    // plain subject; a missed one is covered by the next tick, and losing the
    // stream is what marks the node unschedulable dispatcher-side.
    spawn_announce(
        store.clone(),
        config.node.clone(),
        config.slots,
        state.version.clone(),
    );

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
                // Reap finished handlers opportunistically.
                while tasks.try_join_next().is_some() {}
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down — waiting for in-flight ops");
                break;
            }
        }
    }
    // Bounded grace for in-flight ops; containers keep running regardless.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    Ok(())
}

/// How often the daemon re-announces itself (spec §3.1 dynamic registration).
/// Comfortably shorter than the dispatcher's heartbeat timeout so an occasional
/// dropped publish never trips a spurious deregistration.
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(15);

/// Publish the announce heartbeat immediately, then on every [`ANNOUNCE_INTERVAL`]
/// tick, for the life of the daemon. Detached: the daemon keeps serving RPCs
/// regardless, and a transient publish failure just logs and waits for the next
/// tick.
fn spawn_announce(store: NatsStore, node: String, slots: u32, version: String) {
    tokio::spawn(async move {
        let subject = store::subjects::worker_announce();
        let mut interval = tokio::time::interval(ANNOUNCE_INTERVAL);
        loop {
            let announce = types::worker::WorkerAnnounce {
                node: node.clone(),
                slots,
                version: version.clone(),
            };
            match serde_json::to_vec(&announce) {
                Ok(bytes) => {
                    if let Err(e) = store.publish(&subject, &bytes).await {
                        tracing::warn!(node = %node, "worker announce publish failed: {e}");
                    }
                }
                Err(e) => tracing::warn!(node = %node, "worker announce serialize failed: {e}"),
            }
            interval.tick().await;
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
        Some("logs") => encode_reply(&logs(state, payload).await),
        Some("logs_tail") => encode_reply(&logs_tail(state, payload).await),
        Some("ping") => encode_reply(&ping(state).await),
        Some("remove") => encode_reply(&remove(state, payload).await),
        Some("list_exited") => encode_reply(&list_exited(state).await),
        Some("list_running") => encode_reply(&list_running(state).await),
        Some("refresh") => encode_reply(&refresh(state, payload).await),
        Some("refresh_cancel") => encode_reply(&refresh_cancel(state, payload).await),
        other => encode_reply::<()>(&WorkerReply::Err {
            error: WorkerError::Other {
                message: format!("unknown op {other:?} on {subject}"),
            },
        }),
    }
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
            // Drain guarantee (spec §3.1): once the daemon is quiescing for a
            // self-refresh swap, refuse new launches with NoCapacity — a
            // *transient* signal the dispatcher queues and retries, so no task
            // fails. The permit is held until the container exists, so the swap
            // never lands mid-launch.
            let _permit = state
                .refresh
                .try_launch()
                .ok_or_else(|| WorkerError::NoCapacity {
                    message: format!("node {} is refreshing — launch will be retried", state.node),
                })?;
            let req: WorkerLaunchRequest = parse(payload)?;
            let mut files = Vec::with_capacity(req.files.len());
            for f in req.files {
                let contents = match f.source {
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
                files.push(InjectedFile {
                    container_path: f.container_path,
                    contents,
                    mode: f.mode,
                    artifact: None,
                });
            }
            let mut env = req.env;
            inject_cache_env(&mut env, state.cache_enabled);
            let id = state
                .backend
                .launch(ContainerLaunchConfig {
                    image: req.image,
                    cmd: req.cmd,
                    env,
                    files,
                    cpu_limit: req.cpu_limit,
                    memory_limit: req.memory_limit,
                    // The worker runs a single-node local backend; the fleet
                    // already chose this node, so no further pin applies.
                    node: None,
                })
                .await
                .map_err(backend_err)?;
            Ok(LaunchOk { id })
        }
        .await,
    )
}

/// When node-local caching is on, point cargo at sccache and sccache at the
/// bind-mounted node cache. The worker adds this purely from its own config —
/// the launch message never mentions the cache (spec §3.1). `SCCACHE_DIR` is
/// [`container::docker::CACHE_MOUNT_PATH`] so the env matches the bind the
/// backend adds, with no path drift. Degrades gracefully: if `sccache` is
/// absent from the image, `RUSTC_WRAPPER` points at nothing and cargo still
/// builds — just uncached — so enabling the cache is never fatal.
fn inject_cache_env(env: &mut HashMap<String, String>, cache_enabled: bool) {
    if cache_enabled {
        env.insert("RUSTC_WRAPPER".into(), "sccache".into());
        env.insert(
            "SCCACHE_DIR".into(),
            container::docker::CACHE_MOUNT_PATH.into(),
        );
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

async fn copy_file(state: &WorkerState, payload: &[u8]) -> WorkerReply<CopyFileOk> {
    reply(
        async {
            let req: CopyFileRequest = parse(payload)?;
            let data = state
                .backend
                .copy_file(&req.id, &req.path)
                .await
                .map_err(backend_err)?;
            Ok(CopyFileOk {
                data_b64: data.map(|d| b64_encode(&d)),
            })
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
            // The local backend already caps the chunk at MAX_LOG_TAIL, so the
            // base64 reply fits max_payload — no extra tailing needed here.
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

async fn remove(state: &WorkerState, payload: &[u8]) -> WorkerReply<serde_json::Value> {
    reply(
        async {
            let req: ContainerRef = parse(payload)?;
            state.backend.remove(&req.id).await.map_err(backend_err)?;
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

// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
async fn ping(state: &WorkerState) -> WorkerReply<PingOk> {
    reply(
        async {
            let running = state
                .backend
                .managed_running_total()
                .await
                .map_err(backend_err)?;
            Ok(PingOk {
                running,
                version: state.version.clone(),
                artifacts: state.artifact_hashes.clone(),
                refresh_outcome: state.refresh_outcome.lock().unwrap().clone(),
                refresh_progress: state
                    .refresh_progress
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(RefreshProgressState::wire),
            })
        }
        .await,
    )
}

/// Self-refresh (spec §3.1): accept fast, then rebuild and swap in the
/// background. The build runs while launches continue (it takes minutes);
/// only the brief swap window quiesces launches. Returns the version we are
/// refreshing away from — the new one shows up on a later `ping`, clearing the
/// dispatcher's version-drift warning.
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
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
            // No git credential ⇒ the refresh would fetch nothing. Report the
            // skip in the reply LOUDLY (spec §3.1 / #114) rather than accepting
            // and no-oping in the background — the deploy then shows the skip
            // instead of a silent "success".
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
            if !state.refresh.begin_refresh() {
                // A refresh is already converging — not an error, and not drift:
                // report it as not-accepted so the caller skips the swap-wait
                // rather than logging a scary "not accepted (drift remains)".
                return Ok(RefreshOk {
                    accepted: false,
                    skipped: None,
                    from_version,
                });
            }
            // Record the accepted refresh as in-progress (ticket #187): a later
            // ping carries this, and `run_refresh` overwrites it with the
            // terminal verdict on failure (a success swaps the daemon away).
            *state.refresh_outcome.lock().unwrap() = Some(RefreshOutcome {
                accepted_at: chrono::Utc::now(),
                finished_at: None,
                result: RefreshResult::InProgress,
                from_sha: from_version.clone(),
                to_sha: req.sha.clone(),
            });
            // Open the live progress record for this refresh (ticket #253) at
            // accept time, so the deploy's very first poll already reads a phase
            // rather than an empty wait. Replaces any previous refresh's record:
            // progress is live state, not history.
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
    // Cancellation is checked BEFORE the build's own verdict: a cancel kills the
    // build, so the build error it produces is a CONSEQUENCE of the cancel, not
    // the cause. Attributing it to `build` would put "the node's build broke" in
    // a deploy leg for a node the deploy itself stopped (ticket #254).
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

    // Drain guarantee: refuse new launches, then wait for accepted-but-not-yet-
    // created launches to finish before swapping (spec §3.1).
    refresh_progress_phase(progress, "drain");
    state.refresh.quiesce();
    if !drain(&state.refresh, DRAIN_TIMEOUT).await {
        let e = format!("in-flight launches did not drain in {DRAIN_TIMEOUT:?}");
        tracing::error!(node = %state.node, "worker refresh: {e}; aborting swap");
        record_refresh_failure(&state, "drain", &e);
        state.refresh.abort();
        return;
    }

    // Last cancellable instant: past `begin_swap` the node is going onto the new
    // images and a cancel is refused, so this is where a cancel that arrived
    // during the build or the drain actually stops the refresh.
    if !state.refresh.begin_swap() {
        refresh_end_cancelled(&state, "drain");
        return;
    }
    tracing::info!(node = %state.node, "worker refresh: swapping daemon (job containers survive)");
    refresh_progress_phase(progress, "daemon-swap");
    if let Err(e) = run_script(&script, &["swap", &req.tag], Some(progress), None).await {
        // The swap spawns a detached replacement, so on success this process is
        // simply removed and never returns here. Reaching this arm means the
        // swap itself failed to launch — reopen so the node keeps serving.
        tracing::error!(node = %state.node, "worker refresh: swap failed: {e}");
        record_refresh_failure(&state, "swap", &e);
        state.refresh.abort();
    }
}

/// Stamp the in-flight refresh outcome (ticket #187) as failed at `stage`, so
/// the next `ping` reports it and the failure becomes durable, queryable fleet
/// state rather than only this node's log. The error tail is trimmed for the
/// operator; the swapped-in daemon (on success) never reaches here.
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
fn record_refresh_failure(state: &WorkerState, stage: &str, error: &str) {
    // `error` already carries the script's captured tail on a build/swap
    // failure; bound it the same way the daemon streams so the ping reply and
    // the durable outcome stay small.
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
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
async fn refresh_cancel(state: &Arc<WorkerState>, payload: &[u8]) -> WorkerReply<RefreshCancelOk> {
    reply(
        async {
            let req: RefreshCancelRequest = parse(payload)?;
            // Only OUR target may be cancelled: a node converging on some other
            // SHA (a concurrent deploy, a hand-run refresh) is none of this
            // deploy's business.
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
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
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
    // SAFETY: `kill` is async-signal-safe and takes no pointers; the only
    // failure mode is ESRCH (the group already exited), which is a no-op.
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
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::unwrap_used)]
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
    // With `process_group(0)` the child leads its own group, so its pid IS the
    // group id.
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
// TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched.
#[allow(clippy::expect_used)]
async fn run_script_stream(
    child: &mut tokio::process::Child,
    phase: &str,
    progress: Option<&std::sync::Mutex<Option<RefreshProgressState>>>,
) -> std::collections::VecDeque<String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Feed both streams' lines into one channel; a single consumer prints and
    // buffers them so stdout/stderr interleave in arrival order.
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

    // Keep only the last REFRESH_TAIL_LINES lines so a long build log stays
    // memory-bounded; each line is streamed out immediately as it arrives.
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

    /// The swap window refuses new launches (retryable NoCapacity), and each
    /// permit is released on drop so the drain can complete — the core of the
    /// spec §3.1 drain guarantee, exercised without Docker or NATS.
    #[test]
    fn refresh_gate_quiesce_and_drain() {
        let gate = RefreshGate::default();

        // Normal operation: launches are admitted.
        let p1 = gate.try_launch().expect("admitted before quiescing");
        assert!(!gate.drained(), "a live launch means not drained");

        // First refresh claims the slot; a concurrent one is refused.
        assert!(gate.begin_refresh());
        assert!(!gate.begin_refresh(), "only one refresh at a time");

        // Swap window opens: new launches are refused so the swap can't land
        // between accepting a launch and the container existing.
        gate.quiesce();
        assert!(
            gate.try_launch().is_none(),
            "launches refused while quiescing"
        );

        // The swap must wait for the launch already in flight to finish.
        assert!(!gate.drained());
        drop(p1);
        assert!(gate.drained(), "drained once the in-flight launch releases");

        // Aborting reopens the node (build/drain failure path).
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

        // The cancel belongs to the refresh that just ended — a leftover flag
        // would poison the node's NEXT refresh into cancelling itself.
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
    /// pinned to the same mount path the backend binds.
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
        // Untouched request env survives.
        assert_eq!(env.get("JOB_ID").map(String::as_str), Some("7"));
    }

    /// The refresh skip decision (spec §3.1 / #114): a node with no git URL, or
    /// a missing key file, is reported as skipped; a fully-credentialed node is
    /// not. This is the guard that turns a silent background no-op into a loud
    /// skip in the RPC reply.
    #[test]
    fn refresh_skips_without_git_credential() {
        // No git URL ⇒ skipped, whatever the key.
        let reason = refresh_skip_reason(None, Path::new("/data/keys/worker_git"));
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("WORKER_REFRESH_GIT_URL")),
            "unexpected: {reason:?}"
        );

        // URL set but the key file is absent ⇒ still skipped (the #114 prod
        // condition: empty URL AND no /data/keys/worker_git).
        let missing = Path::new("/definitely/not/a/real/worker_git");
        let reason = refresh_skip_reason(Some("ssh://git@front:2222/acme/chug.git"), missing);
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.contains("does not exist")),
            "unexpected: {reason:?}"
        );

        // URL set and the key file exists (this test binary is a real file) ⇒
        // no skip: the refresh may proceed.
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
        assert_eq!(env.len(), 1);
    }

    /// `bounded_tail` keeps the LAST `max_lines` lines and then trims to the last
    /// `max_bytes` — so a huge build log collapses to a small, recent tail
    /// (deploy #212). Both caps and the char-boundary guard are exercised.
    #[test]
    fn bounded_tail_caps_lines_then_bytes() {
        // Line cap: 200 lines in, keep the last 50.
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

        // Byte cap: a single very long line is trimmed to its most-recent bytes.
        let long = "x".repeat(10_000);
        let tail = bounded_tail(&long, 50, 100);
        assert_eq!(tail.len(), 100);
        assert!(long.ends_with(&tail));

        // Char-boundary safety: cutting mid-multibyte never panics and yields
        // valid UTF-8 no longer than the cap.
        let unicode = "😀".repeat(100); // 4 bytes each
        let tail = bounded_tail(&unicode, 50, 10);
        assert!(tail.len() <= 10);
        assert!(tail.chars().all(|c| c == '😀'));

        // Short input is returned whole.
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

        // A script that succeeds is Ok with no error text.
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
        // The daemon reads the script's stdout through a line reader, so a
        // trailing \r (or leading indent) must not hide a phase.
        assert_eq!(
            refresh_phase_marker("  worker-refresh: phase swap-daemon\r"),
            Some("swap-daemon")
        );
        // Near-misses are not phases: an ordinary log line, and a marker with no
        // name at all.
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

        // No refresh in flight ⇒ nothing recorded, nothing reported.
        refresh_progress_note(&slot, "worker-refresh: phase build-image 1/3 worker");
        assert!(slot.lock().unwrap().is_none());

        *slot.lock().unwrap() = Some(RefreshProgressState::new("target", "accepted"));
        refresh_progress_note(&slot, "worker-refresh: disk pre-flight: 41.2GB free on /");
        refresh_progress_note(&slot, "worker-refresh: phase build-image 1/3 worker");
        let wire = slot.lock().unwrap().as_ref().unwrap().wire();
        assert_eq!(wire.to_sha, "target");
        assert_eq!(wire.phase, "build-image 1/3 worker");
        assert_eq!(wire.recent.len(), 2, "both lines kept: {:?}", wire.recent);

        // The recent window is bounded — a long build cannot grow the ping.
        for i in 0..50 {
            refresh_progress_note(&slot, &format!("build line {i}"));
        }
        let wire = slot.lock().unwrap().as_ref().unwrap().wire();
        assert_eq!(wire.recent.len(), REFRESH_PROGRESS_LINES);
        assert_eq!(
            wire.recent.last().map(String::as_str),
            Some("build line 49")
        );
        // A phase set by the daemon's own stages (drain/swap) shows too.
        refresh_progress_phase(&slot, "drain");
        assert_eq!(slot.lock().unwrap().as_ref().unwrap().wire().phase, "drain");

        // Individual lines are capped, so one pathological build line cannot
        // blow the ping reply either.
        refresh_progress_note(&slot, &"x".repeat(10_000));
        let wire = slot.lock().unwrap().as_ref().unwrap().wire();
        assert_eq!(
            wire.recent.last().map(String::len),
            Some(REFRESH_PROGRESS_LINE_BYTES)
        );
    }
}

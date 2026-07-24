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
    LogsTailRequest, PingOk, RefreshOk, RefreshOutcome, RefreshRequest, RefreshResult, WireStatus,
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

    /// Abort a refresh that failed before the swap: reopen launches.
    fn abort(&self) {
        self.quiescing.store(false, Ordering::SeqCst);
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
    if let Err(e) = run_script(&script, &["build", &req.sha, &req.tag]).await {
        tracing::error!(node = %state.node, "worker refresh: build failed, aborting: {e}");
        record_refresh_failure(&state, "build", &e);
        state.refresh.abort();
        return;
    }

    // Drain guarantee: refuse new launches, then wait for accepted-but-not-yet-
    // created launches to finish before swapping (spec §3.1).
    state.refresh.quiesce();
    if !drain(&state.refresh, DRAIN_TIMEOUT).await {
        let e = format!("in-flight launches did not drain in {DRAIN_TIMEOUT:?}");
        tracing::error!(node = %state.node, "worker refresh: {e}; aborting swap");
        record_refresh_failure(&state, "drain", &e);
        state.refresh.abort();
        return;
    }

    tracing::info!(node = %state.node, "worker refresh: swapping daemon (job containers survive)");
    if let Err(e) = run_script(&script, &["swap", &req.tag]).await {
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
fn record_refresh_failure(state: &WorkerState, stage: &str, error: &str) {
    let n = error.chars().count();
    let error_tail: String = error.chars().skip(n.saturating_sub(400)).collect();
    let mut slot = state.refresh_outcome.lock().unwrap();
    if let Some(outcome) = slot.as_mut() {
        outcome.finished_at = Some(chrono::Utc::now());
        outcome.result = RefreshResult::Failed {
            stage: stage.to_string(),
            error_tail,
        };
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
async fn run_script(script: &Path, args: &[&str]) -> Result<(), String> {
    let status = tokio::process::Command::new(script)
        .args(args)
        .status()
        .await
        .map_err(|e| format!("spawning {}: {e}", script.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} {:?} exited {status}", script.display(), args))
    }
}

#[cfg(test)]
mod tests {
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
}

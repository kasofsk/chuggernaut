//! Dispatcher-side fleet backend: a [`ContainerBackend`] over a mixed fleet of
//! docker-endpoint nodes (driven directly, exactly as before) and worker nodes
//! (proxied over NATS to the node's `chuggernaut worker` daemon).
//!
//! Placement follows the §3.1 policy (`PLACEMENT_POLICY`) across all in-service
//! nodes — busyness (fewest running) or headroom (most free slots). Worker load
//! (running + free slots) comes from the ping reply
//! (the worker counts its own managed containers); a worker that fails its
//! ping is out-of-service — skipped by placement, re-probed on the next
//! placement attempt, and NEVER fatal at startup. Startup capacity is a
//! fleet-level property (§3.6): every node (docker or worker) is probed and
//! marked in/out-of-service, and the "no live capacity" hard-fail is applied
//! once across the whole fleet — a placement-inert 0-slot node never vetoes a
//! fleet that has slots elsewhere.

use async_trait::async_trait;
use container::docker::{DockerBackend, DockerNodeConfig};
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus, LogTail,
    NodeLoad, NodeStatus, PlacementCandidate, PlacementPolicy, RunningContainer, choose_placement,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use store::NatsStore;
use store::worker::{WorkerRpc, WorkerRpcError};
use types::worker::{
    FileSource, RefreshOutcome, WireFile, WireStatus, WorkerError, WorkerLaunchRequest, b64_decode,
    b64_encode,
};

/// Poll interval for `wait` on worker-node containers — no long-held NATS
/// request, so worker restarts are transparent.
const WAIT_POLL: std::time::Duration = std::time::Duration::from_secs(3);

/// `worker` in the DOCKER_NODES endpoint position selects the NATS-proxied
/// node kind: `nuc|worker|4`.
pub const WORKER_ENDPOINT: &str = "worker";

enum NodeHandle {
    // Boxed: the bollard-backed variant is much larger than Worker
    // (clippy::large_enum_variant), and nodes are few and long-lived.
    Docker {
        backend: Box<DockerBackend>,
    },
    Worker {
        rpc: Box<WorkerRpc>,
        /// Slot cap. `Atomic` because a runtime re-announce (spec §3.1 dynamic
        /// registration) can change it (e.g. air 4→5) without replacing the node.
        slots: AtomicU32,
        /// Cleared on ping failure, restored on success; placement skips
        /// out-of-service nodes but always re-probes them.
        in_service: AtomicBool,
        /// Cleared when the node's announce heartbeat lapses (spec §3.1 dynamic
        /// registration): placement skips it, but `route` still reaches it so
        /// running containers keep being waited on. Re-set by a fresh announce.
        /// Static (`DOCKER_NODES`-seeded) nodes stay `true` and rely on the
        /// ping-based `in_service` instead — the dispatcher only heartbeat-gates
        /// nodes it learned about dynamically.
        schedulable: AtomicBool,
        version_warned: AtomicBool,
        /// Last version reported by a successful ping (spec §3.1), surfaced in
        /// the platform config snapshot so the UI can show fleet versions and
        /// spot drift. `None` until the node first answers.
        last_version: Mutex<Option<String>>,
        /// Last self-refresh outcome reported by a successful ping (ticket #187),
        /// surfaced in the fleet/config snapshot so a failed refresh is durable
        /// platform state. `None` until the node reports one.
        last_refresh: Mutex<Option<RefreshOutcome>>,
    },
}

struct FleetNode {
    name: String,
    handle: NodeHandle,
    /// In-flight launches this dispatcher has *placed* on the node but whose
    /// containers the node's live count (ping / docker list) does not yet
    /// report. Added to the node's running count during placement so two
    /// launches dispatched back-to-back — agent launches run on their own
    /// spawned tasks, so their `place()` calls race — don't both read the same
    /// stale `running: 0` and tie onto the same node under `Busyness` (spec
    /// §3.1). Incremented while the placement lock is held, decremented once the
    /// launch RPC returns (the container then exists and the node counts it).
    reserved: AtomicU32,
    /// Set when the node's most recent occupancy listing (`list_running` RPC)
    /// failed, cleared on the next success. A worker can answer `ping`/`launch`
    /// (so it stays schedulable and reachable) while an older daemon rejects the
    /// `list_running` op it never learned — leaving occupancy unable to see the
    /// node's containers. Surfaced via [`FleetBackend::occupancy_unavailable_nodes`]
    /// so the fleet snapshot shows the node out-of-service rather than falsely
    /// idle (spec §3.1; the job/181 prod outage).
    list_failed: AtomicBool,
}

/// A placed-but-not-yet-launched slot on a fleet node. Held across the launch
/// RPC; its drop releases the reservation once the container exists (or the
/// launch failed), so the node's live count takes over the accounting with no
/// double-count and no leak on error.
struct Reservation {
    node: Arc<FleetNode>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.node.reserved.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct FleetBackend {
    /// The live node set. Behind an `RwLock` because a worker announce can add or
    /// update a node at runtime (spec §3.1 dynamic registration), while `wait`/
    /// `inspect` monitors read it concurrently. Each node is an `Arc` so a reader
    /// snapshots (clones the `Arc`s, drops the guard) and then does its awaits
    /// with no lock held. The dispatcher's single-writer actor is the *only*
    /// writer — announcements reach it as mailbox messages — so the lock guards
    /// memory safety for concurrent readers, not multi-writer coordination.
    nodes: RwLock<Vec<Arc<FleetNode>>>,
    /// The store, kept so a newly-announced worker can be given its own
    /// [`WorkerRpc`] on the spot without threading it through every call.
    store: NatsStore,
    /// Platform placement policy (spec §3.1), applied across the whole fleet.
    /// Set from `PLACEMENT_POLICY`; defaults to [`PlacementPolicy::Busyness`].
    policy: PlacementPolicy,
    /// Serializes the read-loads → choose → reserve step of [`Self::place`] so
    /// concurrent launches can't both observe the pre-reservation counts and
    /// pick the same node. Placements are infrequent and the body is short, so
    /// serializing them is free at our scale and makes the reservation
    /// authoritative rather than merely narrowing the race window.
    place_lock: tokio::sync::Mutex<()>,
}

/// The precedence decision for one announce against the current roster (spec
/// §3.1 dynamic registration). Pure over the roster shape so precedence is
/// unit-tested without NATS or Docker. The live announcement wins: a matching
/// worker is updated in place, an unknown name is added, and a name already held
/// by a *docker-endpoint* node is rejected (an announce can't repurpose a
/// directly-driven daemon into a NATS-proxied one).
#[derive(Debug, PartialEq, Eq)]
enum RegisterAction {
    Update(usize),
    Add,
    RejectDockerName,
}

fn plan_register(nodes: &[(&str, bool)], name: &str) -> RegisterAction {
    match nodes.iter().position(|(n, _)| *n == name) {
        Some(i) if nodes[i].1 => RegisterAction::Update(i),
        Some(_) => RegisterAction::RejectDockerName,
        None => RegisterAction::Add,
    }
}

/// Build a fresh NATS-proxied worker node handle for the fleet — used both by
/// the static seed (`DOCKER_NODES` `worker` entries) and by a first-time
/// announce (spec §3.1 dynamic registration).
fn worker_handle(store: NatsStore, name: &str, slots: u32) -> NodeHandle {
    NodeHandle::Worker {
        rpc: Box::new(WorkerRpc::new(store, name.to_string())),
        slots: AtomicU32::new(slots),
        in_service: AtomicBool::new(true),
        schedulable: AtomicBool::new(true),
        version_warned: AtomicBool::new(false),
        last_version: Mutex::new(None),
        last_refresh: Mutex::new(None),
    }
}

/// One fleet node's boot-time capacity, transport-agnostic: its configured
/// slots and whether it answered its startup probe. Feeds [`evaluate_startup`].
struct NodeCapacity {
    slots: u32,
    reachable: bool,
}

/// The §3.6 startup rule as a fleet-level property (spec §3.1), evaluated ONCE
/// across every transport: the fleet may start iff at least one reachable node
/// has slots > 0. A 0-slot node is placement-inert and never blocks startup,
/// whatever its transport; an unreachable node is out of service, not fatal —
/// unless it was the fleet's only capacity.
fn evaluate_startup(nodes: &[NodeCapacity]) -> Result<(), BackendError> {
    if nodes.iter().any(|n| n.reachable && n.slots > 0) {
        return Ok(());
    }
    let detail = if nodes.iter().any(|n| n.slots > 0) {
        "no node with slots > 0 is reachable"
    } else {
        "no node has slots > 0"
    };
    Err(BackendError::Unavailable(detail.into()))
}

impl FleetBackend {
    /// Partition `DOCKER_NODES` entries: `worker` endpoints become NATS-proxied
    /// nodes; everything else gets its own single-node [`DockerBackend`].
    pub fn new(
        configs: Vec<DockerNodeConfig>,
        store: NatsStore,
        policy: PlacementPolicy,
    ) -> Result<Self, BackendError> {
        let mut nodes = Vec::new();
        for c in configs {
            let handle = if c.endpoint == WORKER_ENDPOINT {
                worker_handle(store.clone(), &c.name, c.slots)
            } else {
                NodeHandle::Docker {
                    backend: Box::new(DockerBackend::new(vec![c.clone()])?),
                }
            };
            nodes.push(Arc::new(FleetNode {
                name: c.name,
                handle,
                reserved: AtomicU32::new(0),
                list_failed: AtomicBool::new(false),
            }));
        }
        // An empty node set is legal: a dynamic fleet may boot with zero seeds
        // (`DOCKER_NODES` empty) and gain capacity when workers announce (spec
        // §3.1 dynamic registration). Launches queue via the NoCapacity path
        // until the first announce arrives (`startup_check` permits it).
        Ok(Self {
            nodes: RwLock::new(nodes),
            store,
            policy,
            place_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// A cheap snapshot of the node set: clone the `Arc`s under a brief read
    /// lock, then release it so callers do their awaits lock-free. Registration
    /// (the sole writer, the single-threaded dispatcher actor) may append or
    /// mutate a node in between, which a reader simply sees on its next snapshot.
    fn snapshot(&self) -> Vec<Arc<FleetNode>> {
        self.nodes.read().unwrap().clone()
    }

    /// §3.6 startup as a fleet-level property (spec §3.1): probe every node,
    /// mark each in/out-of-service and log, then apply the "no live capacity"
    /// hard-fail ONCE across every transport. Capacity is a fleet property, not
    /// a per-sub-backend one: a reachable 0-slot docker node is placement-inert
    /// and must never veto a fleet whose worker (or another docker node) has
    /// slots to spare — the regression that crash-looped prod 2026-07-22.
    ///
    /// A worker that doesn't answer is logged and out-of-service, not fatal —
    /// its daemon dialing in later brings it back without a dispatcher restart.
    /// The pure-`DockerBackend` single-node path (`run.rs`) still fails fast via
    /// [`DockerBackend::ping_all`].
    pub async fn startup_check(&self) -> Result<(), BackendError> {
        let nodes = self.snapshot();
        // Zero seeds is a dynamic fleet awaiting announcements (spec §3.1): start
        // successfully and let launches queue via NoCapacity until the first
        // worker announces. Only a *configured* fleet with no live capacity is a
        // fatal misconfiguration (the crash-loop guard `evaluate_startup` keeps).
        if nodes.is_empty() {
            tracing::warn!(
                "no fleet nodes configured — starting with zero capacity; launches queue until a worker announces (spec §3.1 dynamic registration)"
            );
            return Ok(());
        }
        let mut caps = Vec::with_capacity(nodes.len());
        for node in &nodes {
            match &node.handle {
                NodeHandle::Docker { backend } => {
                    for probe in backend.probe_all().await {
                        caps.push(NodeCapacity {
                            slots: probe.slots,
                            reachable: probe.error.is_none(),
                        });
                    }
                }
                NodeHandle::Worker { slots, .. } => {
                    let reachable = self.probe_worker(node).await.is_some();
                    if !reachable {
                        tracing::warn!(
                            node = %node.name,
                            "worker node unreachable at startup — out of service until its daemon connects"
                        );
                    }
                    caps.push(NodeCapacity {
                        slots: slots.load(Ordering::Relaxed),
                        reachable,
                    });
                }
            }
        }
        evaluate_startup(&caps)
    }

    /// Ping a worker node; updates in_service and returns its live load
    /// (running + free slots) when live.
    async fn probe_worker(&self, node: &FleetNode) -> Option<NodeLoad> {
        let NodeHandle::Worker {
            rpc,
            slots,
            in_service,
            schedulable,
            version_warned,
            last_version,
            last_refresh,
        } = &node.handle
        else {
            return None;
        };
        // Heartbeat lapsed (spec §3.1 dynamic registration): skip placement
        // without even pinging. `route` ignores this flag, so containers already
        // running on the node keep being waited on.
        if !schedulable.load(Ordering::Relaxed) {
            return None;
        }
        match rpc.ping().await {
            Ok(ping) => {
                if !in_service.swap(true, Ordering::Relaxed) {
                    tracing::info!(node = %node.name, version = %ping.version, "worker node back in service");
                }
                // Record the reported version for the platform snapshot. A
                // refreshed daemon reports the new SHA here, which both clears
                // the drift warning below and flows to the UI.
                {
                    let mut v = last_version.lock().unwrap();
                    if v.as_deref() != Some(ping.version.as_str()) {
                        // Version moved (e.g. after a refresh): re-arm the drift
                        // warning so a *new* mismatch is re-reported.
                        version_warned.store(false, Ordering::Relaxed);
                        *v = Some(ping.version.clone());
                    }
                }
                // Record the last refresh outcome (ticket #187) so a failed
                // refresh surfaces in the fleet snapshot rather than staying a
                // node-local log line. Only overwrite when the node reports one;
                // a swapped-in daemon reports `None`, and we keep the last known
                // outcome until it does.
                if let Some(outcome) = &ping.refresh_outcome {
                    *last_refresh.lock().unwrap() = Some(outcome.clone());
                }
                let own = env!("CARGO_PKG_VERSION");
                if !ping.version.starts_with(own) && !version_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        node = %node.name,
                        worker = %ping.version,
                        dispatcher = %own,
                        "worker version differs from dispatcher — artifacts may be stale"
                    );
                }
                Some(NodeLoad {
                    running: ping.running as i64,
                    free: slots.load(Ordering::Relaxed) as i64 - ping.running as i64,
                })
            }
            Err(e) => {
                if in_service.swap(false, Ordering::Relaxed) {
                    tracing::warn!(node = %node.name, "worker node out of service: {e}");
                }
                None
            }
        }
    }

    /// §3.1 placement across the fleet under the configured [`PlacementPolicy`].
    /// Out-of-service workers are skipped, full/0-slot nodes never chosen (#60).
    /// Pinned (`pin`): that node or an error — never a fallback (full/out-of-
    /// service → `NoCapacity` "no free slots on node {name}", queued and retried
    /// by the dispatcher; unknown → a hard `Launch` naming the known nodes). The
    /// decision itself is [`choose_placement`].
    async fn place(
        &self,
        pin: Option<&str>,
    ) -> Result<(Arc<FleetNode>, Reservation), BackendError> {
        // Hold the placement lock across read-loads → choose → reserve: two
        // launches placed back-to-back (agent launches each run on their own
        // spawned task) otherwise both read the pre-reservation counts and tie
        // onto the same node. Serialized, the second sees the first's
        // reservation and busyness sends it to the idle node (spec §3.1).
        let _guard = self.place_lock.lock().await;
        let nodes = self.snapshot();
        let mut candidates = Vec::with_capacity(nodes.len());
        for (i, node) in nodes.iter().enumerate() {
            let load = self.node_load(node).await?; // None ⇒ out of service
            candidates.push(PlacementCandidate {
                index: i,
                name: node.name.as_str(),
                load,
            });
        }
        let index = choose_placement(self.policy, &candidates, pin)?;
        let node = nodes[index].clone();
        // Reserve before releasing the lock so the next placement counts this
        // launch even though its container does not exist yet.
        node.reserved.fetch_add(1, Ordering::SeqCst);
        Ok((node.clone(), Reservation { node }))
    }

    /// Live load on a fleet node: `Ok(Some)` when live, `Ok(None)` for an
    /// out-of-service worker (skipped by placement), `Err` for an unreachable
    /// docker-endpoint node (strict, spec §3.1).
    async fn node_load(&self, node: &FleetNode) -> Result<Option<NodeLoad>, BackendError> {
        let base = match &node.handle {
            NodeHandle::Docker { backend } => backend
                .load_by_node()
                .await?
                .into_iter()
                .map(|(_, running, free)| NodeLoad { running, free })
                .next(),
            NodeHandle::Worker { .. } => self.probe_worker(node).await,
        };
        // Fold in launches already placed on this node whose containers the
        // live count can't see yet (spec §3.1): they occupy the slot for
        // placement purposes, so busyness and the free-slot check both treat a
        // reserved slot as busy.
        Ok(base.map(|l| {
            let reserved = node.reserved.load(Ordering::SeqCst) as i64;
            NodeLoad {
                running: l.running + reserved,
                free: l.free - reserved,
            }
        }))
    }

    /// Per-node health for the platform snapshot (spec §3.1): `(name, in_service)`
    /// as of the last ping/placement probe.
    pub fn availability(&self) -> Vec<(String, bool)> {
        self.snapshot()
            .iter()
            .map(|n| {
                let up = match &n.handle {
                    NodeHandle::Docker { backend } => backend
                        .availability()
                        .first()
                        .map(|(_, a)| *a)
                        .unwrap_or(true),
                    // A worker counts as available only while both reachable
                    // (ping) and schedulable (heartbeat live) — a deregistered
                    // node shows down in the UI, matching that placement skips it.
                    NodeHandle::Worker {
                        in_service,
                        schedulable,
                        ..
                    } => in_service.load(Ordering::Relaxed) && schedulable.load(Ordering::Relaxed),
                };
                (n.name.clone(), up)
            })
            .collect()
    }

    /// Per-node build version for the platform snapshot (spec §3.1): `(name,
    /// version)` as of the last successful ping. `None` for docker-endpoint
    /// nodes (they carry no chuggernaut version) and for workers that have not
    /// answered yet. Lets the UI show fleet versions and spot deploy drift.
    pub fn node_versions(&self) -> Vec<(String, Option<String>)> {
        self.snapshot()
            .iter()
            .map(|n| {
                let version = match &n.handle {
                    NodeHandle::Worker { last_version, .. } => last_version.lock().unwrap().clone(),
                    NodeHandle::Docker { .. } => None,
                };
                (n.name.clone(), version)
            })
            .collect()
    }

    /// Per-node last self-refresh outcome for the platform snapshot (ticket
    /// #187): `(name, outcome)` as of the last successful ping. `None` for
    /// docker-endpoint nodes and workers that have not reported a refresh.
    pub fn node_refreshes(&self) -> Vec<(String, Option<RefreshOutcome>)> {
        self.snapshot()
            .iter()
            .map(|n| {
                let outcome = match &n.handle {
                    NodeHandle::Worker { last_refresh, .. } => last_refresh.lock().unwrap().clone(),
                    NodeHandle::Docker { .. } => None,
                };
                (n.name.clone(), outcome)
            })
            .collect()
    }

    fn route(&self, id: &ContainerId) -> Result<Arc<FleetNode>, BackendError> {
        let (name, _) = id
            .split_once('/')
            .ok_or_else(|| BackendError::NotFound(id.clone()))?;
        self.snapshot()
            .iter()
            .find(|n| n.name == name)
            .cloned()
            .ok_or_else(|| BackendError::NotFound(id.clone()))
    }
}

fn rpc_err(id: Option<&ContainerId>, e: WorkerRpcError) -> BackendError {
    match e {
        WorkerRpcError::Op(WorkerError::NotFound { id }) => BackendError::NotFound(id),
        WorkerRpcError::Op(WorkerError::Unavailable { message }) => {
            BackendError::Unavailable(message)
        }
        WorkerRpcError::Op(WorkerError::Launch { message }) => BackendError::Launch(message),
        WorkerRpcError::Op(WorkerError::NoCapacity { message }) => {
            BackendError::NoCapacity(message)
        }
        WorkerRpcError::Op(WorkerError::Other { message }) => BackendError::Other(message),
        WorkerRpcError::Transport(m) => match id {
            Some(id) => BackendError::Other(format!("worker transport for {id}: {m}")),
            None => BackendError::Unavailable(format!("worker transport: {m}")),
        },
    }
}

fn to_wire(config: &ContainerLaunchConfig) -> WorkerLaunchRequest {
    WorkerLaunchRequest {
        image: config.image.clone(),
        cmd: config.cmd.clone(),
        env: config.env.clone(),
        files: config
            .files
            .iter()
            .map(|f| WireFile {
                container_path: f.container_path.clone(),
                mode: f.mode,
                source: match &f.artifact {
                    // The worker substitutes its node-local copy; bytes stay home.
                    Some(name) => FileSource::LocalArtifact { name: name.clone() },
                    None => FileSource::Inline {
                        data_b64: b64_encode(&f.contents),
                    },
                },
            })
            .collect(),
        cpu_limit: config.cpu_limit,
        memory_limit: config.memory_limit.clone(),
    }
}

#[async_trait]
impl ContainerBackend for FleetBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        // `_reservation` is held until this method returns — i.e. across the
        // launch RPC. Once the RPC completes the container exists and the node's
        // live count reports it, so releasing the reservation then hands the
        // accounting back to the live count with no gap and no double-count.
        let (node, _reservation) = self.place(config.node.as_deref()).await?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.launch(config).await,
            NodeHandle::Worker { rpc, .. } => {
                let req = to_wire(&config);
                let ok = rpc.launch(&req).await.map_err(|e| rpc_err(None, e))?;
                Ok(ok.id)
            }
        }
    }

    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.wait(id).await,
            NodeHandle::Worker { rpc, .. } => loop {
                match rpc.inspect(id).await {
                    Ok(ok) => match ok.status {
                        Some(WireStatus::Exited { exit_code }) => return Ok(exit_code),
                        Some(WireStatus::Running) => {}
                        None => return Err(BackendError::NotFound(id.clone())),
                    },
                    // Transport blips (worker restart, NATS reconnect) are
                    // survivable — the container is still running on the node;
                    // keep polling. Op-level errors are real.
                    Err(WorkerRpcError::Transport(m)) => {
                        tracing::debug!(container = %id, "wait poll transport error (retrying): {m}");
                    }
                    Err(e) => return Err(rpc_err(Some(id), e)),
                }
                tokio::time::sleep(WAIT_POLL).await;
            },
        }
    }

    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.kill(id).await,
            NodeHandle::Worker { rpc, .. } => rpc.kill(id).await.map_err(|e| rpc_err(Some(id), e)),
        }
    }

    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.inspect(id).await,
            NodeHandle::Worker { rpc, .. } => {
                let ok = rpc.inspect(id).await.map_err(|e| rpc_err(Some(id), e))?;
                Ok(ok.status.map(|s| match s {
                    WireStatus::Running => ContainerStatus::Running,
                    WireStatus::Exited { exit_code } => ContainerStatus::Exited { exit_code },
                }))
            }
        }
    }

    async fn copy_file(
        &self,
        id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.copy_file(id, path).await,
            NodeHandle::Worker { rpc, .. } => {
                let ok = rpc
                    .copy_file(id, path)
                    .await
                    .map_err(|e| rpc_err(Some(id), e))?;
                match ok.data_b64 {
                    Some(b64) => Ok(Some(b64_decode(&b64).map_err(BackendError::Other)?)),
                    None => Ok(None),
                }
            }
        }
    }

    async fn logs(&self, id: &ContainerId) -> Result<Vec<u8>, BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.logs(id).await,
            NodeHandle::Worker { rpc, .. } => {
                let ok = rpc.logs(id).await.map_err(|e| rpc_err(Some(id), e))?;
                let mut data = b64_decode(&ok.data_b64).map_err(BackendError::Other)?;
                if ok.truncated {
                    let mut note = b"[worker: logs truncated to the most recent 700KiB]\n".to_vec();
                    note.append(&mut data);
                    data = note;
                }
                Ok(data)
            }
        }
    }

    async fn logs_tail(&self, id: &ContainerId, since: u64) -> Result<LogTail, BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.logs_tail(id, since).await,
            NodeHandle::Worker { rpc, .. } => {
                let ok = rpc
                    .logs_tail(id, since)
                    .await
                    .map_err(|e| rpc_err(Some(id), e))?;
                let data = b64_decode(&ok.data_b64).map_err(BackendError::Other)?;
                Ok(LogTail {
                    offset: ok.offset,
                    data,
                })
            }
        }
    }

    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.remove(id).await,
            NodeHandle::Worker { rpc, .. } => {
                rpc.remove(id).await.map_err(|e| rpc_err(Some(id), e))
            }
        }
    }

    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
        let mut ids = Vec::new();
        for node in &self.snapshot() {
            match &node.handle {
                NodeHandle::Docker { backend } => ids.extend(backend.list_managed_exited().await?),
                NodeHandle::Worker { rpc, .. } => match rpc.list_exited().await {
                    Ok(ok) => ids.extend(ok.ids),
                    // An unreachable worker must not fail the whole sweep —
                    // its exited containers get reclaimed on a later pass.
                    Err(e) => {
                        tracing::warn!(node = %node.name, "list_exited skipped: {e}");
                    }
                },
            }
        }
        Ok(ids)
    }

    async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError> {
        let mut out = Vec::new();
        for node in &self.snapshot() {
            match &node.handle {
                NodeHandle::Docker { backend } => out.extend(backend.list_managed_running().await?),
                NodeHandle::Worker { rpc, .. } => match rpc.list_running().await {
                    Ok(ok) => {
                        node.list_failed.store(false, Ordering::Relaxed);
                        out.extend(ok.containers.into_iter().map(|c| RunningContainer {
                            id: c.id,
                            project: c.project,
                            job: c.job,
                            task: c.task,
                        }));
                    }
                    // An unreachable worker must not fail the whole sweep — its
                    // orphans get reaped on a later pass. Record the failure so
                    // the occupancy snapshot can show the node out-of-service
                    // rather than falsely idle (spec §3.1; job/181): the node may
                    // still answer ping/launch, so nothing else marks it down.
                    Err(e) => {
                        node.list_failed.store(true, Ordering::Relaxed);
                        tracing::warn!(node = %node.name, "fleet occupancy: list_running failed — node shown out of service: {e}");
                    }
                },
            }
        }
        Ok(out)
    }

    fn fleet_status(&self) -> Vec<NodeStatus> {
        let versions = self.node_versions();
        let refreshes = self.node_refreshes();
        self.availability()
            .into_iter()
            .map(|(name, available)| {
                let version = versions
                    .iter()
                    .find(|(n, _)| n == &name)
                    .and_then(|(_, v)| v.clone());
                let refresh_outcome = refreshes
                    .iter()
                    .find(|(n, _)| n == &name)
                    .and_then(|(_, o)| o.clone());
                NodeStatus {
                    name,
                    available,
                    version,
                    refresh_outcome,
                }
            })
            .collect()
    }

    /// Apply a worker announce (spec §3.1 dynamic registration). The live
    /// announcement wins: an existing worker of the same name has its slot cap,
    /// build version, schedulability, and reachability refreshed; a new name is
    /// added with its own [`WorkerRpc`]; a name already held by a docker-endpoint
    /// node is refused (an announce can't repurpose a directly-driven daemon).
    /// Returns whether fleet membership or capacity changed (a join, or a slot
    /// change) so the caller logs a join and re-drains the launch queue only when
    /// it matters. Runs on the single-writer actor — the fleet's only writer.
    fn register_worker(&self, name: &str, slots: u32, version: Option<String>) -> bool {
        let mut nodes = self.nodes.write().unwrap();
        let shape: Vec<(&str, bool)> = nodes
            .iter()
            .map(|n| {
                (
                    n.name.as_str(),
                    matches!(n.handle, NodeHandle::Worker { .. }),
                )
            })
            .collect();
        match plan_register(&shape, name) {
            RegisterAction::RejectDockerName => {
                tracing::warn!(
                    node = %name,
                    "worker announce ignored — name is held by a docker-endpoint node"
                );
                false
            }
            RegisterAction::Update(i) => {
                let NodeHandle::Worker {
                    slots: slot_cell,
                    in_service,
                    schedulable,
                    last_version,
                    ..
                } = &nodes[i].handle
                else {
                    return false;
                };
                // A live announcement re-admits the node to scheduling and marks
                // it reachable; the next placement ping refines load. Only a slot
                // change (or re-admitting a heartbeat-dropped node) is "capacity
                // moved" for the caller's drain.
                let changed = slot_cell.swap(slots, Ordering::Relaxed) != slots
                    || !schedulable.swap(true, Ordering::Relaxed);
                in_service.store(true, Ordering::Relaxed);
                if let Some(v) = version {
                    *last_version.lock().unwrap() = Some(v);
                }
                changed
            }
            RegisterAction::Add => {
                let handle = worker_handle(self.store.clone(), name, slots);
                if let NodeHandle::Worker { last_version, .. } = &handle
                    && let Some(v) = version
                {
                    *last_version.lock().unwrap() = Some(v);
                }
                nodes.push(Arc::new(FleetNode {
                    name: name.to_string(),
                    handle,
                    reserved: AtomicU32::new(0),
                    list_failed: AtomicBool::new(false),
                }));
                true
            }
        }
    }

    /// The fleet backend is the one backend that routes to announced workers, so
    /// it accepts dynamic registration (spec §3.1). Lets the dispatcher gate its
    /// roster mutation on real acceptance rather than the ambiguous
    /// [`Self::register_worker`] bool.
    fn supports_dynamic_workers(&self) -> bool {
        true
    }

    /// Mark an announced worker unschedulable after its heartbeat lapses (spec
    /// §3.1): placement skips it (`probe_worker` short-circuits), but `route`
    /// still reaches it, so containers already running there keep being waited on
    /// and the poll-based `wait` re-attaches. A no-op for a docker node or an
    /// unknown name. A later announce re-admits it via [`register_worker`].
    fn mark_worker_unschedulable(&self, name: &str) {
        if let Some(node) = self.snapshot().iter().find(|n| n.name == name)
            && let NodeHandle::Worker { schedulable, .. } = &node.handle
        {
            schedulable.store(false, Ordering::Relaxed);
        }
    }

    /// Worker nodes whose last `list_running` failed (spec §3.1 occupancy). A
    /// node that answers `ping` but rejects `list_running` (a stale daemon that
    /// predates the op) stays schedulable, yet its containers are invisible to
    /// occupancy; naming it here lets the snapshot show it out-of-service instead
    /// of a false-idle `occupied: 0` — the silent all-zero of job/181.
    fn occupancy_unavailable_nodes(&self) -> Vec<String> {
        self.snapshot()
            .iter()
            .filter(|n| {
                matches!(n.handle, NodeHandle::Worker { .. })
                    && n.list_failed.load(Ordering::Relaxed)
            })
            .map(|n| n.name.clone())
            .collect()
    }
}

/// Convenience for `run.rs`: does this fleet contain any worker nodes?
pub fn has_worker_nodes(configs: &[DockerNodeConfig]) -> bool {
    configs.iter().any(|c| c.endpoint == WORKER_ENDPOINT)
}

// Arc so run.rs can pass it around like the DockerBackend today.
pub type SharedFleet = Arc<FleetBackend>;

#[cfg(test)]
mod tests {
    //! Fleet-level startup capacity (spec §3.1/§3.6) — the pure decision, no
    //! docker daemon or NATS needed. `(reachable, slots)` faithfully models each
    //! node's boot probe regardless of transport (docker or worker).
    use super::{NodeCapacity, RegisterAction, evaluate_startup, plan_register};

    fn node(reachable: bool, slots: u32) -> NodeCapacity {
        NodeCapacity { slots, reachable }
    }

    /// The announce precedence decision (spec §3.1 dynamic registration), the
    /// pure core of `register_worker`: an unknown name is added, a matching
    /// worker is updated in place (so its slot count/version can move — the
    /// live announcement wins), and a name already held by a docker-endpoint
    /// node is refused.
    #[test]
    fn plan_register_precedence() {
        // Static + dynamic merge: a seeded worker of the same name is updated,
        // not duplicated — the announce's slot count then wins.
        let roster = [("air", true), ("local", false)];
        assert_eq!(plan_register(&roster, "air"), RegisterAction::Update(0));
        // A brand-new node joins.
        assert_eq!(plan_register(&roster, "nuc"), RegisterAction::Add);
        // An announce can't repurpose a docker-endpoint node of the same name.
        assert_eq!(
            plan_register(&roster, "local"),
            RegisterAction::RejectDockerName
        );
        // Zero-seed fleet: the first announce is always an Add.
        assert_eq!(plan_register(&[], "air"), RegisterAction::Add);
    }

    /// The outage case: a reachable 0-slot docker placeholder plus a responding
    /// 4-slot worker starts fine. The 0-slot node must not veto the fleet.
    #[test]
    fn zero_slot_docker_does_not_veto_worker_capacity() {
        assert!(evaluate_startup(&[node(true, 0), node(true, 4)]).is_ok());
    }

    /// A reachable 0-slot docker node with the only worker unreachable ⇒ no live
    /// capacity anywhere ⇒ refuse to start.
    #[test]
    fn zero_slot_docker_plus_dead_worker_fails() {
        let err = evaluate_startup(&[node(true, 0), node(false, 4)]).unwrap_err();
        assert!(err.to_string().contains("reachable"), "{err}");
    }

    /// An unreachable docker node is out-of-service, not fatal, when a responding
    /// worker carries the fleet's capacity.
    #[test]
    fn unreachable_docker_starts_when_worker_responds() {
        assert!(evaluate_startup(&[node(false, 2), node(true, 4)]).is_ok());
    }

    /// A single reachable node with slots is the all-docker single-node path —
    /// unchanged: it starts.
    #[test]
    fn single_reachable_node_with_slots_starts() {
        assert!(evaluate_startup(&[node(true, 4)]).is_ok());
    }

    /// Every node reachable but all 0-slot ⇒ nothing can ever be placed ⇒ refuse.
    #[test]
    fn all_zero_slot_reachable_fails() {
        let err = evaluate_startup(&[node(true, 0), node(true, 0)]).unwrap_err();
        assert!(err.to_string().contains("slots > 0"), "{err}");
    }
}

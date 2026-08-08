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
//! fleet that has slots elsewhere. That hard-fail is narrowed to *capacity*
//! (design #293 §5a): worker capacity is observed and operator-changeable, so
//! zero there warns rather than refusing the boot, while reachability stays
//! fatal — see [`evaluate_startup`].
//!
//! A worker node's capacity arrives over two transports of the same source
//! (spec §3.1 slot source): the `WorkerAnnounce` push and the `ping` reply.
//! Both land through [`ingest_capacity`], which orders them by
//! `(capacity_epoch, capacity_generation)`, so the `DOCKER_NODES` slot field is
//! a pre-observation fallback for worker nodes rather than a competing source —
//! and `fleet_status` reports which of the two each node is running on.
//!
//! Its **capabilities** ride the same two transports and land through
//! [`ingest_capabilities`] (design #309 §4), but under a different rule: there is
//! no ordering key, the `ping` reply is authoritative, and an announce applies
//! only while no ping has answered for the node. A docker-endpoint node can
//! answer neither, so [`FleetBackend::node_capabilities`] synthesizes its
//! capabilities from the node kind. Placement reads their `modes`: a launch is
//! placed only on a node serving the mode its `image` selects (#309 §5a).

use async_trait::async_trait;
use container::docker::{DockerBackend, DockerNodeConfig};
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus, LogTail,
    NodeLoad, NodeStatus, PlacementCandidate, PlacementPolicy, RunningContainer, choose_placement,
};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use store::NatsStore;
use store::worker::{MAX_COPY_FILE_BYTES, WorkerRpc, WorkerRpcError};
use types::job_type::RuntimeMode;
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
    Docker {
        backend: Box<DockerBackend>,
    },
    Worker {
        rpc: Box<WorkerRpc>,
        /// Slot cap — the ONE number placement reads (spec §3.1 slot source).
        /// `Atomic` because an observation over either transport can change it
        /// (e.g. air 4→5) without replacing the node. Seeded from `DOCKER_NODES`
        /// as a pre-observation fallback that no longer wins once the node has
        /// reported (design #293 §7); [`FleetNode::capacity`] records which.
        slots: AtomicU32,
        /// The node's observation watermark and provenance (spec §3.1 slot
        /// source). Every write of `slots` goes through it, so one ordering rule
        /// governs both transports. `Mutex` rather than atomics because the pair
        /// and the observed-at stamp move together — the same reason the daemon
        /// side takes one lock. Boxed for the same reason `backend` above is:
        /// it is what tips this variant past `clippy::large_enum_variant`, and
        /// nodes are few and long-lived so the indirection costs nothing.
        capacity: Box<Mutex<types::ObservedCapacity>>,
        /// What the node says it can do (design #309 §4). Boxed and locked for
        /// the same reasons `capacity` is, but ordered by transport alone — a
        /// capability is a boot-time fact with no ordering key.
        capabilities: Box<Mutex<types::ObservedCapabilities>>,
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
    /// Cadence state for the fleet-wide unadvertised-mode warning (design #309
    /// §5a), so a job type requiring a mode the whole fleet lacks is a logged
    /// configuration error rather than a silently queueing launch.
    mode_warnings: container::ModeWarnings,
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
fn worker_handle(store: NatsStore, name: &str, seed_slots: u32) -> NodeHandle {
    NodeHandle::Worker {
        rpc: Box::new(WorkerRpc::new(store, name.to_string())),
        slots: AtomicU32::new(seed_slots),
        capacity: Box::new(Mutex::new(types::ObservedCapacity::default())),
        capabilities: Box::new(Mutex::new(types::ObservedCapabilities::default())),
        in_service: AtomicBool::new(true),
        schedulable: AtomicBool::new(true),
        version_warned: AtomicBool::new(false),
        last_version: Mutex::new(None),
        last_refresh: Mutex::new(None),
    }
}

/// What one node handle reads as being capable of (design #309 §4): a worker's
/// observation, or the synthesis a docker-endpoint node has no wire path to
/// advertise. The single resolution site, so placement and the snapshot agree.
fn handle_capabilities(handle: &NodeHandle) -> types::worker::NodeCapabilities {
    match handle {
        NodeHandle::Docker { .. } => types::worker::NodeCapabilities::docker_endpoint(),
        NodeHandle::Worker { capabilities, .. } => capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .effective(),
    }
}

/// Ingest one capacity observation for a worker node (spec §3.1 slot source):
/// order it against the node's watermark and, when it wins, install its slot
/// count as the one number placement reads. The single place either transport
/// writes the slot cell, so the ordering rule cannot be bypassed by adding a
/// third caller. Returns whether the live slot count actually MOVED — a
/// re-report of the number already in force is applied but changes nothing, and
/// the caller must not treat it as new capacity.
fn ingest_capacity(
    slot_cell: &AtomicU32,
    capacity: &Mutex<types::ObservedCapacity>,
    observation: &types::CapacityObservation,
) -> bool {
    let mut observed = capacity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !observed.apply(observation, chrono::Utc::now()) {
        return false;
    }
    debug_assert!(
        observed.observed_at.is_some(),
        "an applied observation must demote the seed"
    );
    let moved = slot_cell.swap(observation.slots, Ordering::Relaxed) != observation.slots;
    debug_assert_eq!(
        slot_cell.load(Ordering::Relaxed),
        observation.slots,
        "the slot cell placement reads must hold the number just applied"
    );
    moved
}

/// Ingest one capability advertisement for a worker node (design #309 §4):
/// apply it under the ping-wins precedence and report whether the node now reads
/// differently. The single place either transport writes the cell, so the
/// precedence cannot be bypassed by adding a third caller.
fn ingest_capabilities(
    cell: &Mutex<types::ObservedCapabilities>,
    advertised: &types::worker::NodeCapabilities,
    transport: types::CapacityTransport,
) -> bool {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply(advertised, transport)
}

/// Apply everything a `ping` reply reports about the node itself — its capacity
/// (spec §3.1 slot source) and its capabilities (design #309 §4) — on the reply
/// path, before the load is computed and before the startup gate reads the node.
/// One place, so the two observations cannot drift apart in their ordering.
fn ingest_ping(
    name: &str,
    ping: &types::worker::PingOk,
    slot_cell: &AtomicU32,
    capacity: &Mutex<types::ObservedCapacity>,
    capabilities: &Mutex<types::ObservedCapabilities>,
) {
    if let Some(observation) = types::CapacityObservation::from_ping(ping)
        && ingest_capacity(slot_cell, capacity, &observation)
    {
        tracing::info!(
            node = %name,
            slots = observation.slots,
            "worker capacity updated from ping (spec §3.1 slot source)"
        );
    }
    if let Some(advertised) = &ping.capabilities
        && ingest_capabilities(capabilities, advertised, types::CapacityTransport::Ping)
    {
        tracing::info!(
            node = %name,
            modes = ?advertised.modes,
            platform = %advertised.platform,
            "worker capabilities updated from ping (design #309 §4)"
        );
    }
}

/// One fleet node's boot-time capacity: its slots as of its startup probe,
/// whether it answered, and which transport it is — the §5a rule turns on the
/// last, so [`evaluate_startup`] cannot be expressed without it.
struct NodeCapacity {
    slots: u32,
    reachable: bool,
    /// A NATS-proxied worker node (as opposed to a docker-endpoint node).
    worker: bool,
}

/// The §3.6 startup rule as a fleet-level property (spec §3.1), evaluated ONCE
/// across every transport and **narrowed so that worker capacity never vetoes a
/// boot** (design #293 §5a).
///
/// The dispatcher refuses to start only if no worker-endpoint node is reachable
/// AND no reachable docker-endpoint node has `slots > 0`. The asymmetry follows
/// ownership: a docker-endpoint node's slot count is static config that only a
/// restart can change, so zero there is a fatal misconfiguration and the
/// crash-loop guard should keep catching it; a worker node's capacity is
/// *observed*, arrives after boot, and is operator-changeable at runtime, so
/// zero there means "not yet reported, or deliberately drained" — and refusing
/// to boot on it would make a drain unrecoverable from the UI that caused it.
///
/// **Only capacity is narrowed; reachability is not.** A fleet with no reachable
/// node of either transport still fails fast: whole-fleet-unreachable is the one
/// condition §3.6 reserves for fail-fast and the deploy-time catcher for bad
/// credentials or a wrong `NATS_URL` — the very failure class behind the
/// incident this rule comes from. Widening the rule to the mere *presence* of a
/// worker node would spend that signal; the tests that pin the difference are
/// `zero_slot_docker_plus_dead_worker_fails` here and
/// `no_reachable_capacity_fails_startup` at tier 2.
fn evaluate_startup(nodes: &[NodeCapacity]) -> Result<StartupCapacity, BackendError> {
    if nodes.iter().any(|n| n.reachable && n.slots > 0) {
        return Ok(StartupCapacity::Live);
    }
    if nodes.iter().any(|n| n.worker && n.reachable) {
        return Ok(StartupCapacity::ZeroWithReachableWorker);
    }
    let detail = if nodes.iter().any(|n| n.slots > 0) {
        "no node with slots > 0 is reachable"
    } else {
        "no node has slots > 0"
    };
    Err(BackendError::Unavailable(detail.into()))
}

/// What [`evaluate_startup`] concluded about the fleet's live capacity; the
/// zero-capacity start is a distinct outcome rather than a bare `Ok(())`
/// because §5a's trade of a crash-loop for a warning is only correct while the
/// warning actually happens. The decision stays pure and the caller performs
/// the log (docs/reference/style.md Tier 2: deciders return, they don't do).
#[derive(Debug, PartialEq, Eq)]
enum StartupCapacity {
    /// At least one reachable node has `slots > 0`.
    Live,
    /// Every reachable node reports 0 slots — drained, or nothing has reported
    /// yet — and a reachable worker node makes that recoverable without a
    /// restart.
    ZeroWithReachableWorker,
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
        Ok(Self {
            nodes: RwLock::new(nodes),
            store,
            policy,
            place_lock: tokio::sync::Mutex::new(()),
            mode_warnings: container::ModeWarnings::default(),
        })
    }

    /// A cheap snapshot of the node set: clone the `Arc`s under a brief read
    /// lock, then release it so callers do their awaits lock-free. Registration
    /// (the sole writer, the single-threaded dispatcher actor) may append or
    /// mutate a node in between, which a reader simply sees on its next snapshot.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
    )]
    fn snapshot(&self) -> Vec<Arc<FleetNode>> {
        self.nodes.read().unwrap().clone()
    }

    /// What one node reads as being capable of (design #309 §4): its
    /// advertisement, the absent reading while it has made none, or the
    /// synthesized values for a docker-endpoint node, which has no wire path to
    /// advertise on. `None` for a name the fleet does not hold.
    pub fn node_capabilities(&self, node: &str) -> Option<types::worker::NodeCapabilities> {
        self.snapshot()
            .into_iter()
            .find(|n| n.name == node)
            .map(|n| handle_capabilities(&n.handle))
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
                            worker: false,
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
                        worker: true,
                    });
                }
            }
        }
        if evaluate_startup(&caps)? == StartupCapacity::ZeroWithReachableWorker {
            tracing::warn!(
                "fleet starting with ZERO capacity: every reachable node reports 0 slots \
                 (drained, or no worker has reported one yet). Nothing will be placed — \
                 launches queue via the §3.5 NoCapacity path until capacity is observed \
                 or commanded (spec §3.1)"
            );
        }
        Ok(())
    }

    /// Ping a worker node; updates in_service and returns its live load
    /// (running + free slots) when live.
    ///
    /// This is also the **pull** half of the one capacity source (spec §3.1 slot
    /// source): a `ping` reply carrying `slots` is applied to the node's slot
    /// cell here, on the reply path, BEFORE the load is computed and before
    /// [`Self::startup_check`] reads the cell into the startup gate. A ping
    /// cannot be a stale in-flight message, so it applies unconditionally and
    /// resets the watermark — the backstop that keeps any ordering anomaly
    /// self-healing at the next placement probe rather than terminal.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
    )]
    async fn probe_worker(&self, node: &FleetNode) -> Option<NodeLoad> {
        let NodeHandle::Worker {
            rpc,
            slots,
            capacity,
            capabilities,
            in_service,
            schedulable,
            version_warned,
            last_version,
            last_refresh,
        } = &node.handle
        else {
            return None;
        };
        if !schedulable.load(Ordering::Relaxed) {
            return None;
        }
        match rpc.ping().await {
            Ok(ping) => {
                if !in_service.swap(true, Ordering::Relaxed) {
                    tracing::info!(node = %node.name, version = %ping.version, "worker node back in service");
                }
                {
                    let mut v = last_version.lock().unwrap();
                    if v.as_deref() != Some(ping.version.as_str()) {
                        version_warned.store(false, Ordering::Relaxed);
                        *v = Some(ping.version.clone());
                    }
                }
                if let Some(outcome) = &ping.refresh_outcome {
                    *last_refresh.lock().unwrap() = Some(outcome.clone());
                }
                ingest_ping(&node.name, &ping, slots, capacity, capabilities);
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

    /// §3.1 placement across the fleet under the configured [`PlacementPolicy`],
    /// probing every node for the load and capabilities the decision reads. The
    /// decision itself is [`choose_placement`], whose postcondition — pinned and
    /// unpinned — is `docs/implementation-notes.md`.
    async fn place(
        &self,
        pin: Option<&str>,
        required: RuntimeMode,
    ) -> Result<(Arc<FleetNode>, Reservation), BackendError> {
        let _guard = self.place_lock.lock().await;
        let nodes = self.snapshot();
        let mut probed = Vec::with_capacity(nodes.len());
        for node in nodes.iter() {
            let load = self.node_load(node).await?;
            probed.push((load, handle_capabilities(&node.handle)));
        }
        let candidates: Vec<PlacementCandidate<'_>> = probed
            .iter()
            .enumerate()
            .map(|(i, (load, capabilities))| PlacementCandidate {
                index: i,
                name: nodes[i].name.as_str(),
                load: *load,
                modes: &capabilities.modes,
            })
            .collect();
        self.mode_warnings.observe(&candidates, required);
        let index = choose_placement(self.policy, &candidates, pin, required)?;
        let node = nodes[index].clone();
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
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
    )]
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
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
    )]
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

    /// Per-worker-node live capacity and its provenance for the platform
    /// snapshot (spec §3.1 slot source): `(name, (slots, observed))`, one entry
    /// per *worker* node — docker-endpoint nodes are omitted because
    /// `DOCKER_NODES` remains their capacity's owner (design #293 §7). `slots`
    /// is the one number placement reads; `observed` says whether it came from
    /// the node or is still the boot seed standing in.
    fn node_capacities(&self) -> Vec<(String, (u32, types::ObservedCapacity))> {
        self.snapshot()
            .iter()
            .filter_map(|n| match &n.handle {
                NodeHandle::Worker {
                    slots, capacity, ..
                } => {
                    let observed = *capacity
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    Some((n.name.clone(), (slots.load(Ordering::Relaxed), observed)))
                }
                NodeHandle::Docker { .. } => None,
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

/// Read one container file off a worker node in [`MAX_COPY_FILE_BYTES`] slices
/// (design #362 S1), so an output archive past a single reply's bound still
/// travels. Bounded by construction: `max_bytes` fixes the slice count, so a
/// node returning short chunks forever hits the cap instead of looping.
async fn copy_file_chunked_rpc(
    rpc: &WorkerRpc,
    id: &ContainerId,
    path: &str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, BackendError> {
    debug_assert!(max_bytes > 0, "a zero-byte ceiling reads nothing");
    let chunks_max = max_bytes.div_ceil(MAX_COPY_FILE_BYTES) + 1;
    let mut out: Vec<u8> = Vec::new();
    for _ in 0..chunks_max {
        let ok = rpc
            .copy_file_chunk(id, path, out.len() as u64, max_bytes as u64)
            .await
            .map_err(|e| rpc_err(Some(id), e))?;
        let Some(b64) = ok.data_b64 else {
            return Ok(None);
        };
        let chunk = b64_decode(&b64).map_err(BackendError::Other)?;
        let advanced = chunk.len();
        out.extend_from_slice(&chunk);
        if out.len() as u64 >= ok.total_len {
            return Ok(Some(out));
        }
        if advanced == 0 {
            return Err(BackendError::Other(format!(
                "{path}: worker returned an empty slice at offset {} of {} bytes",
                out.len(),
                ok.total_len
            )));
        }
    }
    Err(BackendError::Other(format!(
        "{path}: still incomplete after {chunks_max} slice reads bounded by {max_bytes} bytes"
    )))
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
                    Some(name) => FileSource::LocalArtifact { name: name.clone() },
                    None => FileSource::Inline {
                        data_b64: b64_encode(&f.contents),
                    },
                },
            })
            .collect(),
        cpu_limit: config.cpu_limit,
        memory_limit: config.memory_limit.clone(),
        runtime_env: config.runtime_env.clone(),
    }
}

#[async_trait]
impl ContainerBackend for FleetBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        let (node, _reservation) = self
            .place(config.node.as_deref(), config.required_mode())
            .await?;
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

    async fn copy_file_chunked(
        &self,
        id: &ContainerId,
        path: &str,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.copy_file_chunked(id, path, max_bytes).await,
            NodeHandle::Worker { rpc, .. } => copy_file_chunked_rpc(rpc, id, path, max_bytes).await,
        }
    }

    async fn find_file(
        &self,
        id: &ContainerId,
        dir: &str,
        name: &str,
    ) -> Result<Vec<String>, BackendError> {
        let node = self.route(id)?;
        match &node.handle {
            NodeHandle::Docker { backend } => backend.find_file(id, dir, name).await,
            NodeHandle::Worker { rpc, .. } => {
                let ok = rpc
                    .find_file(id, dir, name)
                    .await
                    .map_err(|e| rpc_err(Some(id), e))?;
                Ok(ok.paths)
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
        let capacities = self.node_capacities();
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
                let capacity = capacities.iter().find(|(n, _)| n == &name).map(|(_, c)| *c);
                NodeStatus {
                    name,
                    available,
                    version,
                    refresh_outcome,
                    slots: capacity.map(|(slots, _)| slots),
                    capacity: capacity.map(|(_, observed)| observed),
                }
            })
            .collect()
    }

    /// Apply a worker announce (spec §3.1 dynamic registration). An existing
    /// worker of the same name has its build version, schedulability and
    /// reachability refreshed; a new name is added with its own [`WorkerRpc`]; a
    /// name already held by a docker-endpoint node is refused (an announce can't
    /// repurpose a directly-driven daemon).
    ///
    /// Its **capacity**, unlike the rest, is ordered: it is applied through
    /// [`ingest_capacity`] and so lands only when its
    /// `(capacity_epoch, capacity_generation)` pair is at least the node's
    /// watermark. A stale in-flight heartbeat therefore refreshes liveness while
    /// leaving the fresher slot count alone. Returns whether fleet membership or
    /// capacity changed (a join, or a slot change) so the caller logs a join and
    /// re-drains the launch queue only when it matters. Runs on the single-writer
    /// actor — the fleet's only writer.
    #[allow(
        clippy::unwrap_used,
        reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
    )]
    fn register_worker(
        &self,
        name: &str,
        capacity: types::CapacityObservation,
        version: Option<String>,
        advertised: Option<types::worker::NodeCapabilities>,
    ) -> bool {
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
                    capacity: observed,
                    capabilities,
                    in_service,
                    schedulable,
                    last_version,
                    ..
                } = &nodes[i].handle
                else {
                    return false;
                };
                let changed = ingest_capacity(slot_cell, observed, &capacity)
                    || !schedulable.swap(true, Ordering::Relaxed);
                in_service.store(true, Ordering::Relaxed);
                if let Some(a) = &advertised {
                    ingest_capabilities(capabilities, a, types::CapacityTransport::Announce);
                }
                if let Some(v) = version {
                    *last_version.lock().unwrap() = Some(v);
                }
                changed
            }
            RegisterAction::Add => {
                let handle = worker_handle(self.store.clone(), name, capacity.slots);
                if let NodeHandle::Worker {
                    slots: slot_cell,
                    capacity: observed,
                    capabilities,
                    last_version,
                    ..
                } = &handle
                {
                    ingest_capacity(slot_cell, observed, &capacity);
                    if let Some(a) = &advertised {
                        ingest_capabilities(capabilities, a, types::CapacityTransport::Announce);
                    }
                    if let Some(v) = version {
                        *last_version.lock().unwrap() = Some(v);
                    }
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

    /// Relay the operator's desired slot count to one node's daemon (spec §3.1
    /// operator capacity control). Pure push: the reply's capacity fields are
    /// *not* ingested here, because the caller runs this off the actor thread and
    /// the node re-announces immediately anyway — so observation keeps arriving
    /// through the two transports [`ingest_capacity`] orders, and this path can
    /// never become a third, unordered one.
    ///
    /// An unknown name, or a docker-endpoint node (whose capacity `DOCKER_NODES`
    /// owns), is `Unavailable` — the caller refuses those upstream, so reaching
    /// here means the roster and the fleet disagree.
    async fn set_node_slots(
        &self,
        node: &str,
        slots: u32,
    ) -> Result<types::worker::SetSlotsOk, BackendError> {
        let handle = self
            .snapshot()
            .into_iter()
            .find(|n| n.name == node)
            .ok_or_else(|| BackendError::Unavailable(format!("unknown fleet node {node}")))?;
        let NodeHandle::Worker { rpc, .. } = &handle.handle else {
            return Err(BackendError::Unavailable(format!(
                "node {node} is a docker endpoint — DOCKER_NODES owns its capacity"
            )));
        };
        rpc.set_slots(&types::worker::SetSlotsRequest { slots })
            .await
            .map_err(|e| rpc_err(None, e))
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

pub type SharedFleet = Arc<FleetBackend>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Fleet-level startup capacity (spec §3.1/§3.6) — the pure decision, no
    //! docker daemon or NATS needed. `(reachable, slots)` faithfully models each
    //! node's boot probe regardless of transport (docker or worker).
    use super::{NodeCapacity, RegisterAction, StartupCapacity, evaluate_startup, plan_register};

    /// A docker-endpoint node: its slot count is static `DOCKER_NODES` config
    /// that only a restart can change, so zero there stays fatal.
    fn node(reachable: bool, slots: u32) -> NodeCapacity {
        NodeCapacity {
            slots,
            reachable,
            worker: false,
        }
    }

    /// A worker-endpoint node: its capacity is observed after boot and
    /// operator-changeable at runtime (design #293 §5a).
    fn worker(reachable: bool, slots: u32) -> NodeCapacity {
        NodeCapacity {
            slots,
            reachable,
            worker: true,
        }
    }

    /// The announce precedence decision (spec §3.1 dynamic registration), the
    /// pure core of `register_worker`: an unknown name is added, a matching
    /// worker is updated in place (so its slot count/version can move — the
    /// live announcement wins), and a name already held by a docker-endpoint
    /// node is refused.
    #[test]
    fn plan_register_precedence() {
        let roster = [("air", true), ("local", false)];
        assert_eq!(plan_register(&roster, "air"), RegisterAction::Update(0));
        assert_eq!(plan_register(&roster, "nuc"), RegisterAction::Add);
        assert_eq!(
            plan_register(&roster, "local"),
            RegisterAction::RejectDockerName
        );
        assert_eq!(plan_register(&[], "air"), RegisterAction::Add);
    }

    /// The outage case: a reachable 0-slot docker placeholder plus a responding
    /// 4-slot worker starts fine. The 0-slot node must not veto the fleet.
    #[test]
    fn zero_slot_docker_does_not_veto_worker_capacity() {
        assert_eq!(
            evaluate_startup(&[node(true, 0), worker(true, 4)]).unwrap(),
            StartupCapacity::Live
        );
    }

    /// A reachable 0-slot docker node with the only worker unreachable ⇒ no live
    /// capacity anywhere ⇒ refuse to start.
    ///
    /// **Re-asserted unchanged after the §5a narrowing, deliberately.** This case
    /// and its tier-2 twin `no_reachable_capacity_fails_startup` are what pin the
    /// narrowing to *capacity* rather than to *transport*: had the rule been keyed
    /// on the mere presence of a worker node, this would have inverted. If it ever
    /// turns green, the rule was widened too far.
    #[test]
    fn zero_slot_docker_plus_dead_worker_fails() {
        let err = evaluate_startup(&[node(true, 0), worker(false, 4)]).unwrap_err();
        assert!(err.to_string().contains("reachable"), "{err}");
    }

    /// An unreachable docker node is out-of-service, not fatal, when a responding
    /// worker carries the fleet's capacity.
    #[test]
    fn unreachable_docker_starts_when_worker_responds() {
        assert_eq!(
            evaluate_startup(&[node(false, 2), worker(true, 4)]).unwrap(),
            StartupCapacity::Live
        );
    }

    /// A single reachable node with slots is the all-docker single-node path —
    /// unchanged: it starts.
    #[test]
    fn single_reachable_node_with_slots_starts() {
        assert_eq!(
            evaluate_startup(&[node(true, 4)]).unwrap(),
            StartupCapacity::Live
        );
    }

    /// All-docker and all 0-slot ⇒ static config that only a restart can change
    /// says nothing can ever be placed ⇒ refuse. This is the half of the old
    /// `all_zero_slot_reachable_fails` that survives the §5a narrowing.
    #[test]
    fn all_zero_slot_reachable_docker_fails() {
        let err = evaluate_startup(&[node(true, 0), node(true, 0)]).unwrap_err();
        assert!(err.to_string().contains("slots > 0"), "{err}");
    }

    /// The half that INVERTS under §5a: the same all-zero fleet with a reachable
    /// *worker* in it now starts (with a loud warning) instead of crash-looping.
    /// Without this, an operator who drains every node to 0 from the UI leaves a
    /// dispatcher that cannot restart — and the dispatcher is the only thing that
    /// could raise the number back.
    #[test]
    fn all_zero_slot_reachable_with_worker_starts() {
        assert_eq!(
            evaluate_startup(&[node(true, 0), worker(true, 0)]).unwrap(),
            StartupCapacity::ZeroWithReachableWorker,
            "a zero-capacity start must be reported as such so the caller can be loud"
        );
    }

    /// The drain-and-restart case §5a turns on, in its simplest form: a lone
    /// reachable worker reporting 0 slots boots. Placement is inert until
    /// capacity is observed or commanded, and launches queue via §3.5.
    #[test]
    fn reachable_worker_reporting_zero_slots_starts() {
        assert_eq!(
            evaluate_startup(&[worker(true, 0)]).unwrap(),
            StartupCapacity::ZeroWithReachableWorker
        );
    }

    /// Reachability is NOT narrowed: a fleet whose every node is down still
    /// fails fast, whatever the transport. This is the deploy-time catcher for
    /// bad credentials or a wrong `NATS_URL` — the failure class behind the
    /// incident — and §5a deliberately does not spend it.
    #[test]
    fn wholly_unreachable_worker_fleet_still_fails() {
        let err = evaluate_startup(&[worker(false, 2), worker(false, 4)]).unwrap_err();
        assert!(err.to_string().contains("reachable"), "{err}");
    }
}

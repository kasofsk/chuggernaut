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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use store::NatsStore;
use store::worker::{WorkerRpc, WorkerRpcError};
use types::worker::{
    FileSource, WireFile, WireStatus, WorkerError, WorkerLaunchRequest, b64_decode, b64_encode,
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
        slots: u32,
        /// Cleared on ping failure, restored on success; placement skips
        /// out-of-service nodes but always re-probes them.
        in_service: AtomicBool,
        version_warned: AtomicBool,
        /// Last version reported by a successful ping (spec §3.1), surfaced in
        /// the platform config snapshot so the UI can show fleet versions and
        /// spot drift. `None` until the node first answers.
        last_version: Mutex<Option<String>>,
    },
}

struct FleetNode {
    name: String,
    handle: NodeHandle,
}

pub struct FleetBackend {
    nodes: Vec<FleetNode>,
    /// Platform placement policy (spec §3.1), applied across the whole fleet.
    /// Set from `PLACEMENT_POLICY`; defaults to [`PlacementPolicy::Busyness`].
    policy: PlacementPolicy,
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
                NodeHandle::Worker {
                    rpc: Box::new(WorkerRpc::new(store.clone(), c.name.clone())),
                    slots: c.slots,
                    in_service: AtomicBool::new(true),
                    version_warned: AtomicBool::new(false),
                    last_version: Mutex::new(None),
                }
            } else {
                NodeHandle::Docker {
                    backend: Box::new(DockerBackend::new(vec![c.clone()])?),
                }
            };
            nodes.push(FleetNode {
                name: c.name,
                handle,
            });
        }
        if nodes.is_empty() {
            return Err(BackendError::Unavailable("empty node list".into()));
        }
        Ok(Self { nodes, policy })
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
        let mut caps = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
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
                        slots: *slots,
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
            version_warned,
            last_version,
        } = &node.handle
        else {
            return None;
        };
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
                    free: *slots as i64 - ping.running as i64,
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
    async fn place(&self, pin: Option<&str>) -> Result<&FleetNode, BackendError> {
        let mut candidates = Vec::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            let load = self.node_load(node).await?; // None ⇒ out of service
            candidates.push(PlacementCandidate {
                index: i,
                name: node.name.as_str(),
                load,
            });
        }
        let index = choose_placement(self.policy, &candidates, pin)?;
        Ok(&self.nodes[index])
    }

    /// Live load on a fleet node: `Ok(Some)` when live, `Ok(None)` for an
    /// out-of-service worker (skipped by placement), `Err` for an unreachable
    /// docker-endpoint node (strict, spec §3.1).
    async fn node_load(&self, node: &FleetNode) -> Result<Option<NodeLoad>, BackendError> {
        match &node.handle {
            NodeHandle::Docker { backend } => Ok(backend
                .load_by_node()
                .await?
                .into_iter()
                .map(|(_, running, free)| NodeLoad { running, free })
                .next()),
            NodeHandle::Worker { .. } => Ok(self.probe_worker(node).await),
        }
    }

    /// Per-node health for the platform snapshot (spec §3.1): `(name, in_service)`
    /// as of the last ping/placement probe.
    pub fn availability(&self) -> Vec<(String, bool)> {
        self.nodes
            .iter()
            .map(|n| {
                let up = match &n.handle {
                    NodeHandle::Docker { backend } => backend
                        .availability()
                        .first()
                        .map(|(_, a)| *a)
                        .unwrap_or(true),
                    NodeHandle::Worker { in_service, .. } => in_service.load(Ordering::Relaxed),
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
        self.nodes
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

    fn route(&self, id: &ContainerId) -> Result<&FleetNode, BackendError> {
        let (name, _) = id
            .split_once('/')
            .ok_or_else(|| BackendError::NotFound(id.clone()))?;
        self.nodes
            .iter()
            .find(|n| n.name == name)
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
        let node = self.place(config.node.as_deref()).await?;
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
        for node in &self.nodes {
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
        for node in &self.nodes {
            match &node.handle {
                NodeHandle::Docker { backend } => out.extend(backend.list_managed_running().await?),
                NodeHandle::Worker { rpc, .. } => match rpc.list_running().await {
                    Ok(ok) => out.extend(ok.containers.into_iter().map(|c| RunningContainer {
                        id: c.id,
                        project: c.project,
                        job: c.job,
                        task: c.task,
                    })),
                    // An unreachable worker must not fail the whole sweep — its
                    // orphans get reaped on a later pass.
                    Err(e) => {
                        tracing::warn!(node = %node.name, "list_running skipped: {e}");
                    }
                },
            }
        }
        Ok(out)
    }

    fn fleet_status(&self) -> Vec<NodeStatus> {
        let versions = self.node_versions();
        self.availability()
            .into_iter()
            .map(|(name, available)| {
                let version = versions
                    .iter()
                    .find(|(n, _)| n == &name)
                    .and_then(|(_, v)| v.clone());
                NodeStatus {
                    name,
                    available,
                    version,
                }
            })
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
    use super::{NodeCapacity, evaluate_startup};

    fn node(reachable: bool, slots: u32) -> NodeCapacity {
        NodeCapacity { slots, reachable }
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

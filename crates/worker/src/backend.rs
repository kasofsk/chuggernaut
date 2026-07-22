//! Dispatcher-side fleet backend: a [`ContainerBackend`] over a mixed fleet of
//! docker-endpoint nodes (driven directly, exactly as before) and worker nodes
//! (proxied over NATS to the node's `chuggernaut worker` daemon).
//!
//! Placement follows the §3.1 rule across all in-service nodes: most free
//! slots, ties broken by name. Worker free slots come from the ping reply
//! (the worker counts its own managed containers); a worker that fails its
//! ping is out-of-service — skipped by placement, re-probed on the next
//! placement attempt, and NEVER fatal at startup. Docker-endpoint nodes keep
//! the strict §3.6 startup rule.

use async_trait::async_trait;
use container::docker::{DockerBackend, DockerNodeConfig};
use container::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    },
}

struct FleetNode {
    name: String,
    handle: NodeHandle,
}

pub struct FleetBackend {
    nodes: Vec<FleetNode>,
}

impl FleetBackend {
    /// Partition `DOCKER_NODES` entries: `worker` endpoints become NATS-proxied
    /// nodes; everything else gets its own single-node [`DockerBackend`].
    pub fn new(configs: Vec<DockerNodeConfig>, store: NatsStore) -> Result<Self, BackendError> {
        let mut nodes = Vec::new();
        for c in configs {
            let handle = if c.endpoint == WORKER_ENDPOINT {
                NodeHandle::Worker {
                    rpc: Box::new(WorkerRpc::new(store.clone(), c.name.clone())),
                    slots: c.slots,
                    in_service: AtomicBool::new(true),
                    version_warned: AtomicBool::new(false),
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
        Ok(Self { nodes })
    }

    /// §3.6 startup, softened for worker nodes: docker-endpoint nodes must
    /// answer (hard fail, unchanged); a worker that doesn't answer is logged
    /// and marked out-of-service — its daemon dialing in later brings it back
    /// without a dispatcher restart.
    pub async fn startup_check(&self) -> Result<(), BackendError> {
        for node in &self.nodes {
            match &node.handle {
                NodeHandle::Docker { backend } => backend.ping_all().await?,
                NodeHandle::Worker { .. } => {
                    if self.probe_worker(node).await.is_none() {
                        tracing::warn!(
                            node = %node.name,
                            "worker node unreachable at startup — out of service until its daemon connects"
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Ping a worker node; updates in_service and returns free slots when live.
    async fn probe_worker(&self, node: &FleetNode) -> Option<i64> {
        let NodeHandle::Worker {
            rpc,
            slots,
            in_service,
            version_warned,
        } = &node.handle
        else {
            return None;
        };
        match rpc.ping().await {
            Ok(ping) => {
                if !in_service.swap(true, Ordering::Relaxed) {
                    tracing::info!(node = %node.name, version = %ping.version, "worker node back in service");
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
                Some(*slots as i64 - ping.running as i64)
            }
            Err(e) => {
                if in_service.swap(false, Ordering::Relaxed) {
                    tracing::warn!(node = %node.name, "worker node out of service: {e}");
                }
                None
            }
        }
    }

    /// §3.1 placement across the fleet. Unpinned: most free slots, ties by
    /// name, out-of-service workers skipped. Pinned (`pin`): that node or a
    /// launch error — never a fallback (full/out-of-service → "no free slots on
    /// node {name}", unknown → names the known nodes).
    async fn place(&self, pin: Option<&str>) -> Result<&FleetNode, BackendError> {
        if let Some(name) = pin {
            let node = self.nodes.iter().find(|n| n.name == name).ok_or_else(|| {
                let known: Vec<&str> = self.nodes.iter().map(|n| n.name.as_str()).collect();
                BackendError::Launch(format!(
                    "placement pinned to unknown node {name:?}; known nodes: {}",
                    known.join(", ")
                ))
            })?;
            return match self.free_slots(node).await? {
                Some(free) if free > 0 => Ok(node),
                _ => Err(BackendError::Launch(format!(
                    "no free slots on node {name}"
                ))),
            };
        }
        let mut best: Option<(&FleetNode, i64)> = None;
        for node in &self.nodes {
            let Some(free) = self.free_slots(node).await? else {
                continue; // out-of-service worker — skipped, re-probed next time
            };
            let better = match best {
                None => true,
                Some((b, bf)) => free > bf || (free == bf && node.name < b.name),
            };
            if better {
                best = Some((node, free));
            }
        }
        match best {
            Some((node, free)) if free > 0 => Ok(node),
            _ => Err(BackendError::Launch("no free slots on any node".into())),
        }
    }

    /// Free slots on a fleet node: `Ok(Some)` when live, `Ok(None)` for an
    /// out-of-service worker (skipped by placement), `Err` for an unreachable
    /// docker-endpoint node (strict, spec §3.1).
    async fn free_slots(&self, node: &FleetNode) -> Result<Option<i64>, BackendError> {
        match &node.handle {
            NodeHandle::Docker { backend } => Ok(backend
                .free_slots_by_node()
                .await?
                .into_iter()
                .map(|(_, f)| f)
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
}

/// Convenience for `run.rs`: does this fleet contain any worker nodes?
pub fn has_worker_nodes(configs: &[DockerNodeConfig]) -> bool {
    configs.iter().any(|c| c.endpoint == WORKER_ENDPOINT)
}

// Arc so run.rs can pass it around like the DockerBackend today.
pub type SharedFleet = Arc<FleetBackend>;

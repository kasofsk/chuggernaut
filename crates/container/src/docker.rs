//! Docker fleet backend — the v1 production default (spec §3.1).
//!
//! One or more Docker daemons: local socket single-node, TCP endpoints
//! multi-node (mTLS wiring TODO). Slot-capped least-loaded placement;
//! `ContainerId` encodes the owning node as `{node}/{docker_id}`. Files are
//! injected via put-archive after create, before start — no host bind-mounts,
//! so remote nodes need nothing on disk.

use crate::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
    InjectedFile, LogTail, NodeLoad, NodeStatus, PlacementCandidate, PlacementPolicy,
    RunningContainer, choose_placement,
};
use async_trait::async_trait;
use bollard::Docker;
use bollard::models::{ContainerCreateBody, DeviceMapping, HostConfig, Mount, MountTypeEnum};
use bollard::query_parameters::{
    DownloadFromContainerOptionsBuilder, ListContainersOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Label stamped on every container we launch; placement counts by it and the
/// §3.6 startup sweeps reap by it. It means exactly *"a job container the
/// dispatcher owns and may kill or remove"* — nothing weaker.
///
/// **This key must never appear on an image.** A container inherits its image's
/// labels, so every container started from such an image would carry the marker
/// and be treated as a dispatcher-owned job container: the orphan sweep kills
/// it, the exited sweep removes it, and while it runs it occupies a phantom
/// fleet slot. #266 put it on the three built images for prune protection and
/// the long-lived `chug-worker` daemon inherited it — a dispatcher restart then
/// reaped the whole worker fleet (#268). Images use `chug.managed` instead, in
/// the `chug.*` image namespace alongside `chug.git.sha`;
/// `deploy/managed-label.test.sh` fails if the two keys ever converge.
const MANAGED_LABEL: &str = "chuggernaut.managed";

/// Identity labels stamped alongside [`MANAGED_LABEL`] so the §3.6 fleet sweep
/// can match a running container back to its owning task without inspecting the
/// container. Sourced from the launch env the dispatcher already sets.
const PROJECT_LABEL: &str = "chuggernaut.project";
const JOB_LABEL: &str = "chuggernaut.job";
const TASK_LABEL: &str = "chuggernaut.task";

/// Container-side path of the node-local build cache
/// ([`DockerBackend::with_cache_dir`]), exported so the worker daemon points
/// `SCCACHE_DIR` at the same path it mounts here — no drift between the mount
/// and the env. Its contents carry no job state: a build accelerator only, safe
/// to be cold (spec §3.1).
pub const CACHE_MOUNT_PATH: &str = "/cache/sccache";

/// Container-side path of the KVM device — always `/dev/kvm`, whatever the node
/// calls it host-side, because that is where the emulator looks (design #367
/// §2.3).
pub const KVM_DEVICE_PATH: &str = "/dev/kvm";

/// The node's nix store, mounted read-only at its own path for a KVM launch
/// (design #367 §3.3). Its own path is load-bearing: the toolchain's wrappers
/// name their interpreter and libraries by absolute store path, so a store
/// mounted anywhere else does not start.
pub const STORE_MOUNT_PATH: &str = "/nix/store";

/// Container-side path of the node's Android SDK, bound from the operator's
/// stable host path so no nix store hash ever appears in chug configuration
/// (design #367 §3.5). The engine resolves that symlink host-side at each
/// create, so a launch always gets the node's current SDK.
pub const ANDROID_SDK_MOUNT_PATH: &str = "/opt/android-sdk";

/// Container-side path of the node's Flutter SDK, a second independent leaf
/// bound exactly as [`ANDROID_SDK_MOUNT_PATH`] is and just as optional. The two
/// tools are complementary — Flutter ships Dart and the engine artifacts, the
/// Android SDK ships `adb` and the emulator.
pub const FLUTTER_MOUNT_PATH: &str = "/opt/flutter";

/// Container-side path of the node's JDK, a third independent leaf bound exactly
/// as the other two are and just as optional. It is what `JAVA_HOME` names:
/// gradle is not a nix wrapper and cannot resolve a JDK out of the store on its
/// own (design #367 correction 14).
pub const JDK_MOUNT_PATH: &str = "/opt/jdk";

/// Writable `HOME` for a KVM launch. The emulator writes
/// `$HOME/.android/emu-update-last-check.ini` even with `ANDROID_USER_HOME`
/// set, so `HOME` must land in the container's own writable layer, never in a
/// read-only mount (design #367 A1).
pub const KVM_HOME_PATH: &str = "/root";

/// One node's KVM grant (design #367 §2.3, §3.4): the device it passes through,
/// the stable SDK path it mounts beside it, and the projects allowed both.
///
/// [`admits`](Self::admits) is the single decision site, so the device and the
/// read-only mounts can only ever travel together.
#[derive(Debug, Clone)]
pub struct KvmGrant {
    /// Host device node, `/dev/kvm` unless the node names another.
    pub device: PathBuf,
    /// Host path of the node's Android SDK — the operator's activation-
    /// maintained stable path, never a store path.
    pub android_sdk_dir: PathBuf,
    /// Host path of the node's Flutter SDK, held to the same stable-path rule;
    /// `None` (the node does not provision one) adds no mount and no
    /// `FLUTTER_ROOT`, leaving an Android-only node exactly as it was.
    pub flutter_dir: Option<PathBuf>,
    /// Host path of the node's JDK, held to the same stable-path rule; `None`
    /// (the node does not provision one) adds no mount and no `JAVA_HOME`,
    /// leaving a node without it exactly as it was.
    pub jdk_dir: Option<PathBuf>,
    /// `owner/project` allow-list; empty grants nobody (design #367 §2.3).
    pub projects: Vec<String>,
}

impl KvmGrant {
    /// Whether a launch carrying this env is admitted, matched on `JOB_PROJECT`
    /// — the only project identity a node can observe (design #367 correction
    /// 5). An empty allow-list admits nobody, so enabling KVM on a node is one
    /// act and granting it to a project is another.
    pub fn admits(&self, env: &HashMap<String, String>) -> bool {
        env.get("JOB_PROJECT")
            .is_some_and(|project| self.projects.iter().any(|allowed| allowed == project))
    }
}

#[derive(Debug, Clone)]
pub struct DockerNodeConfig {
    pub name: String,
    /// `unix:///var/run/docker.sock` or `tcp://host:2375`. TLS: TODO (§3.1).
    pub endpoint: String,
    /// Max concurrent chuggernaut containers on this node.
    pub slots: u32,
}

/// One node's boot-time probe, reported by [`DockerBackend::probe_all`]: its
/// configured slot count and whether it answered its ping. A fleet owner (the
/// dispatcher's fleet backend) uses these to apply the §3.6 "no live capacity"
/// rule ONCE across every transport instead of per-sub-backend.
#[derive(Debug, Clone)]
pub struct NodeProbe {
    pub name: String,
    pub slots: u32,
    /// `None` when the node answered its ping; `Some(detail)` when unreachable
    /// (already logged and marked out of service in [`availability`]).
    ///
    /// [`availability`]: DockerBackend::availability
    pub error: Option<String>,
}

struct Node {
    name: String,
    slots: u32,
    docker: Docker,
    /// Set on a successful ping/placement probe, cleared on failure. Placement
    /// skips out-of-service nodes but always re-probes them, so a node coming
    /// back needs no dispatcher restart (spec §3.1/§3.6).
    in_service: AtomicBool,
}

pub struct DockerBackend {
    nodes: Vec<Node>,
    /// Host path of the node-local build cache, bind-mounted into every
    /// container at [`CACHE_MOUNT_PATH`]; `None` (the dispatcher's
    /// construction) adds no mount at all, so the fleet stays bind-mount-free
    /// (spec §3.1). Set worker-side via
    /// [`with_cache_dir`](DockerBackend::with_cache_dir); it is a node
    /// property, never carried on the wire or the launch config.
    cache_dir: Option<PathBuf>,
    /// The node's KVM grant (design #367 A1): the device and read-only
    /// toolchain mounts an allow-listed launch gets, `None` (the dispatcher's
    /// construction) adding neither. Set worker-side via
    /// [`with_kvm`](DockerBackend::with_kvm); like [`Self::cache_dir`] it is a
    /// node property, never carried on the wire or the launch config.
    kvm: Option<KvmGrant>,
    /// The node's nix store, mounted read-only into a launch that declares a
    /// `runtime.env` (design #373 P2); `None` (the dispatcher's construction)
    /// mounts nothing. Set worker-side via
    /// [`with_nix_store`](DockerBackend::with_nix_store), like every other node
    /// property here.
    nix_store: Option<PathBuf>,
    /// Platform placement policy (spec §3.1). Defaults to
    /// [`PlacementPolicy::Busyness`]; the dispatcher sets it from
    /// `PLACEMENT_POLICY` via [`with_placement_policy`](DockerBackend::with_placement_policy).
    /// Irrelevant on the single-node dev/worker path (one candidate).
    policy: PlacementPolicy,
}

impl DockerBackend {
    pub fn new(configs: Vec<DockerNodeConfig>) -> Result<Self, BackendError> {
        let mut nodes = Vec::new();
        for c in configs {
            let docker = if c.endpoint.starts_with("unix://") {
                Docker::connect_with_unix(&c.endpoint, 120, bollard::API_DEFAULT_VERSION)
            } else if c.endpoint.starts_with("tcp://") || c.endpoint.starts_with("http://") {
                Docker::connect_with_http(&c.endpoint, 120, bollard::API_DEFAULT_VERSION)
            } else {
                return Err(BackendError::Unavailable(format!(
                    "unsupported endpoint {:?} (expected unix:// or tcp://)",
                    c.endpoint
                )));
            }
            .map_err(|e| BackendError::Unavailable(e.to_string()))?;
            nodes.push(Node {
                name: c.name,
                slots: c.slots,
                docker,
                in_service: AtomicBool::new(true),
            });
        }
        if nodes.is_empty() {
            return Err(BackendError::Unavailable("empty node list".into()));
        }
        Ok(Self {
            nodes,
            cache_dir: None,
            kvm: None,
            nix_store: None,
            policy: PlacementPolicy::default(),
        })
    }

    /// Single local-socket node — the dev and single-node production form.
    pub fn local(slots: u32) -> Result<Self, BackendError> {
        let docker = Docker::connect_with_unix_defaults()
            .map_err(|e| BackendError::Unavailable(e.to_string()))?;
        Ok(Self {
            nodes: vec![Node {
                name: "local".into(),
                slots,
                docker,
                in_service: AtomicBool::new(true),
            }],
            cache_dir: None,
            kvm: None,
            nix_store: None,
            policy: PlacementPolicy::default(),
        })
    }

    /// Set the platform placement policy (spec §3.1). The dispatcher wires this
    /// from `PLACEMENT_POLICY` for a directly-driven multi-node Docker fleet.
    pub fn with_placement_policy(mut self, policy: PlacementPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Enable node-local build caching: bind-mount `host_dir` into every
    /// launched container at [`CACHE_MOUNT_PATH`], writably, and refuse the
    /// launch if it does not exist. Worker-daemon-only — the dispatcher never
    /// calls this, so its fleet stays bind-mount-free (spec §3.1); `host_dir` is
    /// provisioned on the node, and concurrent containers share it safely.
    pub fn with_cache_dir(mut self, host_dir: PathBuf) -> Self {
        self.cache_dir = Some(host_dir);
        self
    }

    /// Enable KVM passthrough for the projects `grant` allow-lists (design #367
    /// A1): those launches get the device and the read-only toolchain mounts,
    /// every other launch on the node is untouched. Worker-daemon-only — the
    /// dispatcher never calls this, so its fleet stays device- and
    /// mount-free.
    pub fn with_kvm(mut self, grant: KvmGrant) -> Self {
        self.kvm = Some(grant);
        self
    }

    /// Mount the node's nix store read-only into every launch that declares a
    /// `runtime.env` (design #373 P2), at its own path — the realised closure's
    /// wrappers name their interpreter and libraries by absolute store path.
    /// Worker-daemon-only, exactly as [`with_kvm`](Self::with_kvm) is.
    pub fn with_nix_store(mut self, store_dir: PathBuf) -> Self {
        self.nix_store = Some(store_dir);
        self
    }

    /// §3.6 startup, degraded per §3.1: ping every node and mark each
    /// in/out-of-service, but only refuse to start when *no* node with slots is
    /// reachable. An unreachable node is logged and excluded from placement; it
    /// is re-probed on each launch, so it rejoins without a restart. A
    /// single-node backend still fails fast — its one node is the whole fleet.
    pub async fn ping_all(&self) -> Result<(), BackendError> {
        let probes = self.probe_all().await;
        if probes.iter().any(|p| p.error.is_none() && p.slots > 0) {
            Ok(())
        } else {
            let last_err = probes.iter().rev().find_map(|p| p.error.clone());
            Err(BackendError::Unavailable(
                last_err.unwrap_or_else(|| "no node has slots > 0".into()),
            ))
        }
    }

    /// Ping every node, marking each in/out-of-service and logging failures,
    /// but apply NO capacity hard-fail — the caller owns that decision.
    /// [`ping_all`](Self::ping_all) layers the single-backend §3.6 rule on top;
    /// the dispatcher's fleet backend aggregates these across transports so a
    /// placement-inert 0-slot node can never veto a fleet that has capacity
    /// elsewhere (the regression that crash-looped prod 2026-07-22).
    pub async fn probe_all(&self) -> Vec<NodeProbe> {
        let mut out = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let error = match node.docker.ping().await {
                Ok(_) => {
                    node.in_service.store(true, Ordering::Relaxed);
                    None
                }
                Err(e) => {
                    node.in_service.store(false, Ordering::Relaxed);
                    tracing::warn!(
                        node = %node.name,
                        "docker node unreachable at startup — out of service until it responds: {e}"
                    );
                    Some(format!("node {}: {e}", node.name))
                }
            };
            out.push(NodeProbe {
                name: node.name.clone(),
                slots: node.slots,
                error,
            });
        }
        out
    }

    /// Per-node health for the platform snapshot (spec §3.1): `(name, in_service)`
    /// as of the last ping/placement probe.
    pub fn availability(&self) -> Vec<(String, bool)> {
        self.nodes
            .iter()
            .map(|n| (n.name.clone(), n.in_service.load(Ordering::Relaxed)))
            .collect()
    }

    fn route<'a>(&'a self, id: &'a ContainerId) -> Result<(&'a Node, &'a str), BackendError> {
        let (name, cid) = id
            .split_once('/')
            .ok_or_else(|| BackendError::NotFound(id.clone()))?;
        let node = self
            .nodes
            .iter()
            .find(|n| n.name == name)
            .ok_or_else(|| BackendError::NotFound(id.clone()))?;
        Ok((node, cid))
    }

    /// `(name, free_slots)` per node — placement input, and the worker
    /// daemon's slot report (it runs a single-node instance of this backend).
    pub async fn free_slots_by_node(&self) -> Result<Vec<(String, i64)>, BackendError> {
        let mut out = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let free = node.slots as i64 - self.managed_running(node).await? as i64;
            out.push((node.name.clone(), free));
        }
        Ok(out)
    }

    /// `(name, running, free)` per node — placement input for the fleet
    /// backend, which needs the running count as well as free slots to apply
    /// the busyness policy (spec §3.1). `free = slots − running`.
    pub async fn load_by_node(&self) -> Result<Vec<(String, i64, i64)>, BackendError> {
        let mut out = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            let running = self.managed_running(node).await? as i64;
            out.push((node.name.clone(), running, node.slots as i64 - running));
        }
        Ok(out)
    }

    /// Managed containers on one node in a single docker `status`, through the
    /// `chuggernaut.managed` label filter every fleet query shares. One place
    /// to state that filter: a query that forgot the label would count (or
    /// reap) containers this platform does not own.
    async fn managed_list_by_status(
        &self,
        node: &Node,
        status: &str,
        include_stopped: bool,
    ) -> Result<Vec<bollard::models::ContainerSummary>, BackendError> {
        let opts = ListContainersOptionsBuilder::default()
            .all(include_stopped)
            .filters(&HashMap::from([
                ("label".to_string(), vec![format!("{MANAGED_LABEL}=true")]),
                ("status".to_string(), vec![status.to_string()]),
            ]))
            .build();
        node.docker
            .list_containers(Some(opts))
            .await
            .map_err(|e| BackendError::Other(e.to_string()))
    }

    async fn managed_running(&self, node: &Node) -> Result<u32, BackendError> {
        let list = self.managed_list_by_status(node, "running", false).await?;
        Ok(list.len() as u32)
    }

    /// Exited managed containers on one node, as `{node}/{docker_id}` ids —
    /// the same encoding as launch, so the sweep can match against task records.
    async fn managed_exited(&self, node: &Node) -> Result<Vec<ContainerId>, BackendError> {
        let list = self.managed_list_by_status(node, "exited", true).await?;
        Ok(list
            .into_iter()
            .filter_map(|c| c.id)
            .map(|id| format!("{}/{}", node.name, id))
            .collect())
    }

    /// Running managed containers on one node, tagged with their `(project,
    /// job, task)` identity from the launch labels — the §3.6 fleet-sweep set.
    async fn managed_running_list(
        &self,
        node: &Node,
    ) -> Result<Vec<RunningContainer>, BackendError> {
        let list = self.managed_list_by_status(node, "running", false).await?;
        Ok(list
            .into_iter()
            .filter_map(|c| {
                let id = c.id?;
                let labels = c.labels.unwrap_or_default();
                Some(RunningContainer {
                    id: format!("{}/{}", node.name, id),
                    project: labels.get(PROJECT_LABEL).cloned(),
                    job: labels.get(JOB_LABEL).and_then(|v| v.parse().ok()),
                    task: labels.get(TASK_LABEL).and_then(|v| v.parse().ok()),
                })
            })
            .collect())
    }

    /// §3.1 placement. Every node is (re-)probed here, which updates its
    /// in-service flag, so a node that has recovered since startup rejoins
    /// placement without a dispatcher restart. The decision itself is
    /// [`choose_placement`] — a pure function over the probed loads under the
    /// configured [`PlacementPolicy`], honoring the optional `pin`.
    async fn place(&self, pin: Option<&str>) -> Result<&Node, BackendError> {
        let mut candidates = Vec::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            let load = self.probe_load(node).await;
            candidates.push(PlacementCandidate {
                index: i,
                name: &node.name,
                load,
            });
        }
        let index = choose_placement(self.policy, &candidates, pin)?;
        Ok(&self.nodes[index])
    }

    /// Live load on a node (running + free slots), re-marking its in-service
    /// state as a side effect. `None` when the node is unreachable (out of
    /// service) — placement skips it and it is re-probed on the next launch.
    async fn probe_load(&self, node: &Node) -> Option<NodeLoad> {
        match self.managed_running(node).await {
            Ok(running) => {
                if !node.in_service.swap(true, Ordering::Relaxed) {
                    tracing::info!(node = %node.name, "docker node back in service");
                }
                Some(NodeLoad {
                    running: running as i64,
                    free: node.slots as i64 - running as i64,
                })
            }
            Err(e) => {
                if node.in_service.swap(false, Ordering::Relaxed) {
                    tracing::warn!(node = %node.name, "docker node out of service: {e}");
                }
                None
            }
        }
    }
}

#[async_trait]
impl ContainerBackend for DockerBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        let node = self.place(config.node.as_deref()).await?;
        let image = image_or_refusal(&node.name, &config)?;
        let body = ContainerCreateBody {
            image: Some(image),
            cmd: Some(config.cmd.clone()),
            env: Some(config.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            labels: Some(managed_labels(&config)),
            host_config: Some(build_host_config(
                &config,
                self.cache_dir.as_deref(),
                self.kvm.as_ref(),
                self.nix_store.as_deref(),
            )?),
            ..Default::default()
        };
        let created = node
            .docker
            .create_container(
                None::<bollard::query_parameters::CreateContainerOptions>,
                body,
            )
            .await
            .map_err(|e| BackendError::Launch(e.to_string()))?;

        if !config.files.is_empty() {
            let tar = build_tar(&config.files).map_err(BackendError::Launch)?;
            node.docker
                .upload_to_container(
                    &created.id,
                    Some(UploadToContainerOptionsBuilder::default().path("/").build()),
                    bollard::body_full(tar.into()),
                )
                .await
                .map_err(|e| BackendError::Launch(format!("file injection: {e}")))?;
        }

        node.docker
            .start_container(
                &created.id,
                None::<bollard::query_parameters::StartContainerOptions>,
            )
            .await
            .map_err(|e| BackendError::Launch(e.to_string()))?;
        Ok(format!("{}/{}", node.name, created.id))
    }

    async fn wait(&self, id: &ContainerId) -> Result<i32, BackendError> {
        let (node, cid) = self.route(id)?;
        let mut stream = node
            .docker
            .wait_container(cid, None::<bollard::query_parameters::WaitContainerOptions>);
        match stream.next().await {
            Some(Ok(resp)) => Ok(resp.status_code as i32),
            Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => {
                Ok(code as i32)
            }
            Some(Err(e)) => Err(map_err(id, e)),
            None => Err(BackendError::Other(format!(
                "wait stream ended early for {id}"
            ))),
        }
    }

    async fn kill(&self, id: &ContainerId) -> Result<(), BackendError> {
        let (node, cid) = self.route(id)?;
        match node
            .docker
            .kill_container(cid, None::<bollard::query_parameters::KillContainerOptions>)
            .await
        {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409, ..
            }) => Ok(()),
            Err(e) => Err(map_err(id, e)),
        }
    }

    async fn inspect(&self, id: &ContainerId) -> Result<Option<ContainerStatus>, BackendError> {
        let (node, cid) = self.route(id)?;
        match node
            .docker
            .inspect_container(
                cid,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await
        {
            Ok(resp) => {
                let state = resp.state.unwrap_or_default();
                if state.running.unwrap_or(false) {
                    Ok(Some(ContainerStatus::Running))
                } else {
                    Ok(Some(ContainerStatus::Exited {
                        exit_code: state.exit_code.unwrap_or(-1) as i32,
                    }))
                }
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(map_err(id, e)),
        }
    }

    async fn copy_file(
        &self,
        id: &ContainerId,
        path: &str,
    ) -> Result<Option<Vec<u8>>, BackendError> {
        let (node, cid) = self.route(id)?;
        let opts = DownloadFromContainerOptionsBuilder::default()
            .path(path)
            .build();
        let mut stream = node.docker.download_from_container(cid, Some(opts));
        let mut archive = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => archive.extend_from_slice(&bytes),
                Err(bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                }) => return Ok(None),
                Err(e) => return Err(map_err(id, e)),
            }
        }
        let wanted = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut ar = tar::Archive::new(archive.as_slice());
        for entry in ar
            .entries()
            .map_err(|e| BackendError::Other(e.to_string()))?
        {
            let mut entry = entry.map_err(|e| BackendError::Other(e.to_string()))?;
            let name = entry
                .path()
                .map_err(|e| BackendError::Other(e.to_string()))?
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name == wanted && entry.header().entry_type().is_file() {
                let mut contents = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut contents)
                    .map_err(|e| BackendError::Other(e.to_string()))?;
                return Ok(Some(contents));
            }
        }
        Ok(None)
    }

    async fn logs(&self, id: &ContainerId) -> Result<Vec<u8>, BackendError> {
        let (node, cid) = self.route(id)?;
        logs_collect(node, cid, id).await
    }

    async fn logs_tail(&self, id: &ContainerId, since: u64) -> Result<LogTail, BackendError> {
        let (node, cid) = self.route(id)?;
        let out = logs_collect(node, cid, id).await?;
        Ok(LogTail::slice(&out, since))
    }

    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        let (node, cid) = self.route(id)?;
        let opts = RemoveContainerOptionsBuilder::default()
            .force(false)
            .build();
        match node.docker.remove_container(cid, Some(opts)).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404 | 409,
                ..
            }) => Ok(()),
            Err(e) => Err(map_err(id, e)),
        }
    }

    async fn list_managed_exited(&self) -> Result<Vec<ContainerId>, BackendError> {
        let mut ids = Vec::new();
        for node in &self.nodes {
            ids.extend(self.managed_exited(node).await?);
        }
        Ok(ids)
    }

    async fn list_managed_running(&self) -> Result<Vec<RunningContainer>, BackendError> {
        let mut out = Vec::new();
        for node in &self.nodes {
            match self.managed_running_list(node).await {
                Ok(cs) => out.extend(cs),
                Err(e) => tracing::warn!(node = %node.name, "list_managed_running skipped: {e}"),
            }
        }
        Ok(out)
    }

    /// The cheap override of the trait's derived count: one label-filtered
    /// docker `status` query per node, counted without materializing each
    /// container's identity. A node that cannot be listed fails the whole count
    /// rather than shrinking it, so the ping never under-reports occupancy.
    async fn managed_running_total(&self) -> Result<u32, BackendError> {
        let mut total = 0;
        for node in &self.nodes {
            total += self.managed_running(node).await?;
        }
        Ok(total)
    }

    fn fleet_status(&self) -> Vec<NodeStatus> {
        self.availability()
            .into_iter()
            .map(|(name, available)| NodeStatus {
                name,
                available,
                version: None,
                refresh_outcome: None,
                slots: None,
                capacity: None,
            })
            .collect()
    }
}

/// Build the container's `HostConfig` from the launch limits plus the node
/// properties in force — factored out of Docker I/O so *which node properties
/// are present* is unit-tested without a daemon.
///
/// The dispatcher's backend passes none of them and so stays bind-mount-free at
/// `devices: None, binds: None, mounts: None` (spec §3.1); a worker's
/// `cache_dir` adds one writable mount at [`CACHE_MOUNT_PATH`], its `kvm` adds
/// the device and the read-only toolchain mounts or none of them per
/// [`KvmGrant::admits`], and its `nix_store` adds the store read-only for a
/// launch declaring `runtime.env` — nothing here sets `binds`.
fn build_host_config(
    config: &ContainerLaunchConfig,
    cache_dir: Option<&Path>,
    kvm: Option<&KvmGrant>,
    nix_store: Option<&Path>,
) -> Result<HostConfig, BackendError> {
    let granted = kvm.filter(|g| g.admits(&config.env));
    let env_store = nix_store.filter(|store| {
        config.runtime_env.is_some() && (granted.is_none() || *store != Path::new(STORE_MOUNT_PATH))
    });
    let mut mounts = Vec::new();
    if let Some(dir) = cache_dir {
        mounts.push(writable_bind(dir, CACHE_MOUNT_PATH));
    }
    if let Some(store) = env_store {
        mounts.push(read_only_bind(store, &store.display().to_string()));
    }
    if let Some(g) = granted {
        mounts.push(read_only_bind(
            Path::new(STORE_MOUNT_PATH),
            STORE_MOUNT_PATH,
        ));
        mounts.push(read_only_bind(&g.android_sdk_dir, ANDROID_SDK_MOUNT_PATH));
        if let Some(flutter_dir) = &g.flutter_dir {
            mounts.push(read_only_bind(flutter_dir, FLUTTER_MOUNT_PATH));
        }
        if let Some(jdk_dir) = &g.jdk_dir {
            mounts.push(read_only_bind(jdk_dir, JDK_MOUNT_PATH));
        }
    }
    let toolchain_mounts = mounts.iter().filter(|m| m.read_only == Some(true)).count();
    let toolchain_mounts_expected = granted.map_or(0, |g| {
        2 + usize::from(g.flutter_dir.is_some()) + usize::from(g.jdk_dir.is_some())
    }) + usize::from(env_store.is_some());
    let host_config = HostConfig {
        nano_cpus: config.cpu_limit.map(|c| (c * 1e9) as i64),
        memory: config
            .memory_limit
            .as_deref()
            .map(parse_memory)
            .transpose()
            .map_err(BackendError::Launch)?,
        devices: granted.map(|g| {
            vec![DeviceMapping {
                path_on_host: Some(g.device.display().to_string()),
                path_in_container: Some(KVM_DEVICE_PATH.to_string()),
                cgroup_permissions: Some("rwm".to_string()),
            }]
        }),
        mounts: (!mounts.is_empty()).then_some(mounts),
        ..Default::default()
    };
    debug_assert_eq!(
        toolchain_mounts, toolchain_mounts_expected,
        "an admitted launch carries the store and the SDK, plus each optional leaf exactly when \
         the node provisions it"
    );
    debug_assert_eq!(
        host_config.devices.is_some(),
        toolchain_mounts >= 2,
        "a launch carries the KVM device and the read-only toolchain mounts together or carries \
         neither"
    );
    Ok(host_config)
}

/// The node's build cache, mounted writable — sccache writes through it, so
/// unlike a toolchain mount it cannot be read-only. Derived from
/// [`read_only_bind`] so both share its refuse-a-missing-source property.
fn writable_bind(host_dir: &Path, container_path: &str) -> Mount {
    Mount {
        read_only: Some(false),
        ..read_only_bind(host_dir, container_path)
    }
}

/// One read-only bind, declared through `HostConfig.mounts` rather than a
/// `binds` string: the engine REFUSES a missing source instead of silently
/// creating an empty directory in its place (design #367 correction 12).
/// Read-only is structural here — it cannot be built writable, and
/// [`writable_bind`] is a separate act.
fn read_only_bind(host_dir: &Path, container_path: &str) -> Mount {
    debug_assert!(
        host_dir.is_absolute(),
        "a bind source is a host path: {}",
        host_dir.display()
    );
    Mount {
        typ: Some(MountTypeEnum::BIND),
        source: Some(host_dir.display().to_string()),
        target: Some(container_path.to_string()),
        read_only: Some(true),
        ..Default::default()
    }
}

/// The image a container task runs, or the refusal for a launch that carries
/// none (design #309 §1). An absent image *is* a host task, so one arriving at
/// a docker backend was misrouted: refused loudly and named, never defaulted.
fn image_or_refusal(node: &str, config: &ContainerLaunchConfig) -> Result<String, BackendError> {
    config.image.clone().ok_or_else(|| {
        BackendError::Launch(format!(
            "node {node} serves container mode and this launch carries no image, which is how a \
             host task is spelled (design #309 §1) — a placement bug, refused rather than \
             defaulted"
        ))
    })
}

/// Labels for a launch: the managed marker plus the `(project, job, task)`
/// identity, lifted from the env the dispatcher already stamps
/// (`JOB_PROJECT`/`JOB_ID`/`CHUG_TASK_ID`). The identity labels let the §3.6
/// fleet sweep resolve a running container's owning task from a single
/// `list_containers` call — no per-container inspect.
fn managed_labels(config: &ContainerLaunchConfig) -> HashMap<String, String> {
    let mut labels = HashMap::from([(MANAGED_LABEL.to_string(), "true".to_string())]);
    for (env_key, label) in [
        ("JOB_PROJECT", PROJECT_LABEL),
        ("JOB_ID", JOB_LABEL),
        ("CHUG_TASK_ID", TASK_LABEL),
    ] {
        if let Some(v) = config.env.get(env_key) {
            labels.insert(label.to_string(), v.clone());
        }
    }
    labels
}

/// Everything a container has written so far, stdout and stderr interleaved.
///
/// `follow: false` is load-bearing on both callers: after exit there is nothing
/// more to come and following would hang, and on a *running* container bollard
/// returns what has been captured so far and ends the stream — so this never
/// blocks. Both streams because a failed build's message is as often on stderr
/// as stdout; cross-stream ordering is Docker's, by timestamp, and is not exact
/// for same-millisecond writes.
async fn logs_collect(node: &Node, cid: &str, id: &ContainerId) -> Result<Vec<u8>, BackendError> {
    let opts = LogsOptionsBuilder::default()
        .follow(false)
        .stdout(true)
        .stderr(true)
        .build();
    let mut stream = node.docker.logs(cid, Some(opts));
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(log) => out.extend_from_slice(log.into_bytes().as_ref()),
            Err(e) => return Err(map_err(id, e)),
        }
    }
    Ok(out)
}

fn map_err(id: &ContainerId, e: bollard::errors::Error) -> BackendError {
    match e {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        } => BackendError::NotFound(id.clone()),
        other => BackendError::Other(other.to_string()),
    }
}

/// Build the put-archive payload: parent directories then files, paths rooted
/// at `/` (entries are extracted relative to the upload path `/`).
fn build_tar(files: &[InjectedFile]) -> Result<Vec<u8>, String> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut dirs_added = std::collections::HashSet::new();
    for f in files {
        let rel = f.container_path.trim_start_matches('/');
        let parents: Vec<_> = Path::new(rel)
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        for dir in parents.into_iter().rev() {
            let dir_str = format!("{}/", dir.to_string_lossy());
            if dirs_added.insert(dir_str.clone()) {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Directory);
                header.set_path(&dir_str).map_err(|e| e.to_string())?;
                header.set_mode(0o755);
                header.set_size(0);
                header.set_cksum();
                builder
                    .append(&header, std::io::empty())
                    .map_err(|e| e.to_string())?;
            }
        }
        let mut header = tar::Header::new_gnu();
        header.set_path(rel).map_err(|e| e.to_string())?;
        header.set_mode(f.mode);
        header.set_size(f.contents.len() as u64);
        header.set_cksum();
        builder
            .append(&header, f.contents.as_slice())
            .map_err(|e| e.to_string())?;
    }
    builder.into_inner().map_err(|e| e.to_string())
}

/// Parse "512Mi" / "4Gi" / plain bytes into bytes. The accepted grammar is
/// owned by `types` so field-rules validation rejects a bad limit offline
/// (`chuggernaut validate`) before it ever reaches this launch-time parse.
fn parse_memory(s: &str) -> Result<i64, String> {
    types::parse_memory(s).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn memory_parsing() {
        assert_eq!(parse_memory("4Gi").unwrap(), 4 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory("512Mi").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory("1048576").unwrap(), 1_048_576);
        assert!(parse_memory("4GB").is_err());
    }

    /// Pin the launch-time parse to the `types` field-rules grammar: every case
    /// must resolve identically (ok→same bytes, err→err) in both crates, so a
    /// limit that passes `chuggernaut validate` can never be rejected at launch
    /// (the dogfood `5g` bug) and vice-versa.
    #[test]
    fn parse_memory_agrees_with_types_grammar() {
        for case in [
            "5Gi", "512Mi", "4Ki", "1048576", "5g", "4GB", "", "  ", "-5", "0", "1.5Gi", "Gi",
            "5gi",
        ] {
            assert_eq!(
                parse_memory(case).ok(),
                types::parse_memory(case).ok(),
                "launch-time parse and types validation disagree on {case:?}"
            );
        }
    }

    fn cand<'a>(name: &'a str, free: Option<i64>, index: usize) -> PlacementCandidate<'a> {
        PlacementCandidate {
            index,
            name,
            load: free.map(|free| NodeLoad { running: 0, free }),
        }
    }

    /// Headroom placement is unchanged: most free slots, ties broken by name,
    /// out-of-service nodes skipped, and empty/full fleets error identically.
    #[test]
    fn choose_unpinned_is_most_free_then_name() {
        let hr = PlacementPolicy::Headroom;
        let nodes = [
            cand("local", Some(1), 0),
            cand("nuc", Some(4), 1),
            cand("aaa", Some(4), 2),
        ];
        assert_eq!(choose_placement(hr, &nodes, None).unwrap(), 2);

        let nodes = [cand("local", Some(2), 0), cand("nuc", None, 1)];
        assert_eq!(choose_placement(hr, &nodes, None).unwrap(), 0);

        let nodes = [cand("local", Some(0), 0), cand("nuc", None, 1)];
        let err = choose_placement(hr, &nodes, None).unwrap_err().to_string();
        assert!(err.contains("no free slots on any node"), "{err}");
    }

    /// A pin places on the named node even when another has more free slots,
    /// and never spills over: a full or unknown pin errors. Policy-independent.
    #[test]
    fn choose_pin_honored_and_never_falls_back() {
        let hr = PlacementPolicy::Headroom;
        let nodes = [cand("local", Some(1), 0), cand("nuc", Some(4), 1)];
        assert_eq!(choose_placement(hr, &nodes, Some("local")).unwrap(), 0);

        let nodes = [cand("local", Some(0), 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("local"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free slots on node local"), "{err}");

        let nodes = [cand("local", None, 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("local"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free slots on node local"), "{err}");

        let nodes = [cand("local", Some(1), 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("mini"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown node \"mini\"") && err.contains("local, nuc"),
            "{err}"
        );
    }

    fn launch_config() -> ContainerLaunchConfig {
        ContainerLaunchConfig {
            image: Some("img".into()),
            cmd: vec!["run".into()],
            env: HashMap::new(),
            files: vec![],
            cpu_limit: Some(2.0),
            memory_limit: Some("4Gi".into()),
            node: None,
            runtime_env: None,
        }
    }

    /// A container launch is unchanged by #309 §1 — it names its image and
    /// runs — while an image-less one, which is how a host task is spelled, is
    /// refused with the node named rather than launched against a default.
    #[test]
    fn an_image_less_launch_is_refused_and_named() {
        assert_eq!(image_or_refusal("nuc", &launch_config()).unwrap(), "img");

        let err = image_or_refusal(
            "nuc",
            &ContainerLaunchConfig {
                image: None,
                ..launch_config()
            },
        )
        .unwrap_err();
        assert!(matches!(err, BackendError::Launch(_)), "{err}");
        assert!(err.to_string().contains("node nuc"), "{err}");
    }

    /// The dispatcher's backend (no node properties) adds NO devices, binds or
    /// mounts — the fleet stays bind-mount-free (spec §3.1). Regression guard:
    /// the dispatcher path is untouched by node-local caching or by KVM
    /// passthrough (design #367 A1).
    #[test]
    fn host_config_without_node_properties_is_bare() {
        let hc = build_host_config(&launch_config(), None, None, None).unwrap();
        assert!(hc.binds.is_none(), "dispatcher path must add no binds");
        assert!(hc.devices.is_none(), "dispatcher path must add no devices");
        assert!(hc.mounts.is_none(), "dispatcher path must add no mounts");
        assert_eq!(hc.nano_cpus, Some(2_000_000_000));
        assert_eq!(hc.memory, Some(4 * 1024 * 1024 * 1024));
    }

    /// A worker backend with a cache dir mounts exactly that dir at the fixed
    /// container path and nothing else — as a typed *mount*, never a `binds`
    /// string, so a cache dir that does not exist is refused by the engine
    /// instead of silently created empty; and writable, since sccache writes
    /// through it.
    #[test]
    fn host_config_with_cache_adds_one_writable_mount() {
        let dir = PathBuf::from("/var/cache/chuggernaut/sccache");
        let hc = build_host_config(&launch_config(), Some(dir.as_path()), None, None).unwrap();
        assert!(hc.binds.is_none(), "the cache is a mount, not a bind");
        assert!(hc.devices.is_none(), "a cache dir grants no device");

        let mounts = hc.mounts.expect("a cache dir carries its mount");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].typ, Some(MountTypeEnum::BIND));
        assert_eq!(
            mounts[0].source.as_deref(),
            Some("/var/cache/chuggernaut/sccache")
        );
        assert_eq!(mounts[0].target.as_deref(), Some(CACHE_MOUNT_PATH));
        assert_eq!(
            mounts[0].read_only,
            Some(false),
            "a write-through cache cannot be read-only"
        );
    }

    /// A launch declaring `runtime.env` (design #373 P2) gets the node's store
    /// read-only at its own path and NOTHING else — no device, no SDK leaf: the
    /// realised closure is the whole toolchain, which is what retires the
    /// one-mount-per-tool shape.
    #[test]
    fn host_config_mounts_the_store_for_a_declared_runtime_env() {
        let store = PathBuf::from("/nix/store");
        let mut config = launch_config();
        config.runtime_env = Some("nix:.#chug-mobile".into());
        let hc = build_host_config(&config, None, None, Some(store.as_path())).unwrap();

        assert!(hc.devices.is_none(), "an environment grants no device");
        let mounts = hc.mounts.expect("a declared env carries the store");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].source.as_deref(), Some("/nix/store"));
        assert_eq!(
            mounts[0].target.as_deref(),
            Some("/nix/store"),
            "the store must land at its own path: the closure names it absolutely"
        );
        assert_eq!(mounts[0].read_only, Some(true));

        let hc = build_host_config(&launch_config(), None, None, Some(store.as_path())).unwrap();
        assert!(
            hc.mounts.is_none(),
            "a launch declaring no env is exactly what it is today"
        );

        let hc = build_host_config(&config, None, None, None).unwrap();
        assert!(
            hc.mounts.is_none(),
            "a node that provisions no store mounts none, whatever a launch declares"
        );
    }

    /// A KVM-admitted launch that ALSO declares an environment carries the store
    /// once — the two grants name the same read-only mount, and a second bind at
    /// the same target would fail the create — but the de-duplication is on the
    /// PATH, never on the grant: a node whose store is elsewhere still gets the
    /// store its closure actually lives in, or `CHUG_ENV_PATH` would name a
    /// directory the container does not have.
    #[test]
    fn host_config_mounts_the_store_once_for_a_kvm_launch_declaring_an_env() {
        let mut config = launch_config_for("acme/beacon");
        config.runtime_env = Some("nix:.#chug-mobile".into());
        let stores_in = |hc: &HostConfig| -> Vec<String> {
            hc.mounts
                .iter()
                .flatten()
                .filter_map(|m| m.target.clone())
                .filter(|t| t.contains("store"))
                .collect()
        };

        let hc = build_host_config(
            &config,
            None,
            Some(&kvm_grant()),
            Some(Path::new(STORE_MOUNT_PATH)),
        )
        .unwrap();
        assert_eq!(
            stores_in(&hc),
            vec![STORE_MOUNT_PATH.to_string()],
            "one store mount, not two"
        );

        let elsewhere = Path::new("/data/nix/store");
        let hc = build_host_config(&config, None, Some(&kvm_grant()), Some(elsewhere)).unwrap();
        assert_eq!(
            stores_in(&hc),
            vec!["/data/nix/store".to_string(), STORE_MOUNT_PATH.to_string()],
            "the realised closure's own store is mounted even beside the KVM one"
        );
    }

    fn kvm_grant() -> KvmGrant {
        KvmGrant {
            device: PathBuf::from(KVM_DEVICE_PATH),
            android_sdk_dir: PathBuf::from("/var/lib/chuggernaut/android-sdk"),
            flutter_dir: None,
            jdk_dir: None,
            projects: vec!["acme/beacon".to_string()],
        }
    }

    fn kvm_grant_with_flutter() -> KvmGrant {
        KvmGrant {
            flutter_dir: Some(PathBuf::from("/var/lib/chuggernaut/toolchain/flutter")),
            ..kvm_grant()
        }
    }

    fn kvm_grant_with_jdk() -> KvmGrant {
        KvmGrant {
            jdk_dir: Some(PathBuf::from("/var/lib/chuggernaut/toolchain/jdk")),
            ..kvm_grant_with_flutter()
        }
    }

    fn launch_config_for(project: &str) -> ContainerLaunchConfig {
        let mut config = launch_config();
        config.env.insert("JOB_PROJECT".into(), project.to_string());
        config
    }

    /// The mounts an admitted launch on this node carries, with the pairing
    /// every toolchain case shares — the device, and no legacy binds — checked
    /// in passing.
    fn admitted_mounts(grant: &KvmGrant, cache: Option<&Path>) -> Vec<Mount> {
        let hc =
            build_host_config(&launch_config_for("acme/beacon"), cache, Some(grant), None).unwrap();
        assert_eq!(hc.devices.as_ref().map(Vec::len), Some(1));
        assert!(hc.binds.is_none(), "a toolchain leaf adds no legacy binds");
        hc.mounts
            .expect("an allow-listed launch carries the mounts")
    }

    /// Where the read-only mounts land, in the order the backend adds them.
    fn read_only_targets(mounts: &[Mount]) -> Vec<&str> {
        mounts
            .iter()
            .filter(|m| m.read_only == Some(true))
            .filter_map(|m| m.target.as_deref())
            .collect()
    }

    /// The host path bound at `target`, or `None` when nothing is mounted there.
    fn source_at<'a>(mounts: &'a [Mount], target: &str) -> Option<&'a str> {
        mounts
            .iter()
            .find(|m| m.target.as_deref() == Some(target))?
            .source
            .as_deref()
    }

    /// An allow-listed launch on a KVM node gets `/dev/kvm` at `/dev/kvm` with
    /// `rwm`, plus BOTH toolchain mounts read-only — the store at its own path
    /// (the wrappers name their libraries by store path) and the node's stable
    /// SDK path at the fixed container path, with no store hash anywhere
    /// (design #367 §3.3/§3.5).
    #[test]
    fn host_config_with_kvm_adds_device_and_read_only_mounts() {
        let hc = build_host_config(
            &launch_config_for("acme/beacon"),
            None,
            Some(&kvm_grant()),
            None,
        )
        .unwrap();

        let devices = hc
            .devices
            .expect("an allow-listed launch carries the device");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].path_on_host.as_deref(), Some(KVM_DEVICE_PATH));
        assert_eq!(
            devices[0].path_in_container.as_deref(),
            Some(KVM_DEVICE_PATH)
        );
        assert_eq!(devices[0].cgroup_permissions.as_deref(), Some("rwm"));

        let mounts = hc
            .mounts
            .expect("an allow-listed launch carries the mounts");
        assert_eq!(mounts.len(), 2);
        for mount in &mounts {
            assert_eq!(mount.typ, Some(MountTypeEnum::BIND));
            assert_eq!(
                mount.read_only,
                Some(true),
                "a toolchain mount is read-only, always: {mount:?}"
            );
        }
        assert_eq!(mounts[0].source.as_deref(), Some(STORE_MOUNT_PATH));
        assert_eq!(mounts[0].target.as_deref(), Some(STORE_MOUNT_PATH));
        assert_eq!(
            mounts[1].source.as_deref(),
            Some("/var/lib/chuggernaut/android-sdk")
        );
        assert_eq!(mounts[1].target.as_deref(), Some(ANDROID_SDK_MOUNT_PATH));
        assert!(
            !format!("{mounts:?}").contains("/nix/store/"),
            "no store path may reach the mount spec: {mounts:?}"
        );
        assert!(hc.binds.is_none(), "KVM adds no legacy binds");
    }

    /// The two node properties are independent and compose: a KVM node that
    /// also caches carries all three mounts, with read-only holding for the
    /// toolchain pair and only for it. Each is equally legal alone —
    /// [`host_config_with_cache_adds_one_writable_mount`] is a cache with no
    /// grant, and the test above is a grant with no cache.
    #[test]
    fn host_config_with_cache_and_kvm_carries_both_independently() {
        let dir = PathBuf::from("/var/cache/chuggernaut/sccache");
        let hc = build_host_config(
            &launch_config_for("acme/beacon"),
            Some(dir.as_path()),
            Some(&kvm_grant()),
            None,
        )
        .unwrap();

        assert!(hc.binds.is_none(), "neither property sets binds");
        assert_eq!(hc.devices.map(|d| d.len()), Some(1));

        let mounts = hc.mounts.expect("both properties carry mounts");
        assert_eq!(mounts.len(), 3);
        let by_target = |target: &str| {
            mounts
                .iter()
                .find(|m| m.target.as_deref() == Some(target))
                .unwrap_or_else(|| panic!("no mount at {target}"))
        };
        assert_eq!(by_target(CACHE_MOUNT_PATH).read_only, Some(false));
        assert_eq!(by_target(STORE_MOUNT_PATH).read_only, Some(true));
        assert_eq!(by_target(ANDROID_SDK_MOUNT_PATH).read_only, Some(true));
    }

    /// A node that also provisions Flutter mounts it as a THIRD read-only leaf
    /// at its own container path, from its own stable host path — independent of
    /// the Android SDK, which is unmoved and still at
    /// [`ANDROID_SDK_MOUNT_PATH`], so a node that never sets it needs no
    /// migration.
    #[test]
    fn host_config_with_flutter_adds_a_third_read_only_mount() {
        let mounts = admitted_mounts(&kvm_grant_with_flutter(), None);
        assert_eq!(mounts.len(), 3);
        assert_eq!(
            read_only_targets(&mounts),
            vec![STORE_MOUNT_PATH, ANDROID_SDK_MOUNT_PATH, FLUTTER_MOUNT_PATH]
        );
        assert!(mounts.iter().all(|m| m.typ == Some(MountTypeEnum::BIND)));
        assert_eq!(
            source_at(&mounts, ANDROID_SDK_MOUNT_PATH),
            Some("/var/lib/chuggernaut/android-sdk")
        );
        assert_eq!(
            source_at(&mounts, FLUTTER_MOUNT_PATH),
            Some("/var/lib/chuggernaut/toolchain/flutter"),
            "the leaf itself is the bind source — the engine resolves the symlink host-side"
        );
        assert_eq!(source_at(&mounts, STORE_MOUNT_PATH), Some(STORE_MOUNT_PATH));
        assert!(
            !format!("{mounts:?}").contains("/nix/store/"),
            "no store path may reach the mount spec: {mounts:?}"
        );
    }

    /// A node that also provisions a JDK mounts it as a FOURTH read-only leaf at
    /// its own container path, from its own stable host path — the toolchain
    /// gradle needs, since gradle is not a nix wrapper and cannot resolve a JDK
    /// out of the store the way the SDK tools do (design #367 correction 14).
    #[test]
    fn host_config_with_jdk_adds_a_fourth_read_only_mount() {
        let mounts = admitted_mounts(&kvm_grant_with_jdk(), None);
        assert_eq!(mounts.len(), 4);
        assert_eq!(
            read_only_targets(&mounts),
            vec![
                STORE_MOUNT_PATH,
                ANDROID_SDK_MOUNT_PATH,
                FLUTTER_MOUNT_PATH,
                JDK_MOUNT_PATH
            ],
            "each tool keeps its own leaf: {mounts:?}"
        );
        for (target, source) in [
            (ANDROID_SDK_MOUNT_PATH, "/var/lib/chuggernaut/android-sdk"),
            (FLUTTER_MOUNT_PATH, "/var/lib/chuggernaut/toolchain/flutter"),
            (JDK_MOUNT_PATH, "/var/lib/chuggernaut/toolchain/jdk"),
        ] {
            assert_eq!(
                source_at(&mounts, target),
                Some(source),
                "the leaf itself is the bind source — the engine resolves the symlink host-side"
            );
        }
        assert!(
            !format!("{mounts:?}").contains("/nix/store/"),
            "no store path may reach the mount spec: {mounts:?}"
        );
    }

    /// An optional leaf UNSET leaves the node byte-identical to what it was —
    /// the device, the same mounts at the same paths in the same order, and
    /// nothing at the unprovisioned leaf's path. This is the property that lets
    /// `gumbo-nuc-0` take each of these changes with no migration.
    #[test]
    fn host_config_without_an_optional_leaf_is_unchanged() {
        let dir = PathBuf::from("/var/cache/chuggernaut/sccache");
        for (grant, expected) in [
            (kvm_grant(), vec![STORE_MOUNT_PATH, ANDROID_SDK_MOUNT_PATH]),
            (
                kvm_grant_with_flutter(),
                vec![STORE_MOUNT_PATH, ANDROID_SDK_MOUNT_PATH, FLUTTER_MOUNT_PATH],
            ),
        ] {
            for cache in [None, Some(dir.as_path())] {
                let mounts = admitted_mounts(&grant, cache);
                assert_eq!(read_only_targets(&mounts), expected);
                assert_eq!(source_at(&mounts, JDK_MOUNT_PATH), None, "{mounts:?}");
                assert_eq!(
                    source_at(&mounts, FLUTTER_MOUNT_PATH).is_some(),
                    grant.flutter_dir.is_some(),
                    "an unprovisioned node mounts no Flutter: {mounts:?}"
                );
            }
        }
    }

    /// Every legal combination of the two optional leaves, with and without a
    /// cache, exercises the re-pinned arithmetic: the read-only count is what
    /// the grant declares, the writable cache is never counted among it, and the
    /// device rides exactly when the count reaches the mandatory pair.
    #[test]
    fn host_config_toolchain_arithmetic_holds_for_every_legal_combination() {
        let dir = PathBuf::from("/var/cache/chuggernaut/sccache");
        let jdk_only = KvmGrant {
            flutter_dir: None,
            ..kvm_grant_with_jdk()
        };
        for (grant, read_only_expected) in [
            (kvm_grant(), 2),
            (kvm_grant_with_flutter(), 3),
            (jdk_only, 3),
            (kvm_grant_with_jdk(), 4),
        ] {
            for cache in [None, Some(dir.as_path())] {
                let mounts = admitted_mounts(&grant, cache);
                assert_eq!(read_only_targets(&mounts).len(), read_only_expected);
                assert_eq!(
                    mounts.len(),
                    read_only_expected + usize::from(cache.is_some()),
                    "the cache is an independent writable mount: {grant:?}"
                );
            }
        }
    }

    /// The negative space, and the all-or-nothing pairing: a launch for a
    /// project the node did not allow-list — or one with no project at all —
    /// carries NEITHER the device nor the mounts, on the same node that grants
    /// both to the allow-listed project. An empty allow-list grants nobody.
    #[test]
    fn host_config_without_allow_list_entry_carries_neither() {
        for grant in [kvm_grant(), kvm_grant_with_flutter(), kvm_grant_with_jdk()] {
            for config in [
                launch_config_for("acme/other"),
                launch_config(),
                launch_config_for(""),
            ] {
                let hc = build_host_config(&config, None, Some(&grant), None).unwrap();
                assert!(hc.devices.is_none(), "unlisted launch got the device");
                assert!(hc.mounts.is_none(), "unlisted launch got the mounts");
            }
        }

        let nobody = KvmGrant {
            projects: vec![],
            ..kvm_grant()
        };
        let hc = build_host_config(&launch_config_for("acme/beacon"), None, Some(&nobody), None)
            .unwrap();
        assert!(hc.devices.is_none(), "an empty allow-list grants nobody");
        assert!(hc.mounts.is_none(), "an empty allow-list grants nobody");
    }

    /// Every launch carries the managed marker plus the `(project, job, task)`
    /// identity lifted from the dispatcher's env, so the §3.6 fleet sweep can
    /// resolve a running container's owning task from labels alone. A launch
    /// with no identity env still carries the marker and nothing more (a
    /// pre-labels container the sweep treats as an unmatchable orphan).
    #[test]
    fn managed_labels_carry_task_identity() {
        let mut cfg = launch_config();
        cfg.env.insert("JOB_PROJECT".into(), "acme/api".into());
        cfg.env.insert("JOB_ID".into(), "51".into());
        cfg.env.insert("CHUG_TASK_ID".into(), "2".into());
        let labels = managed_labels(&cfg);
        assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
        assert_eq!(
            labels.get(PROJECT_LABEL).map(String::as_str),
            Some("acme/api")
        );
        assert_eq!(labels.get(JOB_LABEL).map(String::as_str), Some("51"));
        assert_eq!(labels.get(TASK_LABEL).map(String::as_str), Some("2"));

        let bare = managed_labels(&launch_config());
        assert_eq!(bare.get(MANAGED_LABEL).map(String::as_str), Some("true"));
        assert!(!bare.contains_key(PROJECT_LABEL));
        assert!(!bare.contains_key(JOB_LABEL));
        assert!(!bare.contains_key(TASK_LABEL));
    }

    #[test]
    fn tar_includes_parent_dirs() {
        let tar_bytes = build_tar(&[InjectedFile {
            container_path: "/chuggernaut/prompt.md".into(),
            contents: b"hello".to_vec(),
            mode: 0o644,
            artifact: None,
        }])
        .unwrap();
        let mut ar = tar::Archive::new(tar_bytes.as_slice());
        let paths: Vec<String> = ar
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(paths, vec!["chuggernaut/", "chuggernaut/prompt.md"]);
    }
}

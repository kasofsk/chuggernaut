//! Docker fleet backend — the v1 production default (spec §3.1).
//!
//! One or more Docker daemons: local socket single-node, TCP endpoints
//! multi-node (mTLS wiring TODO). Slot-capped least-loaded placement;
//! `ContainerId` encodes the owning node as `{node}/{docker_id}`. Files are
//! injected via put-archive after create, before start — no host bind-mounts,
//! so remote nodes need nothing on disk.

use crate::{
    BackendError, CONTAINER_ONLY_MODES, ContainerBackend, ContainerId, ContainerLaunchConfig,
    ContainerStatus, InjectedFile, LaunchRequirements, LogTail, ModeWarnings, NO_ENVS, NodeLoad,
    NodeStatus, PlacementCandidate, PlacementPolicy, RunningContainer, choose_placement,
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
use std::time::Duration;

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

/// Container-side path of the node's docker socket — the conventional one,
/// whatever the node calls it host-side, because that is where a docker client
/// looks when nothing names an endpoint (design #517 D3).
pub const DOCKER_SOCKET_MOUNT_PATH: &str = "/var/run/docker.sock";

/// The launch-env stamp naming the level a launch runs at, which
/// [`DockerGrant::admits`] scopes the socket on (design #543 D5). The
/// `CHANNEL_ROLE` stamp beside it says the same thing, but only this name is
/// under a prefix spec §4.1 seals — a job type's `vars:` may declare that one
/// and would re-obtain node root with it.
pub const PHASE_ENV: &str = "CHUG_PHASE";

/// The [`PHASE_ENV`] value a work or wrap-up launch carries, and the only level
/// a docker grant admits. Exported so the dispatcher composing the stamp pins
/// the spelling this crate matches, rather than two crates sharing a literal by
/// convention.
pub const PHASE_WORK: &str = "Work";

/// One `owner/project:job_type` allow-list entry, the `(project, job type)`
/// identity a node consents to (design #517 D3). Parsed once at declaration so
/// a malformed entry is a refusal rather than a grant that silently never
/// matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerGrantEntry {
    /// `owner/project`, matched against the launch's `JOB_PROJECT`.
    pub project: String,
    /// The job type's `name:`, matched against the launch's `JOB_TYPE`.
    pub job_type: String,
}

impl DockerGrantEntry {
    /// Parse one `owner/project:job_type` entry, or say why it is not one. The
    /// single decision site for the entry shape, so what an operator may write
    /// and what [`DockerGrant::admits`] matches cannot drift apart.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let entry = raw.trim();
        let (project, job_type) = entry.split_once(':').unwrap_or_default();
        let (owner, name) = project.split_once('/').unwrap_or_default();
        if owner.is_empty()
            || name.is_empty()
            || name.contains('/')
            || job_type.is_empty()
            || job_type.contains(':')
        {
            return Err(format!(
                "entry {entry:?} is not an owner/project:job_type pair"
            ));
        }
        Ok(Self {
            project: project.to_string(),
            job_type: job_type.to_string(),
        })
    }
}

/// One node's docker grant (design #517 D3): the socket it binds and the
/// `(project, job type)` launches allowed it — node root, accepted rather than
/// mitigated, which is why it is node config and never a job-type field.
///
/// [`admits`](Self::admits) is the single decision site, matched on the
/// dispatcher-composed stamps the `JOB_` and `CHUG_` prefixes seal (spec §4.1).
#[derive(Debug, Clone)]
pub struct DockerGrant {
    /// Host path of the socket, bound writable at
    /// [`DOCKER_SOCKET_MOUNT_PATH`] — a client cannot connect through a
    /// read-only bind.
    pub socket: PathBuf,
    /// `owner/project:job_type` allow-list; empty grants nobody, so binding a
    /// socket on a node and granting it to a workload are two separate acts.
    /// It names the launches of that pair the node consents to at **work
    /// level**, never that pair's evaluators (design #543 D5).
    pub allowed: Vec<DockerGrantEntry>,
}

impl DockerGrant {
    /// Whether a launch carrying this env is admitted: an allow-listed
    /// `(JOB_PROJECT, JOB_TYPE)` at work level, the three stamps a node can
    /// observe and project config cannot move (spec §4.1, design #543 D5). A
    /// launch missing any of them, or carrying any other [`PHASE_ENV`] value,
    /// is admitted by nothing.
    pub fn admits(&self, env: &HashMap<String, String>) -> bool {
        if env.get(PHASE_ENV).map(String::as_str) != Some(PHASE_WORK) {
            return false;
        }
        let (Some(project), Some(job_type)) = (env.get("JOB_PROJECT"), env.get("JOB_TYPE")) else {
            return false;
        };
        self.allowed
            .iter()
            .any(|entry| &entry.project == project && &entry.job_type == job_type)
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

/// One docker client for an endpoint — the single place the two supported
/// transports are spelled, so a probe and a backend cannot disagree about what
/// an endpoint string means. TLS: TODO (§3.1).
fn connect_endpoint(endpoint: &str) -> Result<Docker, BackendError> {
    if endpoint.starts_with("unix://") {
        Docker::connect_with_unix(endpoint, 120, bollard::API_DEFAULT_VERSION)
    } else if endpoint.starts_with("tcp://") || endpoint.starts_with("http://") {
        Docker::connect_with_http(endpoint, 120, bollard::API_DEFAULT_VERSION)
    } else {
        return Err(BackendError::Unavailable(format!(
            "unsupported endpoint {endpoint:?} (expected unix:// or tcp://)"
        )));
    }
    .map_err(|e| BackendError::Unavailable(e.to_string()))
}

/// Whether a docker endpoint answers, asked with the API's own `GET /_ping` so
/// the probe reads and never creates, starts or stops anything (design #517
/// D4). Bounded by `timeout`, so an endpoint that accepts a connection and then
/// never replies reports unreachable rather than hanging its caller.
pub async fn endpoint_answers(endpoint: &str, timeout: Duration) -> Result<(), BackendError> {
    let docker = connect_endpoint(endpoint)?;
    match tokio::time::timeout(timeout, docker.ping()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(BackendError::Unavailable(format!("{endpoint}: {e}"))),
        Err(_) => Err(BackendError::Unavailable(format!(
            "{endpoint}: no answer within {}s",
            timeout.as_secs()
        ))),
    }
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
    /// The node's docker grant (design #517 D3): the socket an allow-listed
    /// launch gets bound in, `None` (the dispatcher's construction) binding
    /// none. Set worker-side via
    /// [`with_docker_grant`](DockerBackend::with_docker_grant); like
    /// [`Self::kvm`] it is a node property, never carried on the wire or the
    /// launch config.
    docker_grant: Option<DockerGrant>,
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
    /// Cadence state for the fleet-wide unadvertised-mode warning (design #309
    /// §5a). Every node here is container-only, so it fires only for a host
    /// launch that reached a docker fleet.
    mode_warnings: ModeWarnings,
}

impl DockerBackend {
    pub fn new(configs: Vec<DockerNodeConfig>) -> Result<Self, BackendError> {
        let mut nodes = Vec::new();
        for c in configs {
            let docker = connect_endpoint(&c.endpoint)?;
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
            docker_grant: None,
            nix_store: None,
            policy: PlacementPolicy::default(),
            mode_warnings: ModeWarnings::default(),
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
            docker_grant: None,
            nix_store: None,
            policy: PlacementPolicy::default(),
            mode_warnings: ModeWarnings::default(),
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

    /// Bind the node's docker socket into the launches `grant` allow-lists
    /// (design #517 D3): those launches reach the node's own daemon, every
    /// other launch on the node is untouched, and an empty allow-list reaches
    /// nobody. Worker-daemon-only — the dispatcher never calls this, so its
    /// fleet stays bind-mount-free.
    pub fn with_docker_grant(mut self, grant: DockerGrant) -> Self {
        self.docker_grant = Some(grant);
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

    /// §3.1 placement: every node is (re-)probed here (so one that recovered
    /// rejoins without a dispatcher restart) and [`choose_placement`] decides.
    /// Every docker-endpoint node serves [`CONTAINER_ONLY_MODES`] and nothing
    /// else, so a host launch finds no candidate here (design #309 §5a), and
    /// every one of them enforces `resources.cpu`/`memory` — the limits reach
    /// the `HostConfig` this very backend builds (§7).
    async fn place(
        &self,
        pin: Option<&str>,
        required: LaunchRequirements<'_>,
    ) -> Result<&Node, BackendError> {
        let mut candidates = Vec::with_capacity(self.nodes.len());
        for (i, node) in self.nodes.iter().enumerate() {
            let load = self.probe_load(node).await;
            candidates.push(PlacementCandidate {
                index: i,
                name: &node.name,
                load,
                modes: CONTAINER_ONLY_MODES,
                resources_enforced: true,
                envs: NO_ENVS,
            });
        }
        self.mode_warnings.observe(&candidates, required.mode);
        let index = choose_placement(self.policy, &candidates, pin, required)?;
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
        let node = self
            .place(config.node.as_deref(), config.requirements())
            .await?;
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
                self.docker_grant.as_ref(),
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
        let Some(archive) = download_archive(node, cid, id, path).await? else {
            return Ok(None);
        };
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

    /// Resolve by streaming the `dir` tar and reading its headers (design #490
    /// D1a): the container has **exited** by harvest time, so `exec find` is
    /// not available and the archive endpoint is the only post-exit read.
    async fn find_file(
        &self,
        id: &ContainerId,
        dir: &str,
        name: &str,
    ) -> Result<Vec<String>, BackendError> {
        let (node, cid) = self.route(id)?;
        let Some(archive) = download_archive(node, cid, id, dir).await? else {
            return Ok(Vec::new());
        };
        archive_matches(dir, name, &archive)
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
/// [`KvmGrant::admits`], its `docker_grant` adds the node's socket per
/// [`DockerGrant::admits`], and its `nix_store` adds the store read-only for a
/// launch declaring `runtime.env` — nothing here sets `binds`.
fn build_host_config(
    config: &ContainerLaunchConfig,
    cache_dir: Option<&Path>,
    kvm: Option<&KvmGrant>,
    docker_grant: Option<&DockerGrant>,
    nix_store: Option<&Path>,
) -> Result<HostConfig, BackendError> {
    let granted = kvm.filter(|g| g.admits(&config.env));
    let socket = docker_grant.filter(|g| g.admits(&config.env));
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
    if let Some(g) = socket {
        mounts.push(writable_bind(&g.socket, DOCKER_SOCKET_MOUNT_PATH));
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
    build_host_config_socket_invariants(&host_config, &config.env, socket.is_some());
    Ok(host_config)
}

/// The socket's negative space, asserted on the built config rather than on the
/// decision that produced it: it rides exactly the launches the node's
/// allow-list names, once (design #517 D3), and none at any level but work
/// (design #543 D5).
fn build_host_config_socket_invariants(
    host_config: &HostConfig,
    env: &HashMap<String, String>,
    granted: bool,
) {
    debug_assert_eq!(
        mounts_target_count(host_config, DOCKER_SOCKET_MOUNT_PATH),
        usize::from(granted),
        "the socket rides exactly the launches the node's allow-list names, once (design #517 D3)"
    );
    debug_assert!(
        !granted || env.get(PHASE_ENV).map(String::as_str) == Some(PHASE_WORK),
        "the socket rides no launch at any level but work, whatever the allow-list says \
         (design #543 D5)"
    );
}

/// How many mounts a built host config lands at `target` — the negative-space
/// assertions above read it, so "granted nobody" is checked on the config
/// itself rather than on the decision that produced it.
fn mounts_target_count(host_config: &HostConfig, target: &str) -> usize {
    host_config
        .mounts
        .iter()
        .flatten()
        .filter(|m| m.target.as_deref() == Some(target))
        .count()
}

/// The two mounts a node adds that cannot be read-only: its build cache, which
/// sccache writes through, and its docker socket on an allow-listed launch,
/// which a client cannot connect through when read-only. Derived from
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

/// The tar the archive endpoint serves for `path`, or `None` when the endpoint
/// answers 404 — the one transport `copy_file` and `find_file` share, so each
/// keeps its own reading of the tar and of what an absent `path` means.
async fn download_archive(
    node: &Node,
    cid: &str,
    id: &ContainerId,
    path: &str,
) -> Result<Option<Vec<u8>>, BackendError> {
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
    Ok(Some(archive))
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

/// The wire paths of the archive members named `name`, for the tar the archive
/// endpoint roots at the requested directory's own basename (design #490 D1a).
/// Reassembled onto the caller's `dir`, so a match is expressed in the path the
/// caller asked about rather than in whatever the endpoint called its root.
fn archive_matches(dir: &str, name: &str, archive: &[u8]) -> Result<Vec<String>, BackendError> {
    let mut ar = tar::Archive::new(archive);
    let mut found: Vec<String> = Vec::new();
    for entry in ar
        .entries()
        .map_err(|e| BackendError::Other(e.to_string()))?
    {
        let entry = entry.map_err(|e| BackendError::Other(e.to_string()))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| BackendError::Other(e.to_string()))?
            .to_path_buf();
        let matched = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .is_some_and(|n| n == name);
        if !matched {
            continue;
        }
        let Some(below) = below_archive_root(&path) else {
            continue;
        };
        if found.len() >= crate::FIND_FILE_MATCHES_MAX {
            return Err(BackendError::Other(types::worker::find_file_too_many(
                dir,
                name,
                crate::FIND_FILE_MATCHES_MAX,
            )));
        }
        found.push(format!("{}/{below}", dir.trim_end_matches('/')));
    }
    Ok(found)
}

/// One archive member's path with the root component stripped, `None` for a
/// member that *is* the root. The endpoint names that component after the
/// requested directory, which is the caller's `dir` and not part of what lies
/// below it.
fn below_archive_root(path: &Path) -> Option<String> {
    let mut components = path.components();
    components.next()?;
    let rest = components.as_path();
    (!rest.as_os_str().is_empty()).then(|| rest.to_string_lossy().to_string())
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
    use types::job_type::RuntimeMode;

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
            modes: CONTAINER_ONLY_MODES,
            resources_enforced: true,
            envs: NO_ENVS,
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
        assert_eq!(
            choose_placement(hr, &nodes, None, RuntimeMode::Container.into()).unwrap(),
            2
        );

        let nodes = [cand("local", Some(2), 0), cand("nuc", None, 1)];
        assert_eq!(
            choose_placement(hr, &nodes, None, RuntimeMode::Container.into()).unwrap(),
            0
        );

        let nodes = [cand("local", Some(0), 0), cand("nuc", None, 1)];
        let err = choose_placement(hr, &nodes, None, RuntimeMode::Container.into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free slots on any node"), "{err}");
    }

    /// A pin places on the named node even when another has more free slots,
    /// and never spills over: a full or unknown pin errors. Policy-independent.
    #[test]
    fn choose_pin_honored_and_never_falls_back() {
        let hr = PlacementPolicy::Headroom;
        let nodes = [cand("local", Some(1), 0), cand("nuc", Some(4), 1)];
        assert_eq!(
            choose_placement(hr, &nodes, Some("local"), RuntimeMode::Container.into()).unwrap(),
            0
        );

        let nodes = [cand("local", Some(0), 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("local"), RuntimeMode::Container.into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free slots on node local"), "{err}");

        let nodes = [cand("local", None, 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("local"), RuntimeMode::Container.into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free slots on node local"), "{err}");

        let nodes = [cand("local", Some(1), 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("mini"), RuntimeMode::Container.into())
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
        let hc = build_host_config(&launch_config(), None, None, None, None).unwrap();
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
        let hc =
            build_host_config(&launch_config(), Some(dir.as_path()), None, None, None).unwrap();
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
        let hc = build_host_config(&config, None, None, None, Some(store.as_path())).unwrap();

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

        let hc =
            build_host_config(&launch_config(), None, None, None, Some(store.as_path())).unwrap();
        assert!(
            hc.mounts.is_none(),
            "a launch declaring no env is exactly what it is today"
        );

        let hc = build_host_config(&config, None, None, None, None).unwrap();
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
            None,
            Some(Path::new(STORE_MOUNT_PATH)),
        )
        .unwrap();
        assert_eq!(
            stores_in(&hc),
            vec![STORE_MOUNT_PATH.to_string()],
            "one store mount, not two"
        );

        let elsewhere = Path::new("/data/nix/store");
        let hc =
            build_host_config(&config, None, Some(&kvm_grant()), None, Some(elsewhere)).unwrap();
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
        let hc = build_host_config(
            &launch_config_for("acme/beacon"),
            cache,
            Some(grant),
            None,
            None,
        )
        .unwrap();
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
                let hc = build_host_config(&config, None, Some(&grant), None, None).unwrap();
                assert!(hc.devices.is_none(), "unlisted launch got the device");
                assert!(hc.mounts.is_none(), "unlisted launch got the mounts");
            }
        }

        let nobody = KvmGrant {
            projects: vec![],
            ..kvm_grant()
        };
        let hc = build_host_config(
            &launch_config_for("acme/beacon"),
            None,
            Some(&nobody),
            None,
            None,
        )
        .unwrap();
        assert!(hc.devices.is_none(), "an empty allow-list grants nobody");
        assert!(hc.mounts.is_none(), "an empty allow-list grants nobody");
    }

    fn docker_grant() -> DockerGrant {
        DockerGrant {
            socket: PathBuf::from("/var/run/docker.sock"),
            allowed: vec![DockerGrantEntry {
                project: "acme/beacon".to_string(),
                job_type: "build-image".to_string(),
            }],
        }
    }

    /// A work-level launch of `(project, job_type)` — the level a grant admits,
    /// so it is what every socket case that is not about the level uses.
    fn launch_config_for_pair(project: &str, job_type: &str) -> ContainerLaunchConfig {
        launch_config_at_level(project, job_type, Some(PHASE_WORK))
    }

    /// The same launch at whichever level the dispatcher stamped, `None` being
    /// a launch carrying no level stamp at all.
    fn launch_config_at_level(
        project: &str,
        job_type: &str,
        phase: Option<&str>,
    ) -> ContainerLaunchConfig {
        let mut config = launch_config_for(project);
        config.env.insert("JOB_TYPE".into(), job_type.to_string());
        if let Some(phase) = phase {
            config.env.insert(PHASE_ENV.into(), phase.to_string());
        }
        config
    }

    fn socket_bound(config: &ContainerLaunchConfig, grant: &DockerGrant) -> bool {
        let hc = build_host_config(config, None, None, Some(grant), None).unwrap();
        mounts_target_count(&hc, DOCKER_SOCKET_MOUNT_PATH) == 1
    }

    /// An entry is `owner/project:job_type` and nothing else: every malformed
    /// spelling is refused and named rather than accepted as a grant that
    /// silently never matches a launch (design #517 D3, fail closed).
    #[test]
    fn docker_grant_entries_parse_or_are_refused_by_shape() {
        assert_eq!(
            DockerGrantEntry::parse(" acme/beacon:build-image ").unwrap(),
            DockerGrantEntry {
                project: "acme/beacon".into(),
                job_type: "build-image".into(),
            }
        );

        for malformed in [
            "",
            "acme/beacon",
            "acme:build-image",
            "acme/beacon:",
            ":build-image",
            "/beacon:build-image",
            "acme/:build-image",
            "acme/beacon/extra:build-image",
            "acme/beacon:build:image",
        ] {
            let err = DockerGrantEntry::parse(malformed).unwrap_err();
            assert!(
                err.contains("owner/project:job_type"),
                "{malformed:?}: {err}"
            );
        }
    }

    /// An allow-listed `(project, job type)` on a docker node gets the node's
    /// socket at the conventional container path, writable — a client cannot
    /// connect through a read-only bind — and gets no device and no toolchain
    /// mount with it (design #517 D3).
    #[test]
    fn host_config_with_a_docker_grant_binds_the_socket_for_an_allow_listed_pair() {
        let hc = build_host_config(
            &launch_config_for_pair("acme/beacon", "build-image"),
            None,
            None,
            Some(&docker_grant()),
            None,
        )
        .unwrap();

        assert!(hc.devices.is_none(), "a socket grant carries no device");
        assert!(hc.binds.is_none(), "the socket is a mount, not a bind");
        let mounts = hc
            .mounts
            .expect("an allow-listed launch carries the socket");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].typ, Some(MountTypeEnum::BIND));
        assert_eq!(mounts[0].source.as_deref(), Some("/var/run/docker.sock"));
        assert_eq!(mounts[0].target.as_deref(), Some(DOCKER_SOCKET_MOUNT_PATH));
        assert_eq!(
            mounts[0].read_only,
            Some(false),
            "a client connecting through the socket must be able to write to it"
        );
    }

    /// The negative space: every launch the allow-list does not name gets
    /// NOTHING on the same node that grants the socket to the named pair — a
    /// different project, a different job type of the same project, and a
    /// launch missing either stamp. An empty allow-list grants nobody, and a
    /// node declaring no grant at all is byte-identical to today.
    #[test]
    fn host_config_without_a_docker_allow_list_entry_binds_nothing() {
        let grant = docker_grant();
        for config in [
            launch_config_for_pair("acme/other", "build-image"),
            launch_config_for_pair("acme/beacon", "code"),
            launch_config_for_pair("", ""),
            launch_config_for("acme/beacon"),
            launch_config(),
        ] {
            let hc = build_host_config(&config, None, None, Some(&grant), None).unwrap();
            assert!(hc.mounts.is_none(), "unlisted launch got the socket");
            assert!(hc.devices.is_none(), "unlisted launch got a device");
        }

        let nobody = DockerGrant {
            allowed: vec![],
            ..docker_grant()
        };
        let admitted = launch_config_for_pair("acme/beacon", "build-image");
        let hc = build_host_config(&admitted, None, None, Some(&nobody), None).unwrap();
        assert!(hc.mounts.is_none(), "an empty allow-list grants nobody");

        let hc = build_host_config(&admitted, None, None, None, None).unwrap();
        assert!(
            hc.mounts.is_none() && hc.devices.is_none() && hc.binds.is_none(),
            "a node declaring no grant is what it was before this existed"
        );
    }

    /// The grant is scoped to work level (design #543 D5): the allow-listed
    /// pair that holds the socket while its work step runs holds nothing as an
    /// evaluator — the appended `ci` one included — and nothing a job type can
    /// declare moves that, because the level is read from the `CHUG_` stamp
    /// spec §4.1 seals rather than from the `CHANNEL_ROLE` beside it.
    #[test]
    fn a_docker_grant_reaches_work_level_launches_only() {
        let grant = docker_grant();
        let at = |phase| launch_config_at_level("acme/beacon", "build-image", phase);

        assert!(
            socket_bound(&at(Some(PHASE_WORK)), &grant),
            "the work step of an allow-listed pair is what the node consented to"
        );
        for withheld in [
            at(Some("Evaluation")),
            at(None),
            at(Some("work")),
            at(Some("")),
        ] {
            assert!(
                !socket_bound(&withheld, &grant),
                "only {PHASE_WORK} is work level: {:?}",
                withheld.env.get(PHASE_ENV)
            );
        }

        let mut spoofed = at(Some("Evaluation"));
        spoofed.env.insert("CHANNEL_ROLE".into(), "work".into());
        assert!(
            !socket_bound(&spoofed, &grant),
            "a declarable var must not buy the level a sealed stamp decides"
        );

        for phase in [Some(PHASE_WORK), Some("Evaluation"), None] {
            assert!(
                !socket_bound(
                    &launch_config_at_level("acme/beacon", "code", phase),
                    &grant
                ),
                "a job type the allow-list never named is unaffected at every level"
            );
        }
    }

    /// The socket is a fourth independent node property: a node that caches,
    /// passes KVM through and grants the socket to the same launch carries all
    /// of them, the socket writable and the toolchain pair still read-only, and
    /// each grant is still decided on its own key.
    #[test]
    fn host_config_composes_the_docker_socket_with_the_other_node_properties() {
        let dir = PathBuf::from("/var/cache/chuggernaut/sccache");
        let hc = build_host_config(
            &launch_config_for_pair("acme/beacon", "build-image"),
            Some(dir.as_path()),
            Some(&kvm_grant()),
            Some(&docker_grant()),
            None,
        )
        .unwrap();

        assert_eq!(hc.devices.map(|d| d.len()), Some(1));
        let mounts = hc.mounts.expect("three properties carry mounts");
        assert_eq!(mounts.len(), 4);
        assert_eq!(
            read_only_targets(&mounts),
            vec![STORE_MOUNT_PATH, ANDROID_SDK_MOUNT_PATH],
            "the socket is never counted among the read-only toolchain mounts"
        );
        assert_eq!(
            source_at(&mounts, DOCKER_SOCKET_MOUNT_PATH),
            Some("/var/run/docker.sock")
        );

        let kvm_only = build_host_config(
            &launch_config_for_pair("acme/beacon", "code"),
            None,
            Some(&kvm_grant()),
            Some(&docker_grant()),
            None,
        )
        .unwrap();
        assert_eq!(
            source_at(
                &kvm_only.mounts.unwrap_or_default(),
                DOCKER_SOCKET_MOUNT_PATH
            ),
            None,
            "the KVM allow-list admits this launch and the docker one does not"
        );
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

    /// The tar the archive endpoint returns for a directory, rooted at that
    /// directory's own basename the way docker roots it.
    fn archive_of(root: &str, members: &[(&str, bool)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (rel, is_dir) in members {
            let mut header = tar::Header::new_gnu();
            header.set_path(format!("{root}/{rel}")).unwrap();
            header.set_mode(0o644);
            header.set_size(0);
            if *is_dir {
                header.set_entry_type(tar::EntryType::Directory);
            }
            header.set_cksum();
            builder.append(&header, std::io::empty()).unwrap();
        }
        builder.into_inner().unwrap()
    }

    /// Resolution over the archive endpoint (design #490 D1a): one match comes
    /// back as the caller's own wire path, an absent name as an empty list, and
    /// several as several — "one" and "several" must be distinguishable.
    #[test]
    fn archive_matches_answers_in_wire_paths_and_counts() {
        let dir = "/chuggernaut/claude/projects";
        let session = "0d9e-session.jsonl";
        let archive = archive_of(
            "projects",
            &[
                ("-workspace", true),
                (&format!("-workspace/{session}"), false),
                ("-workspace/other.jsonl", false),
            ],
        );
        assert_eq!(
            archive_matches(dir, session, &archive).unwrap(),
            vec![format!("{dir}/-workspace/{session}")]
        );
        assert!(
            archive_matches(dir, "absent.jsonl", &archive)
                .unwrap()
                .is_empty(),
            "a name nothing carries resolves to nothing, not an error"
        );

        let several = archive_of(
            "projects",
            &[
                (&format!("-workspace/{session}"), false),
                (&format!("-Users-ci-elsewhere/{session}"), false),
            ],
        );
        assert_eq!(
            archive_matches(dir, session, &several).unwrap().len(),
            2,
            "a second cwd in the same config dir is exactly the case D1 candidate 3 died on"
        );

        let directory_only = archive_of("projects", &[(session, true)]);
        assert!(
            archive_matches(dir, session, &directory_only)
                .unwrap()
                .is_empty(),
            "a directory with the name is not a match"
        );
    }

    /// The bound refuses rather than returning a longer list
    /// (docs/reference/style.md Tier 2 rule 3), and names itself so the caller can
    /// tell a refusal from an empty scan.
    #[test]
    fn archive_matches_refuses_past_the_bound() {
        let dir = "/chuggernaut/claude/projects";
        let name = "s.jsonl";
        let at_bound: Vec<(String, bool)> = (0..crate::FIND_FILE_MATCHES_MAX)
            .map(|n| (format!("dir-{n}/{name}"), false))
            .collect();
        let members: Vec<(&str, bool)> = at_bound.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        assert_eq!(
            archive_matches(dir, name, &archive_of("projects", &members))
                .unwrap()
                .len(),
            crate::FIND_FILE_MATCHES_MAX,
            "the bound itself still answers"
        );

        let mut over = at_bound.clone();
        over.push((format!("dir-over/{name}"), false));
        let members: Vec<(&str, bool)> = over.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        let err = archive_matches(dir, name, &archive_of("projects", &members))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(types::worker::FIND_FILE_TOO_MANY) && err.contains(name),
            "{err}"
        );
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

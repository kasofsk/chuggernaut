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
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    DownloadFromContainerOptionsBuilder, ListContainersOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Label stamped on every container we launch; placement counts by it.
const MANAGED_LABEL: &str = "chuggernaut.managed";

/// Identity labels stamped alongside [`MANAGED_LABEL`] so the §3.6 fleet sweep
/// can match a running container back to its owning task without inspecting the
/// container. Sourced from the launch env the dispatcher already sets.
const PROJECT_LABEL: &str = "chuggernaut.project";
const JOB_LABEL: &str = "chuggernaut.job";
const TASK_LABEL: &str = "chuggernaut.task";

/// Container-side path of the node-local build cache, when a backend is
/// configured with one ([`DockerBackend::with_cache_dir`]). Exported so the
/// worker daemon points `SCCACHE_DIR` at the same path it bind-mounts here —
/// one source of truth, no drift between the mount and the env. Carries no job
/// state: it is a build accelerator only, safe to be empty/cold (spec §3.1).
pub const CACHE_MOUNT_PATH: &str = "/cache/sccache";

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
    /// container at [`CACHE_MOUNT_PATH`]. `None` (the dispatcher's construction)
    /// adds no binds at all — the fleet stays bind-mount-free (spec §3.1). Set
    /// worker-side via [`with_cache_dir`](DockerBackend::with_cache_dir); it is
    /// a node property, never carried on the wire or the launch config.
    cache_dir: Option<PathBuf>,
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
    /// launched container at [`CACHE_MOUNT_PATH`]. Worker-daemon-only — the
    /// dispatcher never calls this, so its fleet stays bind-mount-free (spec
    /// §3.1). The daemon owns creating/owning `host_dir`; concurrent containers
    /// on the node share it, which sccache handles by design (it locks).
    pub fn with_cache_dir(mut self, host_dir: PathBuf) -> Self {
        self.cache_dir = Some(host_dir);
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
            // The last unreachable node's error, else the all-reachable-but-no-slots case.
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

    /// Running `chuggernaut.managed` containers across all nodes.
    pub async fn managed_running_total(&self) -> Result<u32, BackendError> {
        let mut total = 0;
        for node in &self.nodes {
            total += self.managed_running(node).await?;
        }
        Ok(total)
    }

    async fn managed_running(&self, node: &Node) -> Result<u32, BackendError> {
        let opts = ListContainersOptionsBuilder::default()
            .filters(&HashMap::from([
                ("label".to_string(), vec![format!("{MANAGED_LABEL}=true")]),
                ("status".to_string(), vec!["running".to_string()]),
            ]))
            .build();
        let list = node
            .docker
            .list_containers(Some(opts))
            .await
            .map_err(|e| BackendError::Other(e.to_string()))?;
        Ok(list.len() as u32)
    }

    /// Exited managed containers on one node, as `{node}/{docker_id}` ids —
    /// the same encoding as launch, so the sweep can match against task records.
    async fn managed_exited(&self, node: &Node) -> Result<Vec<ContainerId>, BackendError> {
        let opts = ListContainersOptionsBuilder::default()
            // `all(true)` is required to see anything but running containers.
            .all(true)
            .filters(&HashMap::from([
                ("label".to_string(), vec![format!("{MANAGED_LABEL}=true")]),
                ("status".to_string(), vec!["exited".to_string()]),
            ]))
            .build();
        let list = node
            .docker
            .list_containers(Some(opts))
            .await
            .map_err(|e| BackendError::Other(e.to_string()))?;
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
        let opts = ListContainersOptionsBuilder::default()
            .filters(&HashMap::from([
                ("label".to_string(), vec![format!("{MANAGED_LABEL}=true")]),
                ("status".to_string(), vec!["running".to_string()]),
            ]))
            .build();
        let list = node
            .docker
            .list_containers(Some(opts))
            .await
            .map_err(|e| BackendError::Other(e.to_string()))?;
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
            let load = self.probe_load(node).await; // None ⇒ out of service
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
        let body = ContainerCreateBody {
            image: Some(config.image.clone()),
            cmd: Some(config.cmd.clone()),
            env: Some(config.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            labels: Some(managed_labels(&config)),
            host_config: Some(build_host_config(&config, self.cache_dir.as_deref())?),
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
            // A non-zero exit surfaces as ContainerWaitError on some daemons —
            // the exit code rides in the error body.
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
            // Already exited: kill is idempotent from the dispatcher's view.
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
        // `follow: false` — this is called after exit, and following would hang.
        // Both streams: a failed build's message is as often on stderr as
        // stdout. Cross-stream ordering is Docker's, by timestamp, and is not
        // exact for same-millisecond writes.
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

    async fn logs_tail(&self, id: &ContainerId, since: u64) -> Result<LogTail, BackendError> {
        let (node, cid) = self.route(id)?;
        // `follow: false` on a *running* container: bollard returns what has
        // been captured so far and the stream ends, so this never blocks (the
        // hang warning on `logs` is specifically about `follow: true`). Both
        // streams, same as `logs`, since a build's progress lands on either.
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
        Ok(LogTail::slice(&out, since))
    }

    async fn remove(&self, id: &ContainerId) -> Result<(), BackendError> {
        let (node, cid) = self.route(id)?;
        // force=false — the caller only removes after the container has exited
        // and its artifacts are harvested.
        let opts = RemoveContainerOptionsBuilder::default()
            .force(false)
            .build();
        match node.docker.remove_container(cid, Some(opts)).await {
            Ok(()) => Ok(()),
            // Already gone (404) or a removal already in flight (409): the
            // overlay is reclaimed either way, so removal is idempotent.
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
                // One unreachable node must not blank the whole fleet sweep —
                // the others' orphans still get reaped (§3.6).
                Err(e) => tracing::warn!(node = %node.name, "list_managed_running skipped: {e}"),
            }
        }
        Ok(out)
    }

    fn fleet_status(&self) -> Vec<NodeStatus> {
        // Docker endpoints carry no chuggernaut version — health only.
        self.availability()
            .into_iter()
            .map(|(name, available)| NodeStatus {
                name,
                available,
                version: None,
                refresh_outcome: None,
            })
            .collect()
    }
}

/// Build the container's `HostConfig` from the launch limits plus the optional
/// node-local cache dir. Factored out of Docker I/O so the produced spec — in
/// particular whether a cache bind-mount is present — is unit-tested without a
/// daemon. `cache_dir = None` (the dispatcher's backend) yields `binds: None`:
/// the fleet stays bind-mount-free (spec §3.1). `Some(dir)` adds exactly one
/// bind, `{dir}:{CACHE_MOUNT_PATH}`, carrying no job state.
fn build_host_config(
    config: &ContainerLaunchConfig,
    cache_dir: Option<&Path>,
) -> Result<HostConfig, BackendError> {
    Ok(HostConfig {
        nano_cpus: config.cpu_limit.map(|c| (c * 1e9) as i64),
        memory: config
            .memory_limit
            .as_deref()
            .map(parse_memory)
            .transpose()
            .map_err(BackendError::Launch)?,
        binds: cache_dir.map(|d| vec![format!("{}:{}", d.display(), CACHE_MOUNT_PATH)]),
        ..Default::default()
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
            "5Gi", "512Mi", "4Ki", "1048576", // legal
            "5g", "4GB", "", "  ", "-5", "0", "1.5Gi", "Gi", "5gi", // illegal
        ] {
            assert_eq!(
                parse_memory(case).ok(),
                types::parse_memory(case).ok(),
                "launch-time parse and types validation disagree on {case:?}"
            );
        }
    }

    fn cand<'a>(name: &'a str, free: Option<i64>, index: usize) -> PlacementCandidate<'a> {
        // running is irrelevant under Headroom (these tests exercise it); model
        // a node with `free` slots and no running load.
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
        // Most free wins; the 4==4 tie breaks to the lexicographically-first name.
        assert_eq!(choose_placement(hr, &nodes, None).unwrap(), 2);

        // An out-of-service node (free = None) is skipped even with more slots.
        let nodes = [cand("local", Some(2), 0), cand("nuc", None, 1)];
        assert_eq!(choose_placement(hr, &nodes, None).unwrap(), 0);

        // No free slots anywhere ⇒ the unchanged fleet-wide message.
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
        // `nuc` has more slots, but the pin to `local` wins.
        assert_eq!(choose_placement(hr, &nodes, Some("local")).unwrap(), 0);

        // Pinned node full ⇒ launch error naming it, no fallback to `nuc`.
        let nodes = [cand("local", Some(0), 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("local"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free slots on node local"), "{err}");

        // Pinned node out of service ⇒ same "no free slots" shape.
        let nodes = [cand("local", None, 0), cand("nuc", Some(4), 1)];
        let err = choose_placement(hr, &nodes, Some("local"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no free slots on node local"), "{err}");

        // Unknown pin ⇒ error naming the known nodes.
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
            image: "img".into(),
            cmd: vec!["run".into()],
            env: HashMap::new(),
            files: vec![],
            cpu_limit: Some(2.0),
            memory_limit: Some("4Gi".into()),
            node: None,
        }
    }

    /// The dispatcher's backend (no cache dir) adds NO binds — the fleet stays
    /// bind-mount-free (spec §3.1). Regression guard: the dispatcher path is
    /// untouched by node-local caching.
    #[test]
    fn host_config_without_cache_has_no_binds() {
        let hc = build_host_config(&launch_config(), None).unwrap();
        assert!(hc.binds.is_none(), "dispatcher path must add no binds");
        // The pre-existing limits are still translated.
        assert_eq!(hc.nano_cpus, Some(2_000_000_000));
        assert_eq!(hc.memory, Some(4 * 1024 * 1024 * 1024));
    }

    /// A worker backend with a cache dir bind-mounts exactly that dir at the
    /// fixed container path, and nothing else.
    #[test]
    fn host_config_with_cache_adds_one_bind() {
        let dir = PathBuf::from("/var/cache/chuggernaut/sccache");
        let hc = build_host_config(&launch_config(), Some(dir.as_path())).unwrap();
        assert_eq!(
            hc.binds,
            Some(vec![format!(
                "/var/cache/chuggernaut/sccache:{CACHE_MOUNT_PATH}"
            )])
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

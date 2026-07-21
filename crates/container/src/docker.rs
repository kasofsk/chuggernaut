//! Docker fleet backend — the v1 production default (spec §3.1).
//!
//! One or more Docker daemons: local socket single-node, TCP endpoints
//! multi-node (mTLS wiring TODO). Slot-capped least-loaded placement;
//! `ContainerId` encodes the owning node as `{node}/{docker_id}`. Files are
//! injected via put-archive after create, before start — no host bind-mounts,
//! so remote nodes need nothing on disk.

use crate::{
    BackendError, ContainerBackend, ContainerId, ContainerLaunchConfig, ContainerStatus,
    InjectedFile,
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
use std::path::Path;

/// Label stamped on every container we launch; placement counts by it.
const MANAGED_LABEL: &str = "chuggernaut.managed";

#[derive(Debug, Clone)]
pub struct DockerNodeConfig {
    pub name: String,
    /// `unix:///var/run/docker.sock` or `tcp://host:2375`. TLS: TODO (§3.1).
    pub endpoint: String,
    /// Max concurrent chuggernaut containers on this node.
    pub slots: u32,
}

struct Node {
    name: String,
    slots: u32,
    docker: Docker,
}

pub struct DockerBackend {
    nodes: Vec<Node>,
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
            });
        }
        if nodes.is_empty() {
            return Err(BackendError::Unavailable("empty node list".into()));
        }
        Ok(Self { nodes })
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
            }],
        })
    }

    /// §3.6 startup rule: the dispatcher will not start if any configured node
    /// is unreachable.
    pub async fn ping_all(&self) -> Result<(), BackendError> {
        for node in &self.nodes {
            node.docker
                .ping()
                .await
                .map_err(|e| BackendError::Unavailable(format!("node {}: {e}", node.name)))?;
        }
        Ok(())
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

    /// §3.1 placement: most free slots, ties broken by name.
    async fn place(&self) -> Result<&Node, BackendError> {
        let mut best: Option<(&Node, i64)> = None;
        for node in &self.nodes {
            let free = node.slots as i64 - self.managed_running(node).await? as i64;
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
}

#[async_trait]
impl ContainerBackend for DockerBackend {
    async fn launch(&self, config: ContainerLaunchConfig) -> Result<ContainerId, BackendError> {
        let node = self.place().await?;
        let body = ContainerCreateBody {
            image: Some(config.image.clone()),
            cmd: Some(config.cmd.clone()),
            env: Some(config.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            labels: Some(HashMap::from([(
                MANAGED_LABEL.to_string(),
                "true".to_string(),
            )])),
            host_config: Some(HostConfig {
                nano_cpus: config.cpu_limit.map(|c| (c * 1e9) as i64),
                memory: config
                    .memory_limit
                    .as_deref()
                    .map(parse_memory)
                    .transpose()
                    .map_err(BackendError::Launch)?,
                ..Default::default()
            }),
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

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
use std::sync::Arc;
use store::worker::{encode_reply, op_from_subject};
use store::{NatsStore, StoreError};
use types::worker::{
    ContainerRef, CopyFileOk, CopyFileRequest, FileSource, InspectOk, LaunchOk, LogsOk, PingOk,
    WireStatus, WorkerError, WorkerLaunchRequest, WorkerReply, b64_decode, b64_encode,
};

/// Logs are tailed to fit the reply under NATS's 1MB max_payload after
/// base64 + JSON overhead.
const LOGS_CAP: usize = 700 * 1024;

/// Concurrent op handlers — a slow launch must not starve inspect polls.
const MAX_INFLIGHT: usize = 16;

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
    let backend = DockerBackend::new(vec![DockerNodeConfig {
        name: config.node.clone(),
        endpoint: config.docker_endpoint.clone(),
        // The dispatcher owns slot policy; the worker only reports usage.
        slots: u32::MAX,
    }])?;
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
    });

    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(&config.node))
        .await?;
    tracing::info!(node = %config.node, nats = %config.nats_url, version = %state.version, "worker up");

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

fn version_string() -> String {
    match option_env!("CHUG_GIT_SHA") {
        Some(sha) => format!("{}+{}", env!("CARGO_PKG_VERSION"), sha),
        None => env!("CARGO_PKG_VERSION").to_string(),
    }
}

async fn handle(state: &WorkerState, subject: &str, payload: &[u8]) -> Vec<u8> {
    match op_from_subject(subject) {
        Some("launch") => encode_reply(&launch(state, payload).await),
        Some("kill") => encode_reply(&kill(state, payload).await),
        Some("inspect") => encode_reply(&inspect(state, payload).await),
        Some("copy_file") => encode_reply(&copy_file(state, payload).await),
        Some("logs") => encode_reply(&logs(state, payload).await),
        Some("ping") => encode_reply(&ping(state).await),
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
            let id = state
                .backend
                .launch(ContainerLaunchConfig {
                    image: req.image,
                    cmd: req.cmd,
                    env: req.env,
                    files,
                    cpu_limit: req.cpu_limit,
                    memory_limit: req.memory_limit,
                })
                .await
                .map_err(backend_err)?;
            Ok(LaunchOk { id })
        }
        .await,
    )
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
            })
        }
        .await,
    )
}

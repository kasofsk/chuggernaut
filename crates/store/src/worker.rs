//! Typed RPC client for the worker-node protocol (spec §3.1). The dispatcher's
//! fleet backend calls these; the daemon side serves the same subjects via
//! [`crate::NatsStore::subscribe_requests`]. NATS never leaks past this crate.

use crate::{NatsStore, StoreError, subjects};
use std::time::Duration;
use types::worker::{
    ContainerRef, CopyFileOk, CopyFileRequest, InspectOk, LaunchOk, ListExitedOk, ListRunningOk,
    LogsOk, LogsTailOk, LogsTailRequest, PingOk, RefreshCancelOk, RefreshCancelRequest, RefreshOk,
    RefreshRequest, WorkerError, WorkerLaunchRequest, WorkerReply,
};

/// Requests must fit NATS's default 1MB max_payload with headroom. Launch
/// payloads are small by design (static artifacts are node-local); tripping
/// this guard means bulk bytes leaked back into the launch path.
pub const MAX_REQUEST_BYTES: usize = 900 * 1024;

/// Ops that just execute a container action.
const OP_TIMEOUT: Duration = Duration::from_secs(60);
/// Liveness probe — placement blocks on this, keep it tight.
const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// Self-refresh: the daemon accepts fast and builds/swaps in the background
/// (spec §3.1), so this only covers the accept round-trip, not the build.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Typed request-reply to one worker node.
#[derive(Clone)]
pub struct WorkerRpc {
    store: NatsStore,
    node: String,
}

/// A worker op outcome: the op reached the worker and either succeeded or
/// failed there (`Op`), or transport failed / timed out (`Transport`).
#[derive(Debug)]
pub enum WorkerRpcError {
    Op(WorkerError),
    Transport(String),
}

impl std::fmt::Display for WorkerRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerRpcError::Op(e) => write!(f, "worker op failed: {e:?}"),
            WorkerRpcError::Transport(m) => write!(f, "worker transport: {m}"),
        }
    }
}

impl WorkerRpc {
    pub fn new(store: NatsStore, node: impl Into<String>) -> Self {
        Self {
            store,
            node: node.into(),
        }
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    async fn call<Req: serde::Serialize, Ok: serde::de::DeserializeOwned>(
        &self,
        op: &str,
        req: &Req,
        timeout: Duration,
    ) -> std::result::Result<Ok, WorkerRpcError> {
        let payload =
            serde_json::to_vec(req).map_err(|e| WorkerRpcError::Transport(e.to_string()))?;
        if payload.len() > MAX_REQUEST_BYTES {
            return Err(WorkerRpcError::Op(WorkerError::Launch {
                message: format!(
                    "request payload {} bytes exceeds {} — static artifacts must be node-local on worker nodes",
                    payload.len(),
                    MAX_REQUEST_BYTES
                ),
            }));
        }
        let subject = subjects::worker_op(&self.node, op);
        let msg = self
            .store
            .request_timeout(&subject, &payload, timeout)
            .await
            .map_err(|e| WorkerRpcError::Transport(e.to_string()))?;
        let reply: WorkerReply<Ok> = serde_json::from_slice(&msg.payload)
            .map_err(|e| WorkerRpcError::Transport(format!("bad reply from {subject}: {e}")))?;
        match reply {
            WorkerReply::Ok { value } => Ok(value),
            WorkerReply::Err { error } => Err(WorkerRpcError::Op(error)),
        }
    }

    pub async fn launch(
        &self,
        req: &WorkerLaunchRequest,
    ) -> std::result::Result<LaunchOk, WorkerRpcError> {
        self.call("launch", req, OP_TIMEOUT).await
    }

    pub async fn kill(&self, id: &str) -> std::result::Result<(), WorkerRpcError> {
        let _: serde_json::Value = self
            .call("kill", &ContainerRef { id: id.into() }, OP_TIMEOUT)
            .await?;
        Ok(())
    }

    pub async fn inspect(&self, id: &str) -> std::result::Result<InspectOk, WorkerRpcError> {
        self.call("inspect", &ContainerRef { id: id.into() }, OP_TIMEOUT)
            .await
    }

    pub async fn copy_file(
        &self,
        id: &str,
        path: &str,
    ) -> std::result::Result<CopyFileOk, WorkerRpcError> {
        self.call(
            "copy_file",
            &CopyFileRequest {
                id: id.into(),
                path: path.into(),
            },
            OP_TIMEOUT,
        )
        .await
    }

    pub async fn logs(&self, id: &str) -> std::result::Result<LogsOk, WorkerRpcError> {
        self.call("logs", &ContainerRef { id: id.into() }, OP_TIMEOUT)
            .await
    }

    pub async fn logs_tail(
        &self,
        id: &str,
        since: u64,
    ) -> std::result::Result<LogsTailOk, WorkerRpcError> {
        self.call(
            "logs_tail",
            &LogsTailRequest {
                id: id.into(),
                since,
            },
            OP_TIMEOUT,
        )
        .await
    }

    pub async fn ping(&self) -> std::result::Result<PingOk, WorkerRpcError> {
        self.call("ping", &serde_json::json!({}), PING_TIMEOUT)
            .await
    }

    /// Request a self-refresh (spec §3.1): the daemon rebuilds its images at
    /// `req.sha` and swaps itself. Returns as soon as the daemon accepts — the
    /// new version shows up on a later `ping`.
    pub async fn refresh(
        &self,
        req: &RefreshRequest,
    ) -> std::result::Result<RefreshOk, WorkerRpcError> {
        self.call("refresh", req, REFRESH_TIMEOUT).await
    }

    /// Cancel an in-flight self-refresh to `req.sha` (ticket #254). Like
    /// `refresh` this only covers the round-trip: the daemon signals its build
    /// and answers, it does not wait for the build to die.
    pub async fn refresh_cancel(
        &self,
        req: &RefreshCancelRequest,
    ) -> std::result::Result<RefreshCancelOk, WorkerRpcError> {
        self.call("refresh_cancel", req, REFRESH_TIMEOUT).await
    }

    pub async fn remove(&self, id: &str) -> std::result::Result<(), WorkerRpcError> {
        let _: serde_json::Value = self
            .call("remove", &ContainerRef { id: id.into() }, OP_TIMEOUT)
            .await?;
        Ok(())
    }

    pub async fn list_exited(&self) -> std::result::Result<ListExitedOk, WorkerRpcError> {
        self.call("list_exited", &serde_json::json!({}), OP_TIMEOUT)
            .await
    }

    pub async fn list_running(&self) -> std::result::Result<ListRunningOk, WorkerRpcError> {
        self.call("list_running", &serde_json::json!({}), OP_TIMEOUT)
            .await
    }
}

/// Parse the op name off an inbound worker subject
/// (`req.worker.{node}.{op}` → `op`). The daemon's dispatch switch.
pub fn op_from_subject(subject: &str) -> Option<&str> {
    subject.rsplit('.').next().filter(|s| !s.is_empty())
}

/// Serialize a reply envelope; used by the daemon.
pub fn encode_reply<T: serde::Serialize>(reply: &WorkerReply<T>) -> Vec<u8> {
    serde_json::to_vec(reply).unwrap_or_else(|e| {
        serde_json::to_vec(&WorkerReply::<()>::Err {
            error: WorkerError::Other {
                message: format!("reply serialization: {e}"),
            },
        })
        .expect("error envelope serializes")
    })
}

impl From<StoreError> for WorkerRpcError {
    fn from(e: StoreError) -> Self {
        WorkerRpcError::Transport(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_parses_from_subject() {
        assert_eq!(op_from_subject("req.worker.nuc.launch"), Some("launch"));
        assert_eq!(
            op_from_subject("req.worker.nuc.copy_file"),
            Some("copy_file")
        );
        assert_eq!(op_from_subject(""), None);
    }
}

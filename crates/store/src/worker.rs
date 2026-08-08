//! Typed RPC client for the worker-node protocol (spec §3.1). The dispatcher's
//! fleet backend calls these; the daemon side serves the same subjects via
//! [`crate::NatsStore::subscribe_requests`]. NATS never leaks past this crate.

use crate::{NatsStore, StoreError, subjects};
use std::time::Duration;
use types::worker::{
    ContainerRef, CopyFileChunkOk, CopyFileChunkRequest, CopyFileOk, CopyFileRequest, FindFileOk,
    FindFileRequest, InspectOk, LaunchOk, ListExitedOk, ListRunningOk, LogsOk, LogsTailOk,
    LogsTailRequest, PingOk, RefreshCancelOk, RefreshCancelRequest, RefreshOk, RefreshRequest,
    SetSlotsOk, SetSlotsRequest, WorkerError, WorkerLaunchRequest, WorkerReply,
};

/// Requests must fit NATS's default 1MB max_payload with headroom. Launch
/// payloads are small by design (static artifacts are node-local); tripping
/// this guard means bulk bytes leaked back into the launch path.
pub const MAX_REQUEST_BYTES: usize = 900 * 1024;

/// The largest file `copy_file` may return over the worker RPC. Requests and
/// replies ride the same `max_payload`, so it is [`MAX_REQUEST_BYTES`] less a
/// JSON envelope, scaled down by base64's 4/3 cost.
pub const MAX_COPY_FILE_BYTES: usize = (MAX_REQUEST_BYTES - COPY_FILE_ENVELOPE_BYTES) / 4 * 3;

/// Headroom left for the `CopyFileOk` JSON around the base64 payload.
const COPY_FILE_ENVELOPE_BYTES: usize = 1024;

pub use types::worker::COPY_FILE_TOO_LARGE;

/// The `copy_file` bound check the daemon applies before encoding its reply
/// (spec §3.1). Returns the named error for a file the reply could not carry,
/// so an oversized read fails fast instead of stalling the caller until its op
/// timeout.
pub fn copy_file_over_bound(path: &str, len: usize) -> Option<WorkerError> {
    copy_file_over_size(path, len, MAX_COPY_FILE_BYTES)
}

/// The same refusal against a caller-chosen ceiling, for `copy_file_chunk`
/// (design #362 S1): the whole file is measured before any slice is sent, so an
/// over-band archive costs one round trip rather than a truncated read.
pub fn copy_file_over_size(path: &str, len: usize, max_bytes: usize) -> Option<WorkerError> {
    (len > max_bytes).then(|| WorkerError::Other {
        message: types::worker::copy_file_too_large(path, len, max_bytes),
    })
}

/// Ops that just execute a container action. Public because a node-side bound
/// inside one of these ops has to fit within it — a wait the caller has already
/// abandoned fails on transport, so the daemon's own named failure is never seen
/// (design #373 3c).
pub const OP_TIMEOUT: Duration = Duration::from_secs(60);
/// Liveness probe — placement blocks on this, keep it tight.
const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// Self-refresh: the daemon accepts fast and builds/swaps in the background
/// (spec §3.1), so this only covers the accept round-trip, not the build.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);
/// Capacity command (spec §3.1 operator capacity control): the daemon validates
/// against its ceiling and answers out of memory, so a slow reply means an
/// unhealthy node rather than a busy one. Kept tighter than [`OP_TIMEOUT`] — the
/// operator is waiting on the fleet view, and every wait is bounded.
const CAPACITY_TIMEOUT: Duration = Duration::from_secs(5);

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

    /// One [`MAX_COPY_FILE_BYTES`] slice of a container file from `offset`
    /// (design #362 S1). A whole file over `max_bytes` comes back as the
    /// [`COPY_FILE_TOO_LARGE`] refusal instead of a slice.
    pub async fn copy_file_chunk(
        &self,
        id: &str,
        path: &str,
        offset: u64,
        max_bytes: u64,
    ) -> std::result::Result<CopyFileChunkOk, WorkerRpcError> {
        self.call(
            "copy_file_chunk",
            &CopyFileChunkRequest {
                id: id.into(),
                path: path.into(),
                offset,
                max_bytes,
            },
            OP_TIMEOUT,
        )
        .await
    }

    /// Which files under `dir` are named `name` (design #490 D1a). The scan is
    /// node-local and only the resolved paths cross the wire, so the bytes ride
    /// [`Self::copy_file_chunk`] afterwards rather than this reply.
    pub async fn find_file(
        &self,
        id: &str,
        dir: &str,
        name: &str,
    ) -> std::result::Result<FindFileOk, WorkerRpcError> {
        self.call(
            "find_file",
            &FindFileRequest {
                id: id.into(),
                dir: dir.into(),
                name: name.into(),
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

    /// Command the node's slot count (spec §3.1 operator capacity control). The
    /// node is the authority: a value above its `slots_max` comes back as a
    /// *rejection* (`accepted: false` with a reason), not an error, and the reply
    /// reports the capacity in force afterwards either way. Never a placement
    /// input — intent is only ever pushed here, never scheduled on.
    pub async fn set_slots(
        &self,
        req: &SetSlotsRequest,
    ) -> std::result::Result<SetSlotsOk, WorkerRpcError> {
        self.call("set_slots", req, CAPACITY_TIMEOUT).await
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
#[allow(
    clippy::expect_used,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The `copy_file` bound exists for exactly one reason: a reply carrying a
    /// file at the bound must still be publishable under NATS's default 1MB
    /// `max_payload`, base64 and JSON envelope included.
    #[test]
    fn copy_file_bound_keeps_the_reply_publishable() {
        const NATS_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
        let reply: WorkerReply<CopyFileOk> = WorkerReply::Ok {
            value: CopyFileOk {
                data_b64: Some(types::worker::b64_encode(&vec![0u8; MAX_COPY_FILE_BYTES])),
            },
        };
        let encoded = serde_json::to_vec(&reply).unwrap().len();
        assert!(
            encoded <= MAX_REQUEST_BYTES,
            "a reply at the bound is {encoded} bytes, over the {MAX_REQUEST_BYTES}-byte guard"
        );
        assert!(
            encoded < NATS_MAX_PAYLOAD_BYTES,
            "a reply at the bound is {encoded} bytes and cannot be published"
        );
    }

    /// The bound refuses rather than truncates, and the refusal is diagnosable:
    /// a partial file is worthless where a partial log tail is not, so the
    /// message names the marker, the path, the size and the bound.
    #[test]
    fn copy_file_over_bound_refuses_with_a_diagnosable_error() {
        assert_eq!(copy_file_over_bound("/workspace/eval-result.json", 0), None);
        assert_eq!(
            copy_file_over_bound("/workspace/eval-result.json", MAX_COPY_FILE_BYTES),
            None,
            "a file AT the bound still fits"
        );

        let over = MAX_COPY_FILE_BYTES + 1;
        let Some(WorkerError::Other { message }) =
            copy_file_over_bound("/workspace/eval-result.json", over)
        else {
            panic!("a file over the bound must be refused as WorkerError::Other");
        };
        for expected in [
            COPY_FILE_TOO_LARGE,
            "/workspace/eval-result.json",
            &over.to_string(),
            &MAX_COPY_FILE_BYTES.to_string(),
        ] {
            assert!(message.contains(expected), "missing {expected}: {message}");
        }
    }

    /// `Other` round-trips on an N-1 dispatcher, which is why the refusal is
    /// carried in it rather than in a new [`WorkerError`] variant: the enum has
    /// no `#[serde(other)]` fallback, so a new variant would fail to decode.
    #[test]
    fn copy_file_refusal_decodes_as_a_pre_existing_variant() {
        let error = copy_file_over_bound("/big", MAX_COPY_FILE_BYTES + 1).unwrap();
        let reply: WorkerReply<CopyFileOk> = WorkerReply::Err { error };
        let json = serde_json::to_string(&reply).unwrap();
        assert!(json.contains("\"kind\":\"other\""), "{json}");
        assert_eq!(
            serde_json::from_str::<WorkerReply<CopyFileOk>>(&json).unwrap(),
            reply
        );
    }

    /// The output archive's ceiling (design #362) is checked at the boundary,
    /// not near it: 16 MiB exactly is stored, one byte more is refused. A
    /// truncated archive carries nothing, so this is a refusal and never a cut.
    #[test]
    fn output_ceiling_refuses_only_past_the_boundary() {
        let max = crate::MAX_BLOB_BYTES;
        let path = "/workspace/chug-output.tar.gz";
        assert_eq!(copy_file_over_size(path, max - 1, max), None);
        assert_eq!(copy_file_over_size(path, max, max), None, "at the cap fits");

        let Some(WorkerError::Other { message }) = copy_file_over_size(path, max + 1, max) else {
            panic!("one byte past the cap must be refused as WorkerError::Other");
        };
        for expected in [COPY_FILE_TOO_LARGE, path, &(max + 1).to_string()] {
            assert!(message.contains(expected), "missing {expected}: {message}");
        }
    }

    #[test]
    fn op_parses_from_subject() {
        assert_eq!(
            op_from_subject("req.worker.nuc.copy_file_chunk"),
            Some("copy_file_chunk")
        );
        assert_eq!(op_from_subject("req.worker.nuc.launch"), Some("launch"));
        assert_eq!(
            op_from_subject("req.worker.nuc.copy_file"),
            Some("copy_file")
        );
        assert_eq!(op_from_subject(""), None);
    }
}

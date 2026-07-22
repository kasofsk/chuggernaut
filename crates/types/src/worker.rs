//! Wire types for the worker-node protocol (spec §3.1): the dispatcher
//! proxies `ContainerBackend` operations to a `chuggernaut worker` daemon
//! over NATS request-reply on `req.worker.{node}.{op}` subjects.
//!
//! The launch request is a SMALL message by design: static artifacts (the
//! channel MCP binary, agent images) are provisioned node-locally at deploy
//! time and referenced by name (`FileSource::LocalArtifact`); only small
//! dynamic per-job files (prompt, credentials, harness config) ride inline.
//! Everything must fit NATS's default 1MB max_payload — the store layer
//! enforces a size guard before sending.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Well-known node-local artifact name for the channel MCP binary.
pub const ARTIFACT_CHANNEL: &str = "channel";

/// Base64 (standard alphabet, padded) — small binary fields on the wire.
pub fn b64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| format!("base64: {e}"))
}

/// Where an injected file's bytes come from on the worker side.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileSource {
    /// Bytes carried inline (base64) — prompts, credentials, harness config.
    Inline { data_b64: String },
    /// A static artifact the worker holds locally (e.g. `"channel"`); the
    /// worker substitutes its own copy. Unknown names fail the launch.
    LocalArtifact { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireFile {
    pub container_path: String,
    pub mode: u32,
    pub source: FileSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerLaunchRequest {
    pub image: String,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    pub files: Vec<WireFile>,
    pub cpu_limit: Option<f64>,
    pub memory_limit: Option<String>,
}

/// Mirrors `container::BackendError` so the proxy round-trips errors
/// losslessly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerError {
    NotFound {
        id: String,
    },
    Unavailable {
        message: String,
    },
    Launch {
        message: String,
    },
    /// Placement found no free slot (spec §3.1/§3.5): transient, queued and
    /// retried by the dispatcher rather than failing the task.
    NoCapacity {
        message: String,
    },
    Other {
        message: String,
    },
}

/// Reply envelope for every worker op.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum WorkerReply<T> {
    Ok { value: T },
    Err { error: WorkerError },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchOk {
    /// Full `{node}/{docker_id}` container id.
    pub id: String,
}

/// Payload for kill / inspect / logs; `copy_file` uses [`CopyFileRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContainerRef {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CopyFileRequest {
    pub id: String,
    pub path: String,
}

/// Payload for `logs_tail`: cursor-paged live output (spec §4.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogsTailRequest {
    pub id: String,
    /// Byte cursor into the captured log; 0 reads from the start.
    pub since: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WireStatus {
    Running,
    Exited { exit_code: i32 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectOk {
    /// `None` — container unknown to the node.
    pub status: Option<WireStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CopyFileOk {
    /// `None` — path not present in the container.
    pub data_b64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogsOk {
    pub data_b64: String,
    /// Set when the worker tailed the logs to fit the payload cap.
    pub truncated: bool,
}

/// Reply for `logs_tail`: a cursor page of live output. The worker already
/// capped the chunk (`container::MAX_LOG_TAIL`), so `data_b64` fits the reply
/// under `max_payload`; `offset` is the next cursor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogsTailOk {
    pub offset: u64,
    pub data_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListExitedOk {
    /// `{node}/{docker_id}` ids of exited managed containers on the node.
    pub ids: Vec<String>,
}

/// Payload for `refresh` (spec §3.1 worker self-refresh): rebuild this node's
/// images at `sha` and swap the daemon. The node fetches the build context
/// itself (git archive over the existing ssh front) — the dispatcher ships no
/// bytes, keeping the message small.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshRequest {
    /// Target git SHA to build the three node images at.
    pub sha: String,
    /// Image tag to build/run (e.g. `prod`).
    pub tag: String,
}

/// Reply for `refresh`: the daemon accepted the request and reports the version
/// it is refreshing *from*. The new version arrives via a later `ping` once the
/// build completes and the daemon has swapped — the dispatcher's version-drift
/// warning clears then (spec §3.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshOk {
    /// The daemon began refreshing (false → a refresh was already in progress).
    pub accepted: bool,
    /// The version the node is refreshing away from.
    pub from_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PingOk {
    /// Running `chuggernaut.managed` containers on the node (slot accounting).
    pub running: u32,
    /// Worker build version (+ git SHA when baked) — the dispatcher warns on
    /// mismatch with its own; never refuses.
    pub version: String,
    /// Local artifact name → sha256 hex, for operator forensics (arches
    /// differ across the fleet, so hashes are informational, not compared).
    pub artifacts: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_request_round_trips() {
        let req = WorkerLaunchRequest {
            image: "chuggernaut/agent-rust:prod".into(),
            cmd: vec!["sh".into(), "-c".into(), "true".into()],
            env: HashMap::from([("JOB_ID".into(), "7".into())]),
            files: vec![
                WireFile {
                    container_path: "/chuggernaut/prompt.md".into(),
                    mode: 0o644,
                    source: FileSource::Inline {
                        data_b64: b64_encode(b"do the thing"),
                    },
                },
                WireFile {
                    container_path: "/usr/local/bin/chuggernaut-channel".into(),
                    mode: 0o755,
                    source: FileSource::LocalArtifact {
                        name: ARTIFACT_CHANNEL.into(),
                    },
                },
            ],
            cpu_limit: Some(3.0),
            memory_limit: Some("5Gi".into()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkerLaunchRequest>(&json).unwrap(),
            req
        );
        // The artifact reference carries no bytes.
        assert!(!json.contains("data_b64\":\"\""));
        assert!(json.contains("local_artifact"));
    }

    #[test]
    fn reply_envelope_tags() {
        let ok: WorkerReply<LaunchOk> = WorkerReply::Ok {
            value: LaunchOk {
                id: "nuc/abc".into(),
            },
        };
        let json = serde_json::to_string(&ok).unwrap();
        assert!(json.contains("\"result\":\"ok\""), "{json}");

        let err: WorkerReply<LaunchOk> = WorkerReply::Err {
            error: WorkerError::Launch {
                message: "no such image".into(),
            },
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"result\":\"err\""), "{json}");
        let back: WorkerReply<LaunchOk> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, err);
    }

    #[test]
    fn b64_round_trip() {
        let data = vec![0u8, 1, 2, 255, 254];
        assert_eq!(b64_decode(&b64_encode(&data)).unwrap(), data);
        assert!(b64_decode("not!!base64").is_err());
    }

    #[test]
    fn status_serde() {
        let s = WireStatus::Exited { exit_code: 137 };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<WireStatus>(&json).unwrap(), s);
    }
}

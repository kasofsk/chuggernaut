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

use chrono::{DateTime, Utc};
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

/// A running managed container tagged with its owning task — the wire mirror of
/// `container::RunningContainer` for the §3.6 fleet sweep proxied over a worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireRunningContainer {
    /// `{node}/{docker_id}` id.
    pub id: String,
    pub project: Option<String>,
    pub job: Option<u64>,
    pub task: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListRunningOk {
    pub containers: Vec<WireRunningContainer>,
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
    /// The daemon began refreshing (false → a refresh was already in progress,
    /// or was skipped — see `skipped`).
    pub accepted: bool,
    /// Set when the node could not even *attempt* a refresh because it has no
    /// git credential to fetch the build context (spec §3.1): `WORKER_REFRESH_GIT_URL`
    /// unset or its key file missing. The daemon reports the skip in the reply
    /// (rather than accepting and no-oping silently in the background) so a
    /// deploy surfaces it LOUDLY instead of a 41s "success" that refreshed
    /// nothing. `None` on a normal accept or already-in-progress reply.
    /// `#[serde(default)]` keeps a pre-field daemon's reply decodable.
    #[serde(default)]
    pub skipped: Option<String>,
    /// The version the node is refreshing away from.
    pub from_version: String,
}

/// Payload for `refresh_cancel` (spec §3.1, ticket #254): abort the in-flight
/// refresh to `sha` on this node. The deploy fans refreshes out to every node at
/// once, so the moment ONE node fails there is nothing left to win by letting
/// the others burn ten more minutes of build against a deploy that is already
/// failing — it cancels them instead.
///
/// The SHA is part of the request, not decoration: a node converging on some
/// OTHER target (a concurrent deploy, a hand-run refresh) must never be aborted
/// by this deploy's cleanup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshCancelRequest {
    /// Target SHA of the refresh to cancel. A mismatch is a no-op.
    pub sha: String,
}

/// Reply for `refresh_cancel`. Never an error for "nothing to cancel": a cancel
/// races the very build it aborts, so "no refresh in flight" and "already past
/// the swap" are ordinary outcomes the caller reports, not failures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshCancelOk {
    /// True only when the refresh was still cancellable and has been aborted —
    /// i.e. the node did NOT swap onto the new images. False leaves the node's
    /// state to the deploy's other signals (`ping`, the fleet snapshot).
    pub cancelled: bool,
    /// Why not, when `cancelled` is false (no refresh in flight, a different
    /// target SHA, or the swap window already opened). Short, bounded, and
    /// meant for the deploy leg's `detail` — it is what records that a node
    /// stayed swapped ahead of a failed deploy.
    pub note: String,
}

/// Stage string a [`RefreshResult::Failed`] carries when the refresh ended
/// because the deploy CANCELLED it (ticket #254) rather than because the build
/// broke. A contract between the daemon (which stamps it) and the deploy, which
/// reads the stage back off `ping` and reports "FAILED at cancelled" — so a
/// cancelled node is never misread as a node whose build was broken. That
/// reader is generic over stage strings, so the value itself is pinned by
/// `worker`'s `refresh_cancel_aborts_only_its_own_sha` tier-2 test rather than
/// by a comparison against this constant.
pub const REFRESH_STAGE_CANCELLED: &str = "cancelled";

/// A worker daemon's announce/heartbeat (spec §3.1 dynamic registration).
/// Published periodically by `chuggernaut worker` on [`crate::worker`]'s announce
/// subject; the dispatcher merges it into the live fleet without a restart. It is
/// a plain fire-and-forget publish (no reply) — a missed one is covered by the
/// next heartbeat, and losing the heartbeat stream is what marks the node
/// unschedulable. The node advertises its *own* capacity (`slots`); the live
/// announcement wins over any static `DOCKER_NODES` seed for the same name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerAnnounce {
    /// Node name — subject-safe, and matches the `req.worker.{node}.>` the same
    /// daemon serves its RPCs on.
    pub node: String,
    /// Concurrent-container capacity the node advertises for itself (`WORKER_SLOTS`).
    pub slots: u32,
    /// Worker build version (+ git SHA when baked), same string `ping` reports.
    pub version: String,
}

/// How a worker's most recent self-refresh ended (spec §3.1, ticket #187). The
/// daemon records this when a refresh completes and reports it in `ping` so a
/// FAILED refresh becomes durable, queryable platform state (the fleet snapshot)
/// instead of a node-local `tracing::error` that only the node's own logs hold.
/// A successful refresh swaps the daemon away, so in practice this surfaces
/// failures — the swapped-in daemon reports the new `version` instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RefreshResult {
    /// Accepted and still building/swapping — no terminal verdict yet.
    InProgress,
    /// The refresh built and swapped cleanly.
    Ok,
    /// The refresh failed before the swap; the old daemon keeps running.
    Failed {
        /// Which stage failed: `build`, `drain`, or `swap`.
        stage: String,
        /// A short tail of the failure detail for the operator.
        error_tail: String,
    },
}

/// The last refresh outcome a worker daemon reports (spec §3.1, ticket #187).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RefreshOutcome {
    /// When the daemon accepted the refresh request.
    pub accepted_at: DateTime<Utc>,
    /// When it reached a terminal verdict; `None` while `InProgress`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// The verdict.
    pub result: RefreshResult,
    /// Version the node was refreshing away from.
    pub from_sha: String,
    /// Target SHA of the refresh.
    pub to_sha: String,
}

/// Live progress of an in-flight self-refresh (ticket #253). The daemon fills
/// this from `worker-refresh.sh`'s own phase markers as the script runs and
/// reports it in `ping`, so the deploy's wait loop can RELAY per-phase progress
/// into the deploy job's task output instead of sitting silent for the whole
/// build window. It is live state, not a verdict — [`RefreshOutcome`] stays the
/// durable record of how a refresh ended.
///
/// Everything here is bounded: `phase` is one short marker line and `recent`
/// keeps only the last few output lines, so a 15-minute build cannot grow the
/// ping reply (NATS 1MB) as it runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefreshProgress {
    /// Target SHA this progress belongs to. The wait loop checks it before
    /// relaying: an unrelated refresh still converging on the node must never be
    /// reported as progress on *our* deploy's request.
    pub to_sha: String,
    /// Current phase marker, e.g. `build-image 3/3 agent-rust`.
    pub phase: String,
    /// Seconds the node has been in this phase. Measured on the NODE (against
    /// its own monotonic clock) so it carries no cross-host clock skew.
    pub phase_secs: u64,
    /// The most recent script output lines, oldest first — what a leg that never
    /// confirms leaves behind for the operator (ticket #253). Bounded by the
    /// daemon; `#[serde(default)]` keeps a pre-field daemon's ping decodable.
    #[serde(default)]
    pub recent: Vec<String>,
}

/// How often the wait loop repeats an unchanged phase (ticket #253). A long
/// build must never go silent: without a heartbeat, "still compiling" and "hung"
/// look identical from the deploy log.
pub const REFRESH_HEARTBEAT_SECS: u64 = 30;

/// What the wait loop has already relayed, so the next poll can tell a phase
/// CHANGE from a heartbeat from silence. Owned by the caller and threaded
/// through [`RefreshProgress::relay`]; `Default` is "nothing relayed yet".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefreshRelayState {
    /// The phase the last relayed line named; `None` before the first line.
    pub phase: Option<String>,
    /// Refresh-elapsed seconds at which that line was relayed.
    pub at_secs: u64,
}

impl RefreshProgress {
    /// Decide the ONE line (if any) the wait loop should print for this poll,
    /// and record it in `state`. A new phase always relays; an unchanged phase
    /// relays again once `heartbeat_secs` have passed since the last line, so a
    /// slow leg keeps ticking with its elapsed time. Progress belonging to a
    /// different target SHA is ignored.
    ///
    /// `elapsed_secs` is the CALLER's elapsed time since it requested the
    /// refresh (its own monotonic clock) — the deploy log's "how long has this
    /// leg been running" — while `phase_secs` comes from the node.
    ///
    /// Pure over its inputs so the relay cadence is unit-tested without NATS, a
    /// daemon, or a fifteen-minute wait.
    pub fn relay(
        &self,
        target_sha: &str,
        state: &mut RefreshRelayState,
        elapsed_secs: u64,
        heartbeat_secs: u64,
    ) -> Option<String> {
        if self.to_sha != target_sha {
            return None;
        }
        let changed = state.phase.as_deref() != Some(self.phase.as_str());
        if !changed && elapsed_secs.saturating_sub(state.at_secs) < heartbeat_secs {
            return None;
        }
        state.phase = Some(self.phase.clone());
        state.at_secs = elapsed_secs;
        Some(if changed {
            format!("phase={}, {elapsed_secs}s elapsed", self.phase)
        } else {
            format!(
                "still phase={} ({}s in phase), {elapsed_secs}s elapsed",
                self.phase, self.phase_secs
            )
        })
    }
}

/// What `admin worker-refresh --wait-secs` should conclude from a node's latest
/// `ping` while waiting for a refresh to land (ticket #187). Pure over its
/// inputs so the wait loop's decision is unit-tested without NATS or a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshConfirmation {
    /// The refresh landed: the node reports the target SHA (the swapped-in
    /// daemon's `version` carries it), or its outcome says `Ok`.
    Confirmed,
    /// The node reports a failed refresh for this target — surface the stage and
    /// error rather than waiting out the timeout.
    Failed { stage: String, error_tail: String },
    /// No verdict yet — keep waiting.
    Pending,
}

impl RefreshOutcome {
    /// Decide whether a refresh to `target_sha` is confirmed, given the node's
    /// reported `version` string and its latest refresh `outcome`. A version
    /// carrying the target SHA (the swapped-in daemon) confirms; otherwise a
    /// terminal outcome for this same target reports Ok/Failed; anything else is
    /// still pending. Reads "the same reported field" the fleet snapshot shows.
    pub fn confirm(
        target_sha: &str,
        version: &str,
        outcome: Option<&RefreshOutcome>,
    ) -> RefreshConfirmation {
        // The swapped-in daemon reports `{pkg}+{sha}` — a `+{short_sha}` needle
        // proves the swap landed even before any outcome field would.
        let needle = format!("+{}", &target_sha[..target_sha.len().min(12)]);
        if version.contains(&needle) {
            return RefreshConfirmation::Confirmed;
        }
        match outcome {
            Some(o) if o.to_sha == target_sha => match &o.result {
                RefreshResult::Ok => RefreshConfirmation::Confirmed,
                RefreshResult::Failed { stage, error_tail } => RefreshConfirmation::Failed {
                    stage: stage.clone(),
                    error_tail: error_tail.clone(),
                },
                RefreshResult::InProgress => RefreshConfirmation::Pending,
            },
            _ => RefreshConfirmation::Pending,
        }
    }
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
    /// The node's last self-refresh outcome (ticket #187), so a failed refresh
    /// is durable platform state. `#[serde(default)]` keeps a pre-field daemon's
    /// ping decodable (old daemons omit it → `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_outcome: Option<RefreshOutcome>,
    /// Live progress of the refresh currently running on the node (ticket
    /// #253), for the deploy's wait loop to relay. `None` when no refresh is in
    /// flight, and on a pre-field daemon (`#[serde(default)]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_progress: Option<RefreshProgress>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
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

    /// A ping carrying a refresh outcome round-trips, and a ping from an older
    /// daemon — no `refresh_outcome` field — still decodes (back-compat).
    #[test]
    fn ping_refresh_outcome_round_trip_and_back_compat() {
        let ping = PingOk {
            running: 2,
            version: "0.1.0+abc123".into(),
            artifacts: HashMap::from([("channel".into(), "deadbeef".into())]),
            refresh_outcome: Some(RefreshOutcome {
                accepted_at: chrono::Utc::now(),
                finished_at: Some(chrono::Utc::now()),
                result: RefreshResult::Failed {
                    stage: "build".into(),
                    error_tail: "cargo build exited 101".into(),
                },
                from_sha: "old000".into(),
                to_sha: "new111".into(),
            }),
            refresh_progress: Some(RefreshProgress {
                to_sha: "new111".into(),
                phase: "build-image 3/3 agent-rust".into(),
                phase_secs: 240,
                recent: vec!["worker-refresh: phase build-image 3/3 agent-rust".into()],
            }),
        };
        let json = serde_json::to_string(&ping).unwrap();
        assert_eq!(serde_json::from_str::<PingOk>(&json).unwrap(), ping);

        // Old daemon: no refresh_outcome / refresh_progress keys at all.
        let old = r#"{"running":0,"version":"0.1.0","artifacts":{}}"#;
        let back: PingOk = serde_json::from_str(old).unwrap();
        assert_eq!(back.refresh_outcome, None);
        assert_eq!(back.refresh_progress, None);
    }

    /// The relay cadence (ticket #253): a phase CHANGE always prints, an
    /// unchanged phase prints again only once the heartbeat is due, and progress
    /// for a different target SHA is never relayed. This is what turns the
    /// silent 15-minute refresh window into a ticking deploy log.
    #[test]
    fn refresh_progress_relays_phase_changes_and_heartbeats() {
        let mut state = RefreshRelayState::default();
        let p = RefreshProgress {
            to_sha: "target".into(),
            phase: "build-image 1/3 worker".into(),
            phase_secs: 5,
            recent: vec![],
        };

        // First sighting of a phase always relays, with the caller's elapsed.
        assert_eq!(
            p.relay("target", &mut state, 12, REFRESH_HEARTBEAT_SECS)
                .as_deref(),
            Some("phase=build-image 1/3 worker, 12s elapsed")
        );
        // Same phase, heartbeat not yet due ⇒ silent (no log spam every poll).
        assert_eq!(
            p.relay("target", &mut state, 20, REFRESH_HEARTBEAT_SECS),
            None
        );
        assert_eq!(
            p.relay("target", &mut state, 41, REFRESH_HEARTBEAT_SECS),
            None
        );
        // Heartbeat due ⇒ a "still" line carrying both clocks. A wait with zero
        // progress lines is then itself signal.
        assert_eq!(
            p.relay("target", &mut state, 42, REFRESH_HEARTBEAT_SECS)
                .as_deref(),
            Some("still phase=build-image 1/3 worker (5s in phase), 42s elapsed")
        );

        // A new phase relays immediately, without waiting out the heartbeat.
        let next = RefreshProgress {
            phase: "build-image 2/3 agent".into(),
            ..p.clone()
        };
        assert_eq!(
            next.relay("target", &mut state, 43, REFRESH_HEARTBEAT_SECS)
                .as_deref(),
            Some("phase=build-image 2/3 agent, 43s elapsed")
        );

        // Progress for some OTHER refresh is never relayed, and leaves the relay
        // state untouched so our own next line is still correct.
        let mut fresh = RefreshRelayState::default();
        assert_eq!(
            p.relay("other", &mut fresh, 12, REFRESH_HEARTBEAT_SECS),
            None
        );
        assert_eq!(fresh, RefreshRelayState::default());
    }

    /// The `--wait-secs` confirmation decision (ticket #187): a version carrying
    /// the target SHA confirms; a matching failed outcome reports stage/error; a
    /// matching ok outcome confirms; an in-progress or mismatched outcome is
    /// pending.
    #[test]
    fn refresh_confirmation_decision() {
        // The swapped-in daemon reports the target SHA in its version → confirmed.
        assert_eq!(
            RefreshOutcome::confirm("abc123def456", "0.1.0+abc123def456", None),
            RefreshConfirmation::Confirmed
        );

        // A failed outcome for this target reports stage/error rather than waiting.
        let failed = RefreshOutcome {
            accepted_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
            result: RefreshResult::Failed {
                stage: "swap".into(),
                error_tail: "boom".into(),
            },
            from_sha: "old".into(),
            to_sha: "target".into(),
        };
        assert_eq!(
            RefreshOutcome::confirm("target", "0.1.0", Some(&failed)),
            RefreshConfirmation::Failed {
                stage: "swap".into(),
                error_tail: "boom".into()
            }
        );

        // An ok outcome for this target confirms.
        let ok = RefreshOutcome {
            result: RefreshResult::Ok,
            ..failed.clone()
        };
        assert_eq!(
            RefreshOutcome::confirm("target", "0.1.0", Some(&ok)),
            RefreshConfirmation::Confirmed
        );

        // Still building, or an outcome for a different target → pending.
        let in_progress = RefreshOutcome {
            result: RefreshResult::InProgress,
            finished_at: None,
            ..failed.clone()
        };
        assert_eq!(
            RefreshOutcome::confirm("target", "0.1.0", Some(&in_progress)),
            RefreshConfirmation::Pending
        );
        assert_eq!(
            RefreshOutcome::confirm("other", "0.1.0", Some(&failed)),
            RefreshConfirmation::Pending
        );
    }
}

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
///
/// This is the **push** half of the one capacity source; [`PingOk`] is the pull
/// half, carrying the same fields from the same owner (spec §3.1 slot source).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerAnnounce {
    /// Node name — subject-safe, and matches the `req.worker.{node}.>` the same
    /// daemon serves its RPCs on.
    pub node: String,
    /// Concurrent-container capacity the node advertises for itself. First-boot
    /// value `WORKER_SLOTS`; changed at runtime by `set_slots`
    /// ([`SetSlotsRequest`]), never by anything dispatcher-side.
    pub slots: u32,
    /// The ceiling the node will adopt (`WORKER_SLOTS_MAX`, default the node's
    /// CPU count): advisory to the UI, which bounds its stepper by it, and
    /// enforced **only** at the daemon — the enforcement point is the only place
    /// that knows what the node can serve. `#[serde(default)]` keeps a pre-field
    /// daemon's announce decodable through the version-skew window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots_max: Option<u32>,
    /// Ordering key, first half: unix **milliseconds** stamped once at daemon
    /// start, from the node's own clock. It advances on every daemon restart, so
    /// a restarted daemon's generation-0 reports are accepted rather than
    /// discarded against the dispatcher's watermark.
    ///
    /// Milliseconds, not seconds (a deliberate deviation from design #293 §1):
    /// a crash-looping daemon under `--restart=always` can restart twice inside
    /// one second, and an equal epoch with the generation back at 0 would have
    /// its announces discarded until something pulled a ping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_epoch: Option<u64>,
    /// Ordering key, second half: a counter from 0 the daemon bumps on every
    /// adoption. The dispatcher compares the pair
    /// `(capacity_epoch, capacity_generation)` lexicographically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_generation: Option<u64>,
    /// Worker build version (+ git SHA when baked), same string `ping` reports.
    pub version: String,
}

/// Payload for `set_slots` (spec §3.1 operator capacity control): the operator's
/// desired slot count, relayed to the node by the dispatcher. The node is the
/// authority — the value is a *request*, validated against the node's own
/// `slots_max` — so this carries no ceiling and no override flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetSlotsRequest {
    /// Desired concurrent-container capacity. `0` is a full drain (the node
    /// finishes what it holds and takes nothing new), not an error.
    pub slots: u32,
}

/// Reply for `set_slots`. A value above the node's ceiling is a **rejection**,
/// not an error: the caller asked a legitimate question and the node answered
/// it, so `note` carries the reason for the operator UI to show, and the
/// dispatcher treats a rejection as terminal (it stops re-pushing a number the
/// node refused). The capacity fields report the node's state **after** the
/// decision, so the caller needs no follow-up ping to learn what is in force.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetSlotsOk {
    /// The node adopted the requested value (and bumped its generation).
    pub accepted: bool,
    /// The slot count now in force — the requested one on an accept, the
    /// unchanged previous one on a rejection.
    pub slots: u32,
    /// The node's ceiling, so a rejected caller can bound its next request.
    pub slots_max: u32,
    /// Ordering key as in [`WorkerAnnounce::capacity_epoch`].
    pub capacity_epoch: u64,
    /// Ordering key as in [`WorkerAnnounce::capacity_generation`]; unchanged by
    /// a rejection, since nothing was adopted.
    pub capacity_generation: u64,
    /// Why the value was refused, when `accepted` is false. Short and meant for
    /// display beside the node's slot widget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
    /// The node's current capacity — the same field from the same owner as
    /// [`WorkerAnnounce::slots`], delivered over the **pull** transport (spec
    /// §3.1 slot source). A ping cannot be a stale in-flight message, so the
    /// dispatcher applies it unconditionally; that is what stops any ordering
    /// anomaly from permanently freezing a node's capacity. `None` on a
    /// pre-field daemon, which supplies no capacity at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots: Option<u32>,
    /// The node's ceiling, as [`WorkerAnnounce::slots_max`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots_max: Option<u32>,
    /// Ordering key, as [`WorkerAnnounce::capacity_epoch`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_epoch: Option<u64>,
    /// Ordering key, as [`WorkerAnnounce::capacity_generation`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_generation: Option<u64>,
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

/// Where the slot count the scheduler is using came from (design #293 §7/§8).
/// Reported per node on the fleet roster and `fleet.status` so a node running
/// on the boot seed is *visible* rather than indistinguishable from a healthy
/// one — the representation whose absence hid the 2026-07-26 incident for
/// weeks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CapacitySource {
    /// The node's own report, over either transport (spec §3.1 slot source).
    Node,
    /// The `DOCKER_NODES` boot seed: the node has never reported capacity, so
    /// the number in force was never confirmed by the hardware serving it.
    Seed,
}

/// Which transport carried a capacity observation (spec §3.1 slot source). The
/// distinction *is* the ordering rule: an announce is a fire-and-forget publish
/// that can arrive out of order behind a fresher one, while a `ping` reply is
/// request-reply on a connection the dispatcher just opened and therefore
/// cannot be stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityTransport {
    Announce,
    Ping,
}

/// One capacity observation as it reaches the dispatcher, normalized across the
/// two transports (spec §3.1 slot source). Both carry the same field from the
/// same owner, so the dispatcher ingests one shape and orders it by one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityObservation {
    pub slots: u32,
    /// The node's ceiling, when it reported one. Advisory to the UI; enforced
    /// only at the daemon.
    pub slots_max: Option<u32>,
    /// `(capacity_epoch, capacity_generation)`, compared lexicographically. A
    /// pre-field daemon supplies neither, which reads as `(0, 0)`.
    pub mark: (u64, u64),
    pub transport: CapacityTransport,
}

impl CapacityObservation {
    /// The push half: a node's announce heartbeat.
    pub fn from_announce(announce: &WorkerAnnounce) -> Self {
        Self {
            slots: announce.slots,
            slots_max: announce.slots_max,
            mark: (
                announce.capacity_epoch.unwrap_or(0),
                announce.capacity_generation.unwrap_or(0),
            ),
            transport: CapacityTransport::Announce,
        }
    }

    /// The pull half: a `ping` reply. `None` for a pre-field daemon, which
    /// reports no `slots` at all and so supplies no capacity — the silence the
    /// design #293 §8 stale-capacity warning is there to surface, rather than a
    /// zero the dispatcher would schedule on.
    pub fn from_ping(ping: &PingOk) -> Option<Self> {
        Some(Self {
            slots: ping.slots?,
            slots_max: ping.slots_max,
            mark: (
                ping.capacity_epoch.unwrap_or(0),
                ping.capacity_generation.unwrap_or(0),
            ),
            transport: CapacityTransport::Ping,
        })
    }
}

/// The dispatcher's per-node record of what the node has reported (spec §3.1
/// slot source): the `(epoch, generation)` watermark of the last applied
/// observation, the ceiling it named, and when it arrived.
///
/// `Default` is "never observed": watermark `(0, 0)` and no
/// `observed_at` — the state in which the `DOCKER_NODES` seed is still the
/// number in force and the node reports `capacity_source: "seed"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObservedCapacity {
    /// Last applied ordering pair. `(0, 0)` before any observation, which is
    /// also what a pre-field daemon's messages read as — so its unordered
    /// reports apply until a real pair arrives, and never after.
    pub mark: (u64, u64),
    /// The node's ceiling as of the last observation that carried one.
    pub slots_max: Option<u32>,
    /// When the node last reported capacity; `None` ⇒ never observed.
    pub observed_at: Option<DateTime<Utc>>,
}

/// The spec §3.1 ordering rule, pure over the node's watermark and the
/// observation in hand.
///
/// An **announce** applies only when its pair is at least the watermark, so a
/// stale in-flight heartbeat cannot undo a fresher observation; because the
/// epoch advances on every daemon restart, a restarted daemon's generation-0
/// reports still land. A **`ping` reply applies unconditionally** — it is
/// request-reply on a live connection and so cannot be stale. That backstop is
/// load-bearing: it is what stops any ordering anomaly (a backwards clock jump,
/// a constant-epoch daemon, a bug in this rule) from *permanently* freezing a
/// node's capacity, making the failure self-healing at the next placement probe
/// rather than terminal.
pub fn capacity_applies(watermark: (u64, u64), observation: &CapacityObservation) -> bool {
    match observation.transport {
        CapacityTransport::Ping => true,
        CapacityTransport::Announce => observation.mark >= watermark,
    }
}

impl ObservedCapacity {
    /// Ingest one observation under [`capacity_applies`], returning whether it
    /// won and so must be installed as the node's live slot count. An applied
    /// `ping` **resets** the watermark to the pair it carries, even downwards;
    /// an applied announce only ever moves it forward.
    pub fn apply(&mut self, observation: &CapacityObservation, at: DateTime<Utc>) -> bool {
        let before = self.mark;
        if !capacity_applies(before, observation) {
            debug_assert!(
                observation.transport == CapacityTransport::Announce,
                "only an announce may be discarded — a ping is the anti-freeze backstop"
            );
            return false;
        }
        self.mark = observation.mark;
        self.slots_max = observation.slots_max.or(self.slots_max);
        self.observed_at = Some(at);
        debug_assert!(
            observation.transport == CapacityTransport::Ping || self.mark >= before,
            "an applied announce may never move the watermark backwards"
        );
        debug_assert!(
            self.observed_at.is_some(),
            "an applied observation must leave the node observed"
        );
        true
    }

    /// Provenance for the fleet records (design #293 §7/§8): the node's own
    /// report once it has ever made one, the boot seed until then.
    pub fn source(&self) -> CapacitySource {
        match self.observed_at {
            Some(_) => CapacitySource::Node,
            None => CapacitySource::Seed,
        }
    }
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
            slots: Some(4),
            slots_max: Some(8),
            capacity_epoch: Some(1_769_000_000_123),
            capacity_generation: Some(3),
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

        let old = r#"{"running":0,"version":"0.1.0","artifacts":{}}"#;
        let back: PingOk = serde_json::from_str(old).unwrap();
        assert_eq!(back.refresh_outcome, None);
        assert_eq!(back.refresh_progress, None);
    }

    /// Both capacity transports carry the same fields (spec §3.1 slot source),
    /// and a pre-field daemon's messages still decode — with the pair absent, so
    /// the dispatcher reads it as `(0, 0)` and applies a seed only until the
    /// node's first real observation.
    #[test]
    fn capacity_fields_round_trip_on_both_transports() {
        let announce = WorkerAnnounce {
            node: "air".into(),
            slots: 2,
            slots_max: Some(6),
            capacity_epoch: Some(1_769_000_000_123),
            capacity_generation: Some(1),
            version: "0.1.0+abc123".into(),
        };
        let json = serde_json::to_string(&announce).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkerAnnounce>(&json).unwrap(),
            announce
        );
        assert!(json.contains("1769000000123"), "{json}");

        let old: WorkerAnnounce =
            serde_json::from_str(r#"{"node":"air","slots":2,"version":"0.1.0"}"#).unwrap();
        assert_eq!(
            (old.slots_max, old.capacity_epoch, old.capacity_generation),
            (None, None, None)
        );

        let rejected = SetSlotsOk {
            accepted: false,
            slots: 2,
            slots_max: 6,
            capacity_epoch: 1_769_000_000_123,
            capacity_generation: 1,
            note: Some("node max is 6".into()),
        };
        let json = serde_json::to_string(&rejected).unwrap();
        assert_eq!(serde_json::from_str::<SetSlotsOk>(&json).unwrap(), rejected);
        let req = SetSlotsRequest { slots: 0 };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<SetSlotsRequest>(&json).unwrap(), req);
    }

    fn announce_at(slots: u32, epoch: u64, generation: u64) -> CapacityObservation {
        CapacityObservation::from_announce(&WorkerAnnounce {
            node: "air".into(),
            slots,
            slots_max: Some(6),
            capacity_epoch: Some(epoch),
            capacity_generation: Some(generation),
            version: "0.1.0".into(),
        })
    }

    fn ping_at(slots: u32, epoch: u64, generation: u64) -> CapacityObservation {
        CapacityObservation::from_ping(&PingOk {
            running: 0,
            slots: Some(slots),
            slots_max: Some(6),
            capacity_epoch: Some(epoch),
            capacity_generation: Some(generation),
            version: "0.1.0".into(),
            artifacts: HashMap::new(),
            refresh_outcome: None,
            refresh_progress: None,
        })
        .expect("a ping reporting slots is an observation")
    }

    /// The spec §3.1 ordering rule, case by case. Each of these is a failure
    /// mode design #293 §1 names by hand, and the pair exists because a
    /// generation counter alone fails the second one.
    #[test]
    fn capacity_ordering_rule() {
        let now = chrono::Utc::now();
        let mut observed = ObservedCapacity::default();
        assert_eq!(observed.source(), CapacitySource::Seed);
        assert_eq!(observed.observed_at, None);

        assert!(observed.apply(&announce_at(4, 1_000, 3), now));
        assert_eq!(observed.mark, (1_000, 3));
        assert_eq!(observed.source(), CapacitySource::Node);
        assert_eq!(observed.slots_max, Some(6));

        assert!(!observed.apply(&announce_at(2, 1_000, 2), now));
        assert_eq!(
            observed.mark,
            (1_000, 3),
            "watermark untouched by a discard"
        );
        assert!(observed.apply(&announce_at(4, 1_000, 3), now));

        assert!(observed.apply(&announce_at(2, 2_000, 0), now));
        assert_eq!(observed.mark, (2_000, 0));

        assert!(observed.apply(&ping_at(5, 500, 0), now));
        assert_eq!(observed.mark, (500, 0), "a ping resets, it does not merge");
        assert!(observed.apply(&announce_at(5, 500, 1), now));
        assert_eq!(observed.mark, (500, 1));
    }

    /// A pre-field daemon supplies no pair, which reads as `(0, 0)`: its
    /// announces apply while the node has no ordered observation, and never
    /// once one has arrived. A rolled-back node therefore stops supplying
    /// capacity rather than silently winning with an unordered number.
    #[test]
    fn no_epoch_observation_applies_only_before_the_first_ordered_one() {
        let now = chrono::Utc::now();
        let unordered = CapacityObservation::from_announce(&WorkerAnnounce {
            node: "air".into(),
            slots: 2,
            slots_max: None,
            capacity_epoch: None,
            capacity_generation: None,
            version: "0.1.0".into(),
        });
        assert_eq!(unordered.mark, (0, 0));

        let mut observed = ObservedCapacity::default();
        assert!(observed.apply(&unordered, now), "nothing ordered yet");
        assert_eq!(observed.slots_max, None);
        assert!(observed.apply(&unordered, now));

        assert!(observed.apply(&announce_at(4, 1_000, 0), now));
        assert!(!observed.apply(&unordered, now));
        assert_eq!(observed.mark, (1_000, 0));

        let silent = PingOk {
            running: 0,
            slots: None,
            slots_max: None,
            capacity_epoch: None,
            capacity_generation: None,
            version: "0.1.0".into(),
            artifacts: HashMap::new(),
            refresh_outcome: None,
            refresh_progress: None,
        };
        assert_eq!(CapacityObservation::from_ping(&silent), None);
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

        assert_eq!(
            p.relay("target", &mut state, 12, REFRESH_HEARTBEAT_SECS)
                .as_deref(),
            Some("phase=build-image 1/3 worker, 12s elapsed")
        );
        assert_eq!(
            p.relay("target", &mut state, 20, REFRESH_HEARTBEAT_SECS),
            None
        );
        assert_eq!(
            p.relay("target", &mut state, 41, REFRESH_HEARTBEAT_SECS),
            None
        );
        assert_eq!(
            p.relay("target", &mut state, 42, REFRESH_HEARTBEAT_SECS)
                .as_deref(),
            Some("still phase=build-image 1/3 worker (5s in phase), 42s elapsed")
        );

        let next = RefreshProgress {
            phase: "build-image 2/3 agent".into(),
            ..p.clone()
        };
        assert_eq!(
            next.relay("target", &mut state, 43, REFRESH_HEARTBEAT_SECS)
                .as_deref(),
            Some("phase=build-image 2/3 agent, 43s elapsed")
        );

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
        assert_eq!(
            RefreshOutcome::confirm("abc123def456", "0.1.0+abc123def456", None),
            RefreshConfirmation::Confirmed
        );

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

        let ok = RefreshOutcome {
            result: RefreshResult::Ok,
            ..failed.clone()
        };
        assert_eq!(
            RefreshOutcome::confirm("target", "0.1.0", Some(&ok)),
            RefreshConfirmation::Confirmed
        );

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

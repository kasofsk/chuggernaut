//! Platform-level runtime state published for read-only display.
//!
//! The dispatcher's fleet and agent defaults live only in its process
//! environment (spec §12.4) — invisible to the api, which serves the operator
//! UI. To make them visible, the dispatcher writes this snapshot to the
//! `platform` KV bucket at startup (key `dispatcher.config`); the api reads it
//! back for the platform settings page. Read-only today: runtime reconfiguration
//! (add/drain a node without restarting) is a later phase.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One Docker fleet node the dispatcher schedules onto.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkerNode {
    pub name: String,
    /// `unix:///var/run/docker.sock` or `tcp://host:2375`.
    pub endpoint: String,
    /// Max concurrent chuggernaut containers on this node.
    pub slots: u32,
    /// Node health at snapshot time (spec §3.1): `false` when the node was
    /// unreachable and marked out-of-service — placement skips it until it
    /// answers again. Defaults to `true` for snapshots written before this
    /// field existed.
    #[serde(default = "default_available")]
    pub available: bool,
    /// Build version last reported by a worker node's ping (spec §3.1):
    /// `chuggernaut` version + git SHA. `None` for docker-endpoint nodes and
    /// for workers that have not answered yet. Lets the UI show fleet versions
    /// and spot deploy drift after a worker self-refresh.
    #[serde(default)]
    pub version: Option<String>,
    /// The node's last self-refresh outcome (ticket #187), last reported by a
    /// worker's ping. `None` for docker-endpoint nodes and workers that have
    /// not refreshed. A failed refresh is durable platform state here rather
    /// than a node-local `tracing::error`. Defaults to `None` for snapshots
    /// written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_outcome: Option<crate::worker::RefreshOutcome>,
    /// Where [`Self::slots`] came from (design #293 §7): the node's own report
    /// over either transport, or the `DOCKER_NODES` boot seed. `None` for a
    /// docker-endpoint node, whose capacity `DOCKER_NODES` still owns outright.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_source: Option<crate::worker::CapacitySource>,
    /// When the node last reported its capacity; `None` when it never has.
    /// Together with [`Self::capacity_source`] this is the representation whose
    /// absence let a fleet run for weeks on a boot seed nothing had confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_observed_at: Option<DateTime<Utc>>,
}

fn default_available() -> bool {
    true
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn sample() -> DispatcherConfigSnapshot {
        DispatcherConfigSnapshot {
            nodes: vec![WorkerNode {
                name: "local".into(),
                endpoint: "unix:///var/run/docker.sock".into(),
                slots: 4,
                available: true,
                version: Some("0.1.0+abc123".into()),
                refresh_outcome: None,
                capacity_source: None,
                capacity_observed_at: None,
            }],
            agent_provider_default: "claude".into(),
            agent_model_default: None,
            triage_image: None,
            repos_root: "/data/repos".into(),
            repo_url_base: "file:///data/repos".into(),
            nats_url: "nats://localhost:4222".into(),
            nats_url_container: None,
            channel_binary: None,
            hook_bin: None,
            secrets_encryption: true,
            dispatcher_sha: Some("abc123".into()),
            main_tip_sha: Some("def456".into()),
            commits_behind: Some(3),
            placement_policy: "busyness".into(),
            schema_epoch: 1,
        }
    }

    #[test]
    fn new_fields_roundtrip() {
        let snap = sample();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("dispatcher_sha"));
        assert!(json.contains("main_tip_sha"));
        assert!(json.contains("commits_behind"));
        assert!(json.contains("placement_policy"));
        assert!(json.contains("schema_epoch"));
        let back: DispatcherConfigSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    /// A snapshot serialized before the CD/deploy-drift fields existed must
    /// still deserialize (the api reads it back from the platform bucket) — as
    /// must one written *after*, carrying the since-removed `auto_deploy` key
    /// (ticket #276): the bucket holds the last dispatcher's bytes until the new
    /// one republishes, so an unknown field must be ignored, not fatal.
    #[test]
    fn old_snapshot_deserializes() {
        let old = r#"{
            "nodes": [{"name":"local","endpoint":"unix:///var/run/docker.sock","slots":4}],
            "auto_deploy": true,
            "agent_provider_default": "claude",
            "agent_model_default": null,
            "repos_root": "/data/repos",
            "repo_url_base": "file:///data/repos",
            "nats_url": "nats://localhost:4222",
            "nats_url_container": null,
            "channel_binary": null,
            "hook_bin": null,
            "secrets_encryption": true
        }"#;
        let snap: DispatcherConfigSnapshot = serde_json::from_str(old).unwrap();
        assert!(snap.nodes[0].available);
        assert_eq!(snap.nodes[0].version, None);
        assert_eq!(snap.nodes[0].refresh_outcome, None);
        assert_eq!(snap.dispatcher_sha, None);
        assert_eq!(snap.main_tip_sha, None);
        assert_eq!(snap.commits_behind, None);
        assert_eq!(snap.placement_policy, "headroom");
        assert_eq!(snap.schema_epoch, 1);
    }
}

/// A snapshot of the dispatcher's runtime configuration for display. Contains
/// no secrets — only names, endpoints, and resolved paths an operator needs to
/// see. Written by the dispatcher at startup, read by the api.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DispatcherConfigSnapshot {
    /// The Docker fleet (`DOCKER_NODES` / `DOCKER_SLOTS`).
    pub nodes: Vec<WorkerNode>,
    /// `AGENT_PROVIDER_DEFAULT` (`claude` | `codex`).
    pub agent_provider_default: String,
    /// `AGENT_MODEL_DEFAULT`, if set.
    pub agent_model_default: Option<String>,
    /// `TRIAGE_IMAGE` — platform image for operator-dispatched triage agents
    /// (§1.2). None → the triage action is unavailable.
    #[serde(default)]
    pub triage_image: Option<String>,
    /// `REPOS_ROOT` — bare repos on disk.
    pub repos_root: String,
    /// `REPO_URL_BASE` — clone URL base injected into containers.
    pub repo_url_base: String,
    /// The dispatcher's own `NATS_URL`.
    pub nats_url: String,
    /// `NATS_URL_CONTAINER` — the URL injected into agent containers, if it
    /// differs from the dispatcher's own.
    pub nats_url_container: Option<String>,
    /// `CHANNEL_BINARY` path, if the channel MCP is wired.
    pub channel_binary: Option<String>,
    /// `HOOK_BIN` — pre-receive hook binary path as seen from the SSH front.
    pub hook_bin: Option<String>,
    /// Whether the dispatcher loaded the age identity — i.e. secrets are
    /// encrypted at rest rather than injected raw (§8.2 dev mode).
    pub secrets_encryption: bool,
    /// The running dispatcher binary's own build SHA (`CHUG_GIT_SHA`, baked at
    /// build time — the SHA that `version_string()` embeds). `None` for local/
    /// dev builds with no SHA baked in. Compared against `main_tip_sha` to show
    /// whether prod is in sync. Defaults to `None` for snapshots written before
    /// this field existed.
    #[serde(default)]
    pub dispatcher_sha: Option<String>,
    /// Current `main` tip SHA of the platform's own source repo (`SELF_REPO`),
    /// re-resolved each scan tick. `None` when `SELF_REPO` is unset or the tip
    /// can't be resolved. This is the deploy target the running dispatcher is
    /// measured against.
    #[serde(default)]
    pub main_tip_sha: Option<String>,
    /// How many commits `dispatcher_sha` is behind `main_tip_sha`
    /// (`rev-list --count`, cached per tip). `Some(0)` = in sync; `None` when
    /// drift can't be computed (no self repo, or the deployed SHA is absent
    /// from its history).
    #[serde(default)]
    pub commits_behind: Option<u64>,
    /// `PLACEMENT_POLICY` — the active fleet placement policy (`busyness` |
    /// `headroom`, §3.1), so the UI can show how the fleet schedules. Defaults
    /// to `busyness` for snapshots written before this field existed.
    #[serde(default = "default_placement_policy")]
    pub placement_policy: String,
    /// The job-type config schema epoch this dispatcher understands
    /// ([`crate::CONFIG_SCHEMA_EPOCH`], spec §14). Exposed so the merge-time CI
    /// check can compare a config's `min_dispatcher` against the *deployed*
    /// dispatcher and fail a config that would otherwise merge ahead of the
    /// binary. Defaults to `1` (the epoch before this field existed) for older
    /// snapshots.
    #[serde(default = "default_schema_epoch")]
    pub schema_epoch: u32,
}

/// Back-compat default for [`DispatcherConfigSnapshot::schema_epoch`]: `1`, the
/// only epoch that existed before the field was added.
fn default_schema_epoch() -> u32 {
    1
}

/// Back-compat default for [`DispatcherConfigSnapshot::placement_policy`]:
/// snapshots written before the field existed predate the configurable policy,
/// when placement was hardcoded to headroom.
fn default_placement_policy() -> String {
    "headroom".to_string()
}

/// Live fleet occupancy (spec §3.1): which slots on which node are busy and what
/// job/task each busy slot is running. The [`DispatcherConfigSnapshot`]
/// describes the fleet *statically* (names, slot counts, versions); this reports
/// live *usage*, which the config snapshot can't — with more than one node the
/// UI can't otherwise place work on nodes.
///
/// Published by the dispatcher (the single writer) to the `platform` bucket
/// (key `fleet.status`) on every task launch/exit and after restart
/// re-attachment — a full snapshot each change, cheap at our scale. Read back by
/// the api at `GET /api/v1/platform/fleet`. No dedicated event announces the
/// change: every occupancy change coincides with a task lifecycle event
/// (`task-launched`/`task-queued`, task/job state) already on the job-event
/// stream, on which an SSE client refetches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FleetStatus {
    /// One entry per fleet node, whether idle or busy.
    pub nodes: Vec<FleetNode>,
    /// Launches parked waiting for a free slot — the §3.5 launch capacity queue
    /// depth. Best-effort: whatever the dispatcher can surface, `0` when nothing
    /// waits.
    pub queue_depth: u32,
}

/// One fleet node's live occupancy. `name`/`slots`/`available`/`version` mirror
/// [`WorkerNode`]; `occupied`/`running` add the live slot usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FleetNode {
    pub name: String,
    /// Total concurrent-container capacity ([`WorkerNode::slots`]). `None` for a
    /// node observed only through a running container (not in the configured
    /// roster) — its cap is unknown from occupancy alone.
    pub slots: Option<u32>,
    /// Busy slot count (`running.len()`), denormalized so the UI needn't count.
    pub occupied: u32,
    /// Node health at snapshot time (spec §3.1): `false` when out of service.
    pub available: bool,
    /// Build version last reported by a worker node's ping, if any.
    pub version: Option<String>,
    /// The node's last self-refresh outcome (ticket #187), last reported by a
    /// worker's ping — so a failed refresh is visible in the live fleet, not
    /// just the node's logs. `None` when the node has not refreshed (or is a
    /// docker-endpoint node).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_outcome: Option<crate::worker::RefreshOutcome>,
    /// Where [`Self::slots`] came from (design #293 §7/§8): `node` once the node
    /// has reported over either transport, `seed` while the `DOCKER_NODES` boot
    /// value is still standing in for a report that never arrived. `None` for a
    /// docker-endpoint node, whose capacity `DOCKER_NODES` still owns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_source: Option<crate::worker::CapacitySource>,
    /// When the node last reported its capacity; `None` when it never has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_observed_at: Option<DateTime<Utc>>,
    /// The operator's **desired** slot count for this node (design #293 §2), from
    /// the `fleet.capacity` intent record. `None` when no operator has ever set
    /// one. Display only: [`Self::slots`] stays the number the scheduler uses, and
    /// intent is structurally incapable of placing work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots_desired: Option<u32>,
    /// How far intent and observation are apart (design #293 §4/§10). `None` when
    /// there is no intent to reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_state: Option<CapacityState>,
    /// The daemon's reason for refusing the desired value, when it refused one —
    /// shown beside the node's slot widget until the operator changes the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_note: Option<String>,
    /// The occupied slots on this node.
    pub running: Vec<SlotOccupant>,
}

/// Operator intent for worker capacity (design #293 §2), persisted by the
/// dispatcher — the single writer — in the `platform` bucket under
/// `fleet.capacity`, beside `dispatcher.config` and `fleet.status`.
///
/// **Invariant (STYLE.md Tier 2 #2, asserted): no placement path ever reads this
/// record.** It feeds exactly two consumers — the §4 reconciler and the UI's
/// "desired" display — which is the whole resolution of the design's tension:
/// intent is stored so it can be re-asserted after a daemon restart, and is
/// structurally incapable of placing work. The scheduler reads *observed*
/// capacity ([`crate::worker::ObservedCapacity`]) and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FleetCapacity {
    /// Node name → the operator's last request for it. A node with no entry has
    /// no intent, and reconciliation leaves it entirely alone.
    #[serde(default)]
    pub nodes: std::collections::BTreeMap<String, NodeCapacityIntent>,
}

/// One node's operator-set desired capacity, with its audit stamp. Last-writer
/// only — a retained `platform.events` history is a follow-up (design #293 §9),
/// so the dispatcher also logs a structured line per change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NodeCapacityIntent {
    /// Desired concurrent-container capacity. `0` is a full drain, not an error.
    pub slots: u32,
    /// Identity of the platform admin who set it (spec §7.5).
    pub set_by: String,
    /// When they set it.
    pub set_at: DateTime<Utc>,
}

/// How far a node's observed capacity is from the operator's intent (design #293
/// §4/§10). Derived on read from intent + observation + the push ledger; never
/// stored on the intent record, which holds only what the operator asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CapacityState {
    /// The node reports the desired number: nothing left to do.
    Converged,
    /// A push is in flight, or one has just gone out and the node's report has
    /// not caught up yet. Convergence is normally visible within a second.
    Pending,
    /// The node **refused** the value (above its `slots_max`). Terminal: the
    /// dispatcher stops re-pushing a number the node will not take, and this
    /// state stands until the operator changes the request — otherwise a node
    /// whose maximum dropped would be pushed a value it refuses forever.
    Rejected,
    /// Intent recorded, pushed, not refused — and still not observed. The
    /// signature of a daemon that silently ignores `set_slots` (an old build), and
    /// the reason such a node must never read as converged.
    Unacknowledged,
}

/// One node's capacity intent as the fleet snapshot displays it (design #293 §2,
/// consumer 2 of 2). Handed to the occupancy publisher already resolved, so the
/// snapshot composer never touches the intent record itself — the reconciler and
/// this projection are the record's only readers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCapacityDisplay {
    pub slots_desired: u32,
    pub state: CapacityState,
    /// The daemon's rejection reason, when [`Self::state`] is
    /// [`CapacityState::Rejected`].
    pub note: Option<String>,
}

/// The 202 body of a capacity command (design #293 §3): the actor never blocks on
/// the node RPC, so "recorded and converging" is the honest answer. `observed` is
/// what the scheduler is still using at the moment of the reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NodeCapacityAck {
    pub node: String,
    pub desired: u32,
    /// The node's currently-observed slot count; `None` when it has never
    /// reported one.
    pub observed: Option<u32>,
    pub state: CapacityState,
}

/// What one busy slot is running (spec §3.1) — enough for the UI to link back to
/// the job/task without a second fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SlotOccupant {
    /// `owner/project` slug the job belongs to (a job seq is only unique within
    /// a project).
    pub project: String,
    /// Job sequence (`Job::id`).
    pub job_seq: u64,
    /// Task id within the job (`Task::id`).
    pub task_id: u64,
    /// Task phase: `work` | `eval` | `gate` | `wrap_up` | `triage`
    /// (from `TaskPhase`).
    pub task_kind: String,
    /// The job type (`Job::type`).
    pub job_type: String,
    /// Job phase (`JobState`), lowercased: `work`, `evaluation`, `wrap_up`, …
    pub phase: String,
    /// When the container launched (`Task::started_at`), if known.
    pub started_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod fleet_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn fleet_status_roundtrips() {
        let status = FleetStatus {
            nodes: vec![
                FleetNode {
                    name: "air".into(),
                    slots: Some(4),
                    occupied: 1,
                    available: true,
                    version: Some("0.1.0+abc".into()),
                    refresh_outcome: Some(crate::worker::RefreshOutcome {
                        accepted_at: Utc::now(),
                        finished_at: Some(Utc::now()),
                        result: crate::worker::RefreshResult::Failed {
                            stage: "build".into(),
                            error_tail: "cargo build exited 101".into(),
                        },
                        from_sha: "old".into(),
                        to_sha: "new".into(),
                    }),
                    capacity_source: Some(crate::worker::CapacitySource::Node),
                    capacity_observed_at: Some(Utc::now()),
                    slots_desired: Some(4),
                    capacity_state: Some(CapacityState::Converged),
                    capacity_note: None,
                    running: vec![SlotOccupant {
                        project: "acme/api".into(),
                        job_seq: 42,
                        task_id: 3,
                        task_kind: "work".into(),
                        job_type: "implement-endpoint".into(),
                        phase: "work".into(),
                        started_at: Some(Utc::now()),
                    }],
                },
                FleetNode {
                    name: "nuc".into(),
                    slots: Some(2),
                    occupied: 0,
                    available: false,
                    version: None,
                    refresh_outcome: None,
                    capacity_source: Some(crate::worker::CapacitySource::Seed),
                    capacity_observed_at: None,
                    slots_desired: Some(8),
                    capacity_state: Some(CapacityState::Rejected),
                    capacity_note: Some("node max is 2".into()),
                    running: vec![],
                },
            ],
            queue_depth: 2,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains(r#""capacity_source":"seed""#), "{json}");
        assert!(json.contains(r#""capacity_state":"rejected""#), "{json}");
        let back: FleetStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, back);
    }

    /// The intent record is the operator's ask and its audit stamp — nothing
    /// derived (design #293 §2/§9). A snapshot with no `nodes` key at all is an
    /// empty fleet's intent, not a decode failure: the dispatcher writes the key
    /// only once an operator has set something.
    #[test]
    fn fleet_capacity_roundtrips_and_defaults_empty() {
        let at = Utc::now();
        let mut capacity = FleetCapacity::default();
        capacity.nodes.insert(
            "air".into(),
            NodeCapacityIntent {
                slots: 2,
                set_by: "operator@example.com".into(),
                set_at: at,
            },
        );
        let json = serde_json::to_string(&capacity).unwrap();
        assert!(
            json.contains(r#""set_by":"operator@example.com""#),
            "{json}"
        );
        assert_eq!(capacity, serde_json::from_str(&json).unwrap());

        let empty: FleetCapacity = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, FleetCapacity::default());
    }

    /// The default is an empty fleet — what the api serves before the dispatcher
    /// has published anything.
    #[test]
    fn fleet_status_default_is_empty() {
        let status = FleetStatus::default();
        assert!(status.nodes.is_empty());
        assert_eq!(status.queue_depth, 0);
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(status, serde_json::from_str(&json).unwrap());
    }
}

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
}

fn default_available() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DispatcherConfigSnapshot {
        DispatcherConfigSnapshot {
            nodes: vec![WorkerNode {
                name: "local".into(),
                endpoint: "unix:///var/run/docker.sock".into(),
                slots: 4,
                available: true,
                version: Some("0.1.0+abc123".into()),
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
            auto_deploy: None,
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
        assert!(json.contains("auto_deploy"));
        assert!(json.contains("placement_policy"));
        assert!(json.contains("schema_epoch"));
        let back: DispatcherConfigSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    /// A snapshot serialized before the CD/deploy-drift fields existed must
    /// still deserialize (the api reads it back from the platform bucket).
    #[test]
    fn old_snapshot_deserializes() {
        let old = r#"{
            "nodes": [{"name":"local","endpoint":"unix:///var/run/docker.sock","slots":4}],
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
        // Pre-existing optional fields default.
        assert!(snap.nodes[0].available);
        assert_eq!(snap.nodes[0].version, None);
        // New CD fields default to None.
        assert_eq!(snap.dispatcher_sha, None);
        assert_eq!(snap.main_tip_sha, None);
        assert_eq!(snap.commits_behind, None);
        assert_eq!(snap.auto_deploy, None);
        // Snapshots predating the configurable policy default to the old
        // hardcoded behavior (headroom), not the new busyness default.
        assert_eq!(snap.placement_policy, "headroom");
        // Snapshots predating the schema-epoch field default to epoch 1.
        assert_eq!(snap.schema_epoch, 1);
    }
}

/// A snapshot of the dispatcher's runtime configuration for display. Contains
/// no secrets — only names, endpoints, and resolved paths an operator needs to
/// see. Written by the dispatcher at startup, read by the api.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// CD auto-deploy posture, populated once the CD engine lands (`Some(true)`
    /// = deploys land automatically, `Some(false)` = manual). `None` until then.
    #[serde(default)]
    pub auto_deploy: Option<bool>,
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
    /// The occupied slots on this node.
    pub running: Vec<SlotOccupant>,
}

/// What one busy slot is running (spec §3.1) — enough for the UI to link back to
/// the job/task without a second fetch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
                    running: vec![],
                },
            ],
            queue_depth: 2,
        };
        let json = serde_json::to_string(&status).unwrap();
        let back: FleetStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, back);
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

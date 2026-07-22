//! Platform-level runtime state published for read-only display.
//!
//! The dispatcher's fleet and agent defaults live only in its process
//! environment (spec §12.4) — invisible to the api, which serves the operator
//! UI. To make them visible, the dispatcher writes this snapshot to the
//! `platform` KV bucket at startup (key `dispatcher.config`); the api reads it
//! back for the platform settings page. Read-only today: runtime reconfiguration
//! (add/drain a node without restarting) is a later phase.

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
            wizard_available: false,
            dispatcher_sha: Some("abc123".into()),
            main_tip_sha: Some("def456".into()),
            commits_behind: Some(3),
            auto_deploy: None,
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
        assert!(!snap.wizard_available);
        // New CD fields default to None.
        assert_eq!(snap.dispatcher_sha, None);
        assert_eq!(snap.main_tip_sha, None);
        assert_eq!(snap.commits_behind, None);
        assert_eq!(snap.auto_deploy, None);
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
    /// Whether the New Job "job wizard" LLM chat is configured (`WIZARD_API_KEY`
    /// / `ANTHROPIC_API_KEY`). False → the UI falls back to manual title/
    /// description entry.
    #[serde(default)]
    pub wizard_available: bool,
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
}

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
}

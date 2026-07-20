//! Agent provider abstraction (spec §4.3–4.4).

pub mod claude;
pub mod codex;

use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;
use types::EvalResult;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("backend: {0}")]
    Backend(#[from] container::BackendError),
    #[error("provider: {0}")]
    Provider(String),
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    /// Launch the agent container via the dispatcher's backend, monitor it until
    /// exit, return the result. The declared job `image` provides the full dev
    /// environment including the agent CLI binary.
    async fn run(&self, config: AgentRunConfig) -> Result<AgentOutput, AgentError>;
    /// Governs which mode the channel MCP server starts in (spec §4.2).
    fn supports_push_notifications(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct AgentRunConfig {
    pub image: String,
    /// Resolved prompt content, never a path. Delivered via a temp file
    /// injected into the created container at `/chuggernaut/prompt.md` (spec §4.3).
    pub prompt: String,
    pub model: Option<String>,
    /// Composed from knowledge libraries (spec §4.4).
    pub system_prompt: Option<String>,
    pub mcp_servers: Vec<McpServerConfig>,
    /// Injected by the provider alongside the prompt — the MCP server binaries
    /// and any per-run payloads (spec §4.2 distribution, §13.4 event batch).
    pub files: Vec<container::InjectedFile>,
    pub env: HashMap<String, String>,
    pub task_timeout: Duration,
    /// Empty on cycle 1 and merge-conflict cycles; populated on eval-failure rework.
    pub eval_context: Vec<EvalResult>,
    /// Set when the cycle was triggered by a squash-merge conflict.
    pub merge_conflict: Option<String>,
    /// Dispatcher-generated session id (a UUID) handed to the CLI, making the
    /// transcript filename deterministic. See [`transcript_path`].
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Outcome of an agent run. Carries the container id so the caller can pull
/// artifacts out *after* exit — without it the session transcript is written
/// inside a container nobody can name, which is what used to happen.
///
/// Returned on the timeout path too: a run that blew its `task_timeout` is
/// exactly the one whose transcript you want to read.
#[derive(Debug, Clone)]
pub struct AgentOutput {
    pub exit_code: i32,
    /// `None` only for providers that do not launch a container (fakes).
    pub container_id: Option<container::ContainerId>,
    /// The session id handed to the CLI, so the transcript path is known
    /// rather than discovered.
    pub session_id: Option<String>,
}

impl AgentOutput {
    /// An outcome with no container to harvest — the fake/stub path.
    pub fn bare(exit_code: i32) -> Self {
        Self { exit_code, container_id: None, session_id: None }
    }
}

/// Path where providers inject the resolved prompt inside the container.
pub const PROMPT_PATH: &str = "/chuggernaut/prompt.md";
/// Path where factory triage jobs receive their event batch (spec §13.4).
pub const EVENTS_PATH: &str = "/chuggernaut/events.json";

/// `CLAUDE_CONFIG_DIR` for agent containers. Pinning this decouples the
/// transcript path from `HOME` — which is only `/root` today because
/// `Dockerfile.agent` sets no `USER`.
pub const CLAUDE_CONFIG_DIR: &str = "/chuggernaut/claude";

/// Where the Claude CLI writes the transcript for a run in `/workspace`.
///
/// The CLI slugifies the cwd by replacing each non-alphanumeric character with
/// `-`, including the leading `/`, so `/workspace` becomes `-workspace`.
/// **Measured against CLI 2.1.211**, and note the published docs disagree —
/// they describe `/workspace` mapping to `workspace` with no leading dash.
/// `bootstrap_cmd` guarantees the cwd, and `--session-id` fixes the filename.
pub fn transcript_path(session_id: &str) -> String {
    format!("{CLAUDE_CONFIG_DIR}/projects/-workspace/{session_id}.jsonl")
}

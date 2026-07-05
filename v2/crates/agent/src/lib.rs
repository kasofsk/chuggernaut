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
    pub env: HashMap<String, String>,
    pub task_timeout: Duration,
    /// Empty on cycle 1 and merge-conflict cycles; populated on eval-failure rework.
    pub eval_context: Vec<EvalResult>,
    /// Set when the cycle was triggered by a squash-merge conflict.
    pub merge_conflict: Option<String>,
}

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AgentOutput {
    pub exit_code: i32,
}

/// Path where providers inject the resolved prompt inside the container.
pub const PROMPT_PATH: &str = "/chuggernaut/prompt.md";
/// Path where factory triage jobs receive their event batch (spec §13.4).
pub const EVENTS_PATH: &str = "/chuggernaut/events.json";

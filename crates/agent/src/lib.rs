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
    ///
    /// `on_launch` is fired with the container id the instant the container is
    /// launched — before the (blocking) wait — so the caller can persist the id
    /// onto the task record while it is still Running, instead of only learning
    /// it from [`AgentOutput`] after exit. Firing it is best-effort: a provider
    /// that launches no container (fakes) simply never calls it.
    async fn run(
        &self,
        config: AgentRunConfig,
        on_launch: LaunchReporter,
    ) -> Result<AgentOutput, AgentError>;
    /// Governs which mode the channel MCP server starts in (spec §4.2).
    fn supports_push_notifications(&self) -> bool;
}

/// One-shot side-channel a provider uses to hand its container id back the
/// instant the container launches, so the dispatcher can write it onto the
/// Running task record (previously the id only surfaced in [`AgentOutput`]
/// after exit, leaving `container_id: null` for the whole run). Cloneable and
/// cheap; reporting more than once or never is harmless. `none()` is the
/// no-op used where nobody is listening.
#[derive(Clone, Default)]
pub struct LaunchReporter(Option<tokio::sync::mpsc::UnboundedSender<container::ContainerId>>);

impl LaunchReporter {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<container::ContainerId>) -> Self {
        Self(Some(tx))
    }

    /// A reporter nobody listens to — for provider stubs and tests.
    pub fn none() -> Self {
        Self(None)
    }

    /// Report the launched container's id. Never blocks and never errors: the
    /// receiver may have gone away (task already resolved), which is fine.
    pub fn report(&self, id: &container::ContainerId) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(id.clone());
        }
    }
}

/// Which permission policy the provider grants a run.
///
/// Before this existed every agent launched with `--dangerously-skip-permissions`
/// — one blanket bypass for work and evaluation alike — so "the reviewer must
/// not build" was a request in a prompt and nothing more. The profile is chosen
/// by *role* at the launch site rather than declared in `.chug/jobs/*.yaml`: nothing
/// needs per-evaluator variation, and `types::Evaluator` is
/// `deny_unknown_fields`, so a new YAML key would drag in a schema regen and a
/// spec §14 version-skew gate for no gain.
///
/// The provider owns the translation to CLI-specific settings — see
/// [`claude::settings_json`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionProfile {
    /// Work and factory-triage agents: edit, build, commit, push. Deliberately
    /// as permissive as the old bypass flag — the container is still the
    /// security boundary (spec §4.3). The gain is that policy is now
    /// *expressible*, not that work is newly restricted.
    Work,
    /// Agent evaluators: read the diff, judge it against the brief, publish a
    /// verdict. No build tooling — the stage-1 `ci` gate owns compiling and
    /// testing, and a reviewer that re-runs it spends minutes of shared Docker
    /// host for signal CI is about to produce anyway.
    Review,
}

#[derive(Debug, Clone)]
pub struct AgentRunConfig {
    /// The image the run's container needs, `None` for a host task — the mode
    /// selector [`container::ContainerLaunchConfig::image`] carries (design
    /// #309 §1).
    pub image: Option<String>,
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
    /// Optional fleet node pin from the job type's `placement` (spec §3.1),
    /// forwarded to the backend at launch. `None` = default placement.
    pub node: Option<String>,
    /// Which tools this run may use (spec §4.3). See [`PermissionProfile`].
    pub permissions: PermissionProfile,
    /// The job type's `tools:` grant for this run (design #533 S1), added to the
    /// profile's allow list. Empty for every type that declares none, and for
    /// the platform's own agents, so the payload is byte-identical to one
    /// composed before the feature existed.
    pub tools: Vec<types::AgentTool>,
    /// The job type's declared `runtime.env` (spec §1.1, design #373 P2),
    /// forwarded to the backend at launch beside [`Self::image`]. `None` for a
    /// job type that declares none, and for the platform's own agents.
    pub runtime_env: Option<String>,
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
        Self {
            exit_code,
            container_id: None,
            session_id: None,
        }
    }
}

/// Every env name an implemented provider's CLI reads a provider credential
/// from — the set the reserved `global/agents` grant admits, and nothing else
/// (design #529 S1b). [`codex::CodexProvider`] is a stub with no launch path, so
/// it contributes none.
pub const PROVIDER_CREDENTIAL_NAMES: &[&str] = claude::CREDENTIAL_ENV_NAMES;

/// Whether `name` is one of [`PROVIDER_CREDENTIAL_NAMES`] — the membership test
/// that replaced the reserved grant's whole-scope listing (design #529 S1b).
pub fn is_provider_credential(name: &str) -> bool {
    PROVIDER_CREDENTIAL_NAMES.contains(&name)
}

/// Path where providers inject the resolved prompt inside the container.
pub const PROMPT_PATH: &str = "/chuggernaut/prompt.md";
/// Path where factory triage jobs receive their event batch (spec §13.4).
pub const EVENTS_PATH: &str = "/chuggernaut/events.json";

/// Path where providers inject the run's permission settings (spec §4.3).
///
/// Under `/chuggernaut`, NOT under `/workspace`: the bootstrap clones into
/// `/workspace` and git requires that directory empty, so anything pre-injected
/// there breaks the clone.
pub const SETTINGS_PATH: &str = "/chuggernaut/agent-settings.json";

/// `CLAUDE_CONFIG_DIR` for agent containers. Pinning this decouples the
/// transcript path from `HOME` — which is only `/root` today because
/// `Dockerfile.agent` sets no `USER`.
pub const CLAUDE_CONFIG_DIR: &str = "/chuggernaut/claude";

/// The directory the CLI keeps one transcript directory per cwd under, which
/// the harvest resolves inside rather than guessing the name of (design #490
/// D1). The names below it are the CLI's own slugs and are never computed.
pub fn transcript_dir() -> String {
    format!("{CLAUDE_CONFIG_DIR}/projects")
}

/// The transcript's file name, which is the session id the platform itself
/// supplied on `--session-id`. Depending on our own input is what D1 buys over
/// depending on the CLI's output.
pub fn transcript_name(session_id: &str) -> String {
    format!("{session_id}.jsonl")
}

/// The **fallback** path, for a worker daemon too old to know `find_file`
/// (design #490 D1a): the CLI slugifies its cwd by replacing each
/// non-alphanumeric character with `-`, measured against CLI 2.1.211 while the
/// published docs describe something else.
/// It slugifies the **resolved realpath** (job #492), so this is wrong on any
/// task root reached through a symlink and is only ever correct for a container
/// launch, whose cwd is `/workspace` with no symlink in it.
pub fn transcript_path(session_id: &str) -> String {
    format!(
        "{}/-workspace/{}",
        transcript_dir(),
        transcript_name(session_id)
    )
}

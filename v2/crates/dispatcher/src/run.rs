//! Production startup: wire the store, repos, Docker fleet, and provider into
//! a spawned core (`chuggernaut dispatcher`). Fails fast per §3.6/§12.4 — bad
//! config, old git, or an unreachable fleet node aborts startup.

use crate::config::DispatcherConfig;
use crate::core::{Core, CoreError, CoreHandle, Result, spawn};
use crate::handlers;
use agent::AgentProvider;
use agent::claude::ClaudeProvider;
use container::docker::DockerBackend;
use std::sync::Arc;
use store::NatsStore;
use vcs::RepoManager;

/// Bring up the dispatcher and return its handle; the process stays alive as
/// long as the caller holds it (the bin waits on ctrl-c).
pub async fn run(config: DispatcherConfig) -> Result<CoreHandle> {
    // Operator-mode NATS requires the dispatcher credentials from init
    // (§12.1); without them (open dev server) connect plain.
    let store = match config.dispatcher_creds().await? {
        Some(creds) => NatsStore::connect_with_creds(&config.nats_url, &creds).await?,
        None => NatsStore::connect(&config.nats_url).await?,
    };

    let repos = RepoManager::new(&config.repos_root);
    repos.check_git_version().await?;

    let backend = Arc::new(DockerBackend::new(config.docker_nodes.clone())?);
    backend.ping_all().await?; // §3.6: refuse to start if any node is unreachable

    let provider: Arc<dyn AgentProvider> = match config.agent_provider_default.as_str() {
        "claude" => Arc::new(ClaudeProvider::new(backend.clone())),
        other => {
            return Err(CoreError::Config(format!(
                "provider {other:?} is not implemented yet (only claude)"
            )));
        }
    };

    let core_config = config.core_config().await?;
    if core_config.age_identity.is_none() {
        tracing::warn!(
            "no age_private.key in {} — secrets will be injected as stored (§8.2 dev mode)",
            config.keys_dir.display()
        );
    }
    if core_config.channel_binary.is_none() {
        tracing::warn!("CHANNEL_BINARY unset — agent containers run without the channel MCP");
    }

    let core = Core::new(store.clone(), repos, backend, provider, core_config).await?;
    let handle = spawn(core);
    handlers::spawn_container_handlers(&store, handle.clone()).await?;
    tracing::info!(nats = %config.nats_url, repos = %config.repos_root.display(), "dispatcher up");
    Ok(handle)
}

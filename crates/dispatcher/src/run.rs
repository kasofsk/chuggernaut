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

    // Worker nodes (spec §3.1) are NATS-proxied and soft-fail at startup (an
    // unreachable worker is out-of-service, not fatal); plain Docker fleets
    // keep the strict §3.6 rule and the exact single-backend path.
    let backend: Arc<dyn container::ContainerBackend> =
        if worker::backend::has_worker_nodes(&config.docker_nodes) {
            let fleet = worker::FleetBackend::new(config.docker_nodes.clone(), store.clone())?;
            fleet.startup_check().await?; // hard-fails only on docker-endpoint nodes
            Arc::new(fleet)
        } else {
            let docker = DockerBackend::new(config.docker_nodes.clone())?;
            docker.ping_all().await?; // §3.6: refuse to start if any node is unreachable
            Arc::new(docker)
        };

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

    // Publish a read-only snapshot of the runtime config (fleet + agent
    // defaults + resolved paths) to the `platform` bucket so the api/UI can
    // display it — this config otherwise lives only in this process's env.
    // Best-effort: a failed write must not stop the dispatcher from starting.
    publish_config_snapshot(&store, &config, core_config.age_identity.is_some()).await;

    let core = Core::new(store.clone(), repos, backend, provider, core_config).await?;
    let handle = spawn(core);
    handlers::spawn_container_handlers(&store, handle.clone()).await?;
    handlers::spawn_api_handlers(
        &store,
        handle.clone(),
        Arc::new(RepoManager::new(&config.repos_root)),
        config.hook_bin.clone(),
        config.wizard.clone().map(Arc::new),
    )
    .await?;
    tracing::info!(nats = %config.nats_url, repos = %config.repos_root.display(), "dispatcher up");
    Ok(handle)
}

/// Write the runtime config snapshot to the `platform` bucket for the api/UI to
/// read (see `types::DispatcherConfigSnapshot`). Best-effort — logs and returns
/// on any failure so a missing bucket never blocks startup.
async fn publish_config_snapshot(
    store: &NatsStore,
    config: &DispatcherConfig,
    secrets_encryption: bool,
) {
    let snapshot = types::DispatcherConfigSnapshot {
        nodes: config
            .docker_nodes
            .iter()
            .map(|n| types::WorkerNode {
                name: n.name.clone(),
                endpoint: n.endpoint.clone(),
                slots: n.slots,
            })
            .collect(),
        agent_provider_default: config.agent_provider_default.clone(),
        agent_model_default: config.agent_model_default.clone(),
        triage_image: config.triage_image.clone(),
        repos_root: config.repos_root.display().to_string(),
        repo_url_base: config.repo_url_base.clone(),
        nats_url: config.nats_url.clone(),
        nats_url_container: config.nats_url_container.clone(),
        channel_binary: config
            .channel_binary
            .as_ref()
            .map(|p| p.display().to_string()),
        hook_bin: config.hook_bin.as_ref().map(|p| p.display().to_string()),
        secrets_encryption,
        wizard_available: config.wizard.is_some(),
    };
    match store.raw_bucket(store::buckets::PLATFORM).await {
        Ok(bucket) => {
            if let Err(e) = bucket.put_json("dispatcher.config", &snapshot).await {
                tracing::warn!("config snapshot write failed: {e}");
            }
        }
        Err(e) => tracing::warn!("config snapshot: platform bucket unavailable: {e}"),
    }
}

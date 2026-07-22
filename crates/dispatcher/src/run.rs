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

    // Mixed fleets (spec §3.1/§3.6) probe every node — docker or worker — mark
    // each in/out-of-service, and apply the "no live capacity" hard-fail once
    // across the whole fleet: capacity is a fleet-level property, so a
    // placement-inert 0-slot node never vetoes a fleet with slots elsewhere.
    // A plain Docker fleet (no worker nodes) keeps the exact single-backend
    // path below.
    // Per-node health captured at startup for the platform snapshot (spec
    // §3.1). Placement keeps it fresh as nodes drop/recover, but the snapshot
    // is written once, so this is the boot-time view.
    let node_availability: Vec<(String, bool)>;
    // Per-node build version at boot for the snapshot (spec §3.1); empty for a
    // pure Docker fleet (docker endpoints carry no chuggernaut version).
    let node_versions: Vec<(String, Option<String>)>;
    let backend: Arc<dyn container::ContainerBackend> =
        if worker::backend::has_worker_nodes(&config.docker_nodes) {
            let fleet = worker::FleetBackend::new(config.docker_nodes.clone(), store.clone())?;
            // Fleet-level rule: refuses only when no reachable node has slots > 0.
            fleet.startup_check().await?;
            node_availability = fleet.availability();
            node_versions = fleet.node_versions();
            Arc::new(fleet)
        } else {
            let docker = DockerBackend::new(config.docker_nodes.clone())?;
            // §3.1/§3.6: start as long as one node with slots responds; any
            // unreachable node is logged and excluded until it answers again.
            docker.ping_all().await?;
            node_availability = docker.availability();
            node_versions = Vec::new();
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
    publish_config_snapshot(
        &store,
        &config,
        &node_availability,
        &node_versions,
        core_config.age_identity.is_some(),
    )
    .await;

    // §7.3 user-cert minting signs with the CA key directly (no job-record
    // write), so it rides the API handlers rather than the single-writer core.
    let ssh_ca = core_config.ssh_ca.clone();
    // Kept for the read-only live-output tail (`req.tasks.output`), which reads
    // the backend directly off the core actor.
    let output_backend = backend.clone();
    let core = Core::new(store.clone(), repos, backend, provider, core_config).await?;
    let handle = spawn(core);
    handlers::spawn_container_handlers(&store, handle.clone()).await?;
    handlers::spawn_api_handlers(
        &store,
        handle.clone(),
        Arc::new(RepoManager::new(&config.repos_root)),
        config.hook_bin.clone(),
        config.wizard.clone().map(Arc::new),
        ssh_ca,
        output_backend,
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
    node_availability: &[(String, bool)],
    node_versions: &[(String, Option<String>)],
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
                // Absent from the availability list ⇒ assume up (the list is
                // built from the same node set, so this is belt-and-suspenders).
                available: node_availability
                    .iter()
                    .find(|(name, _)| name == &n.name)
                    .map(|(_, up)| *up)
                    .unwrap_or(true),
                version: node_versions
                    .iter()
                    .find(|(name, _)| name == &n.name)
                    .and_then(|(_, v)| v.clone()),
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

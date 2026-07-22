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
use std::time::Duration;
use store::NatsStore;
use vcs::RepoManager;

/// Bounded window a graceful drain gets before the process exits anyway (spec
/// §3.6). launchd's `kickstart -k` follows SIGTERM with SIGKILL, so the drain
/// must be robust to being cut short — it only ever makes records more accurate.
const DRAIN_BUDGET: Duration = Duration::from_secs(10);

/// A running dispatcher: the live [`CoreHandle`] plus what a graceful shutdown
/// needs (the store and the boot config snapshot to re-publish). The process
/// holds this for its lifetime and calls [`Dispatcher::shutdown`] on SIGTERM.
pub struct Dispatcher {
    handle: CoreHandle,
    store: NatsStore,
    snapshot: types::DispatcherConfigSnapshot,
}

impl Dispatcher {
    pub fn handle(&self) -> &CoreHandle {
        &self.handle
    }

    /// Graceful shutdown (spec §3.6 drain): quiesce the single-writer actor and
    /// flush memory-only state to KV within [`DRAIN_BUDGET`], then re-publish the
    /// config snapshot, then return. A drain that outruns the budget is cut short
    /// (launchd will SIGKILL regardless) — safe, because the drain only ever
    /// makes records more accurate.
    pub async fn shutdown(self) {
        match tokio::time::timeout(DRAIN_BUDGET, self.handle.drain()).await {
            Ok(Ok(())) => tracing::info!("dispatcher drained cleanly"),
            Ok(Err(e)) => tracing::warn!("dispatcher drain error: {e}"),
            Err(_) => tracing::warn!(
                "dispatcher drain exceeded {DRAIN_BUDGET:?} — exiting; records already flushed stand"
            ),
        }
        // The snapshot is static for the process lifetime; re-publishing on exit
        // keeps the platform record present and current at the last moment.
        write_config_snapshot(&self.store, &self.snapshot).await;
    }
}

/// Block until the process receives a termination signal. SIGTERM is the deploy
/// path's stop signal (launchd `kickstart -k`); SIGINT covers an interactive
/// ctrl-c. Either one triggers the graceful drain.
pub async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("installing SIGTERM handler failed: {e}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("installing SIGINT handler failed: {e}");
                let _ = term.recv().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => tracing::info!("received SIGTERM"),
            _ = interrupt.recv() => tracing::info!("received SIGINT"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Bring up the dispatcher and return the running handle; the process stays
/// alive as long as the caller holds it and drives shutdown via
/// [`Dispatcher::shutdown`] on SIGTERM/SIGINT.
pub async fn run(config: DispatcherConfig) -> Result<Dispatcher> {
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

    // Resolve the job wizard (§12.5) now that the store is connected: env keys
    // win, else reuse the age-encrypted CLAUDE_CODE_OAUTH_TOKEN agent
    // credential (§8.2) so one credential powers both agent containers and the
    // wizard. The decrypting secret store needs the age identity; without it
    // (dev raw mode) the secret fallback is skipped. Resolved once at
    // startup — setting the secret later needs a dispatcher restart.
    let wizard_secrets = match &core_config.age_identity {
        Some(identity) => Some(store::secrets::AgeSecretStore::for_dispatcher(
            store.raw_bucket(store::buckets::SECRETS).await?,
            identity,
        )?),
        None => None,
    };
    let wizard = crate::wizard::WizardConfig::from_env_or_secrets(wizard_secrets.as_ref()).await;
    if wizard.is_none() {
        tracing::info!("job wizard unconfigured — the New Job screen uses manual entry");
    }
    if core_config.channel_binary.is_none() {
        tracing::warn!("CHANNEL_BINARY unset — agent containers run without the channel MCP");
    }

    // Publish a read-only snapshot of the runtime config (fleet + agent
    // defaults + resolved paths) to the `platform` bucket so the api/UI can
    // display it — this config otherwise lives only in this process's env.
    // Best-effort: a failed write must not stop the dispatcher from starting.
    // Kept so the graceful-shutdown drain (§3.6) can re-publish it at exit.
    let snapshot = build_config_snapshot(
        &config,
        &node_availability,
        &node_versions,
        core_config.age_identity.is_some(),
        wizard.is_some(),
    );
    write_config_snapshot(&store, &snapshot).await;

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
        wizard.map(Arc::new),
        ssh_ca,
        output_backend,
    )
    .await?;
    tracing::info!(nats = %config.nats_url, repos = %config.repos_root.display(), "dispatcher up");
    Ok(Dispatcher {
        handle,
        store,
        snapshot,
    })
}

/// Build the runtime config snapshot (fleet + agent defaults + resolved paths)
/// the api/UI reads (`types::DispatcherConfigSnapshot`). Pure — the caller
/// publishes it at startup and re-publishes it on graceful shutdown (§3.6).
fn build_config_snapshot(
    config: &DispatcherConfig,
    node_availability: &[(String, bool)],
    node_versions: &[(String, Option<String>)],
    secrets_encryption: bool,
    wizard_available: bool,
) -> types::DispatcherConfigSnapshot {
    types::DispatcherConfigSnapshot {
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
        wizard_available,
    }
}

/// Write the config snapshot to the `platform` bucket for the api/UI to read.
/// Best-effort — logs and returns on any failure so a missing bucket never
/// blocks startup or shutdown.
async fn write_config_snapshot(store: &NatsStore, snapshot: &types::DispatcherConfigSnapshot) {
    match store.raw_bucket(store::buckets::PLATFORM).await {
        Ok(bucket) => {
            if let Err(e) = bucket.put_json("dispatcher.config", snapshot).await {
                tracing::warn!("config snapshot write failed: {e}");
            }
        }
        Err(e) => tracing::warn!("config snapshot: platform bucket unavailable: {e}"),
    }
}

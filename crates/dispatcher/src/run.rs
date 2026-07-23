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
    // A worker-capable fleet is used whenever any node is a worker endpoint OR
    // the seed list is empty — a zero-seed dynamic fleet (spec §3.1 dynamic
    // registration) that gains capacity only from worker announcements.
    let backend: Arc<dyn container::ContainerBackend> = if worker::backend::has_worker_nodes(
        &config.docker_nodes,
    ) || config.docker_nodes.is_empty()
    {
        let fleet = worker::FleetBackend::new(
            config.docker_nodes.clone(),
            store.clone(),
            config.placement_policy,
        )?;
        // Fleet-level rule: refuses only when a *configured* fleet has no
        // reachable node with slots > 0; a zero-seed fleet starts and waits
        // for announcements.
        fleet.startup_check().await?;
        Arc::new(fleet)
    } else {
        let docker = DockerBackend::new(config.docker_nodes.clone())?
            .with_placement_policy(config.placement_policy);
        // §3.1/§3.6: start as long as one node with slots responds; any
        // unreachable node is logged and excluded until it answers again.
        docker.ping_all().await?;
        Arc::new(docker)
    };
    // Per-node health and build version as of the boot probe, for the platform
    // snapshot (spec §3.1). The scan tick keeps it fresh as nodes drop/recover
    // and workers self-refresh (see `crate::cd`).
    let fleet_status = backend.fleet_status();

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
    // defaults + resolved paths + deploy drift) to the `platform` bucket so the
    // api/UI can display it — this config otherwise lives only in this process's
    // env. Best-effort: a failed write must not stop the dispatcher from
    // starting. The returned state lets the scan tick republish it live
    // (`crate::cd`).
    let snapshot = publish_config_snapshot(
        &store,
        &config,
        &fleet_status,
        core_config.age_identity.is_some(),
    )
    .await;
    // The boot snapshot the graceful-shutdown drain (§3.6) re-publishes at exit;
    // the live copy moves into the core, which republishes it from the scan tick.
    let boot_snapshot = snapshot.base.clone();

    // §7.3 user-cert minting signs with the CA key directly (no job-record
    // write), so it rides the API handlers rather than the single-writer core.
    let ssh_ca = core_config.ssh_ca.clone();
    // Kept for the read-only live-output tail (`req.tasks.output`), which reads
    // the backend directly off the core actor.
    let output_backend = backend.clone();
    let core = Core::new(store.clone(), repos, backend, provider, core_config)
        .await?
        .with_fleet_roster(boot_snapshot.nodes.clone())
        .with_config_snapshot(snapshot);
    let handle = spawn(core);
    handlers::spawn_container_handlers(&store, handle.clone()).await?;
    // Runtime dynamic worker registration (spec §3.1): merge worker announce
    // heartbeats into the live fleet with no dispatcher restart.
    handlers::spawn_worker_announce_handler(&store, handle.clone()).await?;
    handlers::spawn_api_handlers(
        &store,
        handle.clone(),
        Arc::new(RepoManager::new(&config.repos_root)),
        config.hook_bin.clone(),
        ssh_ca,
        output_backend,
    )
    .await?;
    tracing::info!(nats = %config.nats_url, repos = %config.repos_root.display(), "dispatcher up");
    Ok(Dispatcher {
        handle,
        store,
        snapshot: boot_snapshot,
    })
}

/// Write the boot-time runtime config snapshot to the `platform` bucket for the
/// api/UI to read (see `types::DispatcherConfigSnapshot`), and return the
/// republish state the scan tick uses to keep it fresh (`crate::cd`).
/// Best-effort — logs and returns on any write failure so a missing bucket
/// never blocks startup; `last_published` is left unset on failure so the first
/// scan retries the write.
async fn publish_config_snapshot(
    store: &NatsStore,
    config: &DispatcherConfig,
    fleet_status: &[container::NodeStatus],
    secrets_encryption: bool,
) -> crate::cd::ConfigSnapshot {
    let deployed_sha = crate::cd::deployed_sha();
    let base = crate::cd::build_base_snapshot(
        config,
        fleet_status,
        deployed_sha.clone(),
        secrets_encryption,
    );
    let last_published = match store.raw_bucket(store::buckets::PLATFORM).await {
        Ok(bucket) => match bucket.put_json("dispatcher.config", &base).await {
            Ok(()) => serde_json::to_vec(&base).ok(),
            Err(e) => {
                tracing::warn!("config snapshot write failed: {e}");
                None
            }
        },
        Err(e) => {
            tracing::warn!("config snapshot: platform bucket unavailable: {e}");
            None
        }
    };
    crate::cd::ConfigSnapshot {
        base,
        deployed_sha,
        self_repo: config.self_repo.clone(),
        commits_behind_cache: None,
        last_published,
    }
}

/// Write a config snapshot to the `platform` bucket for the api/UI to read.
/// Best-effort — logs and returns on any failure so a missing bucket never
/// blocks startup or shutdown. The graceful-shutdown drain (§3.6) uses this to
/// re-publish the boot snapshot at exit.
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

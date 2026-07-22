//! Config-snapshot freshness and deploy-drift surfacing (CD plan C).
//!
//! [`run`](crate::run) publishes a [`DispatcherConfigSnapshot`] to the
//! `platform` bucket once at startup, so node health, worker versions, and
//! fleet state in it go stale immediately. For CD, operators need live "is prod
//! in sync" visibility: which SHA is deployed, what `main` points at now, and
//! how far behind the running dispatcher is.
//!
//! The fix is a cheap republish from the periodic scan tick
//! ([`Core::refresh_config_snapshot`]): rebuild the snapshot from live fleet
//! state and the self-repo tip, serialize it, and write it back only when the
//! bytes changed. That is a KV put every 30s at most — usually skipped, because
//! nothing moved.

use crate::core::Core;
use container::NodeStatus;
use types::{DispatcherConfigSnapshot, WorkerNode};

/// The deployed dispatcher's own build SHA (`CHUG_GIT_SHA`, baked at build time
/// — the SHA component that `version_string()` embeds). `None` for local/dev
/// builds without it baked in; the drift fields then stay unpopulated.
pub(crate) fn deployed_sha() -> Option<String> {
    option_env!("CHUG_GIT_SHA").map(|s| s.to_string())
}

/// The static parts of the config snapshot, computed once at startup from the
/// dispatcher config plus the boot fleet probe. The dynamic parts (per-node
/// health/version, self-repo tip, commits-behind) are recomputed each scan tick
/// in [`Core::refresh_config_snapshot`].
pub(crate) fn build_base_snapshot(
    config: &crate::config::DispatcherConfig,
    fleet: &[NodeStatus],
    deployed_sha: Option<String>,
    secrets_encryption: bool,
    wizard_available: bool,
) -> DispatcherConfigSnapshot {
    DispatcherConfigSnapshot {
        nodes: config
            .docker_nodes
            .iter()
            .map(|n| {
                let status = fleet.iter().find(|s| s.name == n.name);
                WorkerNode {
                    name: n.name.clone(),
                    endpoint: n.endpoint.clone(),
                    slots: n.slots,
                    // Absent from the probe ⇒ assume up (same node set, so this
                    // is belt-and-suspenders).
                    available: status.map(|s| s.available).unwrap_or(true),
                    version: status.and_then(|s| s.version.clone()),
                }
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
        dispatcher_sha: deployed_sha,
        // Filled by the scan tick from the self-repo, if configured.
        main_tip_sha: None,
        commits_behind: None,
        // Populated once the CD engine lands.
        auto_deploy: None,
        placement_policy: config.placement_policy.as_str().to_string(),
    }
}

/// Republish state for the config snapshot. Holds the static base, the deployed
/// SHA, the self-repo to measure drift against, a per-tip commits-behind cache
/// (so the `rev-list` runs only when `main` moves), and the last-published
/// bytes for change detection.
pub(crate) struct ConfigSnapshot {
    pub(crate) base: DispatcherConfigSnapshot,
    pub(crate) deployed_sha: Option<String>,
    pub(crate) self_repo: Option<(String, String)>,
    pub(crate) commits_behind_cache: Option<(String, u64)>,
    pub(crate) last_published: Option<Vec<u8>>,
}

impl Core {
    /// Rebuild the config snapshot from live fleet state and the self-repo tip,
    /// and republish it to the `platform` bucket only when the serialized bytes
    /// changed. Runs inside the single-writer loop off the scan tick, so it is
    /// a KV put every 30s at most — usually a no-op. Best-effort: every failure
    /// logs and returns without disturbing the scan.
    pub(crate) async fn refresh_config_snapshot(&mut self) {
        let Some(mut snap) = self.snapshot.take() else {
            return;
        };

        let mut next = snap.base.clone();

        // Live per-node health and worker versions.
        let fleet = self.backend.fleet_status();
        for node in &mut next.nodes {
            if let Some(status) = fleet.iter().find(|s| s.name == node.name) {
                node.available = status.available;
                node.version = status.version.clone();
            }
        }
        next.dispatcher_sha = snap.deployed_sha.clone();

        // Self-repo deploy drift: current `main` tip and commits-behind.
        if let Some((owner, project)) = snap.self_repo.clone() {
            match self.resolve_main_tip(&owner, &project).await {
                Ok(tip) => {
                    next.main_tip_sha = Some(tip.clone());
                    if let Some(deployed) = snap.deployed_sha.clone() {
                        next.commits_behind = self
                            .commits_behind(&mut snap, &owner, &project, &deployed, &tip)
                            .await;
                    }
                }
                Err(e) => tracing::warn!("config snapshot: self-repo tip unresolved: {e}"),
            }
        }

        match serde_json::to_vec(&next) {
            Ok(bytes) => {
                if snap.last_published.as_deref() != Some(bytes.as_slice()) {
                    match self.store.raw_bucket(store::buckets::PLATFORM).await {
                        Ok(bucket) => match bucket.put_json("dispatcher.config", &next).await {
                            Ok(()) => snap.last_published = Some(bytes),
                            Err(e) => tracing::warn!("config snapshot republish failed: {e}"),
                        },
                        Err(e) => {
                            tracing::warn!("config snapshot: platform bucket unavailable: {e}")
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("config snapshot serialize failed: {e}"),
        }

        self.snapshot = Some(snap);
    }

    /// The self-repo's default-branch tip SHA.
    async fn resolve_main_tip(&self, owner: &str, project: &str) -> crate::core::Result<String> {
        let branch = self.repos.default_branch(owner, project).await?;
        Ok(self.repos.resolve_ref(owner, project, &branch).await?)
    }

    /// Commits the deployed SHA is behind the tip, cached per tip so the
    /// `rev-list` runs only when `main` moves. `None` when the count can't be
    /// computed (e.g. the deployed SHA isn't in the repo's history).
    async fn commits_behind(
        &self,
        snap: &mut ConfigSnapshot,
        owner: &str,
        project: &str,
        deployed: &str,
        tip: &str,
    ) -> Option<u64> {
        if let Some((cached_tip, n)) = &snap.commits_behind_cache
            && cached_tip == tip
        {
            return Some(*n);
        }
        match self
            .repos
            .count_commits_beyond(owner, project, deployed, tip)
            .await
        {
            Ok(n) => {
                snap.commits_behind_cache = Some((tip.to_string(), n));
                Some(n)
            }
            Err(e) => {
                tracing::warn!("config snapshot: commits-behind unavailable: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use container::docker::DockerNodeConfig;
    use std::path::PathBuf;

    fn config() -> crate::config::DispatcherConfig {
        crate::config::DispatcherConfig {
            nats_url: "nats://localhost:4222".into(),
            repos_root: PathBuf::from("/data/repos"),
            repo_url_base: "file:///data/repos".into(),
            keys_dir: PathBuf::from("/data/keys"),
            nats_url_container: None,
            channel_binary: None,
            agent_provider_default: "claude".into(),
            agent_model_default: None,
            triage_image: None,
            docker_nodes: vec![DockerNodeConfig {
                name: "nuc".into(),
                endpoint: "worker".into(),
                slots: 4,
            }],
            hook_bin: None,
            self_repo: None,
            placement_policy: container::PlacementPolicy::Busyness,
        }
    }

    /// The base snapshot maps config + boot fleet probe onto the wire type,
    /// carrying the deployed SHA and leaving the scan-filled drift fields empty.
    #[test]
    fn base_snapshot_maps_fleet_and_sha() {
        let fleet = vec![NodeStatus {
            name: "nuc".into(),
            available: false,
            version: Some("0.1.0+abc".into()),
        }];
        let snap = build_base_snapshot(&config(), &fleet, Some("abc".into()), true, false);
        assert_eq!(snap.nodes.len(), 1);
        assert!(!snap.nodes[0].available);
        assert_eq!(snap.nodes[0].version.as_deref(), Some("0.1.0+abc"));
        assert_eq!(snap.dispatcher_sha.as_deref(), Some("abc"));
        assert_eq!(snap.main_tip_sha, None);
        assert_eq!(snap.commits_behind, None);
        assert_eq!(snap.auto_deploy, None);
    }

    /// The republish decision compares serialized bytes: an identical snapshot
    /// is skipped (the common every-30s no-op), a changed node version fires.
    #[test]
    fn republish_only_on_change() {
        let fleet = vec![NodeStatus {
            name: "nuc".into(),
            available: true,
            version: Some("0.1.0+abc".into()),
        }];
        let base = build_base_snapshot(&config(), &fleet, Some("abc".into()), true, false);
        let published = serde_json::to_vec(&base).unwrap();

        // Same content ⇒ identical bytes ⇒ skip.
        let same = serde_json::to_vec(&base).unwrap();
        assert_eq!(published, same);

        // A worker self-refresh bumps the node version ⇒ bytes differ ⇒ fire.
        let mut changed = base.clone();
        changed.nodes[0].version = Some("0.2.0+def".into());
        assert_ne!(published, serde_json::to_vec(&changed).unwrap());

        // So does the self-repo drift moving.
        let mut behind = base.clone();
        behind.main_tip_sha = Some("def456".into());
        behind.commits_behind = Some(2);
        assert_ne!(published, serde_json::to_vec(&behind).unwrap());
    }
}

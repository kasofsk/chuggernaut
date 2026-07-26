//! Config-snapshot freshness and deploy-drift surfacing (CD plan C).
//!
//! [`run`](crate::run) publishes a [`DispatcherConfigSnapshot`] to the
//! `platform` bucket once at startup, so node health, worker versions, and
//! fleet state in it go stale immediately. For CD, operators need live "is prod
//! in sync" visibility: which SHA is deployed, what `main` points at now, and
//! how far behind the running dispatcher is.
//!
//! The fix is a cheap republish from the periodic scan tick ([`refresh`]):
//! rebuild the snapshot from live fleet state and the self-repo tip, serialize
//! it, and write it back only when the bytes changed. That is a KV put every
//! 30s at most — usually skipped, because nothing moved.
//!
//! The *base* snapshot — the static mapping of dispatcher config onto the wire
//! type — stays with the config it reads (`dispatcher::config`); this module
//! owns only the freshness half, which is the half that needs the live fleet.
//!
//! - **Accepts:** the periodic scan tick; the caller's [`ConfigSnapshot`]
//!   republish state, live fleet state and the self-repo git tip.
//! - **Emits:** a `DispatcherConfigSnapshot` KV put to the `platform` bucket,
//!   written only when the serialized bytes change.
//! - **Guarantees:** at most one KV write per scan tick (~30s); no write when
//!   nothing moved. Reads only — no job or task record is written.
//! - **Spec:** CD plan C.

use container::NodeStatus;
use store::NatsStore;
use types::{DispatcherConfigSnapshot, WorkerNode};
use vcs::RepoManager;

/// Reconcile the config snapshot's node list with live fleet state, in place.
///
/// For each existing node, refresh health/version from the backend probe
/// (`fleet`). Slots and membership come from the announce roster (spec §3.1
/// dynamic registration): a seeded worker that re-announced with a changed slot
/// count (the "air 4→5" case) has its live count in the roster, so the snapshot
/// tracks `fleet.status` rather than the boot `DOCKER_NODES` value; and workers
/// announced after boot that aren't in the static seed are appended so the
/// settings page reflects live membership.
pub fn merge_live_fleet(nodes: &mut Vec<WorkerNode>, fleet: &[NodeStatus], roster: &[WorkerNode]) {
    for node in nodes.iter_mut() {
        if let Some(status) = fleet.iter().find(|s| s.name == node.name) {
            node.available = status.available;
            node.version = status.version.clone();
            node.refresh_outcome = status.refresh_outcome.clone();
        }
        // The live announcement wins on slots for a seeded worker.
        if let Some(r) = roster.iter().find(|r| r.name == node.name) {
            node.slots = r.slots;
        }
    }
    for r in roster {
        if !nodes.iter().any(|n| n.name == r.name) {
            nodes.push(r.clone());
        }
    }
}

/// Republish state for the config snapshot. Holds the static base, the deployed
/// SHA, the self-repo to measure drift against, a per-tip commits-behind cache
/// (so the `rev-list` runs only when `main` moves), and the last-published
/// bytes for change detection.
pub struct ConfigSnapshot {
    pub base: DispatcherConfigSnapshot,
    pub deployed_sha: Option<String>,
    pub self_repo: Option<(String, String)>,
    pub commits_behind_cache: Option<(String, u64)>,
    pub last_published: Option<Vec<u8>>,
}

/// Rebuild the config snapshot from live fleet state and the self-repo tip, and
/// republish it to the `platform` bucket only when the serialized bytes changed.
/// Called inside the single-writer loop off the scan tick, so it is a KV put
/// every 30s at most — usually a no-op. Best-effort: every failure logs and
/// returns without disturbing the scan.
pub async fn refresh(
    snap: &mut ConfigSnapshot,
    store: &NatsStore,
    repos: &RepoManager,
    fleet: &[NodeStatus],
    roster: &[WorkerNode],
) {
    let mut next = snap.base.clone();

    // Reconcile the snapshot's node list with the live fleet: per-node
    // health/version from the backend probe, and slots/membership from the
    // announce roster (spec §3.1 dynamic registration).
    merge_live_fleet(&mut next.nodes, fleet, roster);
    next.dispatcher_sha = snap.deployed_sha.clone();

    // Self-repo deploy drift: current `main` tip and commits-behind.
    if let Some((owner, project)) = snap.self_repo.clone() {
        match resolve_main_tip(repos, &owner, &project).await {
            Ok(tip) => {
                next.main_tip_sha = Some(tip.clone());
                if let Some(deployed) = snap.deployed_sha.clone() {
                    next.commits_behind =
                        commits_behind(repos, snap, &owner, &project, &deployed, &tip).await;
                }
            }
            Err(e) => tracing::warn!("config snapshot: self-repo tip unresolved: {e}"),
        }
    }

    match serde_json::to_vec(&next) {
        Ok(bytes) => {
            if snap.last_published.as_deref() != Some(bytes.as_slice()) {
                refresh_publish(snap, store, &next, bytes).await;
            }
        }
        Err(e) => tracing::warn!("config snapshot serialize failed: {e}"),
    }
}

/// The KV half of [`refresh`], reached only once the bytes actually moved.
async fn refresh_publish(
    snap: &mut ConfigSnapshot,
    store: &NatsStore,
    next: &DispatcherConfigSnapshot,
    bytes: Vec<u8>,
) {
    match store.raw_bucket(store::buckets::PLATFORM).await {
        Ok(bucket) => match bucket.put_json("dispatcher.config", next).await {
            Ok(()) => snap.last_published = Some(bytes),
            Err(e) => tracing::warn!("config snapshot republish failed: {e}"),
        },
        Err(e) => tracing::warn!("config snapshot: platform bucket unavailable: {e}"),
    }
}

/// The self-repo's default-branch tip SHA.
async fn resolve_main_tip(repos: &RepoManager, owner: &str, project: &str) -> vcs::Result<String> {
    let branch = repos.default_branch(owner, project).await?;
    repos.resolve_ref(owner, project, &branch).await
}

/// Commits the deployed SHA is behind the tip, cached per tip so the
/// `rev-list` runs only when `main` moves. `None` when the count can't be
/// computed (e.g. the deployed SHA isn't in the repo's history).
async fn commits_behind(
    repos: &RepoManager,
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
    match repos
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn worker(name: &str, slots: u32) -> WorkerNode {
        WorkerNode {
            name: name.into(),
            // The worker-daemon endpoint (`worker::backend::WORKER_ENDPOINT`),
            // spelled out so this leaf crate keeps no edge to `worker`; the
            // field is inert for both tests below.
            endpoint: "worker".into(),
            slots,
            available: true,
            version: Some("0.1.0".into()),
            refresh_outcome: None,
        }
    }

    /// A seeded worker that re-announces with a changed slot count (air 4→5):
    /// `merge_live_fleet` takes the live roster's slots, so the config snapshot
    /// matches `fleet.status` instead of keeping the boot `DOCKER_NODES` count.
    #[test]
    fn seed_worker_reannounce_updates_snapshot_slots() {
        // Snapshot node still carries the boot slot count of 4.
        let mut nodes = vec![worker("air", 4)];
        // Backend probe reports it healthy with a live version.
        let fleet = vec![NodeStatus {
            name: "air".into(),
            available: true,
            version: Some("0.2.0+air".into()),
            refresh_outcome: None,
        }];
        // Roster reflects the re-announce at 5 slots (announce wins).
        let roster = vec![WorkerNode {
            version: Some("0.2.0+air".into()),
            ..worker("air", 5)
        }];
        merge_live_fleet(&mut nodes, &fleet, &roster);

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].slots, 5, "roster slot count wins over the seed");
        assert_eq!(nodes[0].version.as_deref(), Some("0.2.0+air"));
    }

    /// A worker announced after boot (not in the static seed) is appended so the
    /// settings page reflects live fleet membership.
    #[test]
    fn dynamic_worker_appended_to_snapshot() {
        let mut nodes = vec![worker("air", 4)];
        let roster = vec![worker("air", 4), worker("nuc", 2)];
        merge_live_fleet(&mut nodes, &[], &roster);

        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().any(|n| n.name == "nuc" && n.slots == 2));
    }
}

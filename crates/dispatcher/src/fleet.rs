//! Live fleet occupancy publishing (spec §3.1).
//!
//! The config snapshot ([`crate::cd`]) describes the fleet *statically* — node
//! names, slot counts, versions. This module reports live *usage*: which slots
//! on which node are busy and what job/task each busy slot runs. With more than
//! one node the UI can't place work on nodes without it.
//!
//! Occupancy is rebuilt from the *live containers* the backend reports
//! ([`ContainerBackend::list_managed_running`]), never from stale in-memory
//! bookkeeping — so it is correct straight after a restart's re-attachment
//! (§3.6), which reaps orphans and re-attaches survivors before the first
//! publish. The dispatcher (the single writer) republishes a full snapshot to
//! the `platform` bucket (key `fleet.status`) on every task launch/exit — cheap
//! at our scale — writing back only when the serialized bytes change, so an
//! idle fleet republishes nothing. The api serves it at
//! `GET /api/v1/platform/fleet`; the existing per-task lifecycle events
//! (`task-launched`, `task-queued`, task/job state) already ride the job-event
//! stream and tell an SSE client when to refetch.
//!
//! - **Accepts:** task launch/exit events; the live container list from the
//!   backend (`ContainerBackend::list_managed_running`).
//! - **Emits:** a full `fleet.status` snapshot to the `platform` bucket,
//!   written only when the serialized bytes change.
//! - **Guarantees:** occupancy rebuilt from live containers, never stale
//!   bookkeeping — correct straight after restart re-attachment; an idle fleet
//!   republishes nothing.
//! - **Spec:** §3.1, §3.6.

use crate::core::Core;
use std::collections::{BTreeMap, HashSet};
use types::{FleetNode, FleetStatus, JobState, SlotOccupant, TaskPhase};

/// The fleet KV key in the `platform` bucket, beside `dispatcher.config`.
pub(crate) const FLEET_KEY: &str = "fleet.status";

/// Map a task phase to the brief's occupancy vocabulary (`work` | `eval` |
/// `gate` | `wrap_up` | `triage`).
fn phase_kind(phase: TaskPhase) -> &'static str {
    match phase {
        TaskPhase::Work => "work",
        TaskPhase::Evaluation => "eval",
        TaskPhase::MergeGate => "gate",
        TaskPhase::WrapUp => "wrap_up",
        TaskPhase::Triage => "triage",
        // Escalation tasks are Human (#141) and never occupy a fleet slot, so
        // this arm is only for match exhaustiveness.
        TaskPhase::Escalation => "escalation",
    }
}

/// Lowercase the job phase for display (`work`, `evaluation`, `wrap_up`, …).
fn job_phase(state: JobState) -> &'static str {
    match state {
        JobState::Draft => "draft",
        JobState::Frozen => "frozen",
        JobState::Batched => "batched",
        JobState::Blocked => "blocked",
        JobState::Ready => "ready",
        JobState::Work => "work",
        JobState::Evaluation => "evaluation",
        JobState::WrapUp => "wrap_up",
        JobState::Escalated => "escalated",
        JobState::Stalled => "stalled",
        JobState::Done => "done",
        JobState::Revoked => "revoked",
    }
}

/// The fleet node a container runs on, decoded from the `{node}/{docker_id}`
/// id (spec §3.1). A container id without a `/` (the test fake, a legacy
/// container) is its own node bucket.
fn node_of(container_id: &str) -> &str {
    container_id
        .split_once('/')
        .map(|(n, _)| n)
        .unwrap_or(container_id)
}

/// Assemble one node's occupancy entry (spec §3.1). Pure over its inputs so the
/// availability rule is unit-tested without a backend. `occupancy_listed` is the
/// crux: a node whose containers could not be enumerated this snapshot is shown
/// **out of service**, never as a false-idle `occupied: 0, available: true` —
/// the silent all-zero that hid the job/181 outage. It ANDs with the health
/// `base_available`, so a listed node keeps its true (possibly idle) health and
/// only an unlistable one is forced down.
fn fleet_node(
    name: String,
    slots: Option<u32>,
    running: Vec<SlotOccupant>,
    base_available: bool,
    version: Option<String>,
    refresh_outcome: Option<types::worker::RefreshOutcome>,
    occupancy_listed: bool,
) -> FleetNode {
    FleetNode {
        slots,
        occupied: running.len() as u32,
        available: base_available && occupancy_listed,
        version,
        refresh_outcome,
        name,
        running,
    }
}

impl Core {
    /// Recompute live fleet occupancy and republish it to the `platform` bucket
    /// when the serialized bytes changed. Best-effort: every failure logs and
    /// returns without disturbing the caller (a launch, an exit, or the scan).
    /// Runs inside the single-writer loop, so it never races state writes.
    pub(crate) async fn refresh_fleet_status(&mut self) {
        let status = self.compute_fleet_status().await;
        let bytes = match serde_json::to_vec(&status) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("fleet status serialize failed: {e}");
                return;
            }
        };
        if self.last_fleet_status.as_deref() == Some(bytes.as_slice()) {
            return; // nothing moved — the common no-op
        }
        match self.store.raw_bucket(store::buckets::PLATFORM).await {
            Ok(bucket) => match bucket.put_json(FLEET_KEY, &status).await {
                Ok(()) => self.last_fleet_status = Some(bytes),
                Err(e) => tracing::warn!("fleet status republish failed: {e}"),
            },
            Err(e) => tracing::warn!("fleet status: platform bucket unavailable: {e}"),
        }
    }

    /// Build the current [`FleetStatus`] from the live fleet: the running
    /// containers the backend reports (each carrying its `(project, job, task)`
    /// identity), the node roster (names + slot caps, from config), live node
    /// health/version, and the launch-queue depth. Enrichment (job type, phase,
    /// started_at) is read from the KV records the container resolves to.
    pub(crate) async fn compute_fleet_status(&self) -> FleetStatus {
        // Occupied slots grouped by node. A backend that cannot list (an
        // unreachable node) yields an empty occupancy rather than an error, like
        // the §3.6 sweep — occupancy degrades, it never wedges the loop.
        let running = match self.backend.list_managed_running().await {
            Ok(cs) => cs,
            Err(e) => {
                tracing::warn!("fleet status: listing running containers failed: {e}");
                Vec::new()
            }
        };
        let mut by_node: BTreeMap<String, Vec<SlotOccupant>> = BTreeMap::new();
        for rc in running {
            let occupant = self.resolve_occupant(&rc).await;
            by_node
                .entry(node_of(&rc.id).to_string())
                .or_default()
                .push(occupant);
        }
        // Stable ordering so change-detection compares like with like.
        for slots in by_node.values_mut() {
            slots.sort_by_key(|o| (o.project.clone(), o.job_seq, o.task_id));
        }

        // Nodes whose containers could not be listed this pass (spec §3.1): a
        // worker that answers ping but whose `list_running` failed. Their slots
        // are unknown, so they must show out-of-service, not falsely idle.
        let unlisted: HashSet<String> = self
            .backend
            .occupancy_unavailable_nodes()
            .into_iter()
            .collect();

        // Node set = configured roster ∪ live-health nodes ∪ nodes seen busy.
        let live = self.backend.fleet_status();
        let mut names: BTreeMap<String, ()> = BTreeMap::new();
        for n in &self.fleet_roster {
            names.insert(n.name.clone(), ());
        }
        for s in &live {
            names.insert(s.name.clone(), ());
        }
        for name in by_node.keys() {
            names.insert(name.clone(), ());
        }

        let nodes = names
            .into_keys()
            .map(|name| {
                let roster = self.fleet_roster.iter().find(|n| n.name == name);
                let status = live.iter().find(|s| s.name == name);
                let running = by_node.remove(&name).unwrap_or_default();
                let slots = roster.map(|n| n.slots);
                let base_available = status
                    .map(|s| s.available)
                    .or_else(|| roster.map(|n| n.available))
                    .unwrap_or(true);
                let version = status
                    .and_then(|s| s.version.clone())
                    .or_else(|| roster.and_then(|n| n.version.clone()));
                // The live ping-reported outcome (ticket #187) wins; fall back
                // to the roster's last-known so a failed refresh stays visible
                // across the occasional probe miss.
                let refresh_outcome = status
                    .and_then(|s| s.refresh_outcome.clone())
                    .or_else(|| roster.and_then(|n| n.refresh_outcome.clone()));
                let listed = !unlisted.contains(&name);
                fleet_node(
                    name,
                    slots,
                    running,
                    base_available,
                    version,
                    refresh_outcome,
                    listed,
                )
            })
            .collect();

        FleetStatus {
            nodes,
            queue_depth: self.launch_queue.len() as u32,
        }
    }

    /// Describe one busy slot. Fields the container's identity labels resolve to
    /// are filled from the job/task records; an identity-less container (a
    /// pre-labels orphan) still counts as occupied with blank details, so the
    /// occupied count never under-reports a held slot.
    async fn resolve_occupant(&self, rc: &container::RunningContainer) -> SlotOccupant {
        let project = rc.project.clone().unwrap_or_default();
        let job_seq = rc.job.unwrap_or(0);
        let task_id = rc.task.unwrap_or(0);

        let mut occupant = SlotOccupant {
            project: project.clone(),
            job_seq,
            task_id,
            task_kind: String::new(),
            job_type: String::new(),
            phase: String::new(),
            started_at: None,
        };

        if let Some(job) = self.graphs.get(&project).and_then(|g| g.get(job_seq)) {
            occupant.job_type = job.r#type.clone();
            occupant.phase = job_phase(job.state).to_string();
        }
        if let Ok((owner, proj)) = store::split_project(&project)
            && let Ok(Some(task)) = self.tasks.get(owner, proj, job_seq, task_id).await
        {
            occupant.task_kind = phase_kind(task.phase).to_string();
            occupant.started_at = task.started_at;
        }
        occupant
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::fleet_node;

    /// The occupancy availability rule (spec §3.1), the pure core of
    /// `compute_fleet_status`'s node assembly. A node whose containers could not
    /// be listed is shown out-of-service — never a false-idle `occupied: 0,
    /// available: true`, the silent all-zero that hid the job/181 prod outage
    /// (two containers live on a node the snapshot reported empty). A listed node
    /// keeps its true health, idle or busy.
    #[test]
    fn unlisted_node_shows_out_of_service_not_idle() {
        // Healthy (ping-reachable) but its `list_running` failed → occupancy is
        // unknown, so it must NOT read as an available idle node.
        let unlisted = fleet_node("air".into(), Some(4), vec![], true, None, None, false);
        assert_eq!(unlisted.occupied, 0);
        assert!(
            !unlisted.available,
            "an unlistable node must show out of service, not false-idle"
        );

        // A genuinely idle node whose listing succeeded stays available.
        let idle = fleet_node("nuc".into(), Some(4), vec![], true, None, None, true);
        assert!(idle.available);
        assert_eq!(idle.occupied, 0);

        // An already-down node stays down whether or not it was listed.
        let down = fleet_node("old".into(), Some(4), vec![], false, None, None, true);
        assert!(!down.available);
    }
}

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

use crate::core::Core;
use std::collections::BTreeMap;
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
                FleetNode {
                    slots: roster.map(|n| n.slots),
                    occupied: running.len() as u32,
                    available: status
                        .map(|s| s.available)
                        .or_else(|| roster.map(|n| n.available))
                        .unwrap_or(true),
                    version: status
                        .and_then(|s| s.version.clone())
                        .or_else(|| roster.and_then(|n| n.version.clone())),
                    name,
                    running,
                }
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

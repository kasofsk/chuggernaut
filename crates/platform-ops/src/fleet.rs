//! Live fleet occupancy publishing (spec §3.1).
//!
//! The config snapshot ([`super::cd`]) describes the fleet *statically* — node
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
//! - **Accepts:** task launch/exit events; a [`FleetView`] over the caller's
//!   roster, launch-queue depth and job graphs; the live container list from
//!   the backend (`ContainerBackend::list_managed_running`).
//! - **Emits:** a full `fleet.status` snapshot to the `platform` bucket,
//!   written only when the serialized bytes change.
//! - **Guarantees:** occupancy rebuilt from live containers, never stale
//!   bookkeeping — correct straight after restart re-attachment; an idle fleet
//!   republishes nothing. Reads only — no job or task record is written.
//! - **Spec:** §3.1, §3.6.

use container::ContainerBackend;
use std::collections::{BTreeMap, HashSet};
use store::{NatsStore, TaskStore};
use types::{FleetNode, FleetStatus, JobState, SlotOccupant, TaskPhase, WorkerNode};

/// The fleet KV key in the `platform` bucket, beside `dispatcher.config`.
pub const FLEET_KEY: &str = "fleet.status";

/// Operator capacity intent, in the same bucket beside the two above (design
/// #293 §2). Written only by the dispatcher; read by the dispatcher's reconciler
/// at startup. Named here so one module owns the platform bucket's fleet keys.
pub const CAPACITY_KEY: &str = "fleet.capacity";

/// What a busy slot's job record contributes to its occupancy entry.
pub struct JobIdentity {
    pub job_type: String,
    pub state: JobState,
}

/// The single thing this module needs from the dispatcher's in-memory state:
/// name the job a running container belongs to. Synchronous and read-only by
/// design — the caller answers it off its own graphs without leaving the
/// single-writer loop, and an unknown job is simply `None` (an identity-less
/// container still counts as occupied). `Sync` because [`compute`] holds the
/// view across the awaits its record reads need, and the actor is a spawned
/// task.
pub trait JobLookup: Sync {
    fn identify(&self, project: &str, seq: u64) -> Option<JobIdentity>;
}

/// Everything one occupancy snapshot reads, borrowed for the call. This is the
/// context's whole interface to the core (refactor-plan C9): two ports, two
/// values, and one read-only lookup — no `&mut Core`.
pub struct FleetView<'a> {
    pub backend: &'a dyn ContainerBackend,
    pub tasks: &'a TaskStore,
    /// Configured nodes: names, slot caps and last-known health.
    pub roster: &'a [WorkerNode],
    /// Launches parked on capacity (spec §3.5), reported as `queue_depth`.
    pub queue_depth: u32,
    pub jobs: &'a dyn JobLookup,
    /// Node → the operator's desired capacity and its reconciliation state
    /// (design #293 §2, intent's second and last consumer: the UI's "desired"
    /// display). Resolved by the caller from the `fleet.capacity` record, so the
    /// record itself never reaches this side. Never a placement input — `slots`
    /// above stays the one number the scheduler reads.
    pub capacity_intent: &'a BTreeMap<String, types::NodeCapacityDisplay>,
}

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
fn fleet_node(name: String, running: Vec<SlotOccupant>, facts: NodeFacts) -> FleetNode {
    FleetNode {
        slots: facts.slots,
        occupied: running.len() as u32,
        available: facts.base_available && facts.occupancy_listed,
        version: facts.version,
        refresh_outcome: facts.refresh_outcome,
        // Provenance travels with the number (design #293 §7/§8): a node still
        // serving a boot seed reads as such in the fleet view instead of being
        // indistinguishable from one whose daemon confirmed it.
        capacity_source: facts.capacity.map(|c| c.source()),
        capacity_observed_at: facts.capacity.and_then(|c| c.observed_at),
        // Intent, for display only (design #293 §2). It arrives already resolved
        // by the dispatcher, so this composer — which sits on the occupancy path —
        // never reads the intent record itself.
        slots_desired: facts.intent.as_ref().map(|i| i.slots_desired),
        capacity_state: facts.intent.as_ref().map(|i| i.state),
        capacity_note: facts.intent.and_then(|i| i.note),
        name,
        running,
    }
}

/// One node's roster/live-probe merge, before the availability rule is applied
/// to it. A struct rather than seven positional arguments so the merge that
/// produces it ([`compose_node`]) and the rule that consumes it
/// ([`fleet_node`]) each read as one decision.
struct NodeFacts {
    /// The node's capacity: observed if the node has reported over either
    /// transport, the roster's boot seed until then (design #293 §7).
    slots: Option<u32>,
    version: Option<String>,
    refresh_outcome: Option<types::worker::RefreshOutcome>,
    /// Provenance of `slots`; `None` for a docker-endpoint node, whose capacity
    /// `DOCKER_NODES` still owns.
    capacity: Option<types::worker::ObservedCapacity>,
    /// The operator's desired capacity and how far it is from being observed
    /// (design #293 §2/§4), resolved by the caller. `None` when no operator has
    /// ever set one for this node.
    intent: Option<types::NodeCapacityDisplay>,
    base_available: bool,
    occupancy_listed: bool,
}

/// Republish a freshly [`compute`]d occupancy snapshot to the `platform`
/// bucket, but only when the serialized bytes changed. `last_published` is the
/// caller's change-detection cache, updated in place on a successful write.
/// Best-effort: every failure logs and returns without disturbing the caller
/// (a launch, an exit, or the scan). Called inside the single-writer loop, so
/// it never races state writes.
///
/// Split from [`compute`] rather than folded into it because the caller's view
/// borrows the core immutably while this cache is borrowed mutably; the
/// snapshot value between them is the handover.
pub async fn publish(
    status: &FleetStatus,
    store: &NatsStore,
    last_published: &mut Option<Vec<u8>>,
) {
    let bytes = match serde_json::to_vec(status) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("fleet status serialize failed: {e}");
            return;
        }
    };
    if last_published.as_deref() == Some(bytes.as_slice()) {
        return; // nothing moved — the common no-op
    }
    match store.raw_bucket(store::buckets::PLATFORM).await {
        Ok(bucket) => match bucket.put_json(FLEET_KEY, status).await {
            Ok(()) => *last_published = Some(bytes),
            Err(e) => tracing::warn!("fleet status republish failed: {e}"),
        },
        Err(e) => tracing::warn!("fleet status: platform bucket unavailable: {e}"),
    }
}

/// Build the current [`FleetStatus`] from the live fleet: the running
/// containers the backend reports (each carrying its `(project, job, task)`
/// identity), the node roster (names + slot caps, from config), live node
/// health/version, and the launch-queue depth. Enrichment (job type, phase,
/// started_at) is read from the records the container resolves to.
pub async fn compute(view: &FleetView<'_>) -> FleetStatus {
    // Occupied slots grouped by node. A backend that cannot list (an
    // unreachable node) yields an empty occupancy rather than an error, like
    // the §3.6 sweep — occupancy degrades, it never wedges the loop.
    let running = match view.backend.list_managed_running().await {
        Ok(cs) => cs,
        Err(e) => {
            tracing::warn!("fleet status: listing running containers failed: {e}");
            Vec::new()
        }
    };
    let mut by_node: BTreeMap<String, Vec<SlotOccupant>> = BTreeMap::new();
    for rc in running {
        let occupant = resolve_occupant(view, &rc).await;
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
    let unlisted: HashSet<String> = view
        .backend
        .occupancy_unavailable_nodes()
        .into_iter()
        .collect();

    // Node set = configured roster ∪ live-health nodes ∪ nodes seen busy.
    let live = view.backend.fleet_status();
    let mut names: BTreeMap<String, ()> = BTreeMap::new();
    for n in view.roster {
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
        .map(|name| compose_node(view, &live, &unlisted, &mut by_node, name))
        .collect();

    FleetStatus {
        nodes,
        queue_depth: view.queue_depth,
    }
}

/// One node's entry: its occupancy plus the roster/live-probe merge behind the
/// [`fleet_node`] availability rule. Live health wins, the roster is the
/// fallback that keeps a node visible across a probe miss.
fn compose_node(
    view: &FleetView<'_>,
    live: &[container::NodeStatus],
    unlisted: &HashSet<String>,
    by_node: &mut BTreeMap<String, Vec<SlotOccupant>>,
    name: String,
) -> FleetNode {
    let roster = view.roster.iter().find(|n| n.name == name);
    let status = live.iter().find(|s| s.name == name);
    let running = by_node.remove(&name).unwrap_or_default();
    // Observed capacity wins over the boot seed (design #293 §7): the backend
    // reports the number it actually places on — announced or ping-pulled —
    // and the `DOCKER_NODES` roster value is only the pre-observation fallback.
    // A docker-endpoint node reports no live slots, so its roster number stands.
    let slots = status
        .and_then(|s| s.slots)
        .or_else(|| roster.map(|n| n.slots));
    let base_available = status
        .map(|s| s.available)
        .or_else(|| roster.map(|n| n.available))
        .unwrap_or(true);
    let version = status
        .and_then(|s| s.version.clone())
        .or_else(|| roster.and_then(|n| n.version.clone()));
    // The live ping-reported outcome (ticket #187) wins; fall back to the
    // roster's last-known so a failed refresh stays visible across the
    // occasional probe miss.
    let refresh_outcome = status
        .and_then(|s| s.refresh_outcome.clone())
        .or_else(|| roster.and_then(|n| n.refresh_outcome.clone()));
    fleet_node(
        name.clone(),
        running,
        NodeFacts {
            slots,
            version,
            refresh_outcome,
            capacity: status.and_then(|s| s.capacity),
            intent: view.capacity_intent.get(&name).cloned(),
            base_available,
            occupancy_listed: !unlisted.contains(&name),
        },
    )
}

/// Describe one busy slot. Fields the container's identity labels resolve to
/// are filled from the job/task records; an identity-less container (a
/// pre-labels orphan) still counts as occupied with blank details, so the
/// occupied count never under-reports a held slot.
async fn resolve_occupant(view: &FleetView<'_>, rc: &container::RunningContainer) -> SlotOccupant {
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

    if let Some(job) = view.jobs.identify(&project, job_seq) {
        occupant.job_type = job.job_type;
        occupant.phase = job_phase(job.state).to_string();
    }
    if let Ok((owner, proj)) = store::split_project(&project)
        && let Ok(Some(task)) = view.tasks.get(owner, proj, job_seq, task_id).await
    {
        occupant.task_kind = phase_kind(task.phase).to_string();
        occupant.started_at = task.started_at;
    }
    occupant
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::{NodeFacts, fleet_node};

    fn facts(base_available: bool, occupancy_listed: bool) -> NodeFacts {
        NodeFacts {
            slots: Some(4),
            version: None,
            refresh_outcome: None,
            capacity: None,
            intent: None,
            base_available,
            occupancy_listed,
        }
    }

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
        let unlisted = fleet_node("air".into(), vec![], facts(true, false));
        assert_eq!(unlisted.occupied, 0);
        assert!(
            !unlisted.available,
            "an unlistable node must show out of service, not false-idle"
        );

        // A genuinely idle node whose listing succeeded stays available.
        let idle = fleet_node("nuc".into(), vec![], facts(true, true));
        assert!(idle.available);
        assert_eq!(idle.occupied, 0);

        // An already-down node stays down whether or not it was listed.
        let down = fleet_node("old".into(), vec![], facts(false, true));
        assert!(!down.available);
    }

    /// Provenance rides every node entry (design #293 §7/§8). A worker that has
    /// never reported carries `seed` with no observed-at — the chip that would
    /// have read "air — 2 slots from boot seed, node never reported" from the
    /// first minute of the 2026-07-26 incident. A docker-endpoint node carries
    /// no provenance at all, because `DOCKER_NODES` still owns its capacity.
    #[test]
    fn capacity_provenance_rides_the_snapshot() {
        let at = chrono::Utc::now();
        let never_reported = fleet_node(
            "air".into(),
            vec![],
            NodeFacts {
                capacity: Some(types::worker::ObservedCapacity::default()),
                ..facts(true, true)
            },
        );
        assert_eq!(
            never_reported.capacity_source,
            Some(types::worker::CapacitySource::Seed)
        );
        assert_eq!(never_reported.capacity_observed_at, None);

        let reported = fleet_node(
            "nuc".into(),
            vec![],
            NodeFacts {
                capacity: Some(types::worker::ObservedCapacity {
                    mark: (1_000, 2),
                    slots_max: Some(6),
                    observed_at: Some(at),
                }),
                ..facts(true, true)
            },
        );
        assert_eq!(
            reported.capacity_source,
            Some(types::worker::CapacitySource::Node)
        );
        assert_eq!(reported.capacity_observed_at, Some(at));

        // Docker endpoint: no observation, so no provenance to claim.
        let docker = fleet_node("local".into(), vec![], facts(true, true));
        assert_eq!(docker.capacity_source, None);
    }

    /// Intent rides the snapshot for display and never touches `slots` (design
    /// #293 §2): a node the operator asked for 8 on, which the daemon refused,
    /// still reports the observed number the scheduler is using, with the ask and
    /// the refusal alongside it.
    #[test]
    fn intent_rides_the_snapshot_beside_the_observed_number() {
        let refused = fleet_node(
            "air".into(),
            vec![],
            NodeFacts {
                intent: Some(types::NodeCapacityDisplay {
                    slots_desired: 8,
                    state: types::CapacityState::Rejected,
                    note: Some("node max is 4".into()),
                }),
                ..facts(true, true)
            },
        );
        assert_eq!(
            refused.slots,
            Some(4),
            "the scheduler's number is untouched"
        );
        assert_eq!(refused.slots_desired, Some(8));
        assert_eq!(refused.capacity_state, Some(types::CapacityState::Rejected));
        assert_eq!(refused.capacity_note.as_deref(), Some("node max is 4"));

        // No intent for the node: nothing to display, and no state to invent.
        let untouched = fleet_node("nuc".into(), vec![], facts(true, true));
        assert_eq!(untouched.slots_desired, None);
        assert_eq!(untouched.capacity_state, None);
    }
}

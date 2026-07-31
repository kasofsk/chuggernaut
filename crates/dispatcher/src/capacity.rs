//! Operator capacity **intent** and the bounded reconciliation that re-asserts it
//! (spec §3.1 operator capacity control, design #293 §2/§3/§4).
//!
//! The node owns its capacity and the scheduler reads that one observed number
//! (`worker::backend`). What lives here is the other half: what the *operator*
//! asked for, persisted as intent in the `platform` bucket under `fleet.capacity`
//! so a daemon restart or a `worker-refresh.sh` swap cannot silently revert it.
//!
//! - **Accepts:** `Msg::SetNodeCapacity` (the `req.fleet.capacity.set` command),
//!   `Msg::CapacityPushed` (one push's reply, from the spawned RPC), the scan
//!   tick, and a worker announce that moved a node's observed capacity.
//! - **Emits:** the persisted [`types::FleetCapacity`] record, `set_slots` pushes
//!   through [`container::ContainerBackend::set_node_slots`] (spawned, never
//!   awaited on the actor turn), and the per-node
//!   [`types::NodeCapacityDisplay`] the fleet snapshot shows.
//! - **Guarantees:** **no placement path ever reads intent** — the record is
//!   private to this module, every read goes through one accessor, and that
//!   accessor panics while a [`PlacementGuard`] is open. Each dispatcher path
//!   that decides to launch opens one at its top and closes it by handing it to
//!   [`Core::place_container`], so a future change that consults intent anywhere
//!   between the decision and the launch fails a test rather than waiting for a
//!   reviewer. Reconciliation is bounded: at most one push per node per tick, an
//!   explicit rejection is terminal for the value that was refused, and a node
//!   the roster no longer holds is warned about rather than pushed to. The actor
//!   never blocks on a node RPC.
//! - **Spec:** §3.1; design #293 §2 (intent), §3 (the command path), §4
//!   (persistence across a daemon restart).

use crate::core::{Core, Msg, Result};
use crate::platform_ops::fleet;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use types::{CapacityState, NodeCapacityIntent};

/// How long a pushed-but-unobserved value waits before it reads as
/// `unacknowledged` rather than `pending` (design #293 §10). Convergence is
/// normally visible in the next fleet snapshot — within a second — so minutes here
/// name a node that is not converging at all: an old build that ignores
/// `set_slots`, or one that adopts and reverts. It must never read as converged.
pub const UNACKNOWLEDGED_AFTER: std::time::Duration = std::time::Duration::from_secs(3 * 60);

/// The operator's desired capacity per node, and the only way to read it.
///
/// The record itself is private: every read goes through [`Self::nodes`], which
/// refuses while a placement window is open, so the module boundary plus that
/// check is what makes the design's invariant executable. Both public readers are
/// named for their consumer — there are exactly two (design #293 §2), and adding
/// a third should be a deliberate act with a name attached.
#[derive(Debug, Default)]
pub struct CapacityIntent {
    record: types::FleetCapacity,
    /// Placement windows currently open (see [`PlacementGuard`]). Shared with the
    /// guards themselves, because a launch path holds `&mut Core` across its
    /// window and so cannot also lend the guard a borrow of this struct.
    /// `Ordering::Relaxed` is right: the single writer is the only thread that
    /// opens, closes, or reads it.
    placements: Arc<AtomicU32>,
}

/// An open **placement window**: from the moment a dispatcher path decides to
/// launch a container to the launch itself. Reading intent while one is open
/// panics, at the offending read.
///
/// This is design #293 §2's invariant made executable. Intent is what the
/// operator *asked for*; placement reads the node's *observed* capacity and
/// nothing else. The window deliberately spans the whole decision — the
/// admission check, the config build, the launch — because that is where a
/// plausible future violation lives ("skip placing on a node whose intent is 0"),
/// not in the RPC itself. Open one at the **top** of a launch-deciding function
/// and close it by handing it to [`Core::place_container`]; a guard taken on the
/// line before the launch guards nothing.
///
/// Dropping closes the window, so an early `?` return on the way to the launch
/// needs no handling.
#[derive(Debug)]
pub struct PlacementGuard {
    placements: Arc<AtomicU32>,
}

impl Drop for PlacementGuard {
    fn drop(&mut self) {
        self.placements.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A launch that has been decided and is ready to fire: what to run, and the
/// open window it was decided inside. The two travel together so a config can
/// never reach [`Core::place_container`] without the window that covers the
/// decision that built it.
pub(crate) struct DecidedLaunch {
    pub config: container::ContainerLaunchConfig,
    pub placement: PlacementGuard,
}

impl CapacityIntent {
    /// Rehydrate from the persisted record at startup (design #293 §4: the
    /// dispatcher re-pushes intent, so it has to survive its own restart too).
    pub fn restored(record: types::FleetCapacity) -> Self {
        Self {
            record,
            placements: Arc::default(),
        }
    }

    /// Open a placement window. See [`PlacementGuard`] for where it belongs.
    pub fn placement(&self) -> PlacementGuard {
        self.placements.fetch_add(1, Ordering::Relaxed);
        PlacementGuard {
            placements: Arc::clone(&self.placements),
        }
    }

    /// The guarded read. Private, so the two consumers below are the only paths
    /// in — and the guard cannot be bypassed by a future caller reaching for the
    /// map directly.
    fn nodes(&self) -> &BTreeMap<String, NodeCapacityIntent> {
        assert_eq!(
            self.placements.load(Ordering::Relaxed),
            0,
            "a placement path read capacity intent. Intent is what the operator ASKED \
             for, never a scheduling input (spec §3.1 slot source, design #293 §2): \
             placement reads the node's OBSERVED capacity and nothing else."
        );
        &self.record.nodes
    }

    /// Consumer 1 of 2: the §4 reconciler, which re-asserts every node's ask.
    pub fn for_reconcile(&self) -> Vec<(String, u32)> {
        self.nodes()
            .iter()
            .map(|(name, intent)| (name.clone(), intent.slots))
            .collect()
    }

    /// Consumer 2 of 2: the UI's "desired" display, via the fleet snapshot.
    pub fn for_display(&self) -> Vec<(String, u32)> {
        self.nodes()
            .iter()
            .map(|(name, intent)| (name.clone(), intent.slots))
            .collect()
    }

    /// The record a `set` would produce, without installing it. The writer's own
    /// copy-on-write — deliberately not a [`Self::nodes`] read and deliberately
    /// private, so persistence cannot become a third consumer: the caller persists
    /// this and then [`Self::commit`]s it, and a failed write leaves memory
    /// untouched.
    fn staged(&self, node: &str, slots: u32, by: &str, at: DateTime<Utc>) -> types::FleetCapacity {
        let mut record = self.record.clone();
        record.nodes.insert(
            node.to_string(),
            NodeCapacityIntent {
                slots,
                set_by: by.to_string(),
                set_at: at,
            },
        );
        record
    }

    /// Install a staged record once it is durable.
    fn commit(&mut self, record: types::FleetCapacity) {
        self.record = record;
    }
}

/// The dispatcher's memory of what it has pushed to one node, and what came back
/// (design #293 §4). One record per node, retired wholesale when the operator
/// changes their ask — which is what makes a rejection terminal for the value
/// that was refused and never for the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRecord {
    /// The value every push counted here carried.
    pub slots: u32,
    /// When the first push of this value went out — the clock the
    /// `unacknowledged` display runs on.
    pub first_pushed_at: DateTime<Utc>,
    /// Pushes sent for this value. Never a bound on its own: intent must keep
    /// being re-asserted for as long as it diverges, so the bound is the *rate*
    /// (one per node per tick), plus a rejection being terminal. It is what the
    /// `unacknowledged` warning reports, which is the question an operator
    /// actually has about a node that is not converging: how long, and how often.
    pub attempts: u32,
    /// A push is out and its reply has not landed. Stops a second push racing it.
    pub in_flight: bool,
    /// The node's refusal of this value, with the reason it gave.
    pub rejected: Option<String>,
}

impl PushRecord {
    fn first(slots: u32, at: DateTime<Utc>) -> Self {
        Self {
            slots,
            first_pushed_at: at,
            attempts: 0,
            in_flight: false,
            rejected: None,
        }
    }
}

/// What one `set_slots` push came back with. A refusal is a *reply*, not an
/// error: the node answered the question the operator asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// The node adopted `slots` and bumped its generation; its announce carries
    /// the observation along right behind this.
    Accepted { slots: u32 },
    /// Above the node's ceiling. Terminal for this value.
    Refused { note: String },
    /// The RPC failed or timed out — retried on a later tick, like any other
    /// divergence.
    Failed { error: String },
}

/// The reconciler's verdict for one node on one tick, and the state the UI shows
/// for it. One function so the two can never disagree: a node the operator sees
/// as `rejected` is exactly one the dispatcher has stopped pushing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapacityDecision {
    pub state: CapacityState,
    /// Send `set_slots` for this node now. At most one per node per tick, because
    /// the reconciler decides each node exactly once per pass and an in-flight
    /// push suppresses the next.
    pub push: bool,
}

/// Decide one node's reconciliation (design #293 §4), pure over its inputs so
/// every branch is unit-tested without a fleet, a daemon, or a scan tick.
///
/// `observed` is what the node has actually *reported*, not the `DOCKER_NODES`
/// boot seed — `None` means it has never reported, in which case intent is
/// asserted rather than assumed satisfied.
///
/// `in_fleet` is whether the roster still holds the node. Intent deliberately
/// outlives a node leaving the fleet — that is what intent is *for*, and a
/// dynamic node that has not re-announced since a dispatcher restart must not
/// lose the operator's ask — but a node the fleet no longer holds must not be
/// spent an RPC per tick either.
///
/// The order of the tests is load-bearing:
///
/// 1. **Convergence is a fact and beats every memory**, including a recorded
///    rejection: a node whose ceiling was raised out of band and which now
///    reports the desired number really has converged.
/// 2. A node **not in the fleet** is remembered, not pushed to.
/// 3. A **rejection is terminal** for the value that was refused — otherwise a
///    node whose maximum dropped gets pushed a number it refuses forever.
/// 4. A push already **in flight** is not duplicated.
/// 5. Otherwise re-assert, and once the wait passes [`UNACKNOWLEDGED_AFTER`] say
///    so: a node that silently ignores the op must surface as `unacknowledged`,
///    never as converged.
pub fn decide(
    desired: u32,
    observed: Option<u32>,
    in_fleet: bool,
    ledger: Option<&PushRecord>,
    now: DateTime<Utc>,
) -> CapacityDecision {
    let settled = |state| CapacityDecision { state, push: false };
    if observed == Some(desired) {
        return settled(CapacityState::Converged);
    }
    if !in_fleet {
        return settled(CapacityState::Unacknowledged);
    }
    let ledger = ledger.filter(|l| l.slots == desired);
    if let Some(l) = ledger {
        if l.rejected.is_some() {
            return settled(CapacityState::Rejected);
        }
        if l.in_flight {
            return settled(CapacityState::Pending);
        }
    }
    let waited = ledger.is_some_and(|l| {
        (now - l.first_pushed_at).to_std().unwrap_or_default() >= UNACKNOWLEDGED_AFTER
    });
    CapacityDecision {
        state: if waited {
            CapacityState::Unacknowledged
        } else {
            CapacityState::Pending
        },
        push: true,
    }
}

/// The node's own reported slot count, or `None` when it has never reported one.
///
/// Deliberately not `NodeStatus::slots`, which falls back to the `DOCKER_NODES`
/// boot seed before the node's first observation (design #293 §7): reconciling
/// against a seed nothing confirmed would call a node converged on a number it
/// may not be running.
fn observed_slots(live: &[container::NodeStatus], node: &str) -> Option<u32> {
    let status = live.iter().find(|s| s.name == node)?;
    if status.capacity.is_some_and(|c| c.observed_at.is_some()) {
        status.slots
    } else {
        None
    }
}

impl Core {
    /// Load persisted intent at startup (design #293 §4). Best-effort: an absent
    /// key is the normal case (no operator has ever set capacity) and a bucket the
    /// process cannot read yet must not stop the dispatcher booting — but it is
    /// logged, because starting with empty intent means the reconciler will not
    /// re-assert anything.
    pub(crate) async fn load_capacity_intent(&mut self) {
        let loaded = match self.store.raw_bucket(store::buckets::PLATFORM).await {
            Ok(bucket) => {
                bucket
                    .get_json::<types::FleetCapacity>(fleet::CAPACITY_KEY)
                    .await
            }
            Err(e) => Err(e),
        };
        match loaded {
            Ok(Some(record)) => {
                tracing::info!(
                    nodes = record.nodes.len(),
                    "loaded operator capacity intent (spec §3.1 operator capacity control)"
                );
                self.capacity_intent = CapacityIntent::restored(record);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                "capacity intent unreadable — starting with none, so nothing will be \
                 re-asserted until an operator sets it again: {e}"
            ),
        }
    }

    /// `req.fleet.capacity.set` (design #293 §3): record the operator's ask, then
    /// command the node. Persisting first is what makes the ask survive the
    /// dispatcher; the RPC is spawned, because the actor is single-threaded by
    /// design and must never block on a node.
    ///
    /// A capacity edit against a docker-endpoint node is a **409**, not a silent
    /// no-op: `DOCKER_NODES` owns those numbers outright (design #293 §7), and a
    /// control that quietly does nothing is the failure class this whole design
    /// exists to remove.
    pub(crate) async fn set_node_capacity(
        &mut self,
        node: String,
        slots: u32,
        by: String,
    ) -> Result<types::NodeCapacityAck> {
        self.check_capacity_target(&node)?;
        let at = Utc::now();
        let staged = self.capacity_intent.staged(&node, slots, &by, at);
        self.persist_capacity_intent(&staged).await?;
        self.capacity_intent.commit(staged);
        self.capacity_pushes.remove(&node);
        tracing::info!(
            node = %node,
            slots,
            by = %by,
            "operator set worker capacity intent (spec §3.1 operator capacity control)"
        );
        self.push_node_capacity(&node, slots, at);
        let observed = observed_slots(&self.backend.fleet_status(), &node);
        let decision = decide(slots, observed, true, self.capacity_pushes.get(&node), at);
        Ok(types::NodeCapacityAck {
            node,
            desired: slots,
            observed,
            state: decision.state,
        })
    }

    /// 404 for a node the fleet does not hold, 409 for a docker-endpoint one.
    fn check_capacity_target(&self, node: &str) -> Result<()> {
        let Some(entry) = self.fleet_roster.iter().find(|n| n.name == node) else {
            return Err(crate::core::CoreError::NotFound(format!(
                "unknown fleet node {node}"
            )));
        };
        if entry.endpoint != worker::backend::WORKER_ENDPOINT {
            return Err(crate::core::CoreError::Conflict(format!(
                "node {node} is a docker endpoint — its capacity is DOCKER_NODES config \
                 and only a dispatcher restart can change it (spec §3.1)"
            )));
        }
        Ok(())
    }

    /// Write the intent record. The dispatcher is its single writer, like every
    /// other platform record.
    async fn persist_capacity_intent(&self, record: &types::FleetCapacity) -> Result<()> {
        self.store
            .raw_bucket(store::buckets::PLATFORM)
            .await?
            .put_json(fleet::CAPACITY_KEY, record)
            .await?;
        Ok(())
    }

    /// Re-assert intent against what the fleet reports (design #293 §4), on the
    /// scan tick. Bounded on every axis: one decision per node per pass, one push
    /// per node per tick, a rejection terminal, no push at all to a node the
    /// roster no longer holds, and both side tables pruned to nodes that still
    /// have intent so neither can outgrow the roster.
    pub(crate) fn reconcile_capacity_intent(&mut self) {
        let now = Utc::now();
        let live = self.backend.fleet_status();
        let intent = self.capacity_intent.for_reconcile();
        for (node, desired) in &intent {
            let (node, desired) = (node.as_str(), *desired);
            let in_fleet = self.fleet_holds(node);
            let decision = decide(
                desired,
                observed_slots(&live, node),
                in_fleet,
                self.capacity_pushes.get(node),
                now,
            );
            if !in_fleet {
                if self.capacity_intent_warn_due(node, now) {
                    tracing::warn!(
                        node = %node,
                        slots_desired = desired,
                        "capacity intent is held for a node the fleet no longer has — not \
                         pushing to it. It is kept so the node gets its number back if it \
                         re-announces (design #293 §4)"
                    );
                }
                continue;
            }
            if decision.state == CapacityState::Unacknowledged
                && self.capacity_intent_warn_due(node, now)
            {
                let attempts = self.capacity_pushes.get(node).map_or(0, |l| l.attempts);
                tracing::warn!(
                    node = %node,
                    slots_desired = desired,
                    attempts,
                    "worker is NOT converging on the operator's capacity — it has \
                     acknowledged nothing across every push since the ask. Check it is on a \
                     build that handles set_slots (design #293 §4)"
                );
            }
            if decision.push {
                self.push_node_capacity(node, desired, now);
            }
        }
        let with_intent: std::collections::HashSet<&str> =
            intent.iter().map(|(node, _)| node.as_str()).collect();
        self.capacity_pushes
            .retain(|node, _| with_intent.contains(node.as_str()));
        self.capacity_intent_warned_at
            .retain(|node, _| with_intent.contains(node.as_str()));
    }

    /// Does the roster still hold this node? The one definition of "in the fleet"
    /// every `decide` call site shares, so a later refinement of it (counting
    /// `available`, say) cannot land in two of them and miss the third — and so
    /// the announce path can assert the same predicate the reconciler will apply.
    ///
    /// The roster — not the backend's live status — because intent is about
    /// *membership*: a node whose daemon is momentarily down is still one the
    /// operator's number belongs to, and is still worth a push that fails loudly.
    pub(crate) fn fleet_holds(&self, node: &str) -> bool {
        self.fleet_roster.iter().any(|n| n.name == node)
    }

    /// Is a capacity-intent warning due for this node, and record that it fired.
    /// Bounded to one line per node per [`crate::scan::CAPACITY_WARN_INTERVAL`],
    /// like the §8 never-observed warning it sits beside: the reconciler runs
    /// every scan tick, and a node left in either state must not bury the log.
    fn capacity_intent_warn_due(&mut self, node: &str, now: DateTime<Utc>) -> bool {
        let due = match self.capacity_intent_warned_at.get(node) {
            None => true,
            Some(at) => {
                (now - *at).to_std().unwrap_or_default() >= crate::scan::CAPACITY_WARN_INTERVAL
            }
        };
        if due {
            self.capacity_intent_warned_at.insert(node.to_string(), now);
        }
        due
    }

    /// Reconcile one node right after an observation moved (design #293 §4): a
    /// refreshed daemon comes back on its boot `WORKER_SLOTS`, and this is what
    /// restores the operator's number without waiting out a whole scan tick. Only
    /// called when the observation actually *changed*, so a node that reports the
    /// same wrong number every 15s is pushed by the scan tick alone and the
    /// one-push-per-tick bound holds.
    ///
    /// The window between the swap and this push is a real over- or under-cap of a
    /// few seconds, and the design accepts it: `worker-refresh.sh` deliberately
    /// carries `WORKER_SLOTS` forward so a node whose dispatcher is down still
    /// boots at a sane number. Closing it is not attempted here.
    pub(crate) fn reconcile_node_capacity(&mut self, node: &str) {
        let now = Utc::now();
        let Some((_, desired)) = self
            .capacity_intent
            .for_reconcile()
            .into_iter()
            .find(|(name, _)| name == node)
        else {
            return;
        };
        let observed = observed_slots(&self.backend.fleet_status(), node);
        let in_fleet = self.fleet_holds(node);
        if decide(
            desired,
            observed,
            in_fleet,
            self.capacity_pushes.get(node),
            now,
        )
        .push
        {
            self.push_node_capacity(node, desired, now);
        }
    }

    /// Send one `set_slots`, off the actor thread. The reply comes back as
    /// [`Msg::CapacityPushed`], so the ledger is still only ever written by the
    /// single writer.
    fn push_node_capacity(&mut self, node: &str, slots: u32, now: DateTime<Utc>) {
        let Some(tx) = self.self_tx.clone() else {
            tracing::debug!(node = %node, "capacity push skipped — core not spawned");
            return;
        };
        let ledger = self
            .capacity_pushes
            .entry(node.to_string())
            .or_insert_with(|| PushRecord::first(slots, now));
        if ledger.slots != slots {
            *ledger = PushRecord::first(slots, now);
        }
        debug_assert!(
            !ledger.in_flight,
            "a second push must never race one already in flight (design #293 §4)"
        );
        ledger.in_flight = true;
        ledger.attempts += 1;
        let (backend, node) = (self.backend.clone(), node.to_string());
        tokio::spawn(async move {
            let outcome = match backend.set_node_slots(&node, slots).await {
                Ok(reply) if reply.accepted => PushOutcome::Accepted { slots: reply.slots },
                Ok(reply) => PushOutcome::Refused {
                    note: reply.note.unwrap_or_else(|| {
                        format!(
                            "node refused {slots} slots (its maximum is {})",
                            reply.slots_max
                        )
                    }),
                },
                Err(e) => PushOutcome::Failed {
                    error: e.to_string(),
                },
            };
            let _ = tx
                .send(Msg::CapacityPushed {
                    node,
                    slots,
                    outcome,
                })
                .await;
        });
    }

    /// One push's reply landed (design #293 §4). Records it in the ledger; the
    /// node's own announce is what installs the new observation, so nothing here
    /// touches capacity — this path must never become a third, unordered
    /// transport for it (spec §3.1 slot source).
    pub(crate) fn on_capacity_pushed(&mut self, node: &str, slots: u32, outcome: &PushOutcome) {
        let Some(ledger) = self.capacity_pushes.get_mut(node) else {
            return;
        };
        if ledger.slots != slots {
            return;
        }
        ledger.in_flight = false;
        match outcome {
            PushOutcome::Accepted { slots: in_force } if *in_force == slots => tracing::info!(
                node = %node,
                slots,
                "worker adopted the operator's capacity"
            ),
            PushOutcome::Accepted { slots: in_force } => tracing::warn!(
                node = %node,
                requested = slots,
                in_force,
                "worker accepted a capacity it did not adopt — re-asserting"
            ),
            PushOutcome::Refused { note } => {
                ledger.rejected = Some(note.clone());
                tracing::warn!(
                    node = %node,
                    slots,
                    note = %note,
                    "worker REFUSED the operator's capacity — not re-pushing until the \
                     operator changes it (design #293 §4)"
                );
            }
            PushOutcome::Failed { error } => tracing::warn!(
                node = %node,
                slots,
                "capacity push failed, retried on a later tick: {error}"
            ),
        }
    }

    /// Intent as the fleet snapshot displays it (design #293 §2, consumer 2 of 2):
    /// the ask, how far it is from being observed, and the daemon's reason when it
    /// refused. Resolved here so the occupancy publisher never touches the record.
    pub(crate) fn capacity_display(&self) -> BTreeMap<String, types::NodeCapacityDisplay> {
        let now = Utc::now();
        let intent = self.capacity_intent.for_display();
        if intent.is_empty() {
            return BTreeMap::new();
        }
        let live = self.backend.fleet_status();
        intent
            .into_iter()
            .map(|(node, slots_desired)| {
                let ledger = self.capacity_pushes.get(&node);
                let in_fleet = self.fleet_holds(&node);
                let decision = decide(
                    slots_desired,
                    observed_slots(&live, &node),
                    in_fleet,
                    ledger,
                    now,
                );
                let note = match decision.state {
                    CapacityState::Rejected => ledger.and_then(|l| l.rejected.clone()),
                    _ => None,
                };
                (
                    node,
                    types::NodeCapacityDisplay {
                        slots_desired,
                        state: decision.state,
                        note,
                    },
                )
            })
            .collect()
    }

    /// Open a placement window (design #293 §2). Called at the **top** of each
    /// launch-deciding path — see [`PlacementGuard`] for why the top and not the
    /// launch itself.
    pub(crate) fn placement_guard(&self) -> PlacementGuard {
        self.capacity_intent.placement()
    }

    /// The dispatcher's one placement boundary: every container launch it makes
    /// goes through here, and only with a [`PlacementGuard`] in hand, so no launch
    /// can be decided outside a window (design #293 §2). Taking the guard by value
    /// closes the window exactly here — at the launch, which is the end of the
    /// decision it guards. Agent launches go through the provider, which holds
    /// only the backend and so cannot reach intent at all.
    pub(crate) async fn place_container(
        &self,
        launch: DecidedLaunch,
    ) -> std::result::Result<container::ContainerId, container::BackendError> {
        let DecidedLaunch { config, placement } = launch;
        let placed = self.backend.launch(config).await;
        drop(placement);
        placed
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_769_000_000 + minutes * 60, 0).unwrap()
    }

    fn pushed(slots: u32, minutes: i64) -> PushRecord {
        PushRecord {
            attempts: 1,
            ..PushRecord::first(slots, at(minutes))
        }
    }

    /// The reconciler's core case (design #293 §4): a node reporting something
    /// other than the operator's number is re-asserted, and one already reporting
    /// it is left alone. A node that has never reported is asserted too — the
    /// `DOCKER_NODES` seed is not an observation, so "unknown" must not read as
    /// satisfied.
    #[test]
    fn divergence_pushes_and_convergence_does_not() {
        assert_eq!(
            decide(2, Some(4), true, None, at(0)),
            CapacityDecision {
                state: CapacityState::Pending,
                push: true
            }
        );
        assert_eq!(
            decide(2, Some(2), true, None, at(0)),
            CapacityDecision {
                state: CapacityState::Converged,
                push: false
            }
        );
        assert_eq!(
            decide(2, None, true, None, at(0)),
            CapacityDecision {
                state: CapacityState::Pending,
                push: true
            },
            "never observed is not converged — assert the operator's number"
        );
        assert!(!decide(0, Some(0), true, None, at(0)).push);
        assert!(decide(0, Some(1), true, None, at(0)).push);
    }

    /// One push per node per tick: while a push is out, the next pass decides
    /// `pending` and sends nothing — the bound that keeps a node which adopts and
    /// reverts from being hammered every tick.
    #[test]
    fn in_flight_push_suppresses_the_next() {
        let mut ledger = pushed(2, 0);
        ledger.in_flight = true;
        assert_eq!(
            decide(2, Some(4), true, Some(&ledger), at(0)),
            CapacityDecision {
                state: CapacityState::Pending,
                push: false
            }
        );
        ledger.in_flight = false;
        assert!(decide(2, Some(4), true, Some(&ledger), at(0)).push);
    }

    /// A rejection is terminal (design #293 §4): the dispatcher stops re-pushing a
    /// number the node refused and surfaces the refusal, because otherwise a node
    /// whose maximum dropped would be pushed a value it refuses forever.
    #[test]
    fn rejection_is_terminal_until_the_operator_changes_it() {
        let mut refused = pushed(8, 0);
        refused.rejected = Some("node max is 4".into());
        let decision = decide(8, Some(4), true, Some(&refused), at(60));
        assert_eq!(decision.state, CapacityState::Rejected);
        assert!(!decision.push, "a refused number is never re-pushed");

        assert!(!decide(8, Some(4), true, Some(&refused), at(10_000)).push);

        let next = decide(4, Some(2), true, Some(&refused), at(60));
        assert_eq!(next.state, CapacityState::Pending);
        assert!(next.push);

        assert_eq!(
            decide(8, Some(8), true, Some(&refused), at(60)).state,
            CapacityState::Converged
        );
    }

    /// A node that silently ignores `set_slots` (an old build, or one that adopts
    /// and reverts) must surface as `unacknowledged` — never as converged, and
    /// never as an indefinite `pending` that reads like progress.
    #[test]
    fn silent_node_surfaces_as_unacknowledged() {
        let ledger = pushed(4, 0);
        assert_eq!(
            decide(4, Some(2), true, Some(&ledger), at(1)).state,
            CapacityState::Pending
        );
        let stale = decide(
            4,
            Some(2),
            true,
            Some(&ledger),
            at(0) + chrono::Duration::from_std(UNACKNOWLEDGED_AFTER).unwrap(),
        );
        assert_eq!(stale.state, CapacityState::Unacknowledged);
        assert!(
            stale.push,
            "unacknowledged still re-asserts — the bound is the rate, not a count"
        );
        assert_ne!(
            decide(4, Some(2), true, Some(&ledger), at(10_000)).state,
            CapacityState::Converged
        );
    }

    /// Intent outlives the node (design #293 §4): the operator's ask is kept so a
    /// node that re-announces gets its number back, but a node the roster no
    /// longer holds is never pushed to — otherwise a decommissioned name burns one
    /// RPC per scan tick forever, since intent is the one table that never shrinks.
    #[test]
    fn a_node_the_fleet_no_longer_holds_is_remembered_not_pushed() {
        let gone = decide(2, None, false, None, at(0));
        assert!(!gone.push, "a node not in the fleet is never pushed to");
        assert_eq!(
            gone.state,
            CapacityState::Unacknowledged,
            "intent recorded, node not converging — which is what the operator sees"
        );
        assert!(!decide(2, None, false, Some(&pushed(2, 0)), at(10_000)).push);
        assert!(decide(2, None, true, None, at(0)).push);
    }

    /// The two consumers named in design #293 §2 read the record, and the writer's
    /// own copy-on-write does not — so a placement window has something real to
    /// catch rather than passing by never being exercised.
    #[test]
    fn the_two_consumers_read_and_the_writer_does_not() {
        let mut intent = CapacityIntent::default();
        let staged = intent.staged("air", 2, "operator@example.com", at(0));
        let window = intent.placement();
        intent.commit(staged);
        drop(window);

        assert_eq!(intent.for_reconcile(), vec![("air".to_string(), 2)]);
        assert_eq!(intent.for_display(), vec![("air".to_string(), 2)]);
    }

    /// The invariant, executable (design #293 §2): reading intent anywhere inside
    /// a placement window panics, at the read. This is what turns "no placement
    /// path reads `fleet.capacity`" from a review rule into a failing test — the
    /// window each launch path opens spans its whole decision, so the realistic
    /// violation ("skip a node whose intent is 0" in an admission check, or an
    /// intent read while building the launch config) lands inside it.
    #[test]
    #[should_panic(expected = "a placement path read capacity intent")]
    fn an_admission_check_that_reads_intent_panics() {
        let intent = CapacityIntent::default();
        let _placement = intent.placement();
        let _ = intent.for_reconcile();
    }

    /// The display consumer is caught the same way — the guard is on the record,
    /// not on one accessor, so a third reader added later is covered by default.
    #[test]
    #[should_panic(expected = "a placement path read capacity intent")]
    fn a_launch_config_that_reads_intent_panics() {
        let intent = CapacityIntent::default();
        let _placement = intent.placement();
        let _ = intent.for_display();
    }

    /// The window closes when the guard drops — including on the `?` early return
    /// a launch path takes when it never reaches the launch — so the reconciler's
    /// own reads on the very next tick are unaffected.
    #[test]
    fn a_closed_window_lets_the_reconciler_read_again() {
        let intent = CapacityIntent::default();
        {
            let _placement = intent.placement();
        }
        assert!(intent.for_reconcile().is_empty());
        let outer = intent.placement();
        drop(intent.placement());
        drop(outer);
        assert!(intent.for_display().is_empty());
    }

    /// Reconciliation compares against what the node *reported*, never the
    /// `DOCKER_NODES` seed (design #293 §7): a seed-sourced number nothing
    /// confirmed must read as "not observed", or a node would be called converged
    /// on a value it may not be running.
    #[test]
    fn only_a_real_report_counts_as_observed() {
        let node = |capacity| container::NodeStatus {
            name: "air".into(),
            available: true,
            version: None,
            refresh_outcome: None,
            slots: Some(2),
            capacity,
        };
        let seeded = [node(Some(types::ObservedCapacity::default()))];
        assert_eq!(observed_slots(&seeded, "air"), None);

        let reported = [node(Some(types::ObservedCapacity {
            mark: (1_000, 1),
            slots_max: Some(6),
            observed_at: Some(at(0)),
        }))];
        assert_eq!(observed_slots(&reported, "air"), Some(2));

        assert_eq!(observed_slots(&[node(None)], "air"), None);
        assert_eq!(observed_slots(&reported, "nuc"), None);
    }
}

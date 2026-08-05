//! In-memory FIFO of Ready jobs (spec §3.1 step 5), plus the §3.5 launch
//! queue's *pure* half — the drain-priority classification and the max-wait
//! budget arithmetic the dispatcher's `launch_queue` decides with. Both queues
//! live inside the core task; rebuilt on restart by the reconciliation scan, so
//! neither is persisted.
//!
//! - **Accepts:** Ready job IDs enqueued by `core`; dequeue on launch. For the
//!   launch queue: a task phase to classify, and a clock to measure a wait.
//! - **Emits:** the next Ready job ID in FIFO order; a [`LaunchPriority`]; a
//!   waited-for [`std::time::Duration`] and the expiry verdict against a budget.
//! - **Guarantees:** holds only Ready jobs; lives in the actor, never
//!   persisted; rebuilt from KV on restart. Every wait is measured against a
//!   bound (docs/reference/style.md Tier 2 #3) — the budget is [`QueuedLaunch::is_expired`]'s
//!   argument, never an implicit constant here.
//! - **Spec:** §3.1 step 5, §3.5.

use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use std::time::Duration;
use types::TaskPhase;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueuedJob {
    pub owner: String,
    pub project: String,
    pub seq: u64,
}

/// Priority class of a deferred launch (spec §3.5, job #140). A finishing-phase
/// launch (evaluation, merge gate, wrap-up) drains **ahead of** a queued work
/// launch: completing an in-flight job frees fleet capacity fastest and bounds
/// work-in-progress, so a job that has finished its work never loses its
/// evaluation slot to one that has not started. Ordered so the higher priority
/// sorts first (`Finishing < Work`); FIFO within a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LaunchPriority {
    /// Evaluation / merge gate / wrap-up — drains before work.
    Finishing,
    /// Work — drains only once every finishing launch has a slot.
    Work,
}

/// A container launch deferred because the fleet had no free slot (spec §3.5).
/// The task record is already persisted `Pending`; this holds its coordinates
/// so the launch can be re-attempted when a slot frees (a running container
/// exits) or by the periodic sweep. Ordered by [`LaunchPriority`] then FIFO —
/// eval/wrap-up ahead of work (#140) — and no retry budget is consumed while it
/// waits. In-memory like the Ready queue; restart reconciliation re-queues any
/// `Pending`, container-less command task it finds (§3.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedLaunch {
    pub owner: String,
    pub project: String,
    pub seq: u64,
    pub task_id: u64,
    /// Drain-order class (#140): finishing-phase launches jump ahead of work.
    pub priority: LaunchPriority,
    /// When the launch first joined the queue — the anchor for the maximum
    /// queue-wait backstop (§3.5). Preserved across re-queue attempts so a
    /// launch that keeps missing a slot still escalates on time.
    pub queued_at: DateTime<Utc>,
}

/// The drain-priority class of a launch by its phase (spec §3.5, job #140):
/// evaluation, merge gate, and wrap-up are finishing-phase launches that jump
/// ahead of queued work.
pub fn launch_priority(phase: TaskPhase) -> LaunchPriority {
    match phase {
        TaskPhase::Work => LaunchPriority::Work,
        _ => LaunchPriority::Finishing,
    }
}

impl QueuedLaunch {
    /// How long this launch has waited for a free slot (spec §3.5), measured
    /// from the *persisted* enqueue time so the wait accumulates across
    /// dispatcher restarts. A clock that ran backwards (NTP step) reads as no
    /// wait rather than a negative one, which keeps the backstop monotone.
    pub fn waited(&self, now: DateTime<Utc>) -> Duration {
        (now - self.queued_at).to_std().unwrap_or_default()
    }

    /// Has this launch outwaited the queue's max-wait budget (spec §3.5)? The
    /// budget is an argument, not a constant here: the dispatcher's default is
    /// configurable per deployment, and every wait in this codebase is bounded
    /// by an explicit one (docs/reference/style.md Tier 2 #3).
    pub fn is_expired(&self, now: DateTime<Utc>, max_wait: Duration) -> bool {
        self.waited(now) > max_wait
    }
}

#[derive(Default)]
pub struct ReadyQueue {
    queue: VecDeque<QueuedJob>,
}

impl ReadyQueue {
    pub fn enqueue(&mut self, job: QueuedJob) {
        if !self.queue.contains(&job) {
            self.queue.push_back(job);
        }
    }

    pub fn dequeue(&mut self) -> Option<QueuedJob> {
        self.queue.pop_front()
    }

    pub fn remove(&mut self, job: &QueuedJob) {
        self.queue.retain(|j| j != job);
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Read-only iteration over the queued jobs, in FIFO order. Used by the
    /// invariant checker (spec §3.1); the queue is otherwise mutated only
    /// through `enqueue`/`dequeue`/`remove`.
    pub fn iter(&self) -> impl Iterator<Item = &QueuedJob> {
        self.queue.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Tier-1 coverage of the queue arithmetic the dispatcher's `launch_queue`
    //! decides with (refactor-plan C4): pure values in, pure values out.
    use super::*;

    fn at(ts: &str) -> DateTime<Utc> {
        ts.parse().expect("timestamp")
    }

    fn queued(phase: TaskPhase, queued_at: &str) -> QueuedLaunch {
        QueuedLaunch {
            owner: "acme".into(),
            project: "api".into(),
            seq: 7,
            task_id: 2,
            priority: launch_priority(phase),
            queued_at: at(queued_at),
        }
    }

    /// Only Work is work; every finishing phase drains ahead of it (#140), and
    /// `Finishing < Work` is what makes the priority sort do that.
    #[test]
    fn finishing_phases_outrank_work() {
        for phase in [
            TaskPhase::Evaluation,
            TaskPhase::MergeGate,
            TaskPhase::WrapUp,
            TaskPhase::Triage,
            TaskPhase::Escalation,
        ] {
            assert_eq!(
                launch_priority(phase),
                LaunchPriority::Finishing,
                "{phase:?}"
            );
        }
        assert_eq!(launch_priority(TaskPhase::Work), LaunchPriority::Work);
        assert!(LaunchPriority::Finishing < LaunchPriority::Work);
    }

    /// The wait accumulates from the persisted enqueue time, and the budget is
    /// a strict bound — exactly at it is not yet expired.
    #[test]
    fn expiry_is_measured_against_the_budget() {
        let q = queued(TaskPhase::Work, "2026-07-24T10:00:00Z");
        let budget = Duration::from_secs(30 * 60);
        assert_eq!(q.waited(at("2026-07-24T10:20:00Z")), budget / 3 * 2);
        assert!(
            !q.is_expired(at("2026-07-24T10:30:00Z"), budget),
            "at the bound"
        );
        assert!(q.is_expired(at("2026-07-24T10:30:01Z"), budget), "past it");
    }

    /// A clock that stepped backwards reads as no wait, never a negative one:
    /// the backstop must not fire early because NTP moved.
    #[test]
    fn a_backwards_clock_reads_as_no_wait() {
        let q = queued(TaskPhase::WrapUp, "2026-07-24T10:00:00Z");
        assert_eq!(q.waited(at("2026-07-24T09:00:00Z")), Duration::ZERO);
        assert!(!q.is_expired(at("2026-07-24T09:00:00Z"), Duration::from_secs(1)));
    }
}

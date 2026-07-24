//! In-memory FIFO of Ready jobs (spec §3.1 step 5). Lives inside the core
//! task; rebuilt on restart by the reconciliation scan, so it is never
//! persisted.
//!
//! - **Accepts:** Ready job IDs enqueued by `core`; dequeue on launch.
//! - **Emits:** the next Ready job ID in FIFO order.
//! - **Guarantees:** holds only Ready jobs; lives in the actor, never
//!   persisted; rebuilt from KV on restart.
//! - **Spec:** §3.1 step 5.

use chrono::{DateTime, Utc};
use std::collections::VecDeque;

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

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

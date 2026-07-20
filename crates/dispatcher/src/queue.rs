//! In-memory FIFO of Ready jobs (spec §3.1 step 5). Lives inside the core
//! task; rebuilt on restart by the reconciliation scan, so it is never
//! persisted.

use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueuedJob {
    pub owner: String,
    pub project: String,
    pub seq: u64,
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

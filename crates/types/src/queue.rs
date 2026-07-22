//! Capacity launch-queue snapshot for read-only display (spec §3.5).
//!
//! The launch queue lives only in the dispatcher's actor (it is re-derived from
//! the Pending task records on restart). To make a queued launch *visible* — an
//! operator watching a capacity-deferred job otherwise sees a bare Pending task
//! and nothing happening — the dispatcher serves this snapshot on demand over a
//! cheap request subject the api forwards (`GET /projects/{o}/{p}/queue`).
//!
//! The queue is one global FIFO across the whole fleet, so `depth` and each
//! entry's `position` are fleet-wide; `entries` is scoped to the requested
//! project so the response never leaks other projects' coordinates. The UI
//! derives "position N of M" directly: N = `entry.position`, M = `depth`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A point-in-time view of the capacity launch queue, scoped to one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    /// Total launches queued across the whole fleet — the "of M" in the badge.
    pub depth: usize,
    /// The requested project's queued launches, in FIFO order.
    pub entries: Vec<QueueEntry>,
}

/// One queued launch belonging to the requested project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub seq: u64,
    pub task_id: u64,
    /// 1-indexed position in the global FIFO — the "N" in the badge.
    pub position: usize,
    /// When the launch joined the queue (mirrors `Task::queued_at`).
    pub queued_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips() {
        let snap = QueueSnapshot {
            depth: 3,
            entries: vec![QueueEntry {
                seq: 7,
                task_id: 2,
                position: 2,
                queued_at: "2026-07-22T09:00:00Z".parse().unwrap(),
            }],
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert_eq!(serde_json::from_str::<QueueSnapshot>(&json).unwrap(), snap);
    }
}

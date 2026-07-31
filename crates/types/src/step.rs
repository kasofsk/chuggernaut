//! Inline review step log (spec §1.2, §4.5).
//!
//! Steps are sub-task granularity: the author↔reviewer iterations inside a
//! single work container, reported by the harness for observability. They never
//! drive dispatcher state transitions. Stored as a JSON array of records at
//! `steps.{owner}.{project}.{job_seq}.{task_id}` — one key per work task,
//! dispatcher sole writer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    /// 1-indexed within the task.
    pub step: u32,
    pub kind: StepKind,
    /// Ping-pong round, 1-indexed.
    pub iteration: u32,
    pub status: StepStatus,
    /// Inline review verdict; None for author steps and running steps.
    pub pass: Option<bool>,
    /// Inline review findings; None for author steps.
    pub findings: Option<serde_json::Value>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepKind {
    AuthorIteration,
    InlineReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Running,
    Done,
    Failed,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn step_kind_wire_format_matches_events_spec() {
        assert_eq!(
            serde_json::to_string(&StepKind::InlineReview).unwrap(),
            r#""inline-review""#
        );
        assert_eq!(
            serde_json::to_string(&StepStatus::Failed).unwrap(),
            r#""failed""#
        );
    }
}

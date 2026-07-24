//! Escalation task construction (spec §1.2, §3.4). Resolution actions land
//! with the execution slice; this module owns the task shape.
//!
//! - **Accepts:** a job needing escalation and its reason.
//! - **Emits:** the escalation `Task` shape.
//! - **Guarantees:** owns task construction only — performs no state
//!   transition (resolution actions live with `exec`).
//! - **Spec:** §1.2, §3.4.

use chrono::Utc;
use types::{Task, TaskKind, TaskPhase, TaskState};

/// Build a Human escalation task (spec §1.2, §3.4). Stamped with its own
/// `Escalation` phase — not the phase of the step that failed (job #141) — so
/// the UI renders the operator's resolution as an escalation row rather than a
/// confusing `Work · Human · pass` one. The failed phase is recorded separately
/// on the job's `Escalation` record (`failing_task` / `reason`) and drives the
/// resume-at-failed-phase Retry.
pub fn escalation_task(
    task_id: u64,
    job_seq: u64,
    project: &str,
    cycle: u32,
    prompt: String,
) -> Task {
    Task {
        id: task_id,
        job_seq,
        project: project.to_string(),
        phase: TaskPhase::Escalation,
        cycle,
        kind: TaskKind::Human { prompt },
        state: TaskState::Pending,
        // Human task: no agent, no transcript.
        session_id: None,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: None,
        rework_reason: None,
        infra_loss: false,
        pending_reason: None,
        queued_at: None,
        reviewed_tip: None,
        result: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
    }
}

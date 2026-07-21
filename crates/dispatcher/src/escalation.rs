//! Escalation task construction (spec §1.2, §3.4). Resolution actions land
//! with the execution slice; this module owns the task shape.

use chrono::Utc;
use types::{Task, TaskKind, TaskPhase, TaskState};

/// Build a Human escalation task. Pre-Work escalations (spec §1.2) use
/// cycle 1 and the phase of the step that failed — Work for everything in
/// this slice.
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
        phase: TaskPhase::Work,
        cycle,
        kind: TaskKind::Human { prompt },
        state: TaskState::Pending,
        // Human task: no agent, no transcript.
        session_id: None,
        attempt: 1,
        evaluator: None,
        stage: 0,
        container_id: None,
        result: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
    }
}

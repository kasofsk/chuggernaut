//! NATS request-reply subject construction (spec §6.1) and event subjects
//! (spec §6.3). Only the subjects that cross crate boundaries live here —
//! containers publish `req.work.submit` / `req.eval.submit` / `req.step.report`
//! via the injected binaries, the dispatcher subscribes, and the API layer
//! bridges the rest of the §6.1 surface as it gets implemented.

pub fn work_submit(owner: &str, project: &str, seq: u64) -> String {
    format!("req.work.submit.{owner}.{project}.{seq}")
}

pub fn eval_submit(owner: &str, project: &str, seq: u64, task_id: u64) -> String {
    format!("req.eval.submit.{owner}.{project}.{seq}.{task_id}")
}

/// Harness-only step reporting (spec §4.5).
pub fn step_report(owner: &str, project: &str, seq: u64, task_id: u64) -> String {
    format!("req.step.report.{owner}.{project}.{seq}.{task_id}")
}

pub fn steps_list(owner: &str, project: &str, job_seq: u64, task_id: u64) -> String {
    format!("req.steps.list.{owner}.{project}.{job_seq}.{task_id}")
}

/// Job event stream subject (spec §6.3): `job.events.{owner}.{project}.{seq}.{event_type}`.
pub fn job_event(owner: &str, project: &str, seq: u64, event_type: &str) -> String {
    format!("job.events.{owner}.{project}.{seq}.{event_type}")
}

pub fn channel_inbox(owner: &str, project: &str, seq: u64) -> String {
    format!("channel.inbox.{owner}.{project}.{seq}")
}

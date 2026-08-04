//! Escalation decisions (spec §1.2, §3.4) — the C1 template decider.
//!
//! One pure function decides both §1.2 escalation shapes: post-work
//! **escalate** (job → `Escalated`) and its pre-work counterpart **stall**
//! (job → `Stalled`, used when no work task exists: Ready-transition
//! re-validation failure, or a `job_deadline` elapsing while still Ready).
//! The two are structural twins — create the Human task, stamp WHY on the
//! job, flip the state, announce it — so one decider owns the family and
//! [`EscalationKind`] picks the target. Resolution actions land with the
//! execution slice; this module owns the decision shape only.
//!
//! - **Accepts:** an [`EscalationView`] (the target job plus pre-read
//!   scalars) and an [`EscalationEvent`].
//! - **Emits:** one [`Transition`] (the WHY-stamped job → `Escalated` or
//!   `Stalled`) and the effects `[PutTask, PublishEvent]`, in that order.
//! - **Guarantees:** pure — no I/O, no clock, no id allocation; every input
//!   the decision needs arrives in the view. Performs no effect (STYLE.md
//!   Tier 2 #1).
//! - **Spec:** §1.2, §3.4.

use crate::decide::Transition;
use crate::effects::Effect;
use chrono::{DateTime, Utc};
use types::{Job, JobState, Task, TaskKind, TaskPhase, TaskState};

/// The read-only inputs an escalation decision consumes: the target job and
/// the values the shim pre-reads (reads feed the view — they are not
/// effects). This is the narrowest honest view for the phase; wider phases
/// grow wider views from the same seam.
pub struct EscalationView<'a> {
    /// The job being escalated or stalled.
    pub job: &'a Job,
    /// Pre-allocated id for the Human task (§1.2 sequential-within-job).
    pub next_task_id: u64,
    /// The exec cycle the failure happened in; 1 pre-work, where no exec
    /// state exists.
    pub cycle: u32,
    /// The decision moment — stamped on both the escalation record and the
    /// task, so the two can never disagree about when.
    pub now: DateTime<Utc>,
}

/// Which §1.2 escalation shape the event asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationKind {
    /// Post-work: the job holds (or held) a work attempt; target `Escalated`.
    Escalate,
    /// Pre-work: no work task exists; target `Stalled`. The operator resolves
    /// Retry (re-run the failed step) or Revoke only.
    Stall,
}

/// The driving event: why the job needs a human.
#[derive(Debug)]
pub struct EscalationEvent {
    pub kind: EscalationKind,
    /// Machine reason code (also the published event's `reason`).
    pub reason: String,
    /// Human-readable explanation, shown in the intervention task and
    /// mirrored onto the job's [`types::Escalation`] record.
    pub detail: String,
    /// The task whose failure triggered this, when one exists.
    pub failing_task: Option<u64>,
}

/// Decide an escalation (spec §1.2): returns the transition — the job with
/// WHY stamped on it (#69), moving to `Escalated`/`Stalled` — and the effects
/// `[PutTask(Human escalation task), PublishEvent(job-escalated|job-stalled)]`.
///
/// The shim applies the transition first (the §2.1 record is the committed
/// decision), then the effects; a crash between the two is healed by restart
/// reconciliation re-creating the task from the stamped record.
pub fn decide(view: &EscalationView<'_>, event: EscalationEvent) -> (Vec<Transition>, Vec<Effect>) {
    let job = view.job;

    debug_assert!(
        !job.state.is_terminal(),
        "escalation decided for terminal job #{} in {:?}",
        job.id,
        job.state,
    );

    let (owner, project) = job
        .project
        .split_once('/')
        .unwrap_or_else(|| panic!("malformed job project '{}'", job.project));

    let (to, event_type) = match event.kind {
        EscalationKind::Escalate => (JobState::Escalated, "job-escalated"),
        EscalationKind::Stall => (JobState::Stalled, "job-stalled"),
    };

    let task = escalation_task(
        view.next_task_id,
        job.id,
        &job.project,
        view.cycle,
        event.detail.clone(),
        view.now,
    );
    assert_eq!(task.phase, TaskPhase::Escalation, "escalation task phase");
    assert_eq!(
        task.state,
        TaskState::Pending,
        "escalation task starts Pending"
    );

    let mut stamped = job.clone();
    stamped.escalation = Some(types::Escalation {
        reason: event.reason.clone(),
        detail: event.detail,
        failing_task: event.failing_task,
        at: view.now,
    });

    let transitions = vec![Transition {
        job: Box::new(stamped),
        to,
    }];
    let effects = vec![
        Effect::PutTask {
            task: Box::new(task),
        },
        Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: job.id,
            event_type: event_type.to_string(),
            extra: serde_json::json!({ "reason": event.reason }),
        },
    ];
    (transitions, effects)
}

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
    now: DateTime<Utc>,
) -> Task {
    Task {
        id: task_id,
        job_seq,
        project: project.to_string(),
        phase: TaskPhase::Escalation,
        cycle,
        kind: TaskKind::Human { prompt },
        state: TaskState::Pending,
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
        workload_identities: vec![],
        result: None,
        created_at: now,
        started_at: None,
        completed_at: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Tier-1 coverage of the whole decision: pure values in, pure values
    //! out, no NATS/Docker. The dispatcher's golden traces pin the same
    //! decision end-to-end (`stall_on_revalidation_failure.yaml`).
    use super::*;

    fn sample_job(state: JobState) -> Job {
        let mut job: Job = serde_json::from_str(
            r#"{ "id": 7, "project": "acme/api", "type": "build",
                 "deps": [], "state": "Ready", "branch": "job/7",
                 "base_ref": null, "knowledge_tags": [], "factory": null,
                 "created_at": "2026-07-24T10:00:00Z", "ready_at": null }"#,
        )
        .expect("sample job");
        job.state = state;
        job
    }

    fn view(job: &Job) -> EscalationView<'_> {
        EscalationView {
            job,
            next_task_id: 3,
            cycle: 2,
            now: Utc::now(),
        }
    }

    fn event(kind: EscalationKind) -> EscalationEvent {
        EscalationEvent {
            kind,
            reason: "retries_exhausted".into(),
            detail: "3 failed cycles".into(),
            failing_task: Some(2),
        }
    }

    /// The full escalate decision: one transition to Escalated carrying the
    /// stamped WHY, then exactly [PutTask, PublishEvent] — in that order.
    #[test]
    fn escalate_decides_transition_then_task_then_event() {
        let job = sample_job(JobState::Work);
        let (transitions, effects) = decide(&view(&job), event(EscalationKind::Escalate));

        assert_eq!(transitions.len(), 1);
        let t = &transitions[0];
        assert_eq!(t.to, JobState::Escalated);
        assert_eq!(t.job.state, JobState::Work, "state flip is set_state's job");
        let esc = t.job.escalation.as_ref().expect("WHY stamped on the job");
        assert_eq!(esc.reason, "retries_exhausted");
        assert_eq!(esc.detail, "3 failed cycles");
        assert_eq!(esc.failing_task, Some(2));

        assert_eq!(effects.len(), 2);
        match &effects[0] {
            Effect::PutTask { task } => {
                assert_eq!(task.id, 3, "pre-read task id is used verbatim");
                assert_eq!(task.job_seq, 7);
                assert_eq!(task.cycle, 2);
                assert_eq!(task.phase, TaskPhase::Escalation);
                match &task.kind {
                    TaskKind::Human { prompt } => assert_eq!(prompt, "3 failed cycles"),
                    other => panic!("expected Human task, got {other:?}"),
                }
            }
            other => panic!("expected PutTask first, got {other:?}"),
        }
        match &effects[1] {
            Effect::PublishEvent {
                owner,
                project,
                seq,
                event_type,
                extra,
            } => {
                assert_eq!((owner.as_str(), project.as_str(), *seq), ("acme", "api", 7));
                assert_eq!(event_type, "job-escalated");
                assert_eq!(extra["reason"], "retries_exhausted");
            }
            other => panic!("expected PublishEvent second, got {other:?}"),
        }
    }

    /// The stall twin: identical shape, Stalled target, job-stalled event.
    #[test]
    fn stall_decides_the_stalled_twin() {
        let job = sample_job(JobState::Blocked);
        let (transitions, effects) = decide(&view(&job), event(EscalationKind::Stall));

        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::Stalled);
        match &effects[1] {
            Effect::PublishEvent { event_type, .. } => assert_eq!(event_type, "job-stalled"),
            other => panic!("expected PublishEvent second, got {other:?}"),
        }
    }

    /// The decision moment is stamped once: escalation record and task agree.
    #[test]
    fn escalation_record_and_task_share_the_decision_moment() {
        let job = sample_job(JobState::Work);
        let v = view(&job);
        let now = v.now;
        let (transitions, effects) = decide(&v, event(EscalationKind::Escalate));
        assert_eq!(transitions[0].job.escalation.as_ref().unwrap().at, now);
        match &effects[0] {
            Effect::PutTask { task } => assert_eq!(task.created_at, now),
            other => panic!("expected PutTask first, got {other:?}"),
        }
    }

    /// Negative space: a terminal job must never reach the decider.
    #[test]
    #[should_panic(expected = "terminal job")]
    #[cfg(debug_assertions)]
    fn escalating_a_terminal_job_is_a_caller_bug() {
        let job = sample_job(JobState::Done);
        decide(&view(&job), event(EscalationKind::Escalate));
    }
}

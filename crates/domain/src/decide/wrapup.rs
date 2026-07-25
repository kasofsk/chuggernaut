//! WrapUp-phase decisions (spec §3.2 step 12, §2.1 terminal stamping) —
//! refactor-plan C3, the second carve from `eval.rs`.
//!
//! The phase opens the moment the squash is on the default branch and closes
//! when the job is stamped terminal. One pure function owns all four of its
//! decisions, told apart by [`WrapUpEvent`]:
//!
//! - **Landed** — the single post-merge fork (§3.2). A job type declaring a
//!   `wrap_up.run` publish command (the web self-publish) holds the job in
//!   WrapUp while that command runs against merged main; a plain code job has
//!   nothing to publish and completes directly.
//! - **PublishExited** — exit 0 completes the job, any non-zero exit (a launch
//!   failure included) escalates. The squash already landed, so the merge is
//!   never undone; only the external publish failed.
//! - **RetryRequested** — the operator's Retry on a `wrap_up_failed`
//!   escalation (#141): back to WrapUp, re-run only the publish.
//! - **Completing** — the terminal bookkeeping every job reaches, whichever
//!   route it took: branch cleanup, Done, the announcement, and a batch's Done
//!   fan-out to its members (§2.1 batches).
//!
//! The `Msg` contracts this decider owns (contracts.md §1):
//!
//! - `Msg::TaskExited` for a `TaskPhase::WrapUp` task — **pre:** the task is
//!   `Running` (the shim drops stale exits) and its job is `WrapUp`;
//!   **post:** the task is terminal (`Done`/`Failed`) and the job is `Done` or
//!   `Escalated` — never still `WrapUp`, and never back to a work phase.
//! - `Msg::ResolveEscalation` with `Retry` on a WrapUp-phase failing task —
//!   **pre:** the job is `Escalated`; **post:** the job is `WrapUp` with a
//!   fresh publish attempt launched and no second squash performed.
//!
//! - **Accepts:** a [`WrapUpView`] (the target job, its batch members' records,
//!   the pre-read `wrap_up.run` presence, the clock) and a [`WrapUpEvent`].
//! - **Emits:** `(Vec<Transition>, Vec<Effect>, WrapUpStep)` — values only.
//!   The owned effect set (contracts.md §2): `LaunchWrapupTask`, `PutTask`,
//!   `DeleteBranch`, `Escalate`, and `PublishEvent` for `task-completed`,
//!   `task-failed`, `job-done` and `job-completed-via-batch`. The
//!   [`WrapUpStep`] names the dispatcher-side bookkeeping that follows —
//!   releasing the execution slice and the dependents fan-out — both of which
//!   read shell state the pure crate cannot see.
//! - **Guarantees:** pure and synchronous; every branch exhaustively matched
//!   and unit-tested; asserts negative space (STYLE.md Tier 2 #2) — never
//!   completes a terminal job, never completes a batch member twice, never
//!   decides a publish exit for a task from another phase. Performs no effect,
//!   holds no `&mut Core`.
//! - **Spec:** §3.2 step 12, §2.1 (terminal stamping, batches), §3.4;
//!   contracts.md §2; refactor-plan C3.

use crate::decide::Transition;
use crate::effects::Effect;
use chrono::{DateTime, Utc};
use types::{Job, JobState, Task, TaskPhase, TaskResult, TaskState};

/// The read-only inputs one WrapUp decision consumes. The shim re-gathers it
/// before every [`decide`] call; reads feed the view, they are not effects.
pub struct WrapUpView<'a> {
    /// The job in (or entering) WrapUp.
    pub job: &'a Job,
    /// Whether the job type declares a `wrap_up.run` publish command — the
    /// pre-read that decides the post-merge fork. False once the execution
    /// slice is gone (a restart-recovered completion has nothing to publish).
    pub publish_command: bool,
    /// The batch's member records, in `job.members` order; empty for an
    /// ordinary job. Gathered by the shim because a batch's completion
    /// transitions every member (§2.1 batches).
    pub members: &'a [Job],
    /// The decision moment, stamped on the publish task's terminal fields.
    pub now: DateTime<Utc>,
}

/// What drove this WrapUp decision.
#[derive(Debug)]
pub enum WrapUpEvent {
    /// The squash reached the default branch (§3.2 step 12) — the merge-gate
    /// decider's hand-off.
    Landed,
    /// The `wrap_up.run` command exited (or failed to launch, reported through
    /// the same fan-in with a `launch_error`).
    PublishExited {
        /// The publish task as persisted, still `Running`.
        task: Box<Task>,
        exit_code: i32,
        launch_error: Option<String>,
    },
    /// An operator resolved a `wrap_up_failed` escalation with Retry (#141).
    RetryRequested,
    /// Terminal bookkeeping is due — the job has nothing left to do.
    Completing,
}

/// The bookkeeping the shim owes after applying a decision. Everything here
/// touches dispatcher-side state (the execution slice, the dependency graph
/// fan-out) that is deliberately outside the pure crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WrapUpStep {
    /// A publish is in flight (or draining swallowed its launch): the job stays
    /// in WrapUp until its exit re-enters as [`WrapUpEvent::PublishExited`].
    AwaitPublish,
    /// Nothing is left to publish: the shim runs the terminal bookkeeping,
    /// which re-enters this decider with [`WrapUpEvent::Completing`].
    Complete,
    /// The job is stamped terminal: the shim releases the execution slice and
    /// then unblocks each listed seq's dependents, in order — the batch job
    /// first, then its members.
    Completed { unblock: Vec<u64> },
    /// The publish failed: the shim releases the execution slice before the
    /// `Escalate` effect runs, so the escalation task is not stamped with the
    /// cycle of a slice the decision just ended.
    EscalatedDropExec,
}

impl WrapUpStep {
    /// True when the shim must release the job's execution slice — after the
    /// transitions, before the effects (the same order C2's
    /// `CompletedDropExec` established).
    pub fn drops_exec(&self) -> bool {
        matches!(
            self,
            WrapUpStep::Completed { .. } | WrapUpStep::EscalatedDropExec
        )
    }
}

/// Decide one WrapUp step (spec §3.2 step 12, §2.1). Transitions are applied
/// by the shim before the effects: the §2.1 record is the committed decision,
/// the publish task and the announcements are its downstream artifacts, and a
/// crash between the two is healed by restart reconciliation
/// (`recover_wrapup_command` re-derives the publish from the task log).
pub fn decide(
    view: &WrapUpView<'_>,
    event: WrapUpEvent,
) -> (Vec<Transition>, Vec<Effect>, WrapUpStep) {
    // `Job::project` is always "owner/name" (§1.1); the publish subject and
    // every repo-scoped effect need the halves.
    let (owner, project) = view
        .job
        .project
        .split_once('/')
        .unwrap_or_else(|| panic!("malformed job project '{}'", view.job.project));

    match event {
        WrapUpEvent::Landed => decide_landed(view, owner, project),
        WrapUpEvent::PublishExited {
            task,
            exit_code,
            launch_error,
        } => decide_publish_exited(view, owner, project, *task, exit_code, launch_error),
        WrapUpEvent::RetryRequested => decide_retry(view, owner, project),
        WrapUpEvent::Completing => decide_completing(view, owner, project),
    }
}

/// The post-merge fork (§3.2): launch the `wrap_up.run` publish against merged
/// main and hold the job in WrapUp, or — a plain code job — go straight to the
/// terminal bookkeeping. The merge queue advances either way; the publish is an
/// external effect, not part of the landing.
fn decide_landed(
    view: &WrapUpView<'_>,
    owner: &str,
    project: &str,
) -> (Vec<Transition>, Vec<Effect>, WrapUpStep) {
    debug_assert_eq!(
        view.job.state,
        JobState::WrapUp,
        "landing decided for job #{} in {:?}",
        view.job.id,
        view.job.state,
    );
    if !view.publish_command {
        return (Vec::new(), Vec::new(), WrapUpStep::Complete);
    }
    let effects = vec![Effect::LaunchWrapupTask {
        owner: owner.to_string(),
        project: project.to_string(),
        seq: view.job.id,
        attempt: 1,
    }];
    (Vec::new(), effects, WrapUpStep::AwaitPublish)
}

/// The publish exited (§3.2). Exit 0 records the task Done and completes the
/// job; any non-zero exit records the failure and escalates — the squash is
/// already on the default branch, so the merge is never undone and only the
/// external publish is outstanding.
fn decide_publish_exited(
    view: &WrapUpView<'_>,
    owner: &str,
    project: &str,
    mut task: Task,
    exit_code: i32,
    launch_error: Option<String>,
) -> (Vec<Transition>, Vec<Effect>, WrapUpStep) {
    debug_assert_eq!(
        task.phase,
        TaskPhase::WrapUp,
        "publish exit decided for a {:?} task",
        task.phase,
    );
    let seq = view.job.id;
    let task_id = task.id;
    let pass = exit_code == 0;
    task.completed_at = Some(view.now);
    task.state = if pass {
        TaskState::Done
    } else {
        TaskState::Failed
    };
    task.result = Some(TaskResult::Command {
        pass,
        exit_code,
        // A launch failure never produced output; its reason IS the output.
        output: if pass {
            String::new()
        } else {
            launch_error.clone().unwrap_or_default()
        },
        structured: None,
    });
    let put = Effect::PutTask {
        task: Box::new(task),
    };

    if pass {
        let effects = vec![
            put,
            Effect::PublishEvent {
                owner: owner.to_string(),
                project: project.to_string(),
                seq,
                event_type: "task-completed".to_string(),
                extra: serde_json::json!({ "task_id": task_id, "phase": "WrapUp" }),
            },
        ];
        return (Vec::new(), effects, WrapUpStep::Complete);
    }

    let effects = vec![
        put,
        Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq,
            event_type: "task-failed".to_string(),
            extra: serde_json::json!({
                "task_id": task_id, "phase": "WrapUp", "exit_code": exit_code,
                "launch_error": launch_error,
            }),
        },
        decide_publish_exited_escalation(owner, project, seq, task_id, exit_code),
    ];
    (Vec::new(), effects, WrapUpStep::EscalatedDropExec)
}

/// The escalation a failed publish raises (§3.4). Its wording is load-bearing:
/// the operator has to know the merge is final and only the external publish is
/// outstanding, because the resolution is a publish-only Retry (#141) or a
/// manual `web-publish` job — never a re-merge.
fn decide_publish_exited_escalation(
    owner: &str,
    project: &str,
    seq: u64,
    task_id: u64,
    exit_code: i32,
) -> Effect {
    Effect::Escalate {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        reason: "wrap_up_failed".to_string(),
        detail: format!(
            "Job {seq}: the wrap-up publish command failed (exit {exit_code}). \
             The squash already landed on the default branch — the merge is final; \
             only the publish did not run. Re-run the publish (jobs/web-publish.yaml) \
             or resolve."
        ),
        failing_task: Some(task_id),
    }
}

/// Re-run only the publish after a `wrap_up_failed` escalation (#141): the
/// squash already landed, so the merge is final — back to WrapUp and relaunch
/// the command at a fresh attempt.
fn decide_retry(
    view: &WrapUpView<'_>,
    owner: &str,
    project: &str,
) -> (Vec<Transition>, Vec<Effect>, WrapUpStep) {
    // Negative space (§2.1): terminal states are absorbing — a Retry decided
    // for a Done/Revoked job is a caller bug, and `assert_transition` would
    // reject the transition anyway.
    debug_assert!(
        !view.job.state.is_terminal(),
        "wrap-up retry decided for terminal job #{} in {:?}",
        view.job.id,
        view.job.state,
    );
    let transitions = vec![Transition {
        job: Box::new(view.job.clone()),
        to: JobState::WrapUp,
    }];
    let effects = vec![Effect::LaunchWrapupTask {
        owner: owner.to_string(),
        project: project.to_string(),
        seq: view.job.id,
        attempt: 1,
    }];
    (transitions, effects, WrapUpStep::AwaitPublish)
}

/// Terminal success (§2.1): drop the scratch branch, stamp Done, announce it —
/// and for a batch, carry every member to Done with it, since one merge landed
/// all of their work (§2.1 batches).
///
/// All transitions precede all effects, so a batch's members are stamped Done
/// before the first dependent is unblocked. That is stricter than the
/// pre-C3 interleaving, which unblocked the batch's own dependents while its
/// members were still `Batched`: a dependent waiting on both the batch and one
/// of its members no longer needs the member's own fan-out to catch it.
fn decide_completing(
    view: &WrapUpView<'_>,
    owner: &str,
    project: &str,
) -> (Vec<Transition>, Vec<Effect>, WrapUpStep) {
    let job = view.job;
    debug_assert!(
        !job.state.is_terminal(),
        "completion decided for already-terminal job #{} in {:?}",
        job.id,
        job.state,
    );
    debug_assert_eq!(
        view.members.len(),
        job.members.len(),
        "batch #{} completed with {} of {} member records gathered",
        job.id,
        view.members.len(),
        job.members.len(),
    );

    let mut transitions = vec![Transition {
        job: Box::new(job.clone()),
        to: JobState::Done,
    }];
    let mut effects = vec![
        Effect::DeleteBranch {
            owner: owner.to_string(),
            project: project.to_string(),
            branch: job.branch.clone(),
        },
        Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: job.id,
            event_type: "job-done".to_string(),
            extra: serde_json::json!({}),
        },
    ];
    let mut unblock = vec![job.id];

    for member in view.members {
        debug_assert!(
            !member.state.is_terminal(),
            "batch member #{} already terminal in {:?}",
            member.id,
            member.state,
        );
        transitions.push(Transition {
            job: Box::new(member.clone()),
            to: JobState::Done,
        });
        effects.push(Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: member.id,
            event_type: "job-completed-via-batch".to_string(),
            extra: serde_json::json!({ "batch_id": job.id }),
        });
        unblock.push(member.id);
    }

    (transitions, effects, WrapUpStep::Completed { unblock })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Tier-1 coverage of every WrapUp branch: pure values in, pure values
    //! out, no NATS/Docker. The dispatcher's golden traces pin the same
    //! decisions end-to-end (`work_eval_merge_no_gate.yaml` and the gate
    //! fixtures all terminate through `Completing`).
    use super::*;
    use types::{TaskKind, TaskPhase};

    fn sample_job(state: JobState) -> Job {
        let mut job: Job = serde_json::from_str(
            r#"{ "id": 7, "project": "acme/api", "type": "build",
                 "deps": [], "state": "WrapUp", "branch": "job/7",
                 "base_ref": null, "knowledge_tags": [], "factory": null,
                 "created_at": "2026-07-24T10:00:00Z", "ready_at": null }"#,
        )
        .expect("sample job");
        job.state = state;
        job
    }

    fn publish_task(state: TaskState) -> Task {
        Task {
            id: 4,
            job_seq: 7,
            project: "acme/api".into(),
            phase: TaskPhase::WrapUp,
            cycle: 1,
            kind: TaskKind::Command {
                run: "./publish.sh".into(),
            },
            state,
            attempt: 1,
            evaluator: None,
            label: Some("publish".into()),
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            session_id: None,
            pending_reason: None,
            queued_at: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        }
    }

    fn view<'a>(job: &'a Job, members: &'a [Job], publish_command: bool) -> WrapUpView<'a> {
        WrapUpView {
            job,
            publish_command,
            members,
            now: Utc::now(),
        }
    }

    /// The post-merge fork, publish arm: no transition (the job is already
    /// WrapUp), one launch, and the job holds until the command exits.
    #[test]
    fn landing_with_a_publish_command_launches_it_and_holds() {
        let job = sample_job(JobState::WrapUp);
        let (transitions, effects, step) = decide(&view(&job, &[], true), WrapUpEvent::Landed);

        assert!(transitions.is_empty(), "already in WrapUp");
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::LaunchWrapupTask {
                owner,
                project,
                seq,
                attempt,
            } => {
                assert_eq!((owner.as_str(), project.as_str()), ("acme", "api"));
                assert_eq!((*seq, *attempt), (7, 1));
            }
            other => panic!("expected LaunchWrapupTask, got {other:?}"),
        }
        assert_eq!(step, WrapUpStep::AwaitPublish);
    }

    /// The post-merge fork, plain-code arm: nothing to publish, so the landing
    /// hands straight to the terminal bookkeeping.
    #[test]
    fn landing_without_a_publish_command_completes_directly() {
        let job = sample_job(JobState::WrapUp);
        let (transitions, effects, step) = decide(&view(&job, &[], false), WrapUpEvent::Landed);

        assert!(transitions.is_empty());
        assert!(effects.is_empty(), "completion is the next decision's job");
        assert_eq!(step, WrapUpStep::Complete);
    }

    /// Exit 0: the task is recorded Done and announced, then the job completes.
    #[test]
    fn publish_success_records_the_task_then_completes() {
        let job = sample_job(JobState::WrapUp);
        let v = view(&job, &[], true);
        let now = v.now;
        let (transitions, effects, step) = decide(
            &v,
            WrapUpEvent::PublishExited {
                task: Box::new(publish_task(TaskState::Running)),
                exit_code: 0,
                launch_error: None,
            },
        );

        assert!(transitions.is_empty(), "Done is stamped by Completing");
        assert_eq!(effects.len(), 2);
        match &effects[0] {
            Effect::PutTask { task } => {
                assert_eq!(task.state, TaskState::Done);
                assert_eq!(task.completed_at, Some(now));
                match task.result.as_ref().expect("command result") {
                    TaskResult::Command {
                        pass, exit_code, ..
                    } => assert_eq!((*pass, *exit_code), (true, 0)),
                    other => panic!("expected a Command result, got {other:?}"),
                }
            }
            other => panic!("expected PutTask first, got {other:?}"),
        }
        match &effects[1] {
            Effect::PublishEvent {
                event_type, extra, ..
            } => {
                assert_eq!(event_type, "task-completed");
                assert_eq!(extra["task_id"], 4);
                assert_eq!(extra["phase"], "WrapUp");
            }
            other => panic!("expected PublishEvent second, got {other:?}"),
        }
        assert_eq!(step, WrapUpStep::Complete);
    }

    /// A non-zero exit: the task is recorded Failed, announced, and escalated
    /// with the merge-is-final wording — and the shim drops the exec slice.
    #[test]
    fn publish_failure_records_announces_and_escalates() {
        let job = sample_job(JobState::WrapUp);
        let (transitions, effects, step) = decide(
            &view(&job, &[], true),
            WrapUpEvent::PublishExited {
                task: Box::new(publish_task(TaskState::Running)),
                exit_code: 3,
                launch_error: None,
            },
        );

        assert!(
            transitions.is_empty(),
            "the merge stays; only the job parks"
        );
        assert_eq!(effects.len(), 3);
        match &effects[0] {
            Effect::PutTask { task } => {
                assert_eq!(task.state, TaskState::Failed);
                match task.result.as_ref().expect("command result") {
                    TaskResult::Command {
                        pass, exit_code, ..
                    } => assert_eq!((*pass, *exit_code), (false, 3)),
                    other => panic!("expected a Command result, got {other:?}"),
                }
            }
            other => panic!("expected PutTask first, got {other:?}"),
        }
        match &effects[1] {
            Effect::PublishEvent {
                event_type, extra, ..
            } => {
                assert_eq!(event_type, "task-failed");
                assert_eq!(extra["exit_code"], 3);
            }
            other => panic!("expected PublishEvent second, got {other:?}"),
        }
        match &effects[2] {
            Effect::Escalate {
                reason,
                detail,
                failing_task,
                seq,
                ..
            } => {
                assert_eq!(reason, "wrap_up_failed");
                assert_eq!(*failing_task, Some(4));
                assert_eq!(*seq, 7);
                assert!(detail.contains("the merge is final"));
            }
            other => panic!("expected Escalate third, got {other:?}"),
        }
        assert_eq!(step, WrapUpStep::EscalatedDropExec);
        assert!(step.drops_exec());
    }

    /// A launch failure arrives through the same fan-in: its reason becomes the
    /// recorded output, and it escalates like any other non-zero exit.
    #[test]
    fn publish_launch_failure_carries_its_reason_as_output() {
        let job = sample_job(JobState::WrapUp);
        let (_, effects, step) = decide(
            &view(&job, &[], true),
            WrapUpEvent::PublishExited {
                task: Box::new(publish_task(TaskState::Running)),
                exit_code: -1,
                launch_error: Some("container launch failed: bad image".into()),
            },
        );

        match &effects[0] {
            Effect::PutTask { task } => match task.result.as_ref().expect("command result") {
                TaskResult::Command { output, .. } => {
                    assert_eq!(output, "container launch failed: bad image")
                }
                other => panic!("expected a Command result, got {other:?}"),
            },
            other => panic!("expected PutTask first, got {other:?}"),
        }
        match &effects[1] {
            Effect::PublishEvent { extra, .. } => {
                assert_eq!(extra["launch_error"], "container launch failed: bad image")
            }
            other => panic!("expected PublishEvent second, got {other:?}"),
        }
        assert_eq!(step, WrapUpStep::EscalatedDropExec);
    }

    /// Retry (#141): Escalated→WrapUp plus a fresh attempt — and no second
    /// squash, because the landing is not re-decided.
    #[test]
    fn retry_returns_to_wrapup_and_relaunches_the_publish() {
        let job = sample_job(JobState::Escalated);
        let (transitions, effects, step) =
            decide(&view(&job, &[], true), WrapUpEvent::RetryRequested);

        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::WrapUp);
        assert_eq!(
            transitions[0].job.state,
            JobState::Escalated,
            "the state flip is set_state's job"
        );
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::LaunchWrapupTask { seq, attempt, .. } => assert_eq!((*seq, *attempt), (7, 1)),
            other => panic!("expected LaunchWrapupTask, got {other:?}"),
        }
        assert_eq!(step, WrapUpStep::AwaitPublish);
    }

    /// Terminal stamping for an ordinary job: Done, branch cleanup, the
    /// announcement, and its own dependents to unblock.
    #[test]
    fn completing_stamps_done_cleans_up_and_announces() {
        let job = sample_job(JobState::WrapUp);
        let (transitions, effects, step) = decide(&view(&job, &[], false), WrapUpEvent::Completing);

        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::Done);
        assert_eq!(effects.len(), 2);
        match &effects[0] {
            Effect::DeleteBranch { branch, .. } => assert_eq!(branch, "job/7"),
            other => panic!("expected DeleteBranch first, got {other:?}"),
        }
        match &effects[1] {
            Effect::PublishEvent {
                event_type, seq, ..
            } => {
                assert_eq!(event_type, "job-done");
                assert_eq!(*seq, 7);
            }
            other => panic!("expected PublishEvent second, got {other:?}"),
        }
        assert_eq!(
            step,
            WrapUpStep::Completed { unblock: vec![7] },
            "an ordinary job unblocks only its own dependents"
        );
        assert!(step.drops_exec());
    }

    /// A batch's completion carries every member (§2.1 batches): all member
    /// transitions precede every announcement, and the fan-out lists the batch
    /// first, then its members.
    #[test]
    fn completing_a_batch_fans_done_out_to_its_members() {
        let mut job = sample_job(JobState::WrapUp);
        job.members = vec![11, 12];
        let members: Vec<Job> = [11, 12]
            .iter()
            .map(|&id| {
                let mut m = sample_job(JobState::Batched);
                m.id = id;
                m.branch = format!("job/{id}");
                m.batch_id = Some(7);
                m
            })
            .collect();

        let (transitions, effects, step) =
            decide(&view(&job, &members, false), WrapUpEvent::Completing);

        let stamped: Vec<u64> = transitions.iter().map(|t| t.job.id).collect();
        assert_eq!(stamped, vec![7, 11, 12], "batch first, then its members");
        assert!(transitions.iter().all(|t| t.to == JobState::Done));

        assert_eq!(
            effects.len(),
            4,
            "cleanup + job-done + one event per member"
        );
        for (i, member) in [11u64, 12].iter().enumerate() {
            match &effects[2 + i] {
                Effect::PublishEvent {
                    event_type,
                    seq,
                    extra,
                    ..
                } => {
                    assert_eq!(event_type, "job-completed-via-batch");
                    assert_eq!(seq, member);
                    assert_eq!(extra["batch_id"], 7);
                }
                other => panic!("expected a member PublishEvent, got {other:?}"),
            }
        }
        assert_eq!(
            step,
            WrapUpStep::Completed {
                unblock: vec![7, 11, 12]
            }
        );
    }

    /// Only the two terminal steps release the execution slice.
    #[test]
    fn await_and_complete_keep_the_execution_slice() {
        assert!(!WrapUpStep::AwaitPublish.drops_exec());
        assert!(!WrapUpStep::Complete.drops_exec());
    }

    /// Negative space: a terminal job must never reach the completion decision.
    #[test]
    #[should_panic(expected = "already-terminal job")]
    #[cfg(debug_assertions)]
    fn completing_a_terminal_job_is_a_caller_bug() {
        let job = sample_job(JobState::Done);
        decide(&view(&job, &[], false), WrapUpEvent::Completing);
    }

    /// Negative space: a batch member is stamped Done exactly once.
    #[test]
    #[should_panic(expected = "already terminal")]
    #[cfg(debug_assertions)]
    fn completing_an_already_done_member_is_a_caller_bug() {
        let mut job = sample_job(JobState::WrapUp);
        job.members = vec![11];
        let mut member = sample_job(JobState::Done);
        member.id = 11;
        decide(&view(&job, &[member], false), WrapUpEvent::Completing);
    }

    /// Negative space: a publish exit is only ever decided for a WrapUp task.
    #[test]
    #[should_panic(expected = "publish exit decided")]
    #[cfg(debug_assertions)]
    fn a_foreign_phase_task_exit_is_a_caller_bug() {
        let job = sample_job(JobState::WrapUp);
        let mut task = publish_task(TaskState::Running);
        task.phase = TaskPhase::Work;
        decide(
            &view(&job, &[], true),
            WrapUpEvent::PublishExited {
                task: Box::new(task),
                exit_code: 0,
                launch_error: None,
            },
        );
    }
}

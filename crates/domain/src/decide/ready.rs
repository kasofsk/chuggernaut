//! Ready-phase decisions (spec §2.1, §2.2, §3.1) — refactor-plan C4, the third
//! carve, this one out of `core.rs` and `exec.rs`.
//!
//! The phase spans a job's whole eligibility story: from "its definition just
//! passed release validation" to "it holds a launch slot and Work may begin".
//! One pure function owns every decision on that road, told apart by
//! [`ReadyEvent`]:
//!
//! - **Released** — dependency satisfaction at release time (§2.2): all deps
//!   Done admits the job `Ready` with `base_ref` pinned at the validated HEAD;
//!   anything outstanding parks it `Blocked`. Either way a Draft batch commits
//!   its membership, and leaving Draft announces the finalize (§2.1).
//! - **DepsChanged** — a dependency reached Done, or restart reconciliation is
//!   re-checking (§3.1 step 2, §3.6 step 3). This decision is *only* about
//!   eligibility: a `Blocked` job with every dep Done earns the §2.2
//!   Ready-transition re-validation, which is ref-reading I/O the pure crate
//!   cannot do — so the decider emits [`ReadyStep::Revalidate`] and terminates
//!   (the C2 continuation contract), and its result re-enters as:
//! - **Revalidated** — the re-validation verdict: a clean pass unblocks the job
//!   at the fresh HEAD, any error parks it `Stalled` with the reason (the
//!   pre-work escalation twin, §1.2).
//! - **Dequeued** — queue admission's other end (§3.1 step 5): the ready queue
//!   handed this job a launch slot, so decide whether it may still take it. A
//!   job revoked or escalated while it waited quietly forfeits the slot.
//!
//! The `Msg` contracts this decider owns (contracts.md §1):
//!
//! - `Msg::ReleaseJob` — **pre:** the job is `Frozen` or `Draft` and its §2.2
//!   release-time pass succeeded (the shim rejects otherwise, so this decider
//!   never sees a failed validation); **post:** the job is `Ready` with
//!   `base_ref`/`ready_at` stamped and one queue entry, or `Blocked` with
//!   neither — never `Ready` with an unsatisfied dependency, and never two
//!   queue entries for one job (`ReadyQueue::enqueue` dedupes).
//! - `Msg::JobDone`'s dependents fan-out (`on_job_done` → `try_unblock`) —
//!   **pre:** none, the event is advisory and fires for every dependent;
//!   **post:** a dependent is `Ready` (re-validated at the current HEAD, which
//!   becomes its `base_ref`) or `Stalled`, or untouched. A `Blocked` job with
//!   an outstanding dep is never admitted, and re-validation I/O never runs
//!   for one.
//! - The ready-queue drain (`drain_queue` → `start_job`) — **pre:** the job was
//!   enqueued `Ready`; **post:** Work is entered at cycle 1, or nothing happens
//!   because the job left `Ready` while queued. Never a launch for a terminal
//!   job.
//!
//! - **Accepts:** a [`ReadyView`] (the target job, the pre-read dependency
//!   verdict, the clock) and a [`ReadyEvent`].
//! - **Emits:** `(Vec<Transition>, Vec<Effect>, ReadyStep)` — values only. The
//!   owned effect set (contracts.md §2): `PublishEvent` for `job-finalized`,
//!   `job-released` and `job-unblocked`, plus `Stall` for a failed
//!   Ready-transition re-validation. The [`ReadyStep`] names the shell
//!   bookkeeping that follows — queue admission, batch absorption, the
//!   re-validation hop, and the Work hand-off — each of which touches
//!   dispatcher state (the ready queue, the member records, the `vcs` port)
//!   the pure crate cannot see.
//! - **Guarantees:** pure and synchronous; every branch exhaustively matched
//!   and unit-tested; asserts negative space (STYLE.md Tier 2 #2) — never
//!   admits a job whose deps are outstanding, never pins a `base_ref` on a job
//!   it parks `Blocked`, never decides for a terminal job. Performs no effect,
//!   holds no `&mut Core`.
//! - **Spec:** §2.1 (Frozen/Draft→Ready|Blocked, Blocked→Ready|Stalled), §2.2
//!   (the release-time and Ready-transition passes), §3.1 (the ready queue),
//!   §3.5 (the launch-slot budget, via [`crate::queue`]); contracts.md §2;
//!   refactor-plan C4.
//!
//! **Boundary.** The §3.5 launch queue's *pure* half — the drain-priority
//! classification and the max-wait budget arithmetic — lives next to the queue
//! types it describes ([`crate::queue::launch_priority`],
//! [`crate::queue::QueuedLaunch::is_expired`]) rather than here: a parked
//! launch belongs to whichever phase asked for the container (Work, Evaluation,
//! MergeGate, WrapUp), so its park/expire *effect sequences* stay phase-agnostic
//! in the dispatcher's `launch_queue` until C5/C6 carve their own phases.

use crate::decide::Transition;
use crate::effects::Effect;
use crate::release::ValidationError;
use chrono::{DateTime, Utc};
use types::{Job, JobState};

/// The read-only inputs one Ready-phase decision consumes. The shim re-gathers
/// it before every [`decide`] call — including the re-entry after a
/// [`ReadyStep::Revalidate`] hop, so a decision never runs on a view the world
/// moved under. Reads feed the view; they are not effects.
pub struct ReadyView<'a> {
    /// The job whose eligibility is being decided.
    pub job: &'a Job,
    /// Are every one of the job's dependencies `Done`? Pre-read from the
    /// in-memory graph (`JobGraph::deps_done`), which is the working copy of
    /// the §1.4 DAG.
    pub deps_done: bool,
    /// The decision moment — stamped as `ready_at` on the admitted record.
    pub now: DateTime<Utc>,
}

/// What drove this Ready-phase decision.
#[derive(Debug)]
pub enum ReadyEvent {
    /// The §2.2 release-time pass succeeded for a `Frozen` (or finalizing
    /// `Draft`) job.
    Released {
        /// The default branch HEAD the pass validated against — the commit an
        /// admitted job pins as its `base_ref` (§2.2).
        head: String,
        /// The job was `Draft`: leaving Draft finalizes its edited definition
        /// and announces it separately from the plain release (§2.1).
        from_draft: bool,
        /// A Draft batch's member seqs, to absorb `Frozen`→`Batched` once the
        /// release commits (§2.1 batches); empty for every other release.
        absorb: Vec<u64>,
    },
    /// A dependency reached `Done`, or restart reconciliation is re-checking a
    /// parked job (§3.1 step 2, §3.6 step 3).
    DepsChanged,
    /// The §2.2 Ready-transition re-validation came back — the continuation of
    /// [`ReadyStep::Revalidate`].
    Revalidated {
        /// The HEAD the re-validation ran at; the `base_ref` of a job it admits.
        head: String,
        /// The verdict: empty is a pass, anything else parks the job `Stalled`.
        errors: Vec<ValidationError>,
    },
    /// The ready queue handed this job a launch slot (§3.1 step 5).
    Dequeued,
}

/// The bookkeeping the shim owes after applying a decision. Everything here
/// touches dispatcher-side state — the in-memory ready queue, the batch members'
/// records, the `vcs` port, the Work phase's entry — that is deliberately
/// outside the pure crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadyStep {
    /// Nothing follows: the event found the job ineligible (not `Blocked`, deps
    /// outstanding, gone from `Ready` while queued) or its decision is complete
    /// in its effects (the `Stall`).
    Idle,
    /// The release or unblock committed. **Both parts run after the transitions
    /// and before the effects** — queue admission and the membership commit are
    /// part of committing the decision, the announcements are its artifacts —
    /// which is exactly the pre-C4 write order.
    Admitted {
        /// Put the job on the ready queue: true only when it was admitted
        /// `Ready` (§3.1 — the queue holds only Ready jobs, the invariant
        /// checker's `ready_queue_only_ready`).
        enqueue: bool,
        /// Batch members to absorb `Frozen`→`Batched`, with the batch's union
        /// deps indexed first (§2.1, §2.3); empty for every non-batch release.
        absorb: Vec<u64>,
    },
    /// The job is eligible for the §2.2 Ready-transition re-validation: the
    /// shim resolves the default branch HEAD, re-runs the pass, and re-enters
    /// [`decide`] with [`ReadyEvent::Revalidated`] against a fresh view.
    Revalidate,
    /// The job may take its launch slot: the shim enters the Work phase at this
    /// cycle (§3.2 steps 1–6). The hand-off is a step rather than an
    /// [`Effect::EnterWork`] because that variant is the *rework* re-entry
    /// (gate failure, conflict, gate fix) the merge-gate decider owns; cycle-1
    /// entry is the Work phase's own contract, carved by C6.
    StartWork { cycle: u32 },
}

/// Decide one Ready-phase step (spec §2.1, §2.2, §3.1). Transitions are applied
/// by the shim before the effects: the §2.1 record is the committed decision and
/// the announcements are its artifacts, and a crash between the two is healed by
/// restart reconciliation re-driving `try_unblock` for every parked job (§3.6
/// step 3).
pub fn decide(
    view: &ReadyView<'_>,
    event: ReadyEvent,
) -> (Vec<Transition>, Vec<Effect>, ReadyStep) {
    // `Job::project` is always "owner/name" (§1.1); every published subject and
    // the `Stall` composite need the halves.
    let (owner, project) = view
        .job
        .project
        .split_once('/')
        .unwrap_or_else(|| panic!("malformed job project '{}'", view.job.project));

    match event {
        ReadyEvent::Released {
            head,
            from_draft,
            absorb,
        } => decide_released(view, owner, project, head, from_draft, absorb),
        ReadyEvent::DepsChanged => decide_deps_changed(view),
        ReadyEvent::Revalidated { head, errors } => {
            decide_revalidated(view, owner, project, head, errors)
        }
        ReadyEvent::Dequeued => decide_dequeued(view),
    }
}

/// Release-time admission (§2.2 pass, §2.1 Frozen|Draft→Ready|Blocked). The
/// dependency verdict is the whole decision: every dep `Done` admits the job at
/// the validated HEAD, anything outstanding parks it `Blocked` with no
/// `base_ref` — an unpinned job re-validates (and re-pins) at unblock time, so
/// pinning here would only bake in a stale commit.
fn decide_released(
    view: &ReadyView<'_>,
    owner: &str,
    project: &str,
    head: String,
    from_draft: bool,
    absorb: Vec<u64>,
) -> (Vec<Transition>, Vec<Effect>, ReadyStep) {
    let job = view.job;
    // Negative space (§2.1): terminal states are absorbing — releasing a
    // Done/Revoked job is a caller bug, and `assert_transition` would reject the
    // transition anyway.
    debug_assert!(
        !job.state.is_terminal(),
        "release decided for terminal job #{} in {:?}",
        job.id,
        job.state,
    );
    debug_assert!(
        matches!(job.state, JobState::Frozen | JobState::Draft),
        "release decided for job #{} in {:?}, not Frozen/Draft",
        job.id,
        job.state,
    );
    let to = if view.deps_done {
        JobState::Ready
    } else {
        JobState::Blocked
    };
    let stamped = admitted_record(job, to, &head, view.now);
    // Postcondition: exactly the admitted job carries a pinned base (§2.2).
    debug_assert!(
        stamped.base_ref.is_some() == (to == JobState::Ready || job.base_ref.is_some()),
        "base_ref pin disagrees with the {to:?} admission of job #{}",
        job.id,
    );

    let mut effects = Vec::new();
    if from_draft {
        // Leaving Draft finalizes the edited definition — announced separately
        // from the release so the UI can tell the two apart (§2.1).
        effects.push(Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: job.id,
            event_type: "job-finalized".to_string(),
            extra: serde_json::json!({}),
        });
    }
    effects.push(Effect::PublishEvent {
        owner: owner.to_string(),
        project: project.to_string(),
        seq: job.id,
        event_type: "job-released".to_string(),
        extra: serde_json::json!({ "state": to }),
    });

    let step = ReadyStep::Admitted {
        enqueue: to == JobState::Ready,
        absorb,
    };
    (
        vec![Transition {
            job: Box::new(stamped),
            to,
        }],
        effects,
        step,
    )
}

/// Eligibility for the §2.1 Blocked→Ready edge, and nothing else. The
/// re-validation it gates is ref-reading I/O, so the decision terminates here
/// and the shim continues it (the C2 continuation contract) — which also means
/// the expensive pass never runs for a job that was not going to move.
fn decide_deps_changed(view: &ReadyView<'_>) -> (Vec<Transition>, Vec<Effect>, ReadyStep) {
    let eligible = view.job.state == JobState::Blocked && view.deps_done;
    let step = if eligible {
        ReadyStep::Revalidate
    } else {
        ReadyStep::Idle
    };
    (Vec::new(), Vec::new(), step)
}

/// The §2.2 Ready-transition verdict (§2.1 Blocked→Ready|Stalled). A pass pins
/// the freshly resolved HEAD as `base_ref` — the job's work starts from what its
/// dependencies actually landed, not from the commit it was created against. A
/// failure is a *pre-work* park: `Stalled`, not `Escalated`, because no work
/// task exists yet (§1.2), so the operator's only moves are Retry and Revoke.
fn decide_revalidated(
    view: &ReadyView<'_>,
    owner: &str,
    project: &str,
    head: String,
    errors: Vec<ValidationError>,
) -> (Vec<Transition>, Vec<Effect>, ReadyStep) {
    let job = view.job;
    debug_assert_eq!(
        job.state,
        JobState::Blocked,
        "re-validation decided for job #{} in {:?}, not Blocked",
        job.id,
        job.state,
    );
    if !errors.is_empty() {
        let detail = errors
            .iter()
            .map(|e| format!("- {}: {}", e.field, e.message))
            .collect::<Vec<_>>()
            .join("\n");
        let effects = vec![Effect::Stall {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: job.id,
            reason: "revalidation_failed".to_string(),
            detail: format!(
                "Job {} failed Ready-transition re-validation at {head}:\n{detail}",
                job.id
            ),
            failing_task: None,
        }];
        return (Vec::new(), effects, ReadyStep::Idle);
    }

    let stamped = admitted_record(job, JobState::Ready, &head, view.now);
    let effects = vec![Effect::PublishEvent {
        owner: owner.to_string(),
        project: project.to_string(),
        seq: job.id,
        event_type: "job-unblocked".to_string(),
        extra: serde_json::json!({}),
    }];
    (
        vec![Transition {
            job: Box::new(stamped),
            to: JobState::Ready,
        }],
        effects,
        ReadyStep::Admitted {
            enqueue: true,
            absorb: Vec::new(),
        },
    )
}

/// The queue's other end (§3.1 step 5): a queued job reached the front, so it
/// may start work — unless it left `Ready` while it waited. Revoke and the
/// pre-work parks both leave stale entries behind (the queue is not indexed by
/// state), so this check is the queue's only admission guard and it must be
/// silent: a forfeited slot is routine, not a fault.
fn decide_dequeued(view: &ReadyView<'_>) -> (Vec<Transition>, Vec<Effect>, ReadyStep) {
    let step = if view.job.state == JobState::Ready {
        ReadyStep::StartWork { cycle: 1 }
    } else {
        ReadyStep::Idle
    };
    (Vec::new(), Vec::new(), step)
}

/// The record an admission persists: the §2.2 `base_ref` pin and the §1.1
/// `ready_at` stamp, both applied only on the way to `Ready`. `ready_at` records
/// when the job *first* became runnable, so a re-entry through Blocked never
/// overwrites it — the queue-wait and lead-time views read it as an origin.
fn admitted_record(job: &Job, to: JobState, head: &str, now: DateTime<Utc>) -> Job {
    let mut stamped = job.clone();
    if to == JobState::Ready {
        stamped.base_ref = Some(head.to_string());
        stamped.ready_at.get_or_insert(now);
    }
    stamped
}

#[cfg(test)]
mod tests {
    //! Tier-1 coverage of every Ready-phase branch: pure values in, pure values
    //! out, no NATS/Docker (`testing.md` tier 1). The dispatcher's golden traces
    //! pin the same decisions end-to-end (`release_block_unblock.yaml`,
    //! `stall_on_revalidation_failure.yaml`).
    use super::*;

    fn sample_job(state: JobState) -> Job {
        let mut job: Job = serde_json::from_str(
            r#"{ "id": 7, "project": "acme/api", "type": "build",
                 "deps": [], "state": "Frozen", "branch": "job/7",
                 "base_ref": null, "knowledge_tags": [], "factory": null,
                 "created_at": "2026-07-24T10:00:00Z", "ready_at": null }"#,
        )
        .expect("sample job");
        job.state = state;
        job
    }

    fn view(job: &Job, deps_done: bool) -> ReadyView<'_> {
        ReadyView {
            job,
            deps_done,
            now: Utc::now(),
        }
    }

    fn released(from_draft: bool, absorb: Vec<u64>) -> ReadyEvent {
        ReadyEvent::Released {
            head: "abc123".into(),
            from_draft,
            absorb,
        }
    }

    fn event_types(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .map(|e| match e {
                Effect::PublishEvent { event_type, .. } => event_type.clone(),
                other => other.port().to_string(),
            })
            .collect()
    }

    /// Deps all Done: admitted Ready with the validated HEAD pinned, `ready_at`
    /// stamped, one queue entry, and the release announcement carrying the state.
    #[test]
    fn released_with_deps_done_admits_ready_and_queues() {
        let job = sample_job(JobState::Frozen);
        let v = view(&job, true);
        let now = v.now;
        let (transitions, effects, step) = decide(&v, released(false, vec![]));

        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::Ready);
        assert_eq!(transitions[0].job.base_ref.as_deref(), Some("abc123"));
        assert_eq!(transitions[0].job.ready_at, Some(now));
        assert_eq!(
            transitions[0].job.state,
            JobState::Frozen,
            "the state flip is set_state's job"
        );
        assert_eq!(event_types(&effects), vec!["job-released"]);
        match &effects[0] {
            Effect::PublishEvent { extra, seq, .. } => {
                assert_eq!(*seq, 7);
                assert_eq!(extra["state"], serde_json::json!(JobState::Ready));
            }
            other => panic!("expected PublishEvent, got {other:?}"),
        }
        assert_eq!(
            step,
            ReadyStep::Admitted {
                enqueue: true,
                absorb: vec![]
            }
        );
    }

    /// A dependency still outstanding: parked Blocked, nothing queued, and no
    /// `base_ref` pinned — the pin happens at unblock, against the HEAD the deps
    /// actually landed on.
    #[test]
    fn released_with_outstanding_deps_parks_blocked_unpinned() {
        let job = sample_job(JobState::Frozen);
        let (transitions, effects, step) = decide(&view(&job, false), released(false, vec![]));

        assert_eq!(transitions[0].to, JobState::Blocked);
        assert!(transitions[0].job.base_ref.is_none(), "no premature pin");
        assert!(transitions[0].job.ready_at.is_none());
        assert_eq!(event_types(&effects), vec!["job-released"]);
        assert_eq!(
            step,
            ReadyStep::Admitted {
                enqueue: false,
                absorb: vec![]
            }
        );
    }

    /// Leaving Draft finalizes the edited definition first, then releases —
    /// two announcements, in that order (§2.1).
    #[test]
    fn released_from_draft_announces_the_finalize_first() {
        let job = sample_job(JobState::Draft);
        let (_, effects, _) = decide(&view(&job, true), released(true, vec![]));
        assert_eq!(event_types(&effects), vec!["job-finalized", "job-released"]);
    }

    /// A Draft batch commits its membership whichever way the dependency
    /// verdict falls — absorption is part of the release, not of admission.
    #[test]
    fn released_draft_batch_absorbs_members_even_when_blocked() {
        let job = sample_job(JobState::Draft);
        let (_, _, step) = decide(&view(&job, false), released(true, vec![4, 5]));
        assert_eq!(
            step,
            ReadyStep::Admitted {
                enqueue: false,
                absorb: vec![4, 5]
            }
        );
    }

    /// `ready_at` marks when a job *first* became runnable: a job cycling back
    /// through Blocked keeps its original stamp.
    #[test]
    fn admission_never_overwrites_an_existing_ready_at() {
        let mut job = sample_job(JobState::Frozen);
        let first = "2026-07-01T09:00:00Z".parse::<DateTime<Utc>>().expect("ts");
        job.ready_at = Some(first);
        let (transitions, _, _) = decide(&view(&job, true), released(false, vec![]));
        assert_eq!(transitions[0].job.ready_at, Some(first));
    }

    /// The one shape that earns the re-validation: Blocked with every dep Done.
    #[test]
    fn deps_changed_on_a_satisfied_blocked_job_revalidates() {
        let job = sample_job(JobState::Blocked);
        let (transitions, effects, step) = decide(&view(&job, true), ReadyEvent::DepsChanged);
        assert!(transitions.is_empty() && effects.is_empty());
        assert_eq!(step, ReadyStep::Revalidate);
    }

    /// A dep landed but others have not: no move, and — the point of gating
    /// here — no re-validation I/O.
    #[test]
    fn deps_changed_with_outstanding_deps_is_idle() {
        let job = sample_job(JobState::Blocked);
        let (_, _, step) = decide(&view(&job, false), ReadyEvent::DepsChanged);
        assert_eq!(step, ReadyStep::Idle);
    }

    /// The fan-out is advisory and reaches every dependent, including ones that
    /// already moved on (§3.1 step 2) — those are silently ignored.
    #[test]
    fn deps_changed_ignores_a_job_that_is_not_blocked() {
        for state in [
            JobState::Ready,
            JobState::Work,
            JobState::Stalled,
            JobState::Done,
        ] {
            let job = sample_job(state);
            let (_, _, step) = decide(&view(&job, true), ReadyEvent::DepsChanged);
            assert_eq!(step, ReadyStep::Idle, "{state:?} must not re-validate");
        }
    }

    /// A clean re-validation unblocks at the fresh HEAD and queues the job.
    #[test]
    fn revalidated_clean_unblocks_at_the_fresh_head() {
        let job = sample_job(JobState::Blocked);
        let v = view(&job, true);
        let now = v.now;
        let (transitions, effects, step) = decide(
            &v,
            ReadyEvent::Revalidated {
                head: "def456".into(),
                errors: vec![],
            },
        );
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::Ready);
        assert_eq!(transitions[0].job.base_ref.as_deref(), Some("def456"));
        assert_eq!(transitions[0].job.ready_at, Some(now));
        assert_eq!(event_types(&effects), vec!["job-unblocked"]);
        assert_eq!(
            step,
            ReadyStep::Admitted {
                enqueue: true,
                absorb: vec![]
            }
        );
    }

    /// A failed re-validation parks the job pre-work: `Stall`, never a
    /// transition of this decider's own, and the operator gets every field
    /// error verbatim.
    #[test]
    fn revalidated_with_errors_stalls_with_the_reasons() {
        let job = sample_job(JobState::Blocked);
        let (transitions, effects, step) = decide(
            &view(&job, true),
            ReadyEvent::Revalidated {
                head: "def456".into(),
                errors: vec![
                    ValidationError::new(Some(7), "work.prompt", "prompt file missing"),
                    ValidationError::new(Some(7), "secrets", "secret 'X' is not set"),
                ],
            },
        );
        assert!(transitions.is_empty(), "the Stall composite owns the flip");
        assert_eq!(step, ReadyStep::Idle);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::Stall {
                owner,
                project,
                seq,
                reason,
                detail,
                failing_task,
            } => {
                assert_eq!((owner.as_str(), project.as_str(), *seq), ("acme", "api", 7));
                assert_eq!(reason, "revalidation_failed");
                assert_eq!(*failing_task, None, "no work task exists pre-work");
                assert_eq!(
                    detail,
                    "Job 7 failed Ready-transition re-validation at def456:\n\
                     - work.prompt: prompt file missing\n\
                     - secrets: secret 'X' is not set"
                );
            }
            other => panic!("expected Stall, got {other:?}"),
        }
    }

    /// The queue handed the slot to a job still Ready: Work begins at cycle 1.
    #[test]
    fn dequeued_ready_job_starts_work_at_cycle_one() {
        let job = sample_job(JobState::Ready);
        let (transitions, effects, step) = decide(&view(&job, true), ReadyEvent::Dequeued);
        assert!(transitions.is_empty() && effects.is_empty());
        assert_eq!(step, ReadyStep::StartWork { cycle: 1 });
    }

    /// Revoked, stalled, or escalated while it waited: the slot is forfeited
    /// silently — a stale queue entry is routine, not a fault.
    #[test]
    fn dequeued_job_that_left_ready_forfeits_its_slot() {
        for state in [
            JobState::Revoked,
            JobState::Stalled,
            JobState::Escalated,
            JobState::Blocked,
        ] {
            let job = sample_job(state);
            let (_, _, step) = decide(&view(&job, true), ReadyEvent::Dequeued);
            assert_eq!(step, ReadyStep::Idle, "{state:?} must not launch");
        }
    }

    /// Negative space: a terminal job must never reach an admission.
    #[test]
    #[should_panic(expected = "terminal job")]
    #[cfg(debug_assertions)]
    fn admitting_a_terminal_job_is_a_caller_bug() {
        let job = sample_job(JobState::Done);
        decide(&view(&job, true), released(false, vec![]));
    }

    /// Negative space: re-validation belongs to the Blocked→Ready edge only.
    #[test]
    #[should_panic(expected = "not Blocked")]
    #[cfg(debug_assertions)]
    fn revalidating_a_non_blocked_job_is_a_caller_bug() {
        let job = sample_job(JobState::Ready);
        decide(
            &view(&job, true),
            ReadyEvent::Revalidated {
                head: "def456".into(),
                errors: vec![],
            },
        );
    }
}

//! Merge-gate decisions (spec §3.3) — refactor-plan C2, the first carve from
//! `eval.rs`.
//!
//! Three pure functions own the phase, driven by the dispatcher's fold
//! (`Core::pump_merges`):
//!
//! - [`decide_enqueue`] — a WrapUp-bound job joins the landing queue.
//! - [`MergeGateState::next_candidate`] — the depth-1 serializer: pop the
//!   next landing seq, or hold (gate occupied, queue held by an Open origin
//!   release, draining, or empty).
//! - [`decide`] — the landing state machine, re-entered per the continuation
//!   contract (docs/reference/contracts.md §2): each emitted effect's result comes back as
//!   the next [`LandingEvent`] against a freshly gathered [`LandingView`], so
//!   a decision never runs on a view the world has moved under.
//!
//! The state the phase owns is a value ([`MergeGateState`]): the shim swaps
//! the whole value per decision, so scheduling writes are atomic, and two
//! invariants hold **by type** — depth-1 (`gating: Option<u64>` cannot hold a
//! second seq; formerly the checker's structural clause) and rework context
//! (`pending_rework` is the one landing's continuation memory, legal because
//! depth-1 means one landing in flight per slug).
//!
//! - **Accepts:** the per-slug [`MergeGateState`] value, a [`LandingView`],
//!   and a [`LandingEvent`].
//! - **Emits:** `(MergeGateState, Vec<Transition>, Vec<Effect>, LandingStep)`
//!   — values only; the [`LandingStep`] tells the fold what re-enters next.
//!   The owned effect set (docs/reference/contracts.md §2): `SquashMerge`,
//!   `CreateSquashCandidate`, `LaunchGateStage`, `LaunchGateFix`,
//!   `AdvanceDefault`, `RebaseOntoWithConflict`, `Escalate`, `DeleteBranch`,
//!   `PublishEvent`, `PutJob`, `EnterWork` — plus `decide_enqueue`'s
//!   Evaluation→WrapUp `Transition` and its `job-wrapup-started`
//!   `PublishEvent`.
//! - **Guarantees:** pure and synchronous; every branch exhaustively matched
//!   and unit-tested; asserts negative space (docs/reference/style.md Tier 2 #2). Performs
//!   no effect, holds no `&mut Core`.
//! - **Spec:** §3.3; docs/reference/contracts.md §2; refactor-plan C2.

use crate::decide::Transition;
use crate::effects::Effect;
use std::collections::VecDeque;
use types::{EvalResult, Evaluator, EvaluatorType, Job, JobState, ReworkReason, Task};

/// Gate-fix fast-path rounds allowed per landing (spec §3.3, job #154). Beyond
/// this, a repeated gate compile failure falls back to the full rework loop —
/// the failure wasn't the mechanical one-shot the fast path assumes.
pub const GATE_FIX_BUDGET: u32 = 2;

/// Why the in-flight landing is mid-rebase — the continuation memory
/// [`decide`] writes when it emits `RebaseOntoWithConflict` and consumes on
/// [`LandingEvent::Rebased`]. Depth-1 makes a single slot sufficient.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingRework {
    /// The squash conflicted (§3.2 step 12): rebase-as-rework, budget NOT
    /// consumed, no eval findings to carry.
    Conflict,
    /// The gate failed CI-class (§3.3): rework on the new base carrying the
    /// gate's failure findings.
    GateFailure { failures: Vec<EvalResult> },
}

/// One project slug's landing pipeline: the FIFO of WrapUp seqs waiting to
/// land, the seq whose gate is in flight, and the in-flight landing's rebase
/// memory. Owned by the decider, swapped wholesale by the shim.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MergeGateState {
    pub queue: VecDeque<u64>,
    pub gating: Option<u64>,
    pub pending_rework: Option<PendingRework>,
}

impl MergeGateState {
    /// True when nothing references this slug anymore — the shim drops the
    /// map entry so an idle project holds no state.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty() && self.gating.is_none() && self.pending_rework.is_none()
    }

    /// Enqueue a landing (idempotent — a re-finalized job must not queue
    /// twice; §3.3).
    pub fn enqueue(&mut self, seq: u64) {
        if !self.queue.contains(&seq) && self.gating != Some(seq) {
            self.queue.push_back(seq);
        }
    }

    /// Drop every reference to `seq` — the revoke cascade's unhook.
    pub fn remove(&mut self, seq: u64) {
        self.queue.retain(|&s| s != seq);
        if self.gating == Some(seq) {
            self.gating = None;
            self.pending_rework = None;
        }
    }

    /// The depth-1 serializer (§3.3): pop the next landing candidate, or
    /// hold. `held` is the origin-release queue hold (an Open release makes
    /// the post-merge integration reset lossless); `draining` is the §3.6
    /// graceful drain — no new gate starts, no landing.
    pub fn next_candidate(&mut self, held: bool, draining: bool) -> Option<u64> {
        if draining || held || self.gating.is_some() {
            return None;
        }
        self.queue.pop_front()
    }
}

/// The domain mirror of `vcs::MergeOutcome` (this crate must stay free of the
/// async `vcs` dependency — machine-checked); the shim owns the mapping,
/// exactly as `CredentialAccess` mirrors the auth type.
#[derive(Debug, Clone, PartialEq)]
pub enum MergeOutcome {
    Merged { commit: String },
    NoOp,
    Conflict { files: Vec<String> },
    UnresolvedMarkers { files: Vec<String> },
}

/// The read-only inputs one landing decision consumes. Re-gathered by the
/// shim before every [`decide`] call — the continuation contract's freshness
/// guarantee.
pub struct LandingView<'a> {
    /// The landing job.
    pub job: &'a Job,
    /// The default branch's current HEAD (pre-read; the fast-vs-gate pivot —
    /// and, mid-gate, the new base a rework rebases onto).
    pub head: String,
    /// Was this landing's current cycle a gate-fix round? (Derived from the
    /// persisted task list via [`force_gate`].) Forces the gate even when
    /// `head == base_ref`: gate CI is the fix's only verdict (job #154).
    pub force_gate: bool,
    /// The squash-commit summary, pre-composed by the shim.
    pub summary: Option<String>,
    /// The exec cycle; 1 when no exec state exists.
    pub cycle: u32,
    /// Gate-fix rounds already spent this landing.
    pub gate_fix_used: u32,
    /// The gate's evaluator set (filtered by [`gate_evaluators`]).
    pub gate_evaluators: Vec<Evaluator>,
    /// A config file on the landing branch declaring a `min_dispatcher` above
    /// this binary's epoch (spec §14.2/§14.3), pre-read by the shim. `Some`
    /// refuses the landing: the dispatcher performs the merge and knows its own
    /// epoch, so this half of the skew gate needs no API call and cannot
    /// degrade to a pass.
    pub config_skew: Option<types::ConfigSkew>,
    /// The parked candidate's commit + the HEAD it was built against — the
    /// promote CAS pair (pre-read from the gate round's parked state; `None`
    /// before a gate opens).
    pub gate_commit: Option<String>,
    pub gate_old_head: Option<String>,
}

/// One step of a landing, per the continuation contract: the first event is
/// [`LandingEvent::Start`]; every later event carries the result of the
/// effect the previous [`decide`] emitted.
#[derive(Debug)]
pub enum LandingEvent {
    /// Begin finalizing this seq (popped by [`MergeGateState::next_candidate`]).
    Start,
    /// The fast-path `SquashMerge` finished building.
    Squashed { outcome: MergeOutcome },
    /// The `CreateSquashCandidate` finished building.
    CandidateBuilt { outcome: MergeOutcome },
    /// The `RebaseOntoWithConflict` ran; the shim composed the conflict
    /// context (rebase outcome folded in — the rework brief's evidence).
    Rebased { conflict_context: String },
    /// The gate round reduced: every stage resolved or a stage failed.
    /// `first_stage_failed` is true when the failing stage was the FIRST,
    /// with later stages still pending — the deterministic compile
    /// classification input (§3.3, job #154). `compiler_output` is the failed
    /// build stage's captured output for the gate-fix brief.
    GateVerdict {
        failures: Vec<EvalResult>,
        first_stage_failed: bool,
        compiler_output: String,
    },
    /// The promote CAS (`AdvanceDefault`) was refused: HEAD moved under the
    /// parked candidate. A decision input, never an error (§3.3).
    PromoteRefused,
}

/// What the fold does after applying a decision's state, transitions, and
/// effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandingStep {
    /// An emitted effect's result re-enters as the next [`LandingEvent`].
    AwaitOutcome,
    /// The landing succeeded: the shim hands off to wrap-up
    /// (`finish_landing`, the C3 seam), then advances the queue.
    FinishLanding,
    /// This landing is finished (landed, reworked out, stale, or re-queued)
    /// — advance the queue.
    Completed,
    /// Escalated out of the landing: drop the exec slice, then advance the
    /// queue (the operator owns it now).
    CompletedDropExec,
    /// Gate tasks are running; the queue holds until the verdict re-enters.
    Gating,
}

/// What an eval-pass does about landing (§3.3 vs docs/reference/design-lifecycle.md
/// `finalize: none`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueStep {
    /// The job joined the queue; the shim pumps.
    Queued,
    /// `wrap_up: none`: nothing to land — the work's effect is external, the
    /// branch is scratch, and eval-pass IS the wrap-up. The shim completes
    /// the job directly (Evaluation→Done bookkeeping).
    CompleteDirectly,
}

/// A WrapUp-bound job joins the landing queue (`finalize_pass`'s decision
/// half): the Evaluation→WrapUp transition when it applies — `refinalize`
/// re-entries arrive already WrapUp — and the idempotent enqueue.
/// `wrap_up_none` is the job type's `wrap_up: none` declaration (a view
/// input, read from the exec slice).
pub fn decide_enqueue(
    mut state: MergeGateState,
    job: &Job,
    wrap_up_none: bool,
) -> (MergeGateState, Vec<Transition>, Vec<Effect>, EnqueueStep) {
    if wrap_up_none {
        return (state, vec![], vec![], EnqueueStep::CompleteDirectly);
    }
    let (owner, project) = split_project(job);
    let mut transitions = Vec::new();
    let mut effects = Vec::new();
    if job.state == JobState::Evaluation {
        transitions.push(Transition {
            job: Box::new(job.clone()),
            to: JobState::WrapUp,
        });
        effects.push(Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: job.id,
            event_type: "job-wrapup-started".to_string(),
            extra: serde_json::json!({}),
        });
    }
    state.enqueue(job.id);
    (state, transitions, effects, EnqueueStep::Queued)
}

/// Decide one landing step. Pure: every await the old `try_finalize` /
/// `gate_reduce` interleaved is either a pre-read in the view or an
/// [`Effect`] whose result re-enters as the next event.
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
)]
pub fn decide(
    state: MergeGateState,
    view: &LandingView<'_>,
    event: LandingEvent,
) -> (MergeGateState, Vec<Transition>, Vec<Effect>, LandingStep) {
    let job = view.job;
    let seq = job.id;
    let (owner, project) = split_project(job);
    let mut state = state;

    debug_assert!(
        !state.queue.contains(&seq),
        "landing #{seq} decided while still queued"
    );

    match event {
        LandingEvent::Start => {
            if job.state != JobState::WrapUp {
                return (state, vec![], vec![], LandingStep::Completed);
            }
            if let Some(skew) = &view.config_skew {
                let effects = vec![escalate_config_skew(owner, project, seq, skew)];
                return (state, vec![], effects, LandingStep::CompletedDropExec);
            }
            let base_ref = job
                .base_ref
                .clone()
                .unwrap_or_else(|| panic!("WrapUp job #{seq} has a base_ref"));
            let effect = if view.head == base_ref && !view.force_gate {
                Effect::SquashMerge {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    seq,
                    base_ref,
                    job_type: job.r#type.clone(),
                    summary: view.summary.clone(),
                }
            } else {
                Effect::CreateSquashCandidate {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    seq,
                    base_ref,
                    job_type: job.r#type.clone(),
                    summary: view.summary.clone(),
                }
            };
            (state, vec![], vec![effect], LandingStep::AwaitOutcome)
        }

        LandingEvent::Squashed { outcome } => match outcome {
            MergeOutcome::Merged { .. } | MergeOutcome::NoOp => {
                (state, vec![], vec![], LandingStep::FinishLanding)
            }
            MergeOutcome::Conflict { .. } => {
                state.pending_rework = Some(PendingRework::Conflict);
                let effects = vec![rebase_effect(owner, project, seq, &view.head)];
                (state, vec![], effects, LandingStep::AwaitOutcome)
            }
            MergeOutcome::UnresolvedMarkers { files } => {
                let effects = vec![escalate_markers(owner, project, seq, &files)];
                (state, vec![], effects, LandingStep::CompletedDropExec)
            }
        },

        LandingEvent::CandidateBuilt { outcome } => match outcome {
            MergeOutcome::NoOp => (state, vec![], vec![], LandingStep::FinishLanding),
            MergeOutcome::Conflict { .. } => {
                state.pending_rework = Some(PendingRework::Conflict);
                let effects = vec![rebase_effect(owner, project, seq, &view.head)];
                (state, vec![], effects, LandingStep::AwaitOutcome)
            }
            MergeOutcome::UnresolvedMarkers { files } => {
                let effects = vec![escalate_markers(owner, project, seq, &files)];
                (state, vec![], effects, LandingStep::CompletedDropExec)
            }
            MergeOutcome::Merged { commit } => {
                if view.gate_evaluators.is_empty() {
                    let effects = vec![
                        Effect::AdvanceDefault {
                            owner: owner.to_string(),
                            project: project.to_string(),
                            commit,
                            expected_old_head: view.head.clone(),
                        },
                        Effect::DeleteBranch {
                            owner: owner.to_string(),
                            project: project.to_string(),
                            branch: format!("merge-gate/{seq}"),
                        },
                    ];
                    return (state, vec![], effects, LandingStep::FinishLanding);
                }
                debug_assert!(state.gating.is_none(), "gate opened while occupied");
                state.gating = Some(seq);
                let effects = vec![
                    Effect::PublishEvent {
                        owner: owner.to_string(),
                        project: project.to_string(),
                        seq,
                        event_type: "job-merge-gate-started".to_string(),
                        extra: serde_json::json!({ "cycle": view.cycle }),
                    },
                    Effect::LaunchGateStage {
                        owner: owner.to_string(),
                        project: project.to_string(),
                        seq,
                        gate_branch: format!("merge-gate/{seq}"),
                        cycle: view.cycle,
                        evaluators: group_stages(view.gate_evaluators.clone())
                            .pop_front()
                            .unwrap_or_else(|| panic!("non-empty gate evaluators")),
                    },
                ];
                (state, vec![], effects, LandingStep::Gating)
            }
        },

        LandingEvent::Rebased { conflict_context } => {
            let pending = state
                .pending_rework
                .take()
                .unwrap_or_else(|| panic!("Rebased event without a pending rework"));
            let (reason, eval_context, rework_reason) = match pending {
                PendingRework::Conflict => {
                    ("merge_conflict", Vec::new(), ReworkReason::MergeConflict)
                }
                PendingRework::GateFailure { failures } => {
                    ("merge_gate_failure", failures, ReworkReason::GateCiFailure)
                }
            };
            let mut pinned = job.clone();
            pinned.base_ref = Some(view.head.clone());
            let effects = vec![
                Effect::PutJob {
                    job: Box::new(pinned),
                },
                Effect::PublishEvent {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    seq,
                    event_type: "job-rework-started".to_string(),
                    extra: serde_json::json!({
                        "cycle": view.cycle + 1, "reason": reason, "eval_context": eval_context,
                    }),
                },
                Effect::EnterWork {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    seq,
                    cycle: view.cycle + 1,
                    eval_context,
                    merge_conflict: Some(conflict_context),
                    rework_reason: Some(rework_reason),
                },
            ];
            (state, vec![], effects, LandingStep::Completed)
        }

        LandingEvent::GateVerdict {
            failures,
            first_stage_failed,
            compiler_output,
        } => {
            debug_assert_eq!(state.gating, Some(seq), "verdict for a non-gating seq");
            state.gating = None;

            if failures.is_empty() {
                let effects = vec![
                    Effect::AdvanceDefault {
                        owner: owner.to_string(),
                        project: project.to_string(),
                        commit: view
                            .gate_commit
                            .clone()
                            .unwrap_or_else(|| panic!("gate parked a candidate")),
                        expected_old_head: view
                            .gate_old_head
                            .clone()
                            .unwrap_or_else(|| panic!("gate parked its CAS base")),
                    },
                    Effect::DeleteBranch {
                        owner: owner.to_string(),
                        project: project.to_string(),
                        branch: format!("merge-gate/{seq}"),
                    },
                ];
                return (state, vec![], effects, LandingStep::FinishLanding);
            }

            let class = if first_stage_failed {
                GateFailureClass::Compile
            } else {
                GateFailureClass::Test
            };
            if class == GateFailureClass::Compile && view.gate_fix_used < GATE_FIX_BUDGET {
                let new_base = view
                    .gate_old_head
                    .clone()
                    .unwrap_or_else(|| panic!("gate parked its CAS base"));
                let effects = vec![Effect::LaunchGateFix {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    seq,
                    new_base,
                    failures,
                    compiler_output,
                }];
                return (state, vec![], effects, LandingStep::Completed);
            }

            let new_base = view
                .gate_old_head
                .clone()
                .unwrap_or_else(|| panic!("gate parked its CAS base"));
            state.pending_rework = Some(PendingRework::GateFailure { failures });
            let effects = vec![
                Effect::DeleteBranch {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    branch: format!("merge-gate/{seq}"),
                },
                rebase_effect(owner, project, seq, &new_base),
            ];
            (state, vec![], effects, LandingStep::AwaitOutcome)
        }

        LandingEvent::PromoteRefused => {
            let effects = vec![Effect::DeleteBranch {
                owner: owner.to_string(),
                project: project.to_string(),
                branch: format!("merge-gate/{seq}"),
            }];
            state.enqueue(seq);
            (state, vec![], effects, LandingStep::Completed)
        }
    }
}

/// How a merge-gate failure was classified (spec §3.3, job #154), determined
/// deterministically from *which gate stage* failed — never by parsing output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateFailureClass {
    /// The first (build/compile) stage failed while a distinct later stage
    /// was still queued: the branch no longer compiles. Eligible for the
    /// scoped gate-fix fast path.
    Compile,
    /// A later stage failed (the build passed), or the gate is a single
    /// opaque stage that can't be classified. Always takes the full rework
    /// loop.
    Test,
}

/// Partition evaluators into stages in ascending `stage` order, preserving
/// the declared order within a stage (stable sort). One distinct stage → one
/// group, which is exactly the unstaged single fan-out (§3.3 staged
/// evaluation).
pub fn group_stages(mut evaluators: Vec<Evaluator>) -> VecDeque<Vec<Evaluator>> {
    evaluators.sort_by_key(|e| e.stage);
    let mut stages: VecDeque<Vec<Evaluator>> = VecDeque::new();
    for e in evaluators {
        match stages.back_mut() {
            Some(last) if last[0].stage == e.stage => last.push(e),
            _ => stages.push_back(vec![e]),
        }
    }
    stages
}

/// The gate's evaluator set (§3.3): required command evaluators re-run
/// against the candidate; agents and advisory evaluators do not.
pub fn gate_evaluators(eval: &[Evaluator]) -> Vec<Evaluator> {
    eval.iter()
        .filter(|ev| ev.r#type == EvaluatorType::Command && ev.required.unwrap_or(true))
        .cloned()
        .collect()
}

/// Was this landing's current cycle a gate-fix round? Derived from the
/// persisted task list (a read → view input) so it holds across a restart: a
/// Work task of this cycle, not evaluator-owned, stamped `GateCompileFix`.
pub fn force_gate(tasks: &[Task], cycle: u32) -> bool {
    tasks.iter().any(|t| {
        t.phase == types::TaskPhase::Work
            && t.cycle == cycle
            && t.evaluator.is_none()
            && t.rework_reason == Some(ReworkReason::GateCompileFix)
    })
}

fn split_project(job: &Job) -> (&str, &str) {
    job.project
        .split_once('/')
        .unwrap_or_else(|| panic!("malformed job project '{}'", job.project))
}

fn rebase_effect(owner: &str, project: &str, seq: u64, new_base: &str) -> Effect {
    Effect::RebaseOntoWithConflict {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        new_base: new_base.to_string(),
    }
}

fn escalate_markers(owner: &str, project: &str, seq: u64, files: &[String]) -> Effect {
    Effect::Escalate {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        reason: "unresolved_conflict_markers".to_string(),
        detail: format!(
            "Job {seq}: the branch still carries unresolved conflict markers in {} \
             after a rebase-with-markers rework — merging would land \
             `<<<<<<< / ======= / >>>>>>>` on the default branch. Resolve the markers \
             on job/{seq} and commit before finalizing.",
            files.join(", ")
        ),
        failing_task: None,
    }
}

/// The merge-time half of the §14.3 skew gate: refuse a landing whose branch
/// carries a config ahead of this binary, naming the file and both epochs.
/// Harsh at the last step, and strictly better than merging a config that parks
/// every future job of its type (§14.2).
fn escalate_config_skew(owner: &str, project: &str, seq: u64, skew: &types::ConfigSkew) -> Effect {
    Effect::Escalate {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        reason: MERGE_CONFIG_SKEW_REASON.to_string(),
        detail: format!(
            "Job {seq}: '{}' on job/{seq} declares min_dispatcher: {} but this dispatcher is at \
             config schema epoch {} — merging it would park every job of that type (spec §14.2). \
             Deploy the newer dispatcher first and retry, or land the config behind a version gate.",
            skew.path, skew.needed, skew.running
        ),
        failing_task: None,
    }
}

/// The escalation reason a §14.3 merge-time skew refusal carries — named so the
/// producer and every consumer (the operator UI, the golden traces) read the
/// same string.
pub const MERGE_CONFIG_SKEW_REASON: &str = "merge_config_skew";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Branch-complete tier-1 coverage (the C2 payoff): every event arm ×
    //! every outcome, pure values in and out — no NATS, no Docker. The
    //! dispatcher's golden traces pin the same decisions end-to-end.
    use super::*;

    fn job(state: JobState) -> Job {
        let mut j: Job = serde_json::from_str(
            r#"{ "id": 7, "project": "acme/api", "type": "build",
                 "deps": [], "state": "WrapUp", "branch": "job/7",
                 "base_ref": "base0", "knowledge_tags": [], "factory": null,
                 "created_at": "2026-07-24T10:00:00Z", "ready_at": null }"#,
        )
        .expect("sample job");
        j.state = state;
        j
    }

    fn cmd_eval(name: &str, stage: u32) -> Evaluator {
        Evaluator {
            name: name.into(),
            r#type: EvaluatorType::Command,
            image: None,
            run: Some("./x.sh".into()),
            prompt: None,
            provider: None,
            model: None,
            secrets: vec![],
            workload_identities: vec![],
            required: None,
            stage,
        }
    }

    fn view<'a>(j: &'a Job, head: &str) -> LandingView<'a> {
        LandingView {
            job: j,
            head: head.into(),
            force_gate: false,
            summary: Some("did things".into()),
            cycle: 1,
            gate_fix_used: 0,
            gate_evaluators: vec![cmd_eval("ci", 0)],
            config_skew: None,
            gate_commit: Some("cand1".into()),
            gate_old_head: Some("head1".into()),
        }
    }

    #[test]
    fn enqueue_transitions_evaluation_and_queues_idempotently() {
        let j = job(JobState::Evaluation);
        let (state, transitions, effects, step) =
            decide_enqueue(MergeGateState::default(), &j, false);
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::WrapUp);
        assert!(matches!(&effects[0],
            Effect::PublishEvent { event_type, .. } if event_type == "job-wrapup-started"));
        assert_eq!(state.queue, [7]);
        assert_eq!(step, EnqueueStep::Queued);

        let j2 = job(JobState::WrapUp);
        let (state, transitions, effects, step) = decide_enqueue(state, &j2, false);
        assert!(transitions.is_empty() && effects.is_empty());
        assert_eq!(state.queue, [7]);
        assert_eq!(step, EnqueueStep::Queued);
    }

    #[test]
    fn wrap_up_none_completes_directly_without_queueing() {
        let j = job(JobState::Evaluation);
        let (state, transitions, effects, step) =
            decide_enqueue(MergeGateState::default(), &j, true);
        assert!(transitions.is_empty() && effects.is_empty());
        assert!(state.is_empty());
        assert_eq!(step, EnqueueStep::CompleteDirectly);
    }

    #[test]
    fn next_candidate_holds_on_gate_hold_drain_and_empty() {
        let mut s = MergeGateState::default();
        assert_eq!(s.next_candidate(false, false), None, "empty queue");
        s.enqueue(1);
        s.enqueue(2);
        assert_eq!(s.next_candidate(true, false), None, "origin hold");
        assert_eq!(s.next_candidate(false, true), None, "draining");
        s.gating = Some(9);
        assert_eq!(s.next_candidate(false, false), None, "gate occupied");
        s.gating = None;
        assert_eq!(s.next_candidate(false, false), Some(1), "FIFO");
        assert_eq!(s.next_candidate(false, false), Some(2));
        assert_eq!(s.next_candidate(false, false), None);
    }

    #[test]
    fn remove_unhooks_queue_gating_and_memory() {
        let mut s = MergeGateState {
            queue: [1, 2].into(),
            gating: Some(3),
            pending_rework: Some(PendingRework::Conflict),
        };
        s.remove(2);
        assert_eq!(s.queue, [1]);
        s.remove(3);
        assert_eq!(s.gating, None);
        assert_eq!(s.pending_rework, None, "gating removal drops the memory");
    }

    #[test]
    fn start_skips_a_stale_landing() {
        let j = job(JobState::Revoked);
        let (_, t, e, step) = decide(
            MergeGateState::default(),
            &view(&j, "base0"),
            LandingEvent::Start,
        );
        assert!(t.is_empty() && e.is_empty());
        assert_eq!(step, LandingStep::Completed);
    }

    #[test]
    fn start_fast_paths_when_head_unmoved() {
        let j = job(JobState::WrapUp);
        let (_, _, e, step) = decide(
            MergeGateState::default(),
            &view(&j, "base0"),
            LandingEvent::Start,
        );
        assert!(
            matches!(&e[0], Effect::SquashMerge { base_ref, summary, .. }
            if base_ref == "base0" && summary.as_deref() == Some("did things"))
        );
        assert_eq!(step, LandingStep::AwaitOutcome);
    }

    #[test]
    fn start_builds_candidate_when_head_moved_or_gate_forced() {
        let j = job(JobState::WrapUp);
        let (_, _, e, _) = decide(
            MergeGateState::default(),
            &view(&j, "head1"),
            LandingEvent::Start,
        );
        assert!(
            matches!(&e[0], Effect::CreateSquashCandidate { .. }),
            "moved HEAD"
        );

        let mut v = view(&j, "base0");
        v.force_gate = true;
        let (_, _, e, _) = decide(MergeGateState::default(), &v, LandingEvent::Start);
        assert!(
            matches!(&e[0], Effect::CreateSquashCandidate { .. }),
            "gate-fix cycle must re-gate even on an unmoved HEAD"
        );
    }

    /// §14.3: a branch carrying a config ahead of this binary lands nothing,
    /// and the escalation names the file and both epochs.
    #[test]
    fn start_refuses_a_branch_whose_config_is_ahead_of_this_binary() {
        let j = job(JobState::WrapUp);
        let mut v = view(&j, "base0");
        v.config_skew = Some(types::ConfigSkew {
            path: ".chug/jobs/gcp-proof.yaml".into(),
            needed: 6,
            running: 5,
        });
        let (state, t, e, step) = decide(MergeGateState::default(), &v, LandingEvent::Start);

        assert!(t.is_empty(), "no transition: the operator owns it now");
        assert_eq!(e.len(), 1);
        let Effect::Escalate { reason, detail, .. } = &e[0] else {
            panic!("expected an escalation, got {:?}", e[0]);
        };
        assert_eq!(reason, MERGE_CONFIG_SKEW_REASON);
        for needle in [".chug/jobs/gcp-proof.yaml", "6", "5"] {
            assert!(detail.contains(needle), "'{needle}' missing from {detail}");
        }
        assert_eq!(step, LandingStep::CompletedDropExec);
        assert!(state.is_empty(), "nothing gating, nothing queued");
    }

    #[test]
    fn merged_or_noop_finishes_the_landing() {
        let j = job(JobState::WrapUp);
        for outcome in [
            MergeOutcome::Merged { commit: "c".into() },
            MergeOutcome::NoOp,
        ] {
            let (_, _, e, step) = decide(
                MergeGateState::default(),
                &view(&j, "base0"),
                LandingEvent::Squashed { outcome },
            );
            assert!(e.is_empty());
            assert_eq!(step, LandingStep::FinishLanding);
        }
    }

    #[test]
    fn conflict_rebases_and_remembers_the_flavor() {
        let j = job(JobState::WrapUp);
        let (s, _, e, step) = decide(
            MergeGateState::default(),
            &view(&j, "head1"),
            LandingEvent::CandidateBuilt {
                outcome: MergeOutcome::Conflict {
                    files: vec!["a.rs".into()],
                },
            },
        );
        assert!(
            matches!(&e[0], Effect::RebaseOntoWithConflict { new_base, .. } if new_base == "head1")
        );
        assert_eq!(s.pending_rework, Some(PendingRework::Conflict));
        assert_eq!(step, LandingStep::AwaitOutcome);
    }

    #[test]
    fn unresolved_markers_escalate_and_release_exec() {
        let j = job(JobState::WrapUp);
        let (_, _, e, step) = decide(
            MergeGateState::default(),
            &view(&j, "base0"),
            LandingEvent::Squashed {
                outcome: MergeOutcome::UnresolvedMarkers {
                    files: vec!["a.rs".into()],
                },
            },
        );
        assert!(matches!(&e[0], Effect::Escalate { reason, detail, .. }
            if reason == "unresolved_conflict_markers" && detail.contains("a.rs")));
        assert_eq!(step, LandingStep::CompletedDropExec);
    }

    #[test]
    fn candidate_with_no_gate_evaluators_promotes_directly() {
        let j = job(JobState::WrapUp);
        let mut v = view(&j, "head1");
        v.gate_evaluators = vec![];
        let (s, _, e, step) = decide(
            MergeGateState::default(),
            &v,
            LandingEvent::CandidateBuilt {
                outcome: MergeOutcome::Merged {
                    commit: "cand1".into(),
                },
            },
        );
        assert!(
            matches!(&e[0], Effect::AdvanceDefault { commit, expected_old_head, .. }
            if commit == "cand1" && expected_old_head == "head1")
        );
        assert!(matches!(&e[1], Effect::DeleteBranch { branch, .. } if branch == "merge-gate/7"));
        assert_eq!(step, LandingStep::FinishLanding);
        assert_eq!(s.gating, None, "no gate opened");
    }

    #[test]
    fn candidate_with_gate_evaluators_opens_the_gate() {
        let j = job(JobState::WrapUp);
        let (s, _, e, step) = decide(
            MergeGateState::default(),
            &view(&j, "head1"),
            LandingEvent::CandidateBuilt {
                outcome: MergeOutcome::Merged {
                    commit: "cand1".into(),
                },
            },
        );
        assert!(matches!(&e[0], Effect::PublishEvent { event_type, .. }
            if event_type == "job-merge-gate-started"));
        assert!(
            matches!(&e[1], Effect::LaunchGateStage { gate_branch, evaluators, .. }
            if gate_branch == "merge-gate/7" && evaluators.len() == 1)
        );
        assert_eq!(step, LandingStep::Gating);
        assert_eq!(s.gating, Some(7), "depth-1 slot taken, by type");
    }

    #[test]
    fn rebased_after_conflict_reworks_without_findings() {
        let j = job(JobState::WrapUp);
        let state = MergeGateState {
            pending_rework: Some(PendingRework::Conflict),
            ..Default::default()
        };
        let (s, _, e, step) = decide(
            state,
            &view(&j, "head1"),
            LandingEvent::Rebased {
                conflict_context: "ctx".into(),
            },
        );
        assert!(
            matches!(&e[0], Effect::PutJob { job } if job.base_ref.as_deref() == Some("head1"))
        );
        assert!(matches!(&e[1], Effect::PublishEvent { extra, .. }
            if extra["reason"] == "merge_conflict"));
        assert!(
            matches!(&e[2], Effect::EnterWork { cycle: 2, rework_reason, merge_conflict, .. }
            if *rework_reason == Some(ReworkReason::MergeConflict)
                && merge_conflict.as_deref() == Some("ctx"))
        );
        assert_eq!(step, LandingStep::Completed);
        assert_eq!(s.pending_rework, None, "memory consumed");
    }

    #[test]
    fn rebased_after_gate_failure_reworks_with_findings() {
        let j = job(JobState::WrapUp);
        let failures = vec![EvalResult {
            evaluator: "ci".into(),
            pass: false,
            structured: None,
            output: Some("boom".into()),
        }];
        let state = MergeGateState {
            pending_rework: Some(PendingRework::GateFailure {
                failures: failures.clone(),
            }),
            ..Default::default()
        };
        let (_, _, e, _) = decide(
            state,
            &view(&j, "head1"),
            LandingEvent::Rebased {
                conflict_context: "ctx".into(),
            },
        );
        assert!(matches!(&e[1], Effect::PublishEvent { extra, .. }
            if extra["reason"] == "merge_gate_failure" && extra["eval_context"][0]["evaluator"] == "ci"));
        assert!(
            matches!(&e[2], Effect::EnterWork { eval_context, rework_reason, .. }
            if eval_context.len() == 1 && *rework_reason == Some(ReworkReason::GateCiFailure))
        );
    }

    fn gating_state() -> MergeGateState {
        MergeGateState {
            gating: Some(7),
            ..Default::default()
        }
    }

    #[test]
    fn clean_verdict_promotes_via_cas() {
        let j = job(JobState::WrapUp);
        let (s, _, e, step) = decide(
            gating_state(),
            &view(&j, "head2"),
            LandingEvent::GateVerdict {
                failures: vec![],
                first_stage_failed: false,
                compiler_output: String::new(),
            },
        );
        assert!(
            matches!(&e[0], Effect::AdvanceDefault { commit, expected_old_head, .. }
            if commit == "cand1" && expected_old_head == "head1")
        );
        assert_eq!(step, LandingStep::FinishLanding);
        assert_eq!(s.gating, None, "verdict releases the slot");
    }

    #[test]
    fn first_stage_failure_within_budget_takes_the_gate_fix() {
        let j = job(JobState::WrapUp);
        let (_, _, e, step) = decide(
            gating_state(),
            &view(&j, "head2"),
            LandingEvent::GateVerdict {
                failures: vec![failure()],
                first_stage_failed: true,
                compiler_output: "error[E0433]".into(),
            },
        );
        assert!(
            matches!(&e[0], Effect::LaunchGateFix { new_base, compiler_output, .. }
            if new_base == "head1" && compiler_output.contains("E0433"))
        );
        assert_eq!(step, LandingStep::Completed);
    }

    #[test]
    fn exhausted_budget_or_later_stage_failure_takes_full_rework() {
        let j = job(JobState::WrapUp);
        let mut v = view(&j, "head2");
        v.gate_fix_used = GATE_FIX_BUDGET;
        let (s, _, e, step) = decide(
            gating_state(),
            &v,
            LandingEvent::GateVerdict {
                failures: vec![failure()],
                first_stage_failed: true,
                compiler_output: String::new(),
            },
        );
        assert!(matches!(&e[0], Effect::DeleteBranch { branch, .. } if branch == "merge-gate/7"));
        assert!(
            matches!(&e[1], Effect::RebaseOntoWithConflict { new_base, .. } if new_base == "head1")
        );
        assert!(
            matches!(&s.pending_rework, Some(PendingRework::GateFailure { failures }) if failures.len() == 1)
        );
        assert_eq!(step, LandingStep::AwaitOutcome);

        let (_, _, e, _) = decide(
            gating_state(),
            &view(&j, "head2"),
            LandingEvent::GateVerdict {
                failures: vec![failure()],
                first_stage_failed: false,
                compiler_output: String::new(),
            },
        );
        assert!(matches!(&e[1], Effect::RebaseOntoWithConflict { .. }));
    }

    #[test]
    fn refused_promote_requeues_fifo() {
        let j = job(JobState::WrapUp);
        let mut state = MergeGateState::default();
        state.queue.push_back(9);
        let (s, _, e, step) = decide(state, &view(&j, "head2"), LandingEvent::PromoteRefused);
        assert!(matches!(&e[0], Effect::DeleteBranch { .. }));
        assert_eq!(s.queue, [9, 7], "re-entry joins the BACK of the queue");
        assert_eq!(step, LandingStep::Completed);
    }

    #[test]
    fn gate_evaluators_filters_to_required_commands() {
        let mut agent = cmd_eval("review", 0);
        agent.r#type = EvaluatorType::Agent;
        let mut advisory = cmd_eval("lint", 0);
        advisory.required = Some(false);
        let evals = vec![cmd_eval("ci", 0), agent, advisory];
        let gate = gate_evaluators(&evals);
        assert_eq!(gate.len(), 1);
        assert_eq!(gate[0].name, "ci");
    }

    fn failure() -> EvalResult {
        EvalResult {
            evaluator: "build".into(),
            pass: false,
            structured: None,
            output: None,
        }
    }
}

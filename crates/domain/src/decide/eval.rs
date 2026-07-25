//! Evaluation-phase decisions (spec §3.3) — refactor-plan C5, the largest
//! carve from `eval.rs`.
//!
//! The phase opens when work is Done and closes when the round has a verdict.
//! Between those two points every decision is here, told apart by
//! [`EvalEvent`]:
//!
//! - **Entered** — the Work→Evaluation fan-out (§3.2 steps 9–10, §3.3 staged
//!   evaluation): no evaluators auto-passes straight to landing; otherwise
//!   stage 0 launches and the later stages wait, so a stage that fails leaves
//!   the ones after it *uncreated*.
//! - **SlotExited** — the verdict source per evaluator type: a command's exit
//!   code IS its verdict (#198), an agent's `submit_eval` is, and a
//!   verdict-*less* exit (an abnormal exit with an empty stream, or an agent
//!   that never submitted) is infrastructure loss, not a product failure
//!   (#167).
//! - **SlotResolved** — a Human evaluator answered through the operator inbox.
//! - **SlotRelaunched** / **StageLaunched** — the continuation events
//!   (contracts.md §2): a launch effect's task ids come back and land on the
//!   round they belong to.
//! - **StageSettled** — restart reconciliation rebuilt the round from the task
//!   log and replays the advance-or-reduce decision (§3.6).
//!
//! Two budgets and one bound live in the reduce, and all three are decisions:
//! `eval_retries` (an infra-failed slot's retries), the [`EvalView::
//! infra_relaunch_cap`] bound on evidence-free relaunches, and `rework_budget`
//! (a product failure's rework cycles). A required **abort** verdict skips the
//! rework budget entirely — "not satisfiable by rework" (design-lifecycle.md).
//!
//! The round is a value ([`EvalRound`]), swapped wholesale by the shim exactly
//! as C2 swaps `MergeGateState`, so "one stage in flight per job" holds by
//! type: `slots` is the live stage, `pending` the stages not yet created,
//! `done` the stages that passed.
//!
//! The `Msg` contracts this decider owns (contracts.md §1):
//!
//! - `Msg::TaskExited` for a `TaskPhase::Evaluation` task — **pre:** the job is
//!   `Evaluation` with a live execution slice, and the task is an *open* slot
//!   of the current round (anything else is a superseded round's monitor or a
//!   duplicate exit, and is [`EvalStep::Ignored`]); **post:** that slot is
//!   resolved exactly once, the task record is terminal (`Done`/`Failed`), and
//!   the round is either still awaiting slots, advanced to the next stage,
//!   or reduced — never left with a resolved-but-unread stage.
//! - `Msg::ResolveTask` for a Human evaluator (`resolve_eval_slot`) —
//!   **pre:** the task is an open slot of the live round (the shim answers a
//!   resolution that is not with `InvalidResolution`); **post:** as above.
//! - **Both:** a round that reduces leaves the job in exactly one of three
//!   shapes — handed to landing ([`EvalStep::Finalize`]), back in Work for a
//!   rework cycle ([`EvalStep::Rework`], one `reworks_used` spent), or
//!   `Escalated` with the execution slice released
//!   ([`EvalStep::EscalatedDropExec`]). It never stays in Evaluation.
//!
//! - **Accepts:** the round value, an [`EvalView`] (the target job, the job
//!   type's evaluator list and budgets, the cycle, the drain flag, the clock)
//!   and an [`EvalEvent`].
//! - **Emits:** `(Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep)` —
//!   values only. The owned effect set (contracts.md §2): `LaunchEvalStage`,
//!   `LaunchEvaluator`, `PutTask`, `Escalate`, and `PublishEvent` for
//!   `job-evaluation-started`, `task-completed`, `task-failed` and
//!   `job-rework-started`. The [`EvalStep`] names the shell bookkeeping that
//!   follows — landing, the rework re-entry, releasing the execution slice —
//!   all of which touch dispatcher state the pure crate cannot see.
//! - **Guarantees:** pure and synchronous; every branch exhaustively matched
//!   and unit-tested; asserts negative space (STYLE.md Tier 2 #2) — never
//!   resolves a slot twice, never reduces over an unresolved slot or an empty
//!   round, never decides an exit for a task from another phase. Performs no
//!   effect, holds no `&mut Core`.
//! - **Spec:** §3.3; §3.2 steps 9–10; §3.6 (drain, round rebuild);
//!   contracts.md §2; refactor-plan C5.

use crate::decide::Transition;
use crate::decide::merge_gate::group_stages;
use crate::effects::Effect;
use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use types::{
    EvalResult, Evaluator, Job, JobState, Task, TaskKind, TaskPhase, TaskResult, TaskState,
    TokenUsage, WorkType,
};

/// Machine code for an evaluator that exited without ever delivering a verdict
/// (#167, narrowed #198): a Command whose container died before it could judge —
/// an ABNORMAL exit (a signal kill `>= 128`, or the negative backend-`wait`
/// sentinel) with an empty captured stream — or an Agent ending without a
/// `submit_eval` verdict. Distinct from a product failure — the code was never
/// actually judged. A *normal* non-zero command exit (1..=127) IS a verdict and
/// reworks like any product fail, even with empty output (#198). Surfaced on the
/// `task-failed`/`job-escalated` event `reason`; the retire path stamps the task
/// `infra_loss` (reusing the §3.6/#83 no-retry-burned machinery).
pub const EVAL_NO_OUTPUT_REASON: &str = "evaluator_no_output";

/// A normal program exit — anything outside it (a signal kill `>= 128`, or the
/// backend's negative `wait` sentinel) is a *verdict-less* exit: the container
/// died before it could judge (#167/#198).
const NORMAL_EXIT_CODES: std::ops::Range<i32> = 0..128;

/// One cycle's evaluation, run as an ascending sequence of stages (spec §3.3
/// staged evaluation). Only the current stage has live tasks: `slots` is the
/// stage in flight, `pending` the stages not yet created, `done` the outcomes
/// of stages that already completed and passed. The reduce folds `done` and
/// `slots` together. A single-stage round leaves `pending`/`done` empty and is
/// byte-for-byte the unstaged behavior; the merge gate always builds one.
#[derive(Debug)]
pub struct EvalRound {
    pub slots: Vec<EvalSlot>,
    /// Evaluators for stages not yet launched, grouped ascending by `stage`.
    pub pending: VecDeque<Vec<Evaluator>>,
    /// Slots from earlier stages that completed and let the round advance.
    pub done: Vec<EvalSlot>,
}

impl EvalRound {
    /// The index of the *open* slot awaiting `task_id`, if any. `None` means
    /// the event belongs to a superseded round, or is a duplicate exit for a
    /// slot already resolved — either way there is nothing to decide.
    pub fn open_slot(&self, task_id: u64) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.task_id == task_id && s.outcome.is_none())
    }

    /// True once every slot in the stage in flight has an outcome.
    fn stage_resolved(&self) -> bool {
        self.slots.iter().all(|s| s.outcome.is_some())
    }
}

#[derive(Debug)]
pub struct EvalSlot {
    pub evaluator: Evaluator,
    pub task_id: u64,
    pub attempt: u32,
    pub outcome: Option<SlotOutcome>,
}

#[derive(Debug, Clone)]
pub enum SlotOutcome {
    Product {
        pass: bool,
        /// "Not satisfiable by rework" (design-lifecycle.md): a required
        /// evaluator's abort escalates at reduce instead of consuming budget.
        abort: bool,
        structured: Option<serde_json::Value>,
        /// A command evaluator's captured output tail (#167), threaded into the
        /// rework/re-review context as the failure evidence. `None` for agent
        /// evaluators, which report through `structured` findings.
        output: Option<String>,
    },
    /// Agent eval exhausted `eval_retries` without a `submit_eval` (§3.3).
    Infra,
}

/// Whether a completed stage lets the next stage start: every *required*
/// evaluator resolved to a product `pass: true`. A required product fail, an
/// abort (which implies `pass: false`), or an infra failure closes the round —
/// later stages are not created. Advisory (`required: false`) outcomes never
/// block progression.
pub fn stage_passed(slots: &[EvalSlot]) -> bool {
    slots.iter().all(|s| {
        !s.evaluator.required.unwrap_or(true)
            || matches!(s.outcome, Some(SlotOutcome::Product { pass: true, .. }))
    })
}

/// The read-only inputs one evaluation decision consumes. The shim re-gathers
/// it before every [`decide`] call; reads feed the view, they are not effects.
pub struct EvalView<'a> {
    /// The job being evaluated. At [`EvalEvent::Entered`] this is the record the
    /// pre-eval rebase just pinned in memory — the entry transition is what
    /// persists its `base_ref` (§3.2).
    pub job: &'a Job,
    /// The job type's evaluator list (§3.3), as declared. Read at entry only:
    /// every later decision reads the round's own slots.
    pub evaluators: &'a [Evaluator],
    /// The execution slice's cycle.
    pub cycle: u32,
    /// Eval-failure reworks already spent (`rework_budget` accounting).
    pub reworks_used: u32,
    /// The job type's `rework_budget` (0 when undeclared).
    pub rework_budget: u32,
    /// The job type's work type: a `command` work job has no author to rework
    /// against, so a product failure escalates rather than reworking (§3.3).
    pub work_type: WorkType,
    /// The job type's `eval_retries` (1 when undeclared) — how many times an
    /// infra-failed slot relaunches before resolving as
    /// [`SlotOutcome::Infra`].
    pub eval_retries: u32,
    /// Evidence-free losses already PERSISTED for the exiting evaluator's
    /// lineage (this cycle, this evaluator), excluding the loss being decided —
    /// [`decide`] counts that one in itself.
    pub infra_losses_prior: u32,
    /// Bound on evidence-free relaunches for one evaluator lineage (§3.6): on
    /// exhaustion the job escalates [`EVAL_NO_OUTPUT_REASON`] instead of
    /// relaunching forever (STYLE.md Tier 2 #3 — every loop has a cap).
    pub infra_relaunch_cap: u32,
    /// §3.6 graceful drain: launch no containers, advance no stage, run no
    /// reduce. Restart reconciliation rebuilds the round and replays the
    /// decision.
    pub draining: bool,
    /// The decision moment, stamped on retired task records.
    pub now: DateTime<Utc>,
}

/// What an evaluator container reported at exit — the dispatcher's `TaskExit`
/// narrowed to the fields the verdict decision reads (its full form carries
/// port types and work-phase fields the pure crate has no use for).
#[derive(Debug, Default)]
pub struct EvalExit {
    pub exit_code: i32,
    /// `/workspace/eval-result.json`, for a command evaluator that wrote one.
    pub eval_json: Option<serde_json::Value>,
    /// Usage measured from the agent CLI's own result — preferred over the
    /// agent's self-report, which it may omit or invent.
    pub usage: Option<TokenUsage>,
    /// Set when the container never launched: an infra failure of the slot
    /// whatever the evaluator's type, since a verdict needs a container.
    pub launch_error: Option<String>,
    /// The captured stdout+stderr tail (#167) — a command evaluator's failure
    /// evidence, threaded into the rework/re-review context.
    pub log_tail: Option<String>,
}

/// What drove this evaluation decision. The first is [`EvalEvent::Entered`];
/// the launch events carry the result of the effect the previous [`decide`]
/// emitted (contracts.md §2's continuation contract).
#[derive(Debug)]
pub enum EvalEvent {
    /// Work is Done and the branch is rebased: fan out (§3.2 steps 9–10).
    Entered,
    /// A `LaunchEvalStage` effect created its tasks; these are the live slots.
    StageLaunched { slots: Vec<EvalSlot> },
    /// A `LaunchEvaluator` effect re-created one slot's task (an infra retry or
    /// an evidence-free relaunch), identified by evaluator name — the slot's
    /// prior task id is gone.
    SlotRelaunched {
        evaluator: String,
        task_id: u64,
        attempt: u32,
    },
    /// An evaluator container exited (§3.3). `task` is the record as persisted
    /// (for an agent evaluator that is the `submit_eval` verdict the shim
    /// re-read).
    SlotExited { task: Box<Task>, exit: EvalExit },
    /// A Human evaluator resolved through the operator inbox (§3.3).
    SlotResolved {
        task_id: u64,
        pass: bool,
        abort: bool,
        structured: Option<serde_json::Value>,
    },
    /// Restart reconciliation rebuilt the round from the task log: replay the
    /// advance-or-reduce decision the crash lost (§3.6).
    StageSettled,
}

/// The bookkeeping the shim owes after applying a decision. Everything here
/// touches dispatcher-side state — the landing queue, the Work phase, the
/// execution slice — that is deliberately outside the pure crate.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalStep {
    /// Evaluator tasks are running (or a Human evaluator is in the inbox):
    /// nothing to do until a slot resolves.
    Await,
    /// An emitted effect's result re-enters as the next [`EvalEvent`].
    AwaitOutcome,
    /// §3.6 drain: the round holds exactly where it is.
    Hold,
    /// The event named no open slot of the live round (a superseded round's
    /// monitor, a duplicate exit, a released slice): nothing to decide.
    Ignored,
    /// The round passed — or there was nothing to evaluate: the shim hands the
    /// job to landing (`finalize_pass`, the C2 seam).
    Finalize,
    /// A product failure with budget left (§3.3): the shim stamps
    /// `reworks_used` on the execution slice — before the effects, because
    /// re-entering Work reads that counter — then re-enters Work at `cycle`
    /// with `eval_context` as the §4.3 rework brief.
    Rework {
        cycle: u32,
        reworks_used: u32,
        eval_context: Vec<EvalResult>,
    },
    /// Escalated out of evaluation: the shim releases the execution slice
    /// before the `Escalate` effect runs, so the escalation task is not
    /// stamped with the cycle of a slice the decision just ended.
    EscalatedDropExec,
}

impl EvalStep {
    /// True when the shim must release the job's execution slice — after the
    /// transitions, before the effects (the order C2/C3 established).
    pub fn drops_exec(&self) -> bool {
        matches!(self, EvalStep::EscalatedDropExec)
    }
}

/// Decide one evaluation step (spec §3.3). Pure: every await the old
/// `enter_evaluation` / `on_eval_exited` / `stage_complete` / `reduce` chain
/// interleaved is either a pre-read in the view or an [`Effect`] whose result
/// re-enters as the next event.
pub fn decide(
    round: Option<EvalRound>,
    view: &EvalView<'_>,
    event: EvalEvent,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let (owner, project) = split_project(view.job);
    match event {
        EvalEvent::Entered => decide_entered(round, view, owner, project),
        EvalEvent::StageLaunched { slots } => decide_stage_launched(round, slots),
        EvalEvent::SlotRelaunched {
            evaluator,
            task_id,
            attempt,
        } => decide_slot_relaunched(round, &evaluator, task_id, attempt),
        EvalEvent::SlotExited { task, exit } => {
            decide_slot_exited(round, view, owner, project, *task, exit)
        }
        EvalEvent::SlotResolved {
            task_id,
            pass,
            abort,
            structured,
        } => decide_slot_resolved(
            round, view, owner, project, task_id, pass, abort, structured,
        ),
        EvalEvent::StageSettled => match round {
            Some(round) => decide_settled(round, view, owner, project, Vec::new()),
            None => (None, Vec::new(), Vec::new(), EvalStep::Ignored),
        },
    }
}

/// Work→Evaluation (§3.2 steps 9–10, §3.3 staged fan-out): move the record,
/// announce the round, and create stage 0 — or, with no evaluators at all,
/// auto-pass straight to landing.
fn decide_entered(
    round: Option<EvalRound>,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let seq = view.job.id;
    debug_assert!(
        round.is_none(),
        "evaluation entered for job #{seq} over a live round",
    );
    // Draining (§3.6): launch no evaluator containers and do not move the
    // record — the job stays in Work with its Done work task and restart
    // reconciliation re-enters here. (The shim skips the pre-eval rebase for
    // the same reason: a drain performs no git work either.)
    if view.draining {
        return (round, Vec::new(), Vec::new(), EvalStep::Hold);
    }
    // Negative space (§2.1): terminal states are absorbing, so a job that was
    // revoked out from under its work task never enters evaluation.
    debug_assert!(
        !view.job.state.is_terminal(),
        "evaluation entered for terminal job #{seq} in {:?}",
        view.job.state,
    );

    let transitions = vec![Transition {
        job: Box::new(view.job.clone()),
        to: JobState::Evaluation,
    }];
    let mut effects = vec![Effect::PublishEvent {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        event_type: "job-evaluation-started".to_string(),
        extra: serde_json::json!({ "cycle": view.cycle }),
    }];

    if view.evaluators.is_empty() {
        // Nothing to judge: the eval-pass IS immediate (§3.3 auto-pass).
        return (None, transitions, effects, EvalStep::Finalize);
    }

    // Staged fan-out (§3.3): create stage 0 now and hold the rest until each
    // prior stage passes. A single-stage job launches everything at once.
    let mut pending = group_stages(view.evaluators.to_vec());
    let first = pending
        .pop_front()
        .unwrap_or_else(|| panic!("job #{seq}: non-empty evaluators group into >= 1 stage"));
    effects.push(launch_stage(owner, project, view, first));
    let round = EvalRound {
        slots: Vec::new(),
        pending,
        done: Vec::new(),
    };
    (Some(round), transitions, effects, EvalStep::AwaitOutcome)
}

/// A stage's tasks exist: they are the round's live slots. The round is the
/// only place their task ids are known, so this is where the fan-out's result
/// lands (contracts.md §2's continuation contract).
fn decide_stage_launched(
    round: Option<EvalRound>,
    slots: Vec<EvalSlot>,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    // The slice was released while the stage launched (a revoke): the launched
    // tasks' exits will find no open slot and be ignored in turn.
    let Some(mut round) = round else {
        return (None, Vec::new(), Vec::new(), EvalStep::Ignored);
    };
    debug_assert!(
        round.slots.is_empty(),
        "a stage launched over {} live slot(s)",
        round.slots.len(),
    );
    debug_assert!(!slots.is_empty(), "a launched stage has at least one slot");
    round.slots = slots;
    (Some(round), Vec::new(), Vec::new(), EvalStep::Await)
}

/// A relaunched slot's new task id (an infra retry, or an evidence-free
/// relaunch that spent no budget). Located by evaluator name: the slot's prior
/// task id is exactly what the relaunch replaced.
fn decide_slot_relaunched(
    round: Option<EvalRound>,
    evaluator: &str,
    task_id: u64,
    attempt: u32,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let Some(mut round) = round else {
        return (None, Vec::new(), Vec::new(), EvalStep::Ignored);
    };
    let Some(idx) = round
        .slots
        .iter()
        .position(|s| s.evaluator.name == evaluator && s.outcome.is_none())
    else {
        return (Some(round), Vec::new(), Vec::new(), EvalStep::Ignored);
    };
    round.slots[idx].task_id = task_id;
    round.slots[idx].attempt = attempt;
    (Some(round), Vec::new(), Vec::new(), EvalStep::Await)
}

/// An evaluator container exited (§3.3). The verdict source depends on the
/// evaluator's type — a command's exit code, an agent's `submit_eval` — and a
/// verdict-*less* exit routes to the evidence-free path instead (#167/#198).
fn decide_slot_exited(
    round: Option<EvalRound>,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    task: Task,
    exit: EvalExit,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let Some(round) = round else {
        return (None, Vec::new(), Vec::new(), EvalStep::Ignored);
    };
    // Stale monitor from a superseded round, or a duplicate exit.
    let Some(idx) = round.open_slot(task.id) else {
        return (Some(round), Vec::new(), Vec::new(), EvalStep::Ignored);
    };
    debug_assert_eq!(
        task.phase,
        TaskPhase::Evaluation,
        "evaluator exit decided for a {:?} task",
        task.phase,
    );

    // The container never launched: an infra failure whatever the evaluator's
    // type (a command exit code or an agent verdict needs a container that
    // ran). Record why, then route through the same `eval_retries` path an
    // agent's missing verdict uses — without this the task stays Running and
    // the job wedges in Evaluation forever (the dogfood-#1 bug).
    if let Some(reason) = exit.launch_error {
        let mut task = task;
        task.result = Some(TaskResult::Command {
            pass: false,
            exit_code: exit.exit_code,
            output: reason,
            structured: None,
        });
        let attempt = task.attempt;
        let task_id = task.id;
        let effects = vec![
            retire(task, view.now, false),
            task_failed(
                owner,
                project,
                view.job.id,
                task_id,
                "container launch failed",
            ),
        ];
        return decide_infra_failure(round, view, owner, project, idx, attempt, effects);
    }

    match &task.kind {
        TaskKind::Command { .. } => {
            decide_command_exit(round, view, owner, project, task, exit, idx)
        }
        TaskKind::Agent { .. } => decide_agent_exit(round, view, owner, project, task, exit, idx),
        // A Human evaluator launches no container, so no exit is its verdict —
        // the inbox resolution is (`SlotResolved`).
        TaskKind::Human { .. } => (Some(round), Vec::new(), Vec::new(), EvalStep::Await),
    }
}

/// A command evaluator's exit (#167/#198): the EXIT CODE is the verdict. A
/// normal non-zero exit (1..=127) is a real product failure the job reworks
/// against — even with a completely empty captured stream (a legitimately
/// silent failure, e.g. `test -f x || exit 1`); #167's original guard
/// mislabelled every empty-output fail as evidence-free and auto-retried it,
/// silently discarding real verdicts (job #198). The evidence-free case it
/// actually targets is an ABNORMAL exit (a signal kill `>= 128` or the negative
/// `wait` sentinel) with no output: the container died before judging.
fn decide_command_exit(
    round: EvalRound,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    mut task: Task,
    exit: EvalExit,
    idx: usize,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let mut round = round;
    let seq = view.job.id;
    let task_id = task.id;
    let pass = exit.exit_code == 0;
    // The captured tail (last ~8 KB, harvested by the eval monitor) is the
    // failure evidence for the job page, the rework brief and #155's re-review.
    let output = exit.log_tail.unwrap_or_default();
    let verdict_less = !NORMAL_EXIT_CODES.contains(&exit.exit_code);

    if verdict_less && output.trim().is_empty() {
        task.result = Some(TaskResult::Command {
            pass: false,
            exit_code: exit.exit_code,
            output,
            structured: exit.eval_json,
        });
        let attempt = task.attempt;
        let effects = vec![
            retire(task, view.now, true),
            task_failed(owner, project, seq, task_id, EVAL_NO_OUTPUT_REASON),
        ];
        return decide_no_output(round, view, owner, project, idx, attempt, task_id, effects);
    }

    // A non-empty output on a fail is what the next work agent reads to see WHY
    // the evaluator failed; an empty one carries no evidence to thread.
    let slot_output = (!output.is_empty()).then(|| output.clone());
    task.result = Some(TaskResult::Command {
        pass,
        exit_code: exit.exit_code,
        output,
        structured: exit.eval_json.clone(),
    });
    task.state = TaskState::Done;
    task.completed_at = Some(view.now);
    let effects = vec![
        Effect::PutTask {
            task: Box::new(task),
        },
        Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq,
            event_type: "task-completed".to_string(),
            extra: serde_json::json!({
                "task_id": task_id, "phase": "Evaluation", "pass": pass,
            }),
        },
    ];
    // Command evaluators can't judge fixability: no abort verdict.
    round.slots[idx].outcome = Some(SlotOutcome::Product {
        pass,
        abort: false,
        structured: exit.eval_json,
        output: slot_output,
    });
    decide_settled(round, view, owner, project, effects)
}

/// An agent evaluator's exit (§3.3). `submit_eval` recorded the verdict (and
/// announced it) before the container exited, so the persisted record IS the
/// verdict; an exit with no such record produced no evidence and takes the
/// evidence-free path (#167).
fn decide_agent_exit(
    round: EvalRound,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    mut task: Task,
    exit: EvalExit,
    idx: usize,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let mut round = round;
    let task_id = task.id;
    let mut effects = Vec::new();
    // `submit_eval` could only self-report usage; now the container is gone we
    // have the CLI's measured figure — prefer it, as the work path does.
    if let (Some(measured), Some(TaskResult::Agent { token_usage, .. })) =
        (exit.usage, task.result.as_mut())
    {
        *token_usage = Some(measured);
        effects.push(Effect::PutTask {
            task: Box::new(task.clone()),
        });
    }

    match &task.result {
        Some(TaskResult::Agent {
            pass,
            abort,
            structured,
            ..
        }) => {
            round.slots[idx].outcome = Some(SlotOutcome::Product {
                pass: *pass,
                abort: *abort,
                structured: structured.clone(),
                // Agents report through structured findings, not a log tail.
                output: None,
            });
            decide_settled(round, view, owner, project, effects)
        }
        // #167: an agent evaluator that ended without a `submit_eval` verdict
        // produced no evidence — the same invalid-fail class as a command with
        // an empty stream. Route it through the evidence-free path (no
        // `eval_retries` burned, escalates `evaluator_no_output`) rather than a
        // plain infra retry, so the reason distinguishes "no verdict" from a
        // real infra loss and the round is never failed on nothing.
        _ => {
            let attempt = task.attempt;
            effects.push(retire(task, view.now, true));
            effects.push(task_failed(
                owner,
                project,
                view.job.id,
                task_id,
                EVAL_NO_OUTPUT_REASON,
            ));
            decide_no_output(round, view, owner, project, idx, attempt, task_id, effects)
        }
    }
}

/// A Human evaluator resolved through the inbox (§3.3): record the verdict on
/// the slot and let the round settle. The task record itself is written by the
/// resolution handler, not here.
#[allow(clippy::too_many_arguments)]
fn decide_slot_resolved(
    round: Option<EvalRound>,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    task_id: u64,
    pass: bool,
    abort: bool,
    structured: Option<serde_json::Value>,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let Some(mut round) = round else {
        return (None, Vec::new(), Vec::new(), EvalStep::Ignored);
    };
    let Some(idx) = round.open_slot(task_id) else {
        return (Some(round), Vec::new(), Vec::new(), EvalStep::Ignored);
    };
    round.slots[idx].outcome = Some(SlotOutcome::Product {
        pass,
        abort,
        structured,
        output: None, // humans report through structured findings
    });
    decide_settled(round, view, owner, project, Vec::new())
}

/// An eval slot failed for infra reasons — the agent produced no verdict, or
/// its container never launched (§3.3). Retry per `eval_retries`; once the
/// budget is spent the slot resolves as [`SlotOutcome::Infra`] and the round
/// settles (a required infra failure escalates at the reduce).
fn decide_infra_failure(
    round: EvalRound,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    idx: usize,
    failed_attempt: u32,
    mut effects: Vec<Effect>,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let mut round = round;
    if failed_attempt <= view.eval_retries {
        effects.push(relaunch(
            owner,
            project,
            view,
            &round.slots[idx].evaluator,
            failed_attempt + 1,
        ));
        return (Some(round), Vec::new(), effects, EvalStep::AwaitOutcome);
    }
    round.slots[idx].outcome = Some(SlotOutcome::Infra);
    decide_settled(round, view, owner, project, effects)
}

/// #167 (narrowed #198): an evaluator exited without ever delivering a verdict.
/// This is infrastructure loss, not a product verdict: relaunch the SAME
/// attempt WITHOUT spending an `eval_retries` budget (the §3.6/#83 infra-loss
/// semantics — no rework, no cycle consumed), bounded by
/// [`EvalView::infra_relaunch_cap`] over the evaluator's lineage. On exhaustion
/// escalate, so a human sees "the evaluator cannot produce evidence" rather
/// than "the code failed review".
#[allow(clippy::too_many_arguments)]
fn decide_no_output(
    round: EvalRound,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    idx: usize,
    failed_attempt: u32,
    failed_task: u64,
    mut effects: Vec<Effect>,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let seq = view.job.id;
    let evaluator = &round.slots[idx].evaluator;
    // The loss being decided is not in `infra_losses_prior` (its record is
    // retired by an effect that has not run yet), so the Nth loss sees N —
    // the same count the pre-C5 read-after-write produced.
    let losses = view.infra_losses_prior + 1;
    if losses > view.infra_relaunch_cap {
        let name = evaluator.name.clone();
        effects.push(Effect::Escalate {
            owner: owner.to_string(),
            project: project.to_string(),
            seq,
            reason: EVAL_NO_OUTPUT_REASON.to_string(),
            detail: format!(
                "Job {seq}: evaluator '{name}' exited without producing any output \
                 {losses} times — it cannot produce evidence of a verdict. A human \
                 should review the evaluator itself rather than the code."
            ),
            failing_task: Some(failed_task),
        });
        return (None, Vec::new(), effects, EvalStep::EscalatedDropExec);
    }
    // Same attempt: `eval_retries` untouched (infra loss, not a real failure).
    effects.push(relaunch(owner, project, view, evaluator, failed_attempt));
    (Some(round), Vec::new(), effects, EvalStep::AwaitOutcome)
}

/// Whatever resolved a slot, the round now either waits, advances, or reduces
/// (§3.3 staged evaluation). Advance only when every *required* evaluator of
/// the finished stage passed AND a later stage remains; a short-circuited stage
/// leaves the pending stages uncreated — they simply have no task records for
/// this cycle.
fn decide_settled(
    round: EvalRound,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    mut effects: Vec<Effect>,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let mut round = round;
    if !round.stage_resolved() {
        return (Some(round), Vec::new(), effects, EvalStep::Await);
    }
    // Draining (§3.6): don't advance to the next stage, run the reduce, or open
    // the merge gate. The resolved slots are kept; restart reconciliation
    // rebuilds the round from the task log and replays this decision.
    if view.draining {
        return (Some(round), Vec::new(), effects, EvalStep::Hold);
    }
    if stage_passed(&round.slots) && !round.pending.is_empty() {
        // Fold the finished stage into `done` and fan out the next one.
        let finished = std::mem::take(&mut round.slots);
        round.done.extend(finished);
        let next = round
            .pending
            .pop_front()
            .unwrap_or_else(|| panic!("job #{}: advance implies a pending stage", view.job.id));
        effects.push(launch_stage(owner, project, view, next));
        return (Some(round), Vec::new(), effects, EvalStep::AwaitOutcome);
    }
    decide_reduce(round, view, owner, project, effects)
}

/// The §3.3 reduce, over every stage that ran: earlier stages that passed
/// (`done`) plus the final stage (`slots`). Stages never created — skipped by a
/// short-circuit — contribute nothing, exactly as intended.
fn decide_reduce(
    round: EvalRound,
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    mut effects: Vec<Effect>,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let seq = view.job.id;
    let cycle = view.cycle;
    let RoundVerdict {
        results,
        required_infra_failure,
        overall_pass,
        aborted,
    } = fold_round(&round, seq);

    if required_infra_failure {
        effects.push(escalate(
            owner,
            project,
            seq,
            "eval_infra_failure",
            format!("Job {seq}: a required evaluator exhausted eval_retries"),
        ));
        return (None, Vec::new(), effects, EvalStep::EscalatedDropExec);
    }
    if overall_pass {
        // The round is spent but kept: the landing reads no slots, and the
        // execution slice (round included) is released when the job completes.
        return (Some(round), Vec::new(), effects, EvalStep::Finalize);
    }

    // Abort verdict: rework can't fix this — skip the remaining budget and hand
    // the evaluators' findings to a human (design-lifecycle.md).
    if !aborted.is_empty() {
        effects.push(escalate(
            owner,
            project,
            seq,
            "eval_abort",
            format!(
                "Job {seq}: evaluator(s) {} declared cycle {cycle} not satisfiable by rework:\n\n{}",
                aborted.join(", "),
                abort_findings(&results, &aborted),
            ),
        ));
        return (None, Vec::new(), effects, EvalStep::EscalatedDropExec);
    }

    decide_product_failure(view, owner, project, results, effects)
}

/// A plain product failure (§3.3): rework under budget, else escalate. A
/// `command` work job has no author to rework against, so it always escalates.
/// Either way the round is spent — re-entering Work installs a fresh slice.
fn decide_product_failure(
    view: &EvalView<'_>,
    owner: &str,
    project: &str,
    results: Vec<EvalResult>,
    mut effects: Vec<Effect>,
) -> (Option<EvalRound>, Vec<Transition>, Vec<Effect>, EvalStep) {
    let (seq, cycle) = (view.job.id, view.cycle);
    if view.work_type == WorkType::Command || view.reworks_used >= view.rework_budget {
        effects.push(escalate(
            owner,
            project,
            seq,
            "rework_budget_exhausted",
            format!("Job {seq}: evaluation failed in cycle {cycle} with no rework budget left"),
        ));
        return (None, Vec::new(), effects, EvalStep::EscalatedDropExec);
    }
    effects.push(Effect::PublishEvent {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        event_type: "job-rework-started".to_string(),
        extra: serde_json::json!({
            "cycle": cycle + 1, "reason": "eval_failure", "eval_context": results,
        }),
    });
    let step = EvalStep::Rework {
        cycle: cycle + 1,
        reworks_used: view.reworks_used + 1,
        eval_context: results,
    };
    (None, Vec::new(), effects, step)
}

/// What the reduce's fold saw across every stage that ran.
struct RoundVerdict {
    /// One result per slot, in stage order — the §4.3 rework context.
    results: Vec<EvalResult>,
    /// A REQUIRED evaluator never produced a verdict (§3.3).
    required_infra_failure: bool,
    /// Every required evaluator passed.
    overall_pass: bool,
    /// Required evaluators that declared the work unsalvageable
    /// (design-lifecycle.md abort verdict). Advisory aborts are plain advisory
    /// fails and never appear here.
    aborted: Vec<String>,
}

/// Fold the round's slots — earlier passed stages (`done`) then the final stage
/// (`slots`) — into the verdict the reduce branches on. Advisory outcomes are
/// recorded as results but never affect the verdict.
fn fold_round(round: &EvalRound, seq: u64) -> RoundVerdict {
    debug_assert!(
        !(round.done.is_empty() && round.slots.is_empty()),
        "job #{seq}: reduce with no stage having run",
    );
    let mut verdict = RoundVerdict {
        results: Vec::new(),
        required_infra_failure: false,
        overall_pass: true,
        aborted: Vec::new(),
    };
    for slot in round.done.iter().chain(round.slots.iter()) {
        let name = slot.evaluator.name.clone();
        let required = slot.evaluator.required.unwrap_or(true);
        let outcome = slot
            .outcome
            .as_ref()
            .unwrap_or_else(|| panic!("job #{seq}: reduce over unresolved slot '{name}'"));
        match outcome {
            SlotOutcome::Product {
                pass,
                abort,
                structured,
                output,
            } => {
                verdict.results.push(EvalResult {
                    evaluator: name.clone(),
                    pass: *pass,
                    structured: structured.clone(),
                    output: output.clone(),
                });
                if required && !*pass {
                    verdict.overall_pass = false;
                }
                if required && *abort {
                    verdict.aborted.push(name);
                }
            }
            SlotOutcome::Infra => {
                verdict.results.push(EvalResult {
                    evaluator: name,
                    pass: false,
                    structured: None,
                    output: None,
                });
                if required {
                    verdict.required_infra_failure = true;
                }
            }
        }
    }
    verdict
}

/// The aborting evaluators' findings, rendered for the escalation detail — the
/// operator reads WHY the work was declared unsalvageable, not just that it was.
fn abort_findings(results: &[EvalResult], aborted: &[String]) -> String {
    results
        .iter()
        .filter(|r| aborted.contains(&r.evaluator))
        .map(|r| {
            let detail = r
                .structured
                .as_ref()
                .and_then(|v| serde_json::to_string_pretty(v).ok())
                .unwrap_or_else(|| "(no structured findings)".into());
            format!("**{}**:\n{detail}", r.evaluator)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Fan one stage out against the job branch (§3.3). The launched slots come
/// back as [`EvalEvent::StageLaunched`].
fn launch_stage(
    owner: &str,
    project: &str,
    view: &EvalView<'_>,
    evaluators: Vec<Evaluator>,
) -> Effect {
    Effect::LaunchEvalStage {
        owner: owner.to_string(),
        project: project.to_string(),
        seq: view.job.id,
        branch: view.job.branch.clone(),
        cycle: view.cycle,
        evaluators,
    }
}

/// Re-create one slot's task at `attempt` (§3.3). The new task id comes back as
/// [`EvalEvent::SlotRelaunched`].
fn relaunch(
    owner: &str,
    project: &str,
    view: &EvalView<'_>,
    evaluator: &Evaluator,
    attempt: u32,
) -> Effect {
    Effect::LaunchEvaluator {
        owner: owner.to_string(),
        project: project.to_string(),
        seq: view.job.id,
        branch: view.job.branch.clone(),
        cycle: view.cycle,
        evaluator: Box::new(evaluator.clone()),
        attempt,
    }
}

/// Retire an evaluator task as Failed at `now`, marking `infra_loss` for the
/// evidence-free losses that spend no budget (§3.6/#83).
fn retire(mut task: Task, now: DateTime<Utc>, infra_loss: bool) -> Effect {
    task.state = TaskState::Failed;
    task.infra_loss = infra_loss;
    task.completed_at = Some(now);
    Effect::PutTask {
        task: Box::new(task),
    }
}

/// The `task-failed` announcement, whose `reason` is the machine code an
/// operator (and the job page) reads to tell an infra loss from a verdict.
fn task_failed(owner: &str, project: &str, seq: u64, task_id: u64, reason: &str) -> Effect {
    Effect::PublishEvent {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        event_type: "task-failed".to_string(),
        extra: serde_json::json!({
            "task_id": task_id, "phase": "Evaluation", "reason": reason,
        }),
    }
}

/// A post-work escalation out of the round (§1.2): the Human task, the
/// Escalated record, and the announcement, as one composite effect.
fn escalate(owner: &str, project: &str, seq: u64, reason: &str, detail: String) -> Effect {
    Effect::Escalate {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        reason: reason.to_string(),
        detail,
        failing_task: None,
    }
}

/// `Job::project` is always "owner/name" (§1.1); every repo-scoped effect needs
/// the halves.
fn split_project(job: &Job) -> (&str, &str) {
    job.project
        .split_once('/')
        .unwrap_or_else(|| panic!("malformed job project '{}'", job.project))
}

#[cfg(test)]
mod tests {
    //! Tier-1 coverage of every evaluation branch: pure values in, pure values
    //! out, no NATS/Docker (testing.md tier 1). This is C5's payoff — the
    //! reduce's budgets, the evidence-free relaunch bound and the staged
    //! short-circuit used to be reachable only through a container harness.
    //! The dispatcher's golden traces pin the same decisions end-to-end
    //! (`eval_failure_rework`, `eval_failure_no_budget_escalates`,
    //! `staged_eval_short_circuit`, `work_eval_merge_no_gate`).
    use super::*;
    use types::{EvaluatorType, TaskKind};

    fn sample_job(state: JobState) -> Job {
        let mut job: Job = serde_json::from_str(
            r#"{ "id": 7, "project": "acme/api", "type": "build",
                 "deps": [], "state": "Work", "branch": "job/7",
                 "base_ref": "abc123", "knowledge_tags": [], "factory": null,
                 "created_at": "2026-07-25T10:00:00Z", "ready_at": null }"#,
        )
        .expect("sample job");
        job.state = state;
        job
    }

    fn evaluator(name: &str, stage: u32, required: Option<bool>) -> Evaluator {
        Evaluator {
            name: name.into(),
            r#type: EvaluatorType::Command,
            image: None,
            run: Some("true".into()),
            prompt: None,
            provider: None,
            model: None,
            secrets: vec![],
            required,
            stage,
        }
    }

    fn eval_task(id: u64, name: &str, kind: TaskKind) -> Task {
        Task {
            id,
            job_seq: 7,
            project: "acme/api".into(),
            phase: TaskPhase::Evaluation,
            cycle: 1,
            kind,
            state: TaskState::Running,
            attempt: 1,
            evaluator: Some(name.into()),
            label: Some(name.into()),
            stage: 0,
            performed_by: None,
            container_id: Some("c1".into()),
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

    fn command_task(id: u64, name: &str) -> Task {
        eval_task(
            id,
            name,
            TaskKind::Command {
                run: "./ci.sh".into(),
            },
        )
    }

    fn agent_task(id: u64, name: &str, result: Option<TaskResult>) -> Task {
        let mut task = eval_task(
            id,
            name,
            TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/eval.md".into(),
            },
        );
        task.result = result;
        task
    }

    fn open_slot(name: &str, task_id: u64) -> EvalSlot {
        EvalSlot {
            evaluator: evaluator(name, 0, None),
            task_id,
            attempt: 1,
            outcome: None,
        }
    }

    fn resolved_slot(name: &str, required: Option<bool>, outcome: SlotOutcome) -> EvalSlot {
        EvalSlot {
            evaluator: evaluator(name, 0, required),
            task_id: 1,
            attempt: 1,
            outcome: Some(outcome),
        }
    }

    fn product(pass: bool, abort: bool) -> SlotOutcome {
        SlotOutcome::Product {
            pass,
            abort,
            structured: None,
            output: None,
        }
    }

    fn open_round(slots: Vec<EvalSlot>, pending: Vec<Vec<Evaluator>>) -> EvalRound {
        EvalRound {
            slots,
            pending: pending.into(),
            done: Vec::new(),
        }
    }

    fn view<'a>(job: &'a Job, evaluators: &'a [Evaluator]) -> EvalView<'a> {
        EvalView {
            job,
            evaluators,
            cycle: 1,
            reworks_used: 0,
            rework_budget: 0,
            work_type: WorkType::Agent,
            eval_retries: 1,
            infra_losses_prior: 0,
            infra_relaunch_cap: 3,
            draining: false,
            now: Utc::now(),
        }
    }

    fn exit(exit_code: i32) -> EvalExit {
        EvalExit {
            exit_code,
            ..Default::default()
        }
    }

    /// Assert an effect list's shape by variant, so a test names the effects it
    /// expects without matching every field.
    fn effect_names(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .map(|e| match e {
                Effect::PublishEvent { event_type, .. } => format!("PublishEvent {event_type}"),
                Effect::PutTask { task } => format!("PutTask {:?}", task.state),
                Effect::LaunchEvalStage { evaluators, .. } => format!(
                    "LaunchEvalStage [{}]",
                    evaluators
                        .iter()
                        .map(|e| e.name.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Effect::LaunchEvaluator {
                    evaluator, attempt, ..
                } => format!("LaunchEvaluator {} #{attempt}", evaluator.name),
                Effect::Escalate { reason, .. } => format!("Escalate {reason}"),
                other => other.port().to_string(),
            })
            .collect()
    }

    // ── entry: the staged fan-out (§3.2 steps 9–10, §3.3) ───────────────────

    /// Entry moves the record, announces the round, and creates STAGE 0 ONLY —
    /// the later stages stay pending, which is what "not created" means.
    #[test]
    fn entry_launches_only_the_first_stage() {
        let job = sample_job(JobState::Work);
        let evaluators = vec![
            evaluator("review", 0, None),
            evaluator("ci", 1, None),
            evaluator("perf", 1, Some(false)),
        ];
        let (round, transitions, effects, step) =
            decide(None, &view(&job, &evaluators), EvalEvent::Entered);

        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::Evaluation);
        assert_eq!(
            transitions[0].job.base_ref.as_deref(),
            Some("abc123"),
            "the entry transition persists the pre-eval rebase's pin",
        );
        assert_eq!(
            effect_names(&effects),
            vec![
                "PublishEvent job-evaluation-started".to_string(),
                "LaunchEvalStage [review]".to_string(),
            ]
        );
        let round = round.expect("a round is open");
        assert!(round.slots.is_empty(), "slots arrive with StageLaunched");
        assert_eq!(round.pending.len(), 1, "stage 1 is not created yet");
        assert_eq!(round.pending[0].len(), 2);
        assert_eq!(step, EvalStep::AwaitOutcome);
    }

    /// No evaluators is an auto-pass (§3.3): the record still moves and the
    /// round is announced, but the job goes straight to landing.
    #[test]
    fn entry_without_evaluators_auto_passes() {
        let job = sample_job(JobState::Work);
        let (round, transitions, effects, step) =
            decide(None, &view(&job, &[]), EvalEvent::Entered);

        assert_eq!(transitions[0].to, JobState::Evaluation);
        assert_eq!(
            effect_names(&effects),
            vec!["PublishEvent job-evaluation-started".to_string()]
        );
        assert!(round.is_none());
        assert_eq!(step, EvalStep::Finalize);
    }

    /// Draining (§3.6): no transition, no container, no announcement — the job
    /// holds in Work and reconciliation re-enters after the restart.
    #[test]
    fn entry_while_draining_holds_the_job_in_work() {
        let job = sample_job(JobState::Work);
        let evaluators = vec![evaluator("ci", 0, None)];
        let mut v = view(&job, &evaluators);
        v.draining = true;
        let (round, transitions, effects, step) = decide(None, &v, EvalEvent::Entered);

        assert!(transitions.is_empty() && effects.is_empty());
        assert!(round.is_none());
        assert_eq!(step, EvalStep::Hold);
    }

    /// The launched stage's task ids only exist after the launch, so they land
    /// on the round through the continuation event.
    #[test]
    fn stage_launched_installs_the_live_slots() {
        let job = sample_job(JobState::Evaluation);
        let evaluators = vec![evaluator("ci", 0, None)];
        let (round, _, effects, step) = decide(
            Some(open_round(vec![], vec![])),
            &view(&job, &evaluators),
            EvalEvent::StageLaunched {
                slots: vec![open_slot("ci", 42)],
            },
        );

        assert!(effects.is_empty());
        let round = round.expect("round");
        assert_eq!(round.slots.len(), 1);
        assert_eq!(round.slots[0].task_id, 42);
        assert_eq!(step, EvalStep::Await);
    }

    /// A relaunch replaced the slot's task id, so the slot is re-found by
    /// evaluator name.
    #[test]
    fn slot_relaunched_repoints_the_slot_by_name() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, _, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotRelaunched {
                evaluator: "ci".into(),
                task_id: 9,
                attempt: 2,
            },
        );

        let round = round.expect("round");
        assert_eq!((round.slots[0].task_id, round.slots[0].attempt), (9, 2));
        assert_eq!(step, EvalStep::Await);
    }

    // ── superseded rounds: nothing to decide ────────────────────────────────

    /// A slice released under a late exit (the revoke case that once panicked
    /// the actor) and a duplicate exit both decide nothing.
    #[test]
    fn events_without_an_open_slot_are_ignored() {
        let job = sample_job(JobState::Evaluation);
        let v = view(&job, &[]);

        let (round, t, e, step) = decide(
            None,
            &v,
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: exit(0),
            },
        );
        assert!(round.is_none() && t.is_empty() && e.is_empty());
        assert_eq!(step, EvalStep::Ignored);

        // A round whose slot already resolved: the exit is a duplicate.
        let resolved = open_round(
            vec![resolved_slot("ci", None, product(true, false))],
            vec![],
        );
        let (_, _, e, step) = decide(
            Some(resolved),
            &v,
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: exit(0),
            },
        );
        assert!(e.is_empty());
        assert_eq!(step, EvalStep::Ignored);

        // A relaunch for an evaluator this round does not know.
        let (_, _, _, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &v,
            EvalEvent::SlotRelaunched {
                evaluator: "other".into(),
                task_id: 2,
                attempt: 1,
            },
        );
        assert_eq!(step, EvalStep::Ignored);

        // An inbox resolution the round has no open slot for.
        let (_, _, _, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &v,
            EvalEvent::SlotResolved {
                task_id: 99,
                pass: true,
                abort: false,
                structured: None,
            },
        );
        assert_eq!(step, EvalStep::Ignored);

        // Reconciliation replaying a settle for a job with no round.
        let (_, _, _, step) = decide(None, &v, EvalEvent::StageSettled);
        assert_eq!(step, EvalStep::Ignored);
    }

    // ── command verdicts (#167/#198) ────────────────────────────────────────

    /// Exit 0 records the verdict, announces it, and — a single-stage round —
    /// reduces to a pass.
    #[test]
    fn command_exit_zero_passes_the_round() {
        let job = sample_job(JobState::Evaluation);
        let (round, transitions, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: EvalExit {
                    exit_code: 0,
                    log_tail: Some("all green\n".into()),
                    ..Default::default()
                },
            },
        );

        assert!(transitions.is_empty(), "landing owns the next transition");
        assert_eq!(
            effect_names(&effects),
            vec![
                "PutTask Done".to_string(),
                "PublishEvent task-completed".to_string(),
            ]
        );
        match &effects[0] {
            Effect::PutTask { task } => match task.result.as_ref().expect("result") {
                TaskResult::Command {
                    pass,
                    exit_code,
                    output,
                    ..
                } => {
                    assert!(*pass);
                    assert_eq!(*exit_code, 0);
                    assert_eq!(output, "all green\n", "the captured tail is the record");
                }
                other => panic!("expected a Command result, got {other:?}"),
            },
            other => panic!("expected PutTask, got {other:?}"),
        }
        assert!(round.is_some(), "the spent round rides along to landing");
        assert_eq!(step, EvalStep::Finalize);
    }

    /// A required product failure with budget left reworks: the findings — the
    /// captured output included (#167) — become the next cycle's brief, and one
    /// `reworks_used` is spent.
    #[test]
    fn command_failure_reworks_under_budget_with_the_output_as_evidence() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.rework_budget = 2;
        let (round, transitions, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &v,
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: EvalExit {
                    exit_code: 1,
                    log_tail: Some("test failed: 3 assertions".into()),
                    ..Default::default()
                },
            },
        );

        assert!(transitions.is_empty(), "re-entering Work owns that");
        assert_eq!(
            effect_names(&effects),
            vec![
                "PutTask Done".to_string(),
                "PublishEvent task-completed".to_string(),
                "PublishEvent job-rework-started".to_string(),
            ]
        );
        match &effects[2] {
            Effect::PublishEvent { extra, .. } => {
                assert_eq!(extra["cycle"], 2);
                assert_eq!(extra["reason"], "eval_failure");
                assert_eq!(extra["eval_context"][0]["evaluator"], "ci");
                assert_eq!(extra["eval_context"][0]["pass"], false);
            }
            other => panic!("expected the rework announcement, got {other:?}"),
        }
        assert!(round.is_none(), "the round is spent");
        match step {
            EvalStep::Rework {
                cycle,
                reworks_used,
                eval_context,
            } => {
                assert_eq!((cycle, reworks_used), (2, 1));
                assert_eq!(eval_context.len(), 1);
                assert_eq!(
                    eval_context[0].output.as_deref(),
                    Some("test failed: 3 assertions"),
                );
            }
            other => panic!("expected Rework, got {other:?}"),
        }
    }

    /// Budget spent: the same failure escalates instead, and the shim releases
    /// the execution slice.
    #[test]
    fn command_failure_escalates_when_the_budget_is_spent() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.rework_budget = 1;
        v.reworks_used = 1;
        v.cycle = 2;
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &v,
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: exit(1),
            },
        );

        assert_eq!(
            effect_names(&effects).last().map(String::as_str),
            Some("Escalate rework_budget_exhausted"),
        );
        match effects.last().expect("escalation") {
            Effect::Escalate { detail, .. } => {
                assert!(detail.contains("cycle 2"), "{detail}");
                assert!(detail.contains("no rework budget left"), "{detail}");
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
        assert!(round.is_none());
        assert_eq!(step, EvalStep::EscalatedDropExec);
        assert!(step.drops_exec());
    }

    /// A `command` work job has no author to rework against (§3.3), so it
    /// escalates even with budget on the type.
    #[test]
    fn command_work_never_reworks() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.rework_budget = 5;
        v.work_type = WorkType::Command;
        let (_, _, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &v,
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: exit(1),
            },
        );

        assert_eq!(
            effect_names(&effects).last().map(String::as_str),
            Some("Escalate rework_budget_exhausted"),
        );
        assert_eq!(step, EvalStep::EscalatedDropExec);
    }

    /// #198: a NORMAL non-zero exit with an empty stream is a real verdict — a
    /// legitimately silent failure — not the evidence-free class. Reworking on
    /// it is the whole point of the #167 narrowing.
    #[test]
    fn silent_normal_failure_is_a_verdict_not_an_infra_loss() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.rework_budget = 1;
        let (_, _, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &v,
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: exit(1),
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec![
                "PutTask Done".to_string(),
                "PublishEvent task-completed".to_string(),
                "PublishEvent job-rework-started".to_string(),
            ],
            "no infra_loss stamp, no relaunch",
        );
        assert!(matches!(step, EvalStep::Rework { .. }));
    }

    /// #167: an ABNORMAL exit (signal kill) with no output never judged the
    /// code. Relaunch the SAME attempt, spending no `eval_retries`.
    #[test]
    fn verdict_less_exit_relaunches_without_spending_eval_retries() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: exit(137),
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec![
                "PutTask Failed".to_string(),
                "PublishEvent task-failed".to_string(),
                "LaunchEvaluator ci #1".to_string(),
            ]
        );
        match &effects[0] {
            Effect::PutTask { task } => assert!(task.infra_loss, "stamped as infrastructure loss"),
            other => panic!("expected PutTask, got {other:?}"),
        }
        match &effects[1] {
            Effect::PublishEvent { extra, .. } => {
                assert_eq!(extra["reason"], EVAL_NO_OUTPUT_REASON)
            }
            other => panic!("expected the failure announcement, got {other:?}"),
        }
        assert!(round.is_some(), "the slot stays open for the relaunch");
        assert_eq!(step, EvalStep::AwaitOutcome);
    }

    /// The evidence-free relaunch is bounded (§3.6 cap): past it the job
    /// escalates naming the evaluator, so a human reviews the evaluator rather
    /// than the code.
    #[test]
    fn verdict_less_exits_past_the_cap_escalate() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.infra_relaunch_cap = 3;
        v.infra_losses_prior = 3; // this exit is the 4th
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &v,
            EvalEvent::SlotExited {
                task: Box::new(command_task(1, "ci")),
                exit: exit(-1),
            },
        );

        match effects.last().expect("escalation") {
            Effect::Escalate {
                reason,
                detail,
                failing_task,
                ..
            } => {
                assert_eq!(reason, EVAL_NO_OUTPUT_REASON);
                assert_eq!(*failing_task, Some(1));
                assert!(detail.contains("'ci'"), "{detail}");
                assert!(detail.contains("4 times"), "{detail}");
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
        assert!(round.is_none());
        assert_eq!(step, EvalStep::EscalatedDropExec);
    }

    // ── infra failures: the `eval_retries` budget (§3.3) ────────────────────

    /// A container that never launched is an infra failure whatever the
    /// evaluator's type: record why, then retry the slot at the next attempt.
    #[test]
    fn launch_failure_retries_under_eval_retries() {
        let job = sample_job(JobState::Evaluation);
        let mut task = command_task(1, "ci");
        task.attempt = 1;
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(task),
                exit: EvalExit {
                    exit_code: -1,
                    launch_error: Some("container launch failed: bad image".into()),
                    ..Default::default()
                },
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec![
                "PutTask Failed".to_string(),
                "PublishEvent task-failed".to_string(),
                "LaunchEvaluator ci #2".to_string(),
            ]
        );
        match &effects[0] {
            Effect::PutTask { task } => {
                assert!(!task.infra_loss, "a launch failure spends eval_retries");
                match task.result.as_ref().expect("result") {
                    TaskResult::Command { output, .. } => {
                        assert_eq!(output, "container launch failed: bad image")
                    }
                    other => panic!("expected a Command result, got {other:?}"),
                }
            }
            other => panic!("expected PutTask, got {other:?}"),
        }
        match &effects[1] {
            Effect::PublishEvent { extra, .. } => {
                assert_eq!(extra["reason"], "container launch failed")
            }
            other => panic!("expected the failure announcement, got {other:?}"),
        }
        assert!(round.is_some());
        assert_eq!(step, EvalStep::AwaitOutcome);
    }

    /// `eval_retries` spent: the slot resolves Infra and the reduce escalates,
    /// because a REQUIRED evaluator never produced a verdict.
    #[test]
    fn exhausted_eval_retries_escalate_as_an_infra_failure() {
        let job = sample_job(JobState::Evaluation);
        let mut task = command_task(1, "ci");
        task.attempt = 2; // > eval_retries (1)
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(task),
                exit: EvalExit {
                    exit_code: -1,
                    launch_error: Some("no such image".into()),
                    ..Default::default()
                },
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec![
                "PutTask Failed".to_string(),
                "PublishEvent task-failed".to_string(),
                "Escalate eval_infra_failure".to_string(),
            ]
        );
        assert!(round.is_none());
        assert_eq!(step, EvalStep::EscalatedDropExec);
    }

    /// An ADVISORY evaluator's infra failure never fails the round: it is
    /// recorded as a failed result and the round still passes.
    #[test]
    fn advisory_infra_failure_does_not_fail_the_round() {
        let job = sample_job(JobState::Evaluation);
        let mut task = command_task(1, "lint");
        task.attempt = 2;
        let advisory = EvalSlot {
            evaluator: evaluator("lint", 0, Some(false)),
            task_id: 1,
            attempt: 2,
            outcome: None,
        };
        let (_, _, effects, step) = decide(
            Some(open_round(
                vec![resolved_slot("ci", None, product(true, false)), advisory],
                vec![],
            )),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(task),
                exit: EvalExit {
                    exit_code: -1,
                    launch_error: Some("gone".into()),
                    ..Default::default()
                },
            },
        );

        assert!(
            !effect_names(&effects)
                .iter()
                .any(|e| e.starts_with("Escalate")),
            "{effects:?}",
        );
        assert_eq!(step, EvalStep::Finalize);
    }

    // ── agent verdicts (§3.3) ───────────────────────────────────────────────

    /// `submit_eval` already recorded and announced the verdict, so the exit
    /// only reads it — no second task write, no second announcement.
    #[test]
    fn agent_exit_reads_the_submitted_verdict() {
        let job = sample_job(JobState::Evaluation);
        let task = agent_task(
            1,
            "review",
            Some(TaskResult::Agent {
                pass: true,
                abort: false,
                structured: Some(serde_json::json!({ "findings": [] })),
                token_usage: None,
                cover_html: None,
            }),
        );
        let (_, _, effects, step) = decide(
            Some(open_round(vec![open_slot("review", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(task),
                exit: exit(0),
            },
        );

        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(step, EvalStep::Finalize);
    }

    /// The CLI's measured usage wins over the agent's self-report, which costs
    /// one task write.
    #[test]
    fn agent_exit_prefers_measured_usage() {
        let job = sample_job(JobState::Evaluation);
        let task = agent_task(
            1,
            "review",
            Some(TaskResult::Agent {
                pass: true,
                abort: false,
                structured: None,
                token_usage: None,
                cover_html: None,
            }),
        );
        let measured = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: None,
            cache_write_tokens: None,
        };
        let (_, _, effects, step) = decide(
            Some(open_round(vec![open_slot("review", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(task),
                exit: EvalExit {
                    exit_code: 0,
                    usage: Some(measured),
                    ..Default::default()
                },
            },
        );

        match &effects[0] {
            Effect::PutTask { task } => match task.result.as_ref().expect("result") {
                TaskResult::Agent { token_usage, .. } => {
                    assert_eq!(
                        token_usage.as_ref().map(|u| u.output_tokens),
                        Some(20),
                        "the measured figure replaced the self-report",
                    );
                }
                other => panic!("expected an Agent result, got {other:?}"),
            },
            other => panic!("expected PutTask, got {other:?}"),
        }
        assert_eq!(step, EvalStep::Finalize);
    }

    /// #167: an agent that ended without `submit_eval` produced no evidence —
    /// the evidence-free path, not a product fail.
    #[test]
    fn agent_exit_without_a_verdict_takes_the_no_output_path() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("review", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(agent_task(1, "review", None)),
                exit: exit(0),
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec![
                "PutTask Failed".to_string(),
                "PublishEvent task-failed".to_string(),
                "LaunchEvaluator review #1".to_string(),
            ]
        );
        assert!(round.is_some());
        assert_eq!(step, EvalStep::AwaitOutcome);
    }

    /// A Human evaluator launches no container, so an exit is never its
    /// verdict — the inbox resolution is.
    #[test]
    fn human_evaluator_exit_decides_nothing() {
        let job = sample_job(JobState::Evaluation);
        let task = eval_task(
            1,
            "signoff",
            TaskKind::Human {
                prompt: "sign off".into(),
            },
        );
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("signoff", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(task),
                exit: exit(0),
            },
        );

        assert!(effects.is_empty());
        assert!(round.expect("round").slots[0].outcome.is_none());
        assert_eq!(step, EvalStep::Await);
    }

    // ── inbox resolutions and the abort verdict ─────────────────────────────

    /// A Human verdict resolves its slot and settles the round.
    #[test]
    fn inbox_resolution_records_the_verdict() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("signoff", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: true,
                abort: false,
                structured: None,
            },
        );

        assert!(effects.is_empty(), "the resolve handler wrote the task");
        assert!(round.is_some());
        assert_eq!(step, EvalStep::Finalize);
    }

    /// A required abort is "not satisfiable by rework": escalate with the
    /// findings, leaving the rework budget untouched.
    #[test]
    fn required_abort_escalates_without_consuming_the_budget() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.rework_budget = 3;
        let (round, _, effects, step) = decide(
            Some(open_round(vec![open_slot("review", 1)], vec![])),
            &v,
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: false,
                abort: true,
                structured: Some(serde_json::json!({ "why": "wrong approach" })),
            },
        );

        match effects.last().expect("escalation") {
            Effect::Escalate { reason, detail, .. } => {
                assert_eq!(reason, "eval_abort");
                assert!(detail.contains("review"), "{detail}");
                assert!(detail.contains("not satisfiable by rework"), "{detail}");
                assert!(detail.contains("wrong approach"), "{detail}");
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
        assert!(round.is_none());
        assert_eq!(step, EvalStep::EscalatedDropExec);
    }

    /// An ADVISORY abort is a plain advisory fail: it neither escalates nor
    /// fails the round.
    #[test]
    fn advisory_abort_is_just_an_advisory_fail() {
        let job = sample_job(JobState::Evaluation);
        let advisory = EvalSlot {
            evaluator: evaluator("vibes", 0, Some(false)),
            task_id: 1,
            attempt: 1,
            outcome: None,
        };
        let (_, _, effects, step) = decide(
            Some(open_round(
                vec![resolved_slot("ci", None, product(true, false)), advisory],
                vec![],
            )),
            &view(&job, &[]),
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: false,
                abort: true,
                structured: None,
            },
        );

        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(step, EvalStep::Finalize);
    }

    // ── staged evaluation: advance, short-circuit, wait ─────────────────────

    /// A stage that passes with a later stage queued advances: the finished
    /// slots retire into `done` and the next stage is created.
    #[test]
    fn a_passed_stage_advances_to_the_next() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, effects, step) = decide(
            Some(open_round(
                vec![open_slot("review", 1)],
                vec![vec![evaluator("ci", 1, None)]],
            )),
            &view(&job, &[]),
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: true,
                abort: false,
                structured: None,
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec!["LaunchEvalStage [ci]".to_string()]
        );
        let round = round.expect("round");
        assert_eq!(round.done.len(), 1, "the passed stage retired into done");
        assert!(round.slots.is_empty(), "the new slots arrive next");
        assert!(round.pending.is_empty());
        assert_eq!(step, EvalStep::AwaitOutcome);
    }

    /// A required stage-0 failure short-circuits: the pending stage is NEVER
    /// created, and the reduce runs over the stages that did run.
    #[test]
    fn a_failed_stage_short_circuits_the_rest() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, effects, step) = decide(
            Some(open_round(
                vec![open_slot("review", 1)],
                vec![vec![evaluator("ci", 1, None)]],
            )),
            &view(&job, &[]),
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: false,
                abort: false,
                structured: None,
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec!["Escalate rework_budget_exhausted".to_string()],
            "no stage-1 launch",
        );
        assert!(round.is_none());
        assert_eq!(step, EvalStep::EscalatedDropExec);
    }

    /// An advisory stage-0 failure never blocks progression (§3.3).
    #[test]
    fn an_advisory_stage_failure_still_advances() {
        let job = sample_job(JobState::Evaluation);
        let advisory = EvalSlot {
            evaluator: evaluator("review", 0, Some(false)),
            task_id: 1,
            attempt: 1,
            outcome: None,
        };
        let (_, _, effects, step) = decide(
            Some(open_round(
                vec![advisory],
                vec![vec![evaluator("ci", 1, None)]],
            )),
            &view(&job, &[]),
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: false,
                abort: false,
                structured: None,
            },
        );

        assert_eq!(
            effect_names(&effects),
            vec!["LaunchEvalStage [ci]".to_string()]
        );
        assert_eq!(step, EvalStep::AwaitOutcome);
    }

    /// A stage with a slot still in flight decides nothing yet.
    #[test]
    fn a_stage_with_an_open_slot_waits() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, effects, step) = decide(
            Some(open_round(
                vec![open_slot("a", 1), open_slot("b", 2)],
                vec![],
            )),
            &view(&job, &[]),
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: true,
                abort: false,
                structured: None,
            },
        );

        assert!(effects.is_empty());
        let round = round.expect("round");
        assert!(round.slots[0].outcome.is_some() && round.slots[1].outcome.is_none());
        assert_eq!(step, EvalStep::Await);
    }

    /// Draining (§3.6): the verdict is recorded but the round neither advances
    /// nor reduces — reconciliation replays the decision after the restart.
    #[test]
    fn a_resolved_stage_holds_while_draining() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.draining = true;
        let (round, _, effects, step) = decide(
            Some(open_round(
                vec![open_slot("ci", 1)],
                vec![vec![evaluator("perf", 1, None)]],
            )),
            &v,
            EvalEvent::SlotResolved {
                task_id: 1,
                pass: true,
                abort: false,
                structured: None,
            },
        );

        assert!(effects.is_empty(), "no stage launches during a drain");
        assert!(round.expect("round").slots[0].outcome.is_some());
        assert_eq!(step, EvalStep::Hold);
    }

    /// Reconciliation's replay (§3.6): a round rebuilt from the task log with
    /// every slot resolved reduces exactly as the lost decision would have.
    #[test]
    fn reconciliation_replays_the_reduce() {
        let job = sample_job(JobState::Evaluation);
        let (round, _, effects, step) = decide(
            Some(open_round(
                vec![resolved_slot("ci", None, product(true, false))],
                vec![],
            )),
            &view(&job, &[]),
            EvalEvent::StageSettled,
        );

        assert!(effects.is_empty());
        assert!(round.is_some());
        assert_eq!(step, EvalStep::Finalize);
    }

    /// The reduce folds every stage that RAN — earlier passed stages included —
    /// so the rework brief carries all of their findings, not just the last
    /// stage's.
    #[test]
    fn the_reduce_folds_earlier_stages_into_the_context() {
        let job = sample_job(JobState::Evaluation);
        let mut v = view(&job, &[]);
        v.rework_budget = 1;
        let mut r = open_round(
            vec![resolved_slot("ci", None, product(false, false))],
            vec![],
        );
        r.done = vec![resolved_slot("review", None, product(true, false))];
        let (_, _, _, step) = decide(Some(r), &v, EvalEvent::StageSettled);

        match step {
            EvalStep::Rework { eval_context, .. } => {
                let names: Vec<&str> = eval_context.iter().map(|r| r.evaluator.as_str()).collect();
                assert_eq!(
                    names,
                    vec!["review", "ci"],
                    "done first, then the last stage"
                );
            }
            other => panic!("expected Rework, got {other:?}"),
        }
    }

    // ── negative space (STYLE.md Tier 2 #2) ────────────────────────────────

    #[test]
    #[should_panic(expected = "terminal job")]
    #[cfg(debug_assertions)]
    fn entering_evaluation_for_a_terminal_job_is_a_caller_bug() {
        let job = sample_job(JobState::Revoked);
        decide(None, &view(&job, &[]), EvalEvent::Entered);
    }

    #[test]
    #[should_panic(expected = "over a live round")]
    #[cfg(debug_assertions)]
    fn entering_evaluation_twice_is_a_caller_bug() {
        let job = sample_job(JobState::Work);
        decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::Entered,
        );
    }

    #[test]
    #[should_panic(expected = "reduce over unresolved slot")]
    #[cfg(debug_assertions)]
    fn reducing_over_an_unresolved_slot_is_a_caller_bug() {
        let job = sample_job(JobState::Evaluation);
        let mut r = open_round(
            vec![resolved_slot("ci", None, product(true, false))],
            vec![],
        );
        // A `done` stage can only hold resolved slots; an open one there means
        // the round was rebuilt wrong.
        r.done = vec![open_slot("review", 2)];
        decide(Some(r), &view(&job, &[]), EvalEvent::StageSettled);
    }

    #[test]
    #[should_panic(expected = "reduce with no stage having run")]
    #[cfg(debug_assertions)]
    fn reducing_an_empty_round_is_a_caller_bug() {
        let job = sample_job(JobState::Evaluation);
        decide(
            Some(open_round(vec![], vec![])),
            &view(&job, &[]),
            EvalEvent::StageSettled,
        );
    }

    #[test]
    #[should_panic(expected = "evaluator exit decided for a Work task")]
    #[cfg(debug_assertions)]
    fn an_exit_from_another_phase_is_a_caller_bug() {
        let job = sample_job(JobState::Evaluation);
        let mut task = command_task(1, "ci");
        task.phase = TaskPhase::Work;
        decide(
            Some(open_round(vec![open_slot("ci", 1)], vec![])),
            &view(&job, &[]),
            EvalEvent::SlotExited {
                task: Box::new(task),
                exit: exit(0),
            },
        );
    }

    // ── the pure fragments the round is built from ─────────────────────────

    #[test]
    fn group_stages_single_stage_is_one_group() {
        // The compatibility story: every evaluator at the default stage 0 →
        // exactly one stage, in declared order.
        let evs = vec![
            evaluator("a", 0, None),
            evaluator("b", 0, None),
            evaluator("c", 0, None),
        ];
        let stages = group_stages(evs);
        assert_eq!(stages.len(), 1);
        let names: Vec<_> = stages[0].iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn group_stages_orders_by_stage_stable_within() {
        // Out-of-order, multi-stage input sorts ascending; declared order is
        // preserved within a stage (stable).
        let evs = vec![
            evaluator("ci", 1, None),
            evaluator("review", 0, None),
            evaluator("lint", 1, None),
            evaluator("gate", 2, None),
        ];
        let stages: Vec<Vec<_>> = group_stages(evs)
            .into_iter()
            .map(|s| s.into_iter().map(|e| e.name).collect())
            .collect();
        assert_eq!(
            stages,
            vec![
                vec!["review".to_string()],
                vec!["ci".to_string(), "lint".to_string()],
                vec!["gate".to_string()],
            ]
        );
    }

    #[test]
    fn stage_passed_all_required_pass() {
        let slots = vec![
            resolved_slot("a", None, product(true, false)),
            resolved_slot("b", Some(true), product(true, false)),
        ];
        assert!(stage_passed(&slots));
    }

    #[test]
    fn stage_passed_required_fail_blocks() {
        let slots = vec![
            resolved_slot("a", None, product(true, false)),
            resolved_slot("b", None, product(false, false)),
        ];
        assert!(!stage_passed(&slots));
    }

    #[test]
    fn stage_passed_required_abort_blocks() {
        assert!(!stage_passed(&[resolved_slot(
            "a",
            None,
            product(false, true)
        )]));
    }

    #[test]
    fn stage_passed_required_infra_blocks() {
        assert!(!stage_passed(&[resolved_slot(
            "a",
            None,
            SlotOutcome::Infra
        )]));
    }

    #[test]
    fn stage_passed_advisory_failures_never_block() {
        // Advisory fail, advisory abort, advisory infra — none stop the next
        // stage from starting.
        let slots = vec![
            resolved_slot("pass", None, product(true, false)),
            resolved_slot("adv-fail", Some(false), product(false, false)),
            resolved_slot("adv-abort", Some(false), product(false, true)),
            resolved_slot("adv-infra", Some(false), SlotOutcome::Infra),
        ];
        assert!(stage_passed(&slots));
    }

    /// An open slot is found by task id only while it is still open — the
    /// duplicate-exit guard the shim also uses for its resolution precondition.
    #[test]
    fn open_slot_finds_only_unresolved_slots() {
        let r = open_round(
            vec![
                resolved_slot("done", None, product(true, false)),
                open_slot("live", 5),
            ],
            vec![],
        );
        assert_eq!(r.open_slot(5), Some(1));
        assert_eq!(r.open_slot(1), None, "a resolved slot is not open");
        assert_eq!(r.open_slot(99), None);
    }
}

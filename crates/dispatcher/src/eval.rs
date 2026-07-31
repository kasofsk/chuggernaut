//! The Evaluation-phase shim (spec §3.3) and post-eval finalization (§3.2
//! step 12): the launch/monitor half of evaluation, plus the merge-gate fold.
//!
//! Everything decided about a round — the staged fan-out, each evaluator's
//! verdict, the retry/rework budgets, abort and escalate — is a pure function
//! in `chuggernaut_domain::decide::eval` (refactor-plan C5); this module gathers
//! its view, applies its transitions, interprets its effects, and performs the
//! bookkeeping its `EvalStep` names. What stays here is the I/O the decision
//! cannot express: composing evaluator prompts (the §3.3 re-review context),
//! launching containers, and the pre-eval rebase whose `base_ref` pin the entry
//! transition persists.
//!
//! Finalization then flows through a per-project depth-1 merge queue. The fast
//! path (default HEAD unmoved, or no commits) merges immediately; a moved HEAD
//! parks the candidate squash commit on `merge-gate/{seq}` and re-runs the
//! required command evaluators against it before promoting — nothing reaches
//! the default branch untested against the exact tree that lands.
//!
//! - **Accepts:** a job entering Evaluation; evaluator exits and submissions;
//!   inbox resolutions of Human evaluators; merge-gate re-runs.
//! - **Emits:** evaluator container launches, the `EvalResult` reduction,
//!   squash-merge to the default branch or conflict re-entry, and merge-gate
//!   parking on `merge-gate/{seq}`.
//! - **Guarantees:** no evaluation decision of its own — every branch in the
//!   fold is a `match` on a value the decider returned; every merge flows
//!   through a per-project depth-1 queue; nothing lands on the default branch
//!   untested against the exact tree.
//! - **Spec:** §3.3; §3.2 step 12; contracts.md §2. Runs as `impl Core` — core
//!   stays the single writer.

use crate::capacity::DecidedLaunch;
use crate::core::{Core, CoreError, EvalSubmission, Msg, Result, TaskExit};
use crate::decide::eval as decide_eval;
use crate::decide::merge_gate;
use crate::decide::wrapup;
use crate::effects::Effect;
use crate::exec::{ChannelRole, INFRA_RELAUNCH_CAP, eval_image, task_timeout};
use crate::interpret::Outcome;
use agent::AgentRunConfig;
use chrono::Utc;
use types::{
    EvalResult, Evaluator, EvaluatorType, Job, JobState, ReworkReason, Task, TaskKind, TaskPhase,
    TaskResult, TaskState, WorkType, WrapUpMode,
};
use vcs::{ConflictRebaseOutcome, RebaseOutcome};

pub use crate::decide::eval::{EvalRound, EvalSlot, SlotOutcome, stage_passed};

/// Hard cap on continuation hops in one evaluation fold (STYLE.md Tier 2 #3).
/// Each hop consumes exactly one launch outcome — a stage fan-out or one slot's
/// relaunch — so a real round finishes in a handful; the cap turns a decider
/// that fails to settle into a loud error instead of a spinning actor.
const EVAL_FOLD_STEPS_MAX: usize = 64;

/// Scoped framing for a gate-fix task's prompt (job #154): the branch was
/// already approved by review; a rebase onto moved main broke compilation only.
/// The task is a narrow repair, not a fresh work cycle.
const GATE_FIX_FRAMING: &str = "\n\n---\n## Gate-Fix (compile only, job #154)\n\
    This branch was **already approved by review**. After rebasing onto the \
    updated main it no longer **compiles** — a mechanical collision (a moved or \
    renamed symbol, a changed signature), not a design problem. Make the \
    **minimal** change to restore compilation and nothing more: do **not** add \
    features, refactor, or restructure. Run the project's build/compile step to \
    reproduce the exact errors, fix them in place, then commit. This goes \
    straight back to the merge gate — gate CI is the final authority — so no \
    re-review runs; keep the change small and obviously-correct.\n";

/// A parked candidate awaiting its gate verdict (§3.3 Merge Gate).
pub struct GateState {
    pub commit: String,
    /// Default HEAD when the candidate was built; the promote CAS target.
    pub old_head: String,
    pub round: EvalRound,
}

/// The owned read-set behind a [`merge_gate::LandingView`] borrow — one
/// landing decision's inputs, assembled by `gather_landing_view`.
struct LandingViewData {
    job: Job,
    head: String,
    force_gate: bool,
    summary: Option<String>,
    cycle: u32,
    gate_fix_used: u32,
    gate_evaluators: Vec<Evaluator>,
    gate_commit: Option<String>,
    gate_old_head: Option<String>,
}

/// The owned read-set behind a [`decide_eval::EvalView`] borrow — one
/// evaluation decision's inputs, assembled by `gather_eval_view`.
struct EvalViewData {
    evaluators: Vec<Evaluator>,
    cycle: u32,
    reworks_used: u32,
    rework_budget: u32,
    work_type: WorkType,
    eval_retries: u32,
    infra_losses_prior: u32,
}

impl EvalViewData {
    /// Borrow the read-set as the decider's view, with the two inputs the
    /// gather does not own: the job record (the entry hop's is the rebase's
    /// in-memory pin) and the live drain flag.
    fn view<'a>(&'a self, job: &'a Job, draining: bool) -> decide_eval::EvalView<'a> {
        decide_eval::EvalView {
            job,
            evaluators: &self.evaluators,
            cycle: self.cycle,
            reworks_used: self.reworks_used,
            rework_budget: self.rework_budget,
            work_type: self.work_type,
            eval_retries: self.eval_retries,
            infra_losses_prior: self.infra_losses_prior,
            infra_relaunch_cap: INFRA_RELAUNCH_CAP,
            draining,
            now: Utc::now(),
        }
    }
}

impl Core {
    /// Work→Evaluation (§3.2 steps 9–10): the decider's entry event. The
    /// pre-eval rebase runs first because its `base_ref` pin is what the entry
    /// transition persists — the one piece of Evaluation entry that is I/O.
    pub(crate) async fn enter_evaluation(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if !self.draining {
            self.rebase_for_evaluation(owner, project, seq, &mut job)
                .await?;
        }
        self.run_eval(
            owner,
            project,
            seq,
            decide_eval::EvalEvent::Entered,
            Some(job),
        )
        .await
    }

    /// The C5 fold for one evaluation decision (contracts.md §2), the four-step
    /// shape C1 set: gather the reads into the view, call the pure decider, swap
    /// the round value it owns, apply its transitions through `set_state`, run
    /// its effects through `interpret` — and when a launch effect returns task
    /// ids, feed them back as the next event (C2's continuation contract) until
    /// the round settles.
    ///
    /// `entry_job` is the pre-eval rebase's in-memory record, which only the
    /// entry hop has; every later hop re-reads the job, so a decision never runs
    /// on a view the world moved under.
    async fn run_eval(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        event: decide_eval::EvalEvent,
        entry_job: Option<Job>,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let mut event = event;
        let mut entry_job = entry_job;
        for _ in 0..EVAL_FOLD_STEPS_MAX {
            let Some(inputs) = self.gather_eval_view(owner, project, seq, &event).await? else {
                tracing::warn!(
                    "eval event for {owner}/{project}#{seq}: no exec state \
                     (job revoked or completed); ignoring"
                );
                return Ok(());
            };
            let job = match entry_job.take() {
                Some(job) => job,
                None => self.must_get(owner, project, seq)?.clone(),
            };
            let view = inputs.view(&job, self.draining);
            let round = self.active.get_mut(&key).and_then(|e| e.round.take());
            let (round, transitions, effects, step) = decide_eval::decide(round, &view, event);
            if let Some(exec) = self.active.get_mut(&key) {
                exec.round = round;
            }
            for mut t in transitions {
                self.set_state(&mut t.job, t.to).await?;
            }
            self.commit_eval_step(&key, &step);
            let next = self.interpret_eval_effects(effects).await?;
            match self.run_eval_step(owner, project, seq, step, next).await? {
                Some(hop) => event = hop,
                None => return Ok(()),
            }
        }
        Err(CoreError::Config(format!(
            "evaluation fold for {owner}/{project}#{seq} did not settle in \
             {EVAL_FOLD_STEPS_MAX} steps"
        )))
    }

    /// The dispatcher-side bookkeeping an [`decide_eval::EvalStep`] names — the
    /// landing hand-off and the rework re-entry, both of which touch shell state
    /// the pure crate cannot see. Returns the event to re-enter the decider with,
    /// or `None` when this fold is finished.
    #[allow(
        clippy::expect_used,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    async fn run_eval_step(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        step: decide_eval::EvalStep,
        next: Option<decide_eval::EvalEvent>,
    ) -> Result<Option<decide_eval::EvalEvent>> {
        match step {
            decide_eval::EvalStep::AwaitOutcome => Ok(Some(
                next.expect("AwaitOutcome step without a launch effect"),
            )),
            decide_eval::EvalStep::Finalize => {
                self.finalize_pass(owner, project, seq).await?;
                Ok(None)
            }
            decide_eval::EvalStep::Rework {
                cycle,
                eval_context,
                ..
            } => {
                self.enter_work(
                    owner,
                    project,
                    seq,
                    cycle,
                    eval_context,
                    None,
                    Some(ReworkReason::EvalFailure),
                )
                .await?;
                Ok(None)
            }
            decide_eval::EvalStep::Await
            | decide_eval::EvalStep::Hold
            | decide_eval::EvalStep::Ignored
            | decide_eval::EvalStep::EscalatedDropExec => Ok(None),
        }
    }

    /// The part of committing an evaluation decision that touches the execution
    /// slice, between the transitions and the effects:
    ///
    /// - a rework spends one `reworks_used` BEFORE the effects, because
    ///   re-entering Work preserves the counter it reads (C4's `Admitted`
    ///   placement);
    /// - an escalation releases the slice BEFORE the effects (parity with C2's
    ///   `CompletedDropExec` and C3), so the escalation task is not stamped with
    ///   the cycle of a slice the decision just ended.
    fn commit_eval_step(&mut self, key: &(String, String, u64), step: &decide_eval::EvalStep) {
        if let decide_eval::EvalStep::Rework { reworks_used, .. } = step
            && let Some(exec) = self.active.get_mut(key)
        {
            exec.reworks_used = *reworks_used;
        }
        if step.drops_exec() {
            self.active.remove(key);
        }
    }

    /// Run one decision's effects, returning the event its launch answered with
    /// (contracts.md §2's continuation contract): a stage's slots or a
    /// relaunched slot's task id, which exist only once the launch ran.
    #[allow(
        clippy::expect_used,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    async fn interpret_eval_effects(
        &mut self,
        effects: Vec<Effect>,
    ) -> Result<Option<decide_eval::EvalEvent>> {
        let mut next = None;
        for effect in effects {
            let relaunched = match &effect {
                Effect::LaunchEvaluator {
                    evaluator, attempt, ..
                } => Some((evaluator.name.clone(), *attempt)),
                _ => None,
            };
            match self.interpret(effect).await? {
                Outcome::EvalSlots(slots) => {
                    next = Some(decide_eval::EvalEvent::StageLaunched { slots });
                }
                Outcome::EvaluatorTask(task_id) => {
                    let (evaluator, attempt) =
                        relaunched.expect("a task id comes from a LaunchEvaluator effect");
                    next = Some(decide_eval::EvalEvent::SlotRelaunched {
                        evaluator,
                        task_id,
                        attempt,
                    });
                }
                Outcome::Done => {}
                Outcome::Merge(_)
                | Outcome::CasRefused
                | Outcome::Rebase(_)
                | Outcome::GateSlots(_) => {
                    debug_assert!(false, "landing outcome in an evaluation decision");
                }
            }
        }
        Ok(next)
    }

    /// Assemble the read-only inputs for one evaluation decision (contracts.md
    /// §2: reads feed the view, they are not effects). `None` when the job has
    /// no execution slice — nothing to decide.
    async fn gather_eval_view(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        event: &decide_eval::EvalEvent,
    ) -> Result<Option<EvalViewData>> {
        let key = (owner.to_string(), project.to_string(), seq);
        let Some(exec) = self.active.get(&key) else {
            return Ok(None);
        };
        let infra_losses_prior = match event {
            decide_eval::EvalEvent::SlotExited { task, .. } => self
                .tasks
                .list_for_job(owner, project, seq)
                .await?
                .iter()
                .filter(|t| {
                    t.id != task.id
                        && t.infra_loss
                        && t.phase == TaskPhase::Evaluation
                        && t.cycle == task.cycle
                        && t.evaluator == task.evaluator
                })
                .count() as u32,
            _ => 0,
        };
        Ok(Some(EvalViewData {
            evaluators: exec.job_type.eval.clone(),
            cycle: exec.cycle,
            reworks_used: exec.reworks_used,
            rework_budget: exec.job_type.rework_budget.unwrap_or(0),
            work_type: exec.job_type.work.r#type,
            eval_retries: exec.job_type.eval_retries.unwrap_or(1),
            infra_losses_prior,
        }))
    }

    /// Restart reconciliation rebuilt the round from the task log (§3.6): replay
    /// the advance-or-reduce decision the crash lost.
    pub(crate) async fn eval_stage_settled(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        self.run_eval(
            owner,
            project,
            seq,
            decide_eval::EvalEvent::StageSettled,
            None,
        )
        .await
    }

    /// Rebase `job/{seq}` onto current default HEAD at Evaluation entry so the
    /// evaluators run against the exact stack that would merge (spec §3.2). On
    /// success `base_ref` advances to HEAD (persisted by the caller's
    /// `set_state`); the wrap-up gate-skip condition (`HEAD == base_ref`) then
    /// covers the common case with no gate rebuild. A conflict (or any git
    /// failure) leaves the branch exactly as pushed and keeps the old base_ref —
    /// evaluation proceeds on the stale stacking and the wrap-up merge
    /// gate/conflict machinery handles it, so no commits are ever lost. This is
    /// bookkeeping, not rework: cycle and rework budget are untouched.
    async fn rebase_for_evaluation(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        job: &mut types::Job,
    ) -> Result<()> {
        let Some(base_ref) = job.base_ref.clone() else {
            return Ok(());
        };
        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;
        if head == base_ref {
            return Ok(());
        }
        match self
            .repos
            .rebase_branch(owner, project, &job.branch, &base_ref, &head)
            .await
        {
            Ok(RebaseOutcome::Rebased { .. }) => {
                job.base_ref = Some(head.clone());
                self.publish(
                    owner,
                    project,
                    seq,
                    "job-rebased",
                    serde_json::json!({ "base_ref": head }),
                )
                .await?;
            }
            Ok(RebaseOutcome::Conflict { files }) => {
                self.publish(
                    owner,
                    project,
                    seq,
                    "job-rebase-conflict",
                    serde_json::json!({ "files": files }),
                )
                .await?;
            }
            Err(e) => {
                tracing::warn!(
                    "pre-eval rebase for {owner}/{project}#{seq} failed: {e}; \
                     evaluating on old base"
                );
            }
        }
        Ok(())
    }

    /// Fan out one stage's evaluators against the job branch (§3.3). Returns the
    /// live slots; the caller installs them on the round.
    pub(crate) async fn launch_eval_stage(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        branch: &str,
        cycle: u32,
        evaluators: Vec<Evaluator>,
    ) -> Result<Vec<EvalSlot>> {
        let mut slots = Vec::new();
        for evaluator in evaluators {
            let task_id = self
                .launch_evaluator_task(
                    owner,
                    project,
                    seq,
                    TaskPhase::Evaluation,
                    branch,
                    cycle,
                    &evaluator,
                    1,
                )
                .await?;
            slots.push(EvalSlot {
                evaluator,
                task_id,
                attempt: 1,
                outcome: None,
            });
        }
        Ok(slots)
    }

    /// Launch one merge-gate stage against the candidate branch (job #154): the
    /// gate runs its required command evaluators grouped by `stage`, ascending
    /// and one stage at a time, so a failure's class falls out of *which* stage
    /// failed (build vs test) rather than output parsing.
    pub(crate) async fn launch_gate_stage(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        gate_branch: &str,
        cycle: u32,
        evaluators: Vec<Evaluator>,
    ) -> Result<Vec<EvalSlot>> {
        let mut slots = Vec::new();
        for evaluator in evaluators {
            let task_id = self
                .launch_evaluator_task(
                    owner,
                    project,
                    seq,
                    TaskPhase::MergeGate,
                    gate_branch,
                    cycle,
                    &evaluator,
                    1,
                )
                .await?;
            slots.push(EvalSlot {
                evaluator,
                task_id,
                attempt: 1,
                outcome: None,
            });
        }
        Ok(slots)
    }

    /// Create + launch one evaluator task (§3.3 evaluator types). Shared by the
    /// Evaluation fan-out (job branch) and the merge gate (candidate branch).
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    pub(crate) async fn launch_evaluator_task(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        phase: TaskPhase,
        branch: &str,
        cycle: u32,
        evaluator: &Evaluator,
        attempt: u32,
    ) -> Result<u64> {
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        let job_type = self.active.get(&key).expect("exec state").job_type.clone();
        let task_id = self.next_task_id(owner, project, seq).await?;

        let (kind, pending_human) = match evaluator.r#type {
            EvaluatorType::Command => (
                TaskKind::Command {
                    run: evaluator.run.clone().unwrap_or_default(),
                },
                false,
            ),
            EvaluatorType::Agent => (
                TaskKind::Agent {
                    provider: crate::exec::provider_name(
                        evaluator.provider,
                        self.config.agent_provider_default.as_deref(),
                    ),
                    model: evaluator
                        .model
                        .clone()
                        .or_else(|| self.config.agent_model_default.clone()),
                    prompt: evaluator.prompt.clone().unwrap_or_default(),
                },
                false,
            ),
            EvaluatorType::Human => (
                TaskKind::Human {
                    prompt: format!(
                        "{}{}",
                        evaluator.prompt.clone().unwrap_or_default(),
                        self.work_brief(owner, project, &job)
                    ),
                },
                true,
            ),
        };
        let session_id = matches!(evaluator.r#type, EvaluatorType::Agent)
            .then(|| uuid::Uuid::new_v4().to_string());
        let reviewed_tip = self.repos.resolve_ref(owner, project, branch).await.ok();
        let mut task = Task {
            id: task_id,
            job_seq: seq,
            project: job.project.clone(),
            phase,
            cycle,
            kind,
            state: if pending_human {
                TaskState::Pending
            } else {
                TaskState::Running
            },
            attempt,
            evaluator: Some(evaluator.name.clone()),
            label: Some(evaluator.name.clone()),
            stage: evaluator.stage,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            session_id: session_id.clone(),
            pending_reason: None,
            queued_at: None,
            reviewed_tip,
            result: None,
            created_at: Utc::now(),
            started_at: (!pending_human).then(Utc::now),
            completed_at: None,
        };
        self.task_create(
            owner,
            project,
            &task,
            serde_json::json!({
                "attempt": attempt, "evaluator": evaluator.name, "stage": evaluator.stage,
            }),
        )
        .await?;
        if pending_human {
            return Ok(task_id);
        }

        let eval_timeout = task_timeout(&job_type);

        match evaluator.r#type {
            EvaluatorType::Command => {
                let placement = self.placement_guard();
                let run = evaluator.run.clone().unwrap_or_default();
                let config = self
                    .command_launch_config(
                        owner,
                        project,
                        seq,
                        branch,
                        &job_type,
                        &evaluator.secrets,
                        eval_image(&job_type, evaluator),
                        run,
                        ChannelRole::Eval {
                            task_id,
                            evaluator: evaluator.name.clone(),
                        },
                        eval_timeout,
                    )
                    .await?;
                match self
                    .place_container(DecidedLaunch { config, placement })
                    .await
                {
                    Ok(id) => {
                        task.container_id = Some(id.clone());
                        self.task_put(&task).await?;
                        self.spawn_eval_monitor(owner, project, seq, task_id, id);
                    }
                    Err(container::BackendError::NoCapacity(reason)) => {
                        self.defer_launch(owner, project, seq, &mut task, reason)
                            .await?;
                    }
                    Err(e) => {
                        self.report_launch_failure(owner, project, seq, task_id, e);
                    }
                }
            }
            EvaluatorType::Agent => {
                self.spawn_eval_agent(
                    owner,
                    project,
                    seq,
                    task_id,
                    session_id.clone(),
                    branch,
                    evaluator,
                )
                .await?;
            }
            EvaluatorType::Human => unreachable!(),
        }
        Ok(task_id)
    }

    /// Build and spawn an agent evaluator run (§3.3). Shared by the initial
    /// Evaluation fan-out and the launch-queue resume ([`Core::resume_launch`]),
    /// so a queued agent eval relaunches byte-identically. The spawned task
    /// reports a `NoCapacity` launch refusal back as [`Msg::LaunchDeferred`] so
    /// the actor queues the launch (§3.5) instead of letting the provider's
    /// erased error surface as a verdict-less exit that burns `eval_retries` —
    /// the #125/#130 saturated-fleet escalation this closes (#140). Any other
    /// outcome reports through the normal exit fan-in.
    /// The §3.3 re-review context block (job #155) for an agent evaluator on
    /// cycle `cycle > 1`, or `None` for a first review / an evaluator that did
    /// not run on a prior cycle / a non-agent prior result. Assembled entirely
    /// from persisted records (the task log + the bare repo), so it is rebuilt
    /// faithfully after a dispatcher restart.
    #[allow(
        clippy::too_many_lines,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    async fn prior_review_block(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        branch: &str,
        evaluator: &Evaluator,
        cycle: u32,
    ) -> Result<Option<String>> {
        if cycle <= 1 {
            return Ok(None);
        }
        let tasks = self.tasks.list_for_job(owner, project, seq).await?;
        let Some(prior) = tasks
            .iter()
            .filter(|t| {
                t.phase == TaskPhase::Evaluation
                    && t.cycle < cycle
                    && t.evaluator.as_deref() == Some(evaluator.name.as_str())
                    && t.result.is_some()
            })
            .max_by_key(|t| (t.cycle, t.id))
        else {
            return Ok(None);
        };
        let (prior_pass, prior_findings) = match &prior.result {
            Some(TaskResult::Agent {
                pass, structured, ..
            }) => (*pass, structured.clone()),
            _ => return Ok(None),
        };

        let mut block = String::from("\n\n---\n## Re-Review Context (job #155)\n");
        block.push_str(&format!(
            "You are reviewing this branch again (cycle {cycle}). Verify your previous \
             findings are addressed and review the delta closely; the full diff remains \
             available and authoritative — spot-check beyond the delta at your judgment. \
             Your pass verdict still asserts the **whole** branch meets the bar, not just \
             the delta.\n"
        ));

        block.push_str("\n### Your previous review\n");
        block.push_str(&format!(
            "Verdict: **{}**\n",
            if prior_pass { "pass" } else { "fail" }
        ));
        let findings = prior_findings
            .as_ref()
            .and_then(|v| serde_json::to_string_pretty(v).ok())
            .unwrap_or_else(|| "(no structured findings)".into());
        block.push_str(&format!("Findings:\n```json\n{findings}\n```\n"));

        let rebased = tasks
            .iter()
            .filter(|t| t.phase == TaskPhase::Work && t.cycle == cycle && t.evaluator.is_none())
            .max_by_key(|t| t.id)
            .and_then(|t| t.rework_reason)
            .is_some_and(|r| {
                matches!(
                    r,
                    ReworkReason::MergeConflict
                        | ReworkReason::GateCiFailure
                        | ReworkReason::GateCompileFix
                )
            });

        let current_tip = self.repos.resolve_ref(owner, project, branch).await.ok();
        match (prior.reviewed_tip.as_deref(), current_tip.as_deref()) {
            (Some(last), Some(now)) => {
                block.push_str("\n### What you reviewed\n");
                block.push_str(&format!(
                    "Last-reviewed tip: `{last}`\nCurrent tip: `{now}`\n"
                ));
                let linear = !rebased
                    && self
                        .repos
                        .is_ancestor(owner, project, last, now)
                        .await
                        .unwrap_or(false);
                if last == now && !rebased {
                    block.push_str(
                        "\n### What changed since\nNothing new since your last review — the \
                         branch tip is unchanged.\n",
                    );
                } else if linear {
                    let delta = self.repos.diff_between(owner, project, last, now).await?;
                    block.push_str(&format!(
                        "\n### What changed since (delta `{last}..{now}`)\n{}\n",
                        fenced_delta(&delta.diff)
                    ));
                } else {
                    block.push_str(
                        "\n### What changed since\nThe branch was **rebased** since your last \
                         review (a conflict/gate rework replayed it onto a moved base), so a \
                         delta from your last-reviewed tip is not meaningful. Re-review the \
                         full diff in your workspace.\n",
                    );
                }
            }
            _ => {
                block.push_str(
                    "\n### What you reviewed\nThe previously-reviewed tip wasn't recorded; \
                     review the full diff in your workspace.\n",
                );
            }
        }

        block.push_str(&history_digest(&tasks, cycle));
        Ok(Some(block))
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    pub(crate) async fn spawn_eval_agent(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        session_id: Option<String>,
        branch: &str,
        evaluator: &Evaluator,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        let job_type = self.active.get(&key).expect("exec state").job_type.clone();
        let base_ref = job.base_ref.clone().expect("base_ref set");
        let eval_timeout = task_timeout(&job_type);
        let tx = self.self_tx.clone().expect("spawned core");
        let (o, p) = (owner.to_string(), project.to_string());
        let mut env = self
            .container_env(
                owner,
                project,
                seq,
                branch,
                &job_type,
                &evaluator.secrets,
                ChannelRole::Eval {
                    task_id,
                    evaluator: evaluator.name.clone(),
                },
                eval_timeout,
            )
            .await?;
        self.inject_platform_agent_secrets(&mut env).await?;
        let mut prompt = format!(
            "{}{}",
            self.repos
                .read_file_at(
                    owner,
                    project,
                    &base_ref,
                    evaluator.prompt.as_deref().unwrap_or_default()
                )
                .await?
                .unwrap_or_default(),
            self.work_brief(owner, project, &job)
        );
        let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
        if let Some(block) = self
            .prior_review_block(owner, project, seq, branch, evaluator, cycle)
            .await?
        {
            prompt.push_str(&block);
        }
        if let Some(pred) = self
            .predecessor_block(
                owner,
                project,
                seq,
                TaskPhase::Evaluation,
                cycle,
                Some(&evaluator.name),
                task_id,
                false,
            )
            .await
        {
            prompt = format!("{pred}{prompt}");
        }
        let (mcp_servers, mut files) = self.channel_mcp(&env);
        files.extend(
            self.ssh_credential_files(
                owner,
                project,
                seq,
                ChannelRole::Eval {
                    task_id,
                    evaluator: evaluator.name.clone(),
                },
                eval_timeout,
            )
            .await?,
        );
        let config = AgentRunConfig {
            image: eval_image(&job_type, evaluator),
            prompt,
            model: evaluator
                .model
                .clone()
                .or_else(|| self.config.agent_model_default.clone()),
            system_prompt: None,
            mcp_servers,
            files,
            env,
            task_timeout: task_timeout(&job_type),
            eval_context: vec![],
            merge_conflict: None,
            session_id: session_id.unwrap_or_default(),
            node: job_type.placement_node().map(String::from),
            permissions: agent::PermissionProfile::Review,
        };
        let provider = self.provider.clone();
        let harvest = self.harvester();
        let on_launch = self.launch_reporter(owner, project, seq, task_id);
        tokio::spawn(async move {
            match provider.run(config, on_launch).await {
                Ok(out) => {
                    let usage = harvest.collect(&o, &p, seq, task_id, &out).await;
                    if let Some(id) = &out.container_id {
                        harvest.dispose(seq, task_id, id).await;
                    }
                    let _ = tx
                        .send(Msg::TaskExited {
                            owner: o,
                            project: p,
                            seq,
                            task_id,
                            exit: TaskExit {
                                exit_code: out.exit_code,
                                eval_json: None,
                                usage,
                                assessment: None,
                                launch_error: None,
                                log_tail: None,
                                infra_loss: false,
                                structured: None,
                            },
                        })
                        .await;
                }
                Err(agent::AgentError::Backend(container::BackendError::NoCapacity(reason))) => {
                    let _ = tx
                        .send(Msg::LaunchDeferred {
                            owner: o,
                            project: p,
                            seq,
                            task_id,
                            reason,
                        })
                        .await;
                }
                Err(e) => {
                    tracing::error!("eval agent run failed: {e}");
                    let _ = tx
                        .send(Msg::TaskExited {
                            owner: o,
                            project: p,
                            seq,
                            task_id,
                            exit: TaskExit {
                                exit_code: -1,
                                eval_json: None,
                                usage: None,
                                assessment: None,
                                launch_error: None,
                                log_tail: None,
                                infra_loss: false,
                                structured: None,
                            },
                        })
                        .await;
                }
            }
        });
        Ok(())
    }

    /// `req.eval.submit.*` (spec §4.2): the authoritative agent verdict.
    /// Idempotent when the task is already Done.
    pub(crate) async fn handle_submit_eval(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        submission: EvalSubmission,
    ) -> Result<()> {
        let Some(mut task) = self.tasks.get(owner, project, seq, task_id).await? else {
            return Err(CoreError::NotFound(format!("task {task_id}")));
        };
        if task.state == TaskState::Done {
            return Ok(());
        }
        task.result = Some(TaskResult::Agent {
            pass: submission.pass && !submission.abort,
            abort: submission.abort,
            structured: submission.structured,
            token_usage: submission.token_usage,
            cover_html: submission.cover_html,
        });
        task.state = TaskState::Done;
        task.completed_at = Some(Utc::now());
        self.task_put(&task).await?;
        self.publish(
            owner,
            project,
            seq,
            "task-completed",
            serde_json::json!({
                "task_id": task_id, "phase": "Evaluation", "pass": submission.pass,
            }),
        )
        .await?;
        Ok(())
    }

    /// Human evaluator resolved via the inbox (§3.3): hand the verdict to the
    /// decider, which records it on the slot and settles the round. Called from
    /// the resolve handler.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resolve_eval_slot(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        pass: bool,
        abort: bool,
        structured: Option<serde_json::Value>,
    ) -> Result<()> {
        let open = self
            .active
            .get(&(owner.to_string(), project.to_string(), seq))
            .and_then(|e| e.round.as_ref())
            .is_some_and(|r| r.open_slot(task_id).is_some());
        if !open {
            return Err(CoreError::InvalidResolution(format!(
                "task {task_id} is not an open evaluator slot"
            )));
        }
        self.run_eval(
            owner,
            project,
            seq,
            decide_eval::EvalEvent::SlotResolved {
                task_id,
                pass,
                abort,
                structured,
            },
            None,
        )
        .await
    }

    /// Eval container exited (§3.3): hand it to the decider as the round's
    /// verdict source — a command's exit code, an agent's `submit_eval`, or a
    /// verdict-less exit that routes to the evidence-free path (#167/#198).
    pub(crate) async fn on_eval_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task: Task,
        exit: TaskExit,
    ) -> Result<()> {
        let task = match task.kind {
            TaskKind::Agent { .. } => self
                .tasks
                .get(owner, project, seq, task.id)
                .await?
                .unwrap_or(task),
            _ => task,
        };
        let exit = decide_eval::EvalExit {
            exit_code: exit.exit_code,
            eval_json: exit.eval_json,
            usage: exit.usage,
            launch_error: exit.launch_error,
            log_tail: exit.log_tail,
        };
        self.run_eval(
            owner,
            project,
            seq,
            decide_eval::EvalEvent::SlotExited {
                task: Box::new(task),
                exit,
            },
            None,
        )
        .await
    }

    /// §3.2 step 12 entry: queue the job for finalization and pump. The
    /// per-project queue is the depth-1 merge-gate serialization (§3.3).
    /// Also the reconcile re-entry point (`refinalize`).
    pub(crate) async fn refinalize(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        self.finalize_pass(owner, project, seq).await
    }

    /// A passed evaluation joins the landing queue (§3.3): the C2 shim —
    /// decide, apply transitions, run effects, pump.
    async fn finalize_pass(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let job = self.must_get(owner, project, seq)?.clone();
        let slug = format!("{owner}/{project}");
        let wrap_up_none = self
            .active
            .get(&(owner.to_string(), project.to_string(), seq))
            .map(|e| e.job_type.wrap_up.r#type == WrapUpMode::None)
            .unwrap_or(false);
        let state = self.merge_gates.remove(&slug).unwrap_or_default();
        let (state, transitions, effects, step) =
            merge_gate::decide_enqueue(state, &job, wrap_up_none);
        self.store_gate_state(&slug, state);
        if step == merge_gate::EnqueueStep::CompleteDirectly {
            return self.complete_done(owner, project, seq).await;
        }
        for mut t in transitions {
            self.set_state(&mut t.job, t.to).await?;
        }
        for effect in effects {
            self.interpret(effect).await?;
        }
        self.pump_merges(owner, project).await
    }

    /// Swap a slug's decider-owned [`merge_gate::MergeGateState`] back in,
    /// dropping the entry when idle so an inactive project holds no state.
    fn store_gate_state(&mut self, slug: &str, state: merge_gate::MergeGateState) {
        if state.is_empty() {
            self.merge_gates.remove(slug);
        } else {
            self.merge_gates.insert(slug.to_string(), state);
        }
    }

    /// Advance the merge queue until it empties or a gate starts — the C2
    /// fold driver over the pure serializer
    /// ([`merge_gate::MergeGateState::next_candidate`]). Wrap-up is designed
    /// to be infallible; when a landing step fails anyway (git plumbing, repo
    /// IO — not a Conflict, which has its own rework path), the job escalates
    /// and the queue moves on instead of wedging (design-lifecycle.md).
    pub(crate) async fn pump_merges(&mut self, owner: &str, project: &str) -> Result<()> {
        let slug = format!("{owner}/{project}");
        loop {
            let held = self.release_holds.contains(&slug);
            let draining = self.draining;
            let mut state = self.merge_gates.remove(&slug).unwrap_or_default();
            let next = state.next_candidate(held, draining);
            self.store_gate_state(&slug, state);
            let Some(seq) = next else {
                return Ok(());
            };
            match self
                .run_landing(owner, project, seq, merge_gate::LandingEvent::Start, None)
                .await
            {
                Ok(merge_gate::LandingStep::Gating) => return Ok(()),
                Ok(_) => continue,
                Err(e) => {
                    tracing::error!("finalize for {owner}/{project}#{seq}: {e}");
                    self.escalate_finalize_failure(owner, project, seq, &e)
                        .await;
                    continue;
                }
            }
        }
    }

    /// Best-effort escalation for an unexpected finalization error. Never
    /// returns Err: the merge queue must keep moving whatever state the job
    /// is in (it may have been revoked out from under the queue).
    async fn escalate_finalize_failure(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        error: &CoreError,
    ) {
        self.active
            .remove(&(owner.to_string(), project.to_string(), seq));
        if let Err(e2) = self
            .escalate(
                owner,
                project,
                seq,
                "finalize_failed",
                format!("Job {seq}: wrap-up failed unexpectedly: {error}"),
                None,
            )
            .await
        {
            tracing::error!("escalating finalize failure for {owner}/{project}#{seq}: {e2}");
        }
    }

    /// Assemble the read-only inputs for one landing decision (contracts.md
    /// §2: reads feed the view, they are not effects). Re-gathered before
    /// EVERY `decide` call — the continuation contract's freshness rule, so a
    /// decision never runs on a view the world moved under. `parked` carries
    /// the gate round's (candidate commit, old head) on a verdict entry.
    async fn gather_landing_view(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        parked: Option<&(String, String)>,
    ) -> Result<LandingViewData> {
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
        let tasks = self.tasks.list_for_job(owner, project, seq).await?;
        let force_gate = merge_gate::force_gate(&tasks, cycle);
        let mut summary = self
            .active
            .get(&key)
            .and_then(|e| e.work_submission.as_ref())
            .and_then(|s| s.summary.clone());
        if force_gate {
            let note = "Includes a gate-fix round (job #154): a compile-only merge-gate \
                        failure was repaired by a scoped fix task and re-gated, without \
                        re-review.";
            summary = Some(match summary {
                Some(prose) if !prose.is_empty() => format!("{prose}\n\n{note}"),
                _ => note.to_string(),
            });
        }
        if job.is_batch() {
            let header = format!(
                "Batch of {} {} jobs: {}",
                job.members.len(),
                job.r#type,
                job.members
                    .iter()
                    .map(|m| format!("#{m}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            summary = Some(match summary {
                Some(prose) if !prose.is_empty() => format!("{header}\n\n{prose}"),
                _ => header,
            });
        }
        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;
        let gate_evaluators = self
            .active
            .get(&key)
            .map(|e| merge_gate::gate_evaluators(&e.job_type.eval))
            .unwrap_or_default();
        let gate_fix_used = self.active.get(&key).map(|e| e.gate_fix_used).unwrap_or(0);
        let (gate_commit, gate_old_head) = match parked {
            Some((c, h)) => (Some(c.clone()), Some(h.clone())),
            None => (None, None),
        };
        Ok(LandingViewData {
            job,
            head,
            force_gate,
            summary,
            cycle,
            gate_fix_used,
            gate_evaluators,
            gate_commit,
            gate_old_head,
        })
    }

    /// The C2 continuation fold for ONE landing (contracts.md §2): gather a
    /// fresh view, call the pure decider, swap its state value, apply
    /// transitions through `set_state`, run effects through `interpret` — and
    /// when an effect carries a result, feed it back as the next event until
    /// the decider settles. This loop IS the shape every later phase decider's
    /// shim copies.
    #[allow(
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    async fn run_landing(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        mut event: merge_gate::LandingEvent,
        parked: Option<(String, String)>,
    ) -> Result<merge_gate::LandingStep> {
        use merge_gate::{LandingEvent, LandingStep, MergeOutcome as Mo};
        let key = (owner.to_string(), project.to_string(), seq);
        let slug = format!("{owner}/{project}");
        let mut conflict_files: Vec<String> = Vec::new();
        let mut candidate_commit: Option<String> = None;
        loop {
            let view_data = self
                .gather_landing_view(owner, project, seq, parked.as_ref())
                .await?;
            let state = self.merge_gates.remove(&slug).unwrap_or_default();
            let view = merge_gate::LandingView {
                job: &view_data.job,
                head: view_data.head.clone(),
                force_gate: view_data.force_gate,
                summary: view_data.summary.clone(),
                cycle: view_data.cycle,
                gate_fix_used: view_data.gate_fix_used,
                gate_evaluators: view_data.gate_evaluators.clone(),
                gate_commit: view_data.gate_commit.clone(),
                gate_old_head: view_data.gate_old_head.clone(),
            };
            let (state, transitions, effects, step) = merge_gate::decide(state, &view, event);
            self.store_gate_state(&slug, state);
            for mut t in transitions {
                self.set_state(&mut t.job, t.to).await?;
            }
            if step == LandingStep::CompletedDropExec {
                self.active.remove(&key);
            }
            let mut next: Option<LandingEvent> = None;
            for effect in effects {
                let fast_path = matches!(effect, Effect::SquashMerge { .. });
                let rebase_target = match &effect {
                    Effect::RebaseOntoWithConflict { new_base, .. } => Some(new_base.clone()),
                    _ => None,
                };
                match self.interpret(effect).await? {
                    Outcome::Done => {}
                    Outcome::Merge(outcome) => {
                        let mirrored = match outcome {
                            vcs::MergeOutcome::Merged { commit } => {
                                candidate_commit = Some(commit.clone());
                                Mo::Merged { commit }
                            }
                            vcs::MergeOutcome::NoOp => Mo::NoOp,
                            vcs::MergeOutcome::Conflict { files } => {
                                conflict_files = files.clone();
                                Mo::Conflict { files }
                            }
                            vcs::MergeOutcome::UnresolvedMarkers { files } => {
                                Mo::UnresolvedMarkers { files }
                            }
                        };
                        next = Some(if fast_path {
                            LandingEvent::Squashed { outcome: mirrored }
                        } else {
                            LandingEvent::CandidateBuilt { outcome: mirrored }
                        });
                    }
                    Outcome::CasRefused => next = Some(LandingEvent::PromoteRefused),
                    Outcome::Rebase(outcome) => {
                        let new_base = rebase_target.expect("rebase outcome from a rebase effect");
                        let old_base = view_data.job.base_ref.clone().expect("base_ref set");
                        let mut context = self
                            .repos
                            .conflict_context(owner, project, &old_base, &new_base, &conflict_files)
                            .await?;
                        augment_conflict_context(&mut context, &outcome);
                        conflict_files = Vec::new();
                        next = Some(LandingEvent::Rebased {
                            conflict_context: context,
                        });
                    }
                    Outcome::GateSlots(slots) => {
                        let mut pending =
                            merge_gate::group_stages(view_data.gate_evaluators.clone());
                        pending.pop_front();
                        let commit = candidate_commit
                            .clone()
                            .expect("gate opens from a built candidate");
                        self.active.get_mut(&key).expect("exec state").gate = Some(GateState {
                            commit,
                            old_head: view_data.head.clone(),
                            round: EvalRound {
                                slots,
                                pending,
                                done: Vec::new(),
                            },
                        });
                    }
                    Outcome::EvalSlots(_) | Outcome::EvaluatorTask(_) => {
                        debug_assert!(false, "evaluation outcome in a landing decision");
                    }
                }
            }
            match step {
                LandingStep::AwaitOutcome => {
                    event = next.expect("AwaitOutcome step without a result-carrying effect");
                }
                LandingStep::FinishLanding => {
                    self.finish_landing(owner, project, seq).await?;
                    return Ok(LandingStep::Completed);
                }
                LandingStep::Completed | LandingStep::CompletedDropExec | LandingStep::Gating => {
                    return Ok(step);
                }
            }
        }
    }

    /// Gate container exited: command evaluators only, exit code is the
    /// verdict (§3.3 Merge Gate).
    #[allow(
        clippy::expect_used,
        clippy::too_many_lines,
        clippy::unwrap_used,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    pub(crate) async fn on_gate_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        mut task: Task,
        exit: TaskExit,
    ) -> Result<()> {
        let TaskExit {
            exit_code,
            eval_json,
            launch_error,
            log_tail,
            ..
        } = exit;
        let key = (owner.to_string(), project.to_string(), seq);
        let Some(slot_idx) = self
            .active
            .get(&key)
            .and_then(|e| e.gate.as_ref())
            .and_then(|g| {
                g.round
                    .slots
                    .iter()
                    .position(|s| s.task_id == task.id && s.outcome.is_none())
            })
        else {
            return Ok(());
        };

        let pass = exit_code == 0;
        task.result = Some(TaskResult::Command {
            pass,
            exit_code,
            output: log_tail.clone().or(launch_error).unwrap_or_default(),
            structured: eval_json.clone(),
        });
        task.state = TaskState::Done;
        task.completed_at = Some(Utc::now());
        self.task_put(&task).await?;
        self.publish(
            owner,
            project,
            seq,
            "task-completed",
            serde_json::json!({
                "task_id": task.id, "phase": "MergeGate", "pass": pass,
            }),
        )
        .await?;

        let gate = self.active.get_mut(&key).unwrap().gate.as_mut().unwrap();
        gate.round.slots[slot_idx].outcome = Some(SlotOutcome::Product {
            pass,
            abort: false,
            structured: eval_json,
            output: None,
        });
        if gate.round.slots.iter().any(|s| s.outcome.is_none()) {
            return Ok(());
        }
        if stage_passed(&gate.round.slots) && !gate.round.pending.is_empty() {
            let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
            let gate_branch = format!("merge-gate/{seq}");
            let (passed, next) = {
                let g = self.active.get_mut(&key).unwrap().gate.as_mut().unwrap();
                let passed: Vec<EvalSlot> = g.round.slots.drain(..).collect();
                let next = g.round.pending.pop_front().expect("pending checked");
                (passed, next)
            };
            if let Some(trace) = &self.trace {
                trace.effect("LaunchGateStage");
            }
            let slots = self
                .launch_gate_stage(owner, project, seq, &gate_branch, cycle, next)
                .await?;
            let g = self.active.get_mut(&key).unwrap().gate.as_mut().unwrap();
            g.round.done.extend(passed);
            g.round.slots = slots;
            return Ok(());
        }
        if let Err(e) = self.gate_reduce(owner, project, seq).await {
            tracing::error!("gate reduce for {owner}/{project}#{seq}: {e}");
            let slug = format!("{owner}/{project}");
            if let Some(state) = self.merge_gates.get_mut(&slug) {
                state.remove(seq);
                if state.is_empty() {
                    self.merge_gates.remove(&slug);
                }
            }
            self.escalate_finalize_failure(owner, project, seq, &e)
                .await;
            return self.pump_merges(owner, project).await;
        }
        Ok(())
    }

    /// The gate's verdict (§3.3): fold the round's slots into the failure
    /// set, derive the deterministic classification inputs, and re-enter the
    /// landing fold with [`merge_gate::LandingEvent::GateVerdict`].
    #[allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    async fn gate_reduce(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let gate = self
            .active
            .get_mut(&key)
            .unwrap()
            .gate
            .take()
            .expect("gate state");
        let failed_ids: Vec<(String, u64)> = gate
            .round
            .slots
            .iter()
            .filter_map(|s| match s.outcome.as_ref() {
                Some(SlotOutcome::Product { pass: false, .. }) => {
                    Some((s.evaluator.name.clone(), s.task_id))
                }
                _ => None,
            })
            .collect();
        let failures: Vec<EvalResult> = gate
            .round
            .slots
            .iter()
            .filter_map(|s| match s.outcome.as_ref() {
                Some(SlotOutcome::Product {
                    pass: false,
                    structured,
                    ..
                }) => Some(EvalResult {
                    evaluator: s.evaluator.name.clone(),
                    pass: false,
                    structured: structured.clone(),
                    output: None,
                }),
                _ => None,
            })
            .collect();
        let first_stage_failed = gate.round.done.is_empty() && !gate.round.pending.is_empty();
        let compiler_output = if !failures.is_empty() && first_stage_failed {
            self.gate_stage_output(owner, project, seq, &failed_ids)
                .await
        } else {
            String::new()
        };
        let step = self
            .run_landing(
                owner,
                project,
                seq,
                merge_gate::LandingEvent::GateVerdict {
                    failures,
                    first_stage_failed,
                    compiler_output,
                },
                Some((gate.commit.clone(), gate.old_head.clone())),
            )
            .await?;
        debug_assert!(
            step != merge_gate::LandingStep::Gating,
            "a verdict never opens a gate directly"
        );
        self.pump_merges(owner, project).await
    }

    /// Launch a scoped gate-fix task (job #154) for a compile-only gate failure.
    /// Rebases `job/{seq}` onto the gated head (the collision the fix must
    /// resolve), bumps the gate-fix budget, and re-enters Work with a narrow
    /// "restore compilation" brief and [`ReworkReason::GateCompileFix`] — which
    /// routes the completed fix straight back to the gate, not to re-review.
    /// Gather the captured container output of the failing gate stage(s) —
    /// the compiler errors [`Core::on_gate_exited`] stored on each failed
    /// command task — so the gate-fix brief (job #154) can show the agent the
    /// exact errors it must repair. Reads the persisted task records, so it is
    /// robust to a restart between the gate failure and the fix launch.
    async fn gate_stage_output(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        failed: &[(String, u64)],
    ) -> String {
        let mut out = String::new();
        for (name, task_id) in failed {
            let Ok(Some(task)) = self.tasks.get(owner, project, seq, *task_id).await else {
                continue;
            };
            if let Some(TaskResult::Command { output, .. }) = &task.result {
                let trimmed = output.trim();
                if !trimmed.is_empty() {
                    out.push_str(&format!(
                        "### `{name}` stage output\n```\n{trimmed}\n```\n\n"
                    ));
                }
            }
        }
        out
    }

    #[allow(
        clippy::expect_used,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    pub(crate) async fn launch_gate_fix(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        new_base: String,
        failures: Vec<EvalResult>,
        compiler_output: String,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let _ = self
            .repos
            .delete_branch(owner, project, &format!("merge-gate/{seq}"))
            .await;
        let job = self.must_get(owner, project, seq)?.clone();
        let old_base = job.base_ref.clone().expect("base_ref set");
        let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
        let outcome = self
            .repos
            .rebase_onto_with_conflict(owner, project, seq, &new_base)
            .await?;
        let mut context = String::from(GATE_FIX_FRAMING);
        if !compiler_output.trim().is_empty() {
            context.push_str("\n### Gate build output (the errors to fix)\n\n");
            context.push_str(&compiler_output);
        }
        let conflict_ctx = self
            .repos
            .conflict_context(owner, project, &old_base, &new_base, &[])
            .await?;
        context.push_str(&conflict_ctx);
        augment_conflict_context(&mut context, &outcome);

        let mut job = job;
        job.base_ref = Some(new_base.clone());
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        if let Some(e) = self.active.get_mut(&key) {
            e.gate_fix_used += 1;
        }
        self.publish(
            owner,
            project,
            seq,
            "job-rework-started",
            serde_json::json!({
                "cycle": cycle + 1, "reason": "gate_compile_fix", "eval_context": failures,
            }),
        )
        .await?;
        self.enter_work(
            owner,
            project,
            seq,
            cycle + 1,
            failures,
            Some(context),
            Some(ReworkReason::GateCompileFix),
        )
        .await
    }

    /// A gate-fix task finished (job #154): re-enter the merge gate directly,
    /// skipping re-review and eval-phase CI. Transitions Work→Evaluation→WrapUp
    /// (both allowed, §2.1) without launching any evaluator, so `finalize_pass`
    /// rebuilds the candidate and re-runs gate CI — the final authority.
    pub(crate) async fn reenter_gate_after_fix(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if job.state == JobState::Work {
            self.set_state(&mut job, JobState::Evaluation).await?;
        }
        self.finalize_pass(owner, project, seq).await
    }

    /// The squash has landed on the default branch (spec §3.2 step 12). If the
    /// job type declares a `wrap_up.run` publish command, launch it against the
    /// merged main content and hold the job in WrapUp until it exits (the merge
    /// queue advances regardless — the publish is an external effect, not a
    /// merge). Otherwise this is a plain code job: go straight to Done.
    ///
    /// This is the single post-merge fork, reached from every merge-success site
    /// (fast path, gate-skip promote, candidate NoOp, gate promote), so a restart
    /// that re-drives finalization re-launches the publish through here too — the
    /// §3.6 gap fix for a crash between the squash landing and the publish
    /// completing.
    pub(crate) async fn finish_landing(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        self.refresh_project_schedules(owner, project).await;
        self.run_wrapup(owner, project, seq, wrapup::WrapUpEvent::Landed)
            .await
    }

    /// The C3 shim (contracts.md §2), the same four-step shape C1 set: gather
    /// the reads into the view, call the pure decider, apply its transitions
    /// through `set_state`, run its effects through `interpret` — plus the
    /// dispatcher-side bookkeeping the returned [`wrapup::WrapUpStep`] names,
    /// which touches shell state (the execution slice, the dependents fan-out)
    /// the pure crate cannot see.
    pub(crate) async fn run_wrapup(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        event: wrapup::WrapUpEvent,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        let members = job
            .members
            .iter()
            .map(|&m| self.must_get(owner, project, m).cloned())
            .collect::<Result<Vec<Job>>>()?;
        let publish_command = self
            .active
            .get(&key)
            .is_some_and(|e| e.job_type.wrap_up.run.is_some());
        let view = wrapup::WrapUpView {
            job: &job,
            publish_command,
            members: &members,
            now: Utc::now(),
        };
        let (transitions, effects, step) = wrapup::decide(&view, event);
        for mut t in transitions {
            self.set_state(&mut t.job, t.to).await?;
        }
        if step.drops_exec() {
            self.active.remove(&key);
        }
        for effect in effects {
            self.interpret(effect).await?;
        }
        match step {
            wrapup::WrapUpStep::AwaitPublish | wrapup::WrapUpStep::EscalatedDropExec => Ok(()),
            wrapup::WrapUpStep::Complete => self.complete_done(owner, project, seq).await,
            wrapup::WrapUpStep::Completed { unblock } => {
                for done_seq in unblock {
                    self.on_job_done(owner, project, done_seq).await?;
                }
                Ok(())
            }
        }
    }

    /// Create + launch the `wrap_up.run` command task (spec §3.2). It clones the
    /// *default* branch — the squash has already landed, so its HEAD carries the
    /// merged content the publish must ship. The task record (phase `WrapUp`) is
    /// the restart marker: its presence tells reconciliation the merge is done
    /// and only the publish remains (§3.6). Idempotent by contract — a restart
    /// may re-launch it.
    #[allow(
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "TODO(io-split): assembly and port calls the decider carve left behind — no decision logic here."
    )]
    pub(crate) async fn launch_wrapup_task(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        attempt: u32,
    ) -> Result<()> {
        if self.draining {
            return Ok(());
        }
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        let job_type = self.active.get(&key).expect("exec state").job_type.clone();
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let run = job_type.wrap_up.run.clone().unwrap_or_default();
        let default_branch = self.repos.default_branch(owner, project).await?;
        let timeout = task_timeout(&job_type);

        let task_id = self.next_task_id(owner, project, seq).await?;
        let mut task = Task {
            id: task_id,
            job_seq: seq,
            project: job.project.clone(),
            phase: TaskPhase::WrapUp,
            cycle,
            kind: TaskKind::Command { run: run.clone() },
            state: TaskState::Running,
            attempt,
            evaluator: None,
            label: Some(job_type.wrap_up.label()),
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
        };
        self.task_create(
            owner,
            project,
            &task,
            serde_json::json!({ "attempt": attempt }),
        )
        .await?;

        let placement = self.placement_guard();
        let image = job_type
            .wrap_up
            .image
            .clone()
            .or_else(|| job_type.image.clone())
            .unwrap_or_default();
        let config = self
            .command_launch_config(
                owner,
                project,
                seq,
                &default_branch,
                &job_type,
                &job_type.wrap_up.secrets,
                image,
                run,
                ChannelRole::Work { task_id },
                timeout,
            )
            .await?;
        self.place_or_defer_launch(
            owner,
            project,
            seq,
            &mut task,
            DecidedLaunch { config, placement },
        )
        .await?;
        Ok(())
    }

    /// The `wrap_up.run` command exited (spec §3.2). Exit 0 lands the job Done;
    /// any non-zero exit (including a launch failure) escalates — the squash is
    /// already on the default branch, so the merge is never undone; only the
    /// external publish failed, and a human (or a manual `web-publish` job)
    /// finishes it.
    pub(crate) async fn on_wrapup_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task: Task,
        exit: TaskExit,
    ) -> Result<()> {
        let TaskExit {
            exit_code,
            launch_error,
            ..
        } = exit;
        self.run_wrapup(
            owner,
            project,
            seq,
            wrapup::WrapUpEvent::PublishExited {
                task: Box::new(task),
                exit_code,
                launch_error,
            },
        )
        .await
    }

    /// Terminal success (spec §2.1): branch cleanup, Done — for a batch, every
    /// member with it — and dependents unblocked. Boxed because the decider
    /// re-enters here for the terminal step of a landing or a publish, closing
    /// an async cycle through [`Core::run_wrapup`].
    pub(crate) async fn complete_done(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        Box::pin(self.run_wrapup(owner, project, seq, wrapup::WrapUpEvent::Completing)).await
    }
}

/// Augment the §4.3 conflict-context block with resolve-in-place guidance: the
/// 3-way merge is ALREADY committed on job/{seq} (Change B), so the agent must
/// resolve the markers where they sit and commit — not reimplement the change.
fn augment_conflict_context(context: &mut String, outcome: &ConflictRebaseOutcome) {
    match outcome {
        ConflictRebaseOutcome::Conflict { files } => {
            context.push_str(
                "\nThe merge with the updated base is ALREADY committed on your job \
                 branch as a WIP commit. Conflict markers \
                 (<<<<<<< / ======= / >>>>>>>) are present in these files:\n",
            );
            for f in files {
                context.push_str(&format!("  {f}\n"));
            }
            context.push_str(
                "Resolve the markers in place and commit. Do NOT reimplement your change \
                 from scratch — everything else already merged cleanly onto the new base.\n",
            );
        }
        ConflictRebaseOutcome::Clean => {
            context.push_str(
                "\nYour branch has ALREADY been rebased onto the updated base and merged \
                 cleanly (no conflict markers). Continue from the current branch state.\n",
            );
        }
    }
}

/// Byte cap on the re-review delta diff embedded in an evaluator prompt (job
/// #155). The delta is *focus*, not the authoritative source — the full diff is
/// in the evaluator's workspace — so an outsized delta is truncated with a note
/// rather than bloating the prompt.
const DELTA_DIFF_MAX_BYTES: usize = 24 * 1024;

/// Wrap a delta diff in a fenced block, truncated to [`DELTA_DIFF_MAX_BYTES`]
/// with a pointer to the workspace when it overflows.
fn fenced_delta(diff: &str) -> String {
    if diff.trim().is_empty() {
        return "(no textual delta — see your workspace)".to_string();
    }
    if diff.len() <= DELTA_DIFF_MAX_BYTES {
        return format!("```diff\n{diff}\n```");
    }
    let mut cut = DELTA_DIFF_MAX_BYTES;
    while cut > 0 && !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "```diff\n{}\n```\n_(delta truncated at {DELTA_DIFF_MAX_BYTES} bytes — run git in \
         your workspace for the full delta.)_",
        &diff[..cut]
    )
}

/// A compact per-cycle job-history digest (job #155): the same story the job
/// page tells a human skimming it — a few lines per cycle with each round's
/// verdicts, rework reasons, and the work agent's summary first line — built
/// from the persisted task log so it survives restart.
#[allow(
    clippy::too_many_lines,
    reason = "TODO(io-split): prose assembly for the §3.3 re-review context, not a decision."
)]
fn history_digest(tasks: &[types::Task], cycles: u32) -> String {
    let first_line = |s: &str| {
        let line = s
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        if line.chars().count() > 120 {
            format!("{}…", line.chars().take(120).collect::<String>())
        } else {
            line.to_string()
        }
    };
    let verdict = |t: &types::Task| -> &'static str {
        match &t.result {
            Some(TaskResult::Agent { pass, abort, .. }) => {
                if *abort {
                    "abort"
                } else if *pass {
                    "pass"
                } else {
                    "fail"
                }
            }
            Some(TaskResult::Command { pass, .. }) => {
                if *pass {
                    "pass"
                } else {
                    "fail"
                }
            }
            Some(TaskResult::Human { pass, .. }) => {
                if *pass {
                    "pass"
                } else {
                    "fail"
                }
            }
            _ => "…",
        }
    };
    let mut s = String::from("\n### Job history at a glance\n");
    for c in 1..=cycles {
        s.push_str(&format!("- **Cycle {c}**"));
        if let Some(w) = tasks
            .iter()
            .filter(|t| t.phase == TaskPhase::Work && t.cycle == c && t.evaluator.is_none())
            .max_by_key(|t| t.id)
        {
            if let Some(reason) = w.rework_reason {
                s.push_str(&format!(" [rework: {reason:?}]"));
            }
            if let Some(TaskResult::Work {
                summary: Some(sm), ..
            }) = &w.result
            {
                s.push_str(&format!(" — work: {}", first_line(sm)));
            }
        }
        let verdicts: Vec<String> = tasks
            .iter()
            .filter(|t| {
                matches!(t.phase, TaskPhase::Evaluation | TaskPhase::MergeGate)
                    && t.cycle == c
                    && t.evaluator.is_some()
                    && t.result.is_some()
            })
            .map(|t| format!("{}={}", t.evaluator.as_deref().unwrap_or("?"), verdict(t)))
            .collect();
        if !verdicts.is_empty() {
            s.push_str(&format!("\n    - reviews: {}", verdicts.join(", ")));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Unit coverage for this module's pure prompt-composition helpers (job
    //! #155): the re-review delta and the per-cycle history digest. The
    //! evaluation decisions themselves are tier-1 tested in
    //! `chuggernaut_domain::decide::eval`, and the flow end-to-end in Tier-2
    //! (`tests/execution.rs`, `tests/golden_traces.rs`).
    use super::*;

    fn eval_task(id: u64, cycle: u32, name: &str, pass: bool) -> types::Task {
        review_task(id, cycle, TaskPhase::Evaluation, Some(name), pass, None)
    }

    fn review_task(
        id: u64,
        cycle: u32,
        phase: TaskPhase,
        evaluator: Option<&str>,
        pass: bool,
        result: Option<TaskResult>,
    ) -> types::Task {
        types::Task {
            id,
            job_seq: 1,
            project: "acme/api".into(),
            phase,
            cycle,
            kind: TaskKind::Command { run: "true".into() },
            state: TaskState::Done,
            attempt: 1,
            evaluator: evaluator.map(String::from),
            label: evaluator.map(String::from),
            stage: 0,
            performed_by: None,
            container_id: None,
            pending_reason: None,
            queued_at: None,
            rework_reason: None,
            infra_loss: false,
            session_id: None,
            reviewed_tip: None,
            result: result.or(Some(TaskResult::Agent {
                pass,
                abort: false,
                structured: None,
                token_usage: None,
                cover_html: None,
            })),
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    fn work_task(id: u64, cycle: u32, summary: &str, reason: Option<ReworkReason>) -> types::Task {
        let mut t = review_task(
            id,
            cycle,
            TaskPhase::Work,
            None,
            true,
            Some(TaskResult::Work {
                summary: Some(summary.into()),
                structured: None,
                token_usage: None,
                cover_html: None,
            }),
        );
        t.rework_reason = reason;
        t
    }

    #[test]
    fn history_digest_summarizes_each_cycle() {
        let tasks = vec![
            work_task(1, 1, "first pass at the feature\nmore detail", None),
            eval_task(2, 1, "reviewer", false),
            work_task(3, 2, "addressed findings", Some(ReworkReason::EvalFailure)),
            eval_task(4, 2, "reviewer", true),
        ];
        let d = history_digest(&tasks, 2);
        assert!(d.contains("Cycle 1"), "{d}");
        assert!(d.contains("first pass at the feature"), "{d}");
        assert!(!d.contains("more detail"), "{d}");
        assert!(d.contains("reviewer=fail"), "{d}");
        assert!(d.contains("Cycle 2"), "{d}");
        assert!(d.contains("[rework: EvalFailure]"), "{d}");
        assert!(d.contains("reviewer=pass"), "{d}");
    }

    #[test]
    fn fenced_delta_wraps_and_truncates() {
        assert_eq!(
            fenced_delta("   \n"),
            "(no textual delta — see your workspace)"
        );
        let small = fenced_delta("+added line");
        assert!(small.starts_with("```diff") && small.contains("+added line"));
        assert!(!small.contains("truncated"));
        let big = "x".repeat(DELTA_DIFF_MAX_BYTES + 500);
        let out = fenced_delta(&big);
        assert!(out.contains("truncated"), "{}", &out[out.len() - 80..]);
        assert!(out.len() < big.len() + 200);
    }
}

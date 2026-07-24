//! Evaluator fan-out and reduce (spec §3.3), and post-eval finalization
//! (§3.2 step 12): squash-merge, conflict re-entry, and the merge gate.
//!
//! All finalization flows through a per-project depth-1 merge queue. The fast
//! path (default HEAD unmoved, or no commits) merges immediately; a moved HEAD
//! parks the candidate squash commit on `merge-gate/{seq}` and re-runs the
//! required command evaluators against it before promoting — nothing reaches
//! the default branch untested against the exact tree that lands.
//!
//! - **Accepts:** a job entering Evaluation; evaluator submissions; merge-gate
//!   re-runs.
//! - **Emits:** evaluator container launches, the `EvalResult` reduction,
//!   squash-merge to the default branch or conflict re-entry, and merge-gate
//!   parking on `merge-gate/{seq}`.
//! - **Guarantees:** every merge flows through a per-project depth-1 queue;
//!   nothing lands on the default branch untested against the exact tree.
//! - **Spec:** §3.3; §3.2 step 12. Runs as `impl Core` — core stays the single
//!   writer.

use crate::core::{Core, CoreError, EvalSubmission, Msg, Result, TaskExit};
use crate::decide::merge_gate::{self, group_stages};
use crate::decide::wrapup;
use crate::effects::Effect;
use crate::exec::{ChannelRole, INFRA_RELAUNCH_CAP, eval_image, task_timeout};
use crate::interpret::Outcome;
use agent::AgentRunConfig;
use chrono::Utc;
use std::collections::VecDeque;
use types::{
    EvalResult, Evaluator, EvaluatorType, Job, JobState, ReworkReason, Task, TaskKind, TaskPhase,
    TaskResult, TaskState, WorkType, WrapUpMode,
};
use vcs::{ConflictRebaseOutcome, RebaseOutcome};

/// Machine code for an evaluator that exited without ever delivering a verdict
/// (#167, narrowed #198): a Command whose container died before it could judge —
/// an ABNORMAL exit (a signal kill `>= 128`, or the negative backend-`wait`
/// sentinel) with an empty captured stream — or an Agent ending without a
/// `submit_eval` verdict. Distinct from a product failure — the code was never
/// actually judged. A *normal* non-zero command exit (1..=127) IS a verdict and
/// reworks like any product fail, even with empty output (#198). Surfaced on the
/// `task-failed`/`job-escalated` event `reason`; the retire path stamps the task
/// `infra_loss` (reusing the §3.6/#83 no-retry-burned machinery).
pub(crate) const EVAL_NO_OUTPUT_REASON: &str = "evaluator_no_output";

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

/// One cycle's evaluation, run as an ascending sequence of stages (spec §3.3
/// staged evaluation). Only the current stage has live tasks: `slots` is the
/// stage in flight, `pending` the stages not yet created, `done` the outcomes
/// of stages that already completed and passed. The reduce folds `done` and
/// `slots` together. A single-stage round leaves `pending`/`done` empty and is
/// byte-for-byte the unstaged behavior; the merge gate always builds one.
pub struct EvalRound {
    pub slots: Vec<EvalSlot>,
    /// Evaluators for stages not yet launched, grouped ascending by `stage`.
    pub pending: VecDeque<Vec<Evaluator>>,
    /// Slots from earlier stages that completed and let the round advance.
    pub done: Vec<EvalSlot>,
}

impl EvalRound {
    /// A single-stage round: the merge gate, and the compatibility shape for a
    /// job whose evaluators all share one stage.
    pub fn single(slots: Vec<EvalSlot>) -> Self {
        EvalRound {
            slots,
            pending: VecDeque::new(),
            done: Vec::new(),
        }
    }
}

/// Whether a completed stage lets the next stage start: every *required*
/// evaluator resolved to a product `pass: true`. A required product fail, an
/// abort (which implies `pass: false`), or an infra failure closes the round —
/// later stages are not created. Advisory (`required: false`) outcomes never
/// block progression.
pub(crate) fn stage_passed(slots: &[EvalSlot]) -> bool {
    slots.iter().all(|s| {
        !s.evaluator.required.unwrap_or(true)
            || matches!(s.outcome, Some(SlotOutcome::Product { pass: true, .. }))
    })
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

impl Core {
    /// Work→Evaluation (§3.2 steps 9–10): one task per evaluator, fanned out.
    /// No evaluators → auto-pass straight to finalization.
    pub(crate) async fn enter_evaluation(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        // Draining (spec §3.6): launch no evaluator containers. The job stays in
        // Work with its Done work task; restart reconciliation re-enters here.
        if self.draining {
            return Ok(());
        }
        let key = (owner.to_string(), project.to_string(), seq);
        let (evaluators, cycle) = {
            let exec = self.active.get(&key).expect("exec state");
            (exec.job_type.eval.clone(), exec.cycle)
        };
        let mut job = self.must_get(owner, project, seq)?.clone();
        // §3.2: rebase `job/{seq}` onto current default HEAD before evaluating,
        // so the evaluators test exactly the stack that would merge. `base_ref`
        // advances to what we tested against, which lets the wrap-up merge gate
        // fire only if main moves *again* during evaluation. Bookkeeping, not
        // rework: no cycle bump, no rework budget consumed (a conflict falls
        // through to old-base evaluation + the wrap-up gate).
        self.rebase_for_evaluation(owner, project, seq, &mut job)
            .await?;
        self.set_state(&mut job, JobState::Evaluation).await?;
        self.publish(
            owner,
            project,
            seq,
            "job-evaluation-started",
            serde_json::json!({ "cycle": cycle }),
        )
        .await?;

        if evaluators.is_empty() {
            return self.finalize_pass(owner, project, seq).await;
        }

        // Staged fan-out (§3.3): launch stage 0 now, hold the rest until each
        // prior stage passes. A single-stage job launches everything at once.
        let branch = job.branch.clone();
        let mut pending = group_stages(evaluators);
        let first = pending.pop_front().expect("non-empty evaluators");
        let slots = self
            .launch_eval_stage(owner, project, seq, &branch, cycle, first)
            .await?;
        self.active.get_mut(&key).expect("exec state").round = Some(EvalRound {
            slots,
            pending,
            done: Vec::new(),
        });
        Ok(())
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
        // No movement since base_ref was pinned: byte-identical to no rebase.
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
        let phase_name = format!("{phase:?}");

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
        // Agent evaluators get a transcript too — an eval that fails the job
        // is exactly the reasoning an operator wants to read back.
        let session_id = matches!(evaluator.r#type, EvaluatorType::Agent)
            .then(|| uuid::Uuid::new_v4().to_string());
        // Record the branch tip this evaluator round is judging (spec §3.3, job
        // #155): a later cycle's re-review shows the reviewer "what you reviewed"
        // and diffs `reviewed_tip..HEAD`. Best-effort — a resolve failure just
        // omits the delta, never blocks the launch.
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
            // Mirror the evaluator name into the shared label field so the UI
            // reads one label mechanism for every task kind (job #146). The
            // `evaluator` field stays populated for back-compat.
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
        self.tasks.put(&task).await?;
        self.publish(
            owner,
            project,
            seq,
            "task-created",
            serde_json::json!({
                "task_id": task_id, "phase": phase_name, "cycle": cycle,
                "attempt": attempt, "evaluator": evaluator.name, "stage": evaluator.stage,
            }),
        )
        .await?;
        if pending_human {
            return Ok(task_id); // operator inbox (§3.3 human)
        }

        // Eval containers get vars but only the evaluator's own secrets (§4.1).
        // Evaluators keep the job type's timeout — the per-job `Job.timeout`
        // override is Work-scoped only (§1.1, §3.5).
        let eval_timeout = task_timeout(&job_type);

        match evaluator.r#type {
            EvaluatorType::Command => {
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
                match self.backend.launch(config).await {
                    Ok(id) => {
                        task.container_id = Some(id.clone());
                        self.tasks.put(&task).await?;
                        self.spawn_eval_monitor(owner, project, seq, task_id, id);
                    }
                    // No free slot: queue the launch and retry when one frees,
                    // rather than failing the slot and burning eval_retries (§3.5).
                    Err(container::BackendError::NoCapacity(reason)) => {
                        self.defer_launch(owner, project, seq, &mut task, reason)
                            .await?;
                    }
                    // Any other launch failure is an infra failure of this slot
                    // (§3.3): report it through the exit fan-in so `on_eval_exited`
                    // marks the task Failed with the launch error and applies
                    // eval_retries → Infra → escalation. Without this the task
                    // stays `Running` and the job wedges in Evaluation forever
                    // (the dogfood-#1 bug).
                    Err(e) => {
                        self.report_launch_failure(owner, project, seq, task_id, e);
                    }
                }
            }
            EvaluatorType::Agent => {
                // Agent evaluators launch through the provider, whose
                // `NoCapacity` is queued (not burned as a verdict-less exit) —
                // shared with the launch-queue resume so a queued agent eval
                // relaunches identically (§3.5, #140).
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
        // This evaluator's most-recent completed review on an earlier cycle.
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
            // Evaluator added to the job between cycles: no prior review → it
            // gets the unchanged cycle-1 form.
            return Ok(None);
        };
        let (prior_pass, prior_findings) = match &prior.result {
            Some(TaskResult::Agent {
                pass, structured, ..
            }) => (*pass, structured.clone()),
            _ => return Ok(None), // command/human prior — not a re-review case
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

        // Was the branch rebased since the prior review? A conflict/gate rework
        // replays it onto a moved base, so the delta from the last-reviewed tip
        // is not meaningful. Signalled by the current cycle's work
        // `rework_reason` (persisted, restart-safe) and double-checked by an
        // ancestry test below.
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

        // What you reviewed + the delta since — or a rebase note.
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
        // Evaluators judge against the same brief the author saw.
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
        // Re-review context (spec §3.3, job #155): on cycle N > 1, if this same
        // evaluator ran on a prior cycle, prepend its prior verdict/findings, the
        // SHA it reviewed, the delta since, and a compact job-history digest — so
        // it focuses on what changed rather than re-deriving the whole review.
        let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
        if let Some(block) = self
            .prior_review_block(owner, project, seq, branch, evaluator, cycle)
            .await?
        {
            prompt.push_str(&block);
        }
        // #168: a relaunched evaluator (a prior attempt in this same round died —
        // #167 no-output invalid fail, container loss, crash) leads with the
        // predecessor's partial output, so it doesn't re-review blind. Distinct
        // from the cross-cycle re-review above (#155): this is same-round
        // attempt-to-attempt continuity. Evaluators push no commits.
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
                // Fleet at capacity: queue this launch behind the freed-slot
                // signal rather than reporting a verdict-less exit that would
                // exhaust eval_retries in milliseconds (#140).
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
        // abort implies fail — the verdicts are pass | fail | abort; a
        // contradictory pass+abort submission normalizes to abort.
        task.result = Some(TaskResult::Agent {
            pass: submission.pass && !submission.abort,
            abort: submission.abort,
            structured: submission.structured,
            token_usage: submission.token_usage,
            cover_html: submission.cover_html,
        });
        task.state = TaskState::Done;
        task.completed_at = Some(Utc::now());
        self.tasks.put(&task).await?;
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

    /// Human evaluator resolved via the inbox: record the verdict on the slot
    /// and reduce if the round is complete. Called from the resolve handler.
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
        let key = (owner.to_string(), project.to_string(), seq);
        let Some(slot_idx) = self
            .active
            .get(&key)
            .and_then(|e| e.round.as_ref())
            .and_then(|r| {
                r.slots
                    .iter()
                    .position(|s| s.task_id == task_id && s.outcome.is_none())
            })
        else {
            return Err(CoreError::InvalidResolution(format!(
                "task {task_id} is not an open evaluator slot"
            )));
        };
        let round = self.active.get_mut(&key).unwrap().round.as_mut().unwrap();
        round.slots[slot_idx].outcome = Some(SlotOutcome::Product {
            pass,
            abort,
            structured,
            output: None, // agents report through structured findings
        });
        self.stage_complete(owner, project, seq).await
    }

    /// Eval container exited. The verdict source depends on the type: command
    /// exit code is the verdict; an agent exit without a prior `submit_eval`
    /// is an infra error retried per `eval_retries` (§3.3).
    pub(crate) async fn on_eval_exited(
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
            usage,
            launch_error,
            log_tail,
            ..
        } = exit;
        let key = (owner.to_string(), project.to_string(), seq);
        let Some(slot_idx) = self
            .active
            .get(&key)
            .and_then(|e| e.round.as_ref())
            .and_then(|r| {
                r.slots
                    .iter()
                    .position(|s| s.task_id == task.id && s.outcome.is_none())
            })
        else {
            return Ok(()); // stale monitor from a superseded round, or duplicate exit
        };

        // The container never launched: an infra failure regardless of the
        // evaluator's type (a Command exit code or an Agent verdict would need a
        // container that ran). Record why on the task, then route through the
        // same eval_retries → Infra path an agent's missing verdict uses.
        if let Some(reason) = launch_error {
            task.result = Some(TaskResult::Command {
                pass: false,
                exit_code,
                output: reason,
                structured: None,
            });
            task.state = TaskState::Failed;
            task.completed_at = Some(Utc::now());
            self.tasks.put(&task).await?;
            self.publish(
                owner,
                project,
                seq,
                "task-failed",
                serde_json::json!({
                    "task_id": task.id, "phase": "Evaluation", "reason": "container launch failed",
                }),
            )
            .await?;
            return self
                .eval_infra_failure(owner, project, seq, task, slot_idx)
                .await;
        }

        let outcome = match &task.kind {
            TaskKind::Command { .. } => {
                let pass = exit_code == 0;
                // #167: embed the captured output tail (last ~8 KB, harvested by
                // the eval monitor) as the result's evidence — the failure reason
                // for the job page, rework context, and #155's re-review. The full
                // stream stays in the logs.
                let output = log_tail.clone().unwrap_or_default();
                // #167/#198: a command's EXIT CODE is its verdict. A normal
                // non-zero exit (1..=127) is a real product failure the job must
                // rework against — even with a completely empty captured stream (a
                // legitimately silent failure, e.g. `test -f x || exit 1`). #167's
                // original guard mislabelled every empty-output fail as evidence-
                // free and auto-retried it, silently discarding real failure
                // verdicts (job #198). The evidence-free infra-loss case #167
                // actually targets — a container that died before it could judge
                // (an OOM/timeout signal kill, or a backend `wait` that never
                // resolved) — surfaces as an ABNORMAL exit: a signal code (>= 128)
                // or the negative wait sentinel. Only such a verdict-less exit with
                // no output is retried via the infra-loss machinery (no
                // `eval_retries` burned, no rework, no cycle consumed); on
                // exhaustion escalate `evaluator_no_output` so a human sees "the
                // evaluator can't produce evidence" rather than "the code failed
                // review". A normal program exit is 0..=127; anything outside that
                // range (a signal kill >= 128, or the negative sentinel) is the
                // verdict-less case.
                let verdict_less = !(0..128).contains(&exit_code);
                if verdict_less && output.trim().is_empty() {
                    task.result = Some(TaskResult::Command {
                        pass: false,
                        exit_code,
                        output,
                        structured: eval_json.clone(),
                    });
                    task.state = TaskState::Failed;
                    task.infra_loss = true;
                    task.completed_at = Some(Utc::now());
                    self.tasks.put(&task).await?;
                    self.publish(
                        owner,
                        project,
                        seq,
                        "task-failed",
                        serde_json::json!({
                            "task_id": task.id, "phase": "Evaluation",
                            "reason": EVAL_NO_OUTPUT_REASON,
                        }),
                    )
                    .await?;
                    return self
                        .eval_no_output_failure(owner, project, seq, task, slot_idx)
                        .await;
                }
                // The captured tail rides along as the failure evidence for the
                // rework/re-review context (#167) — a non-empty output on a fail
                // is what the next work agent reads to see WHY ci failed.
                let slot_output = (!output.is_empty()).then(|| output.clone());
                task.result = Some(TaskResult::Command {
                    pass,
                    exit_code,
                    output,
                    structured: eval_json.clone(),
                });
                task.state = TaskState::Done;
                task.completed_at = Some(Utc::now());
                self.tasks.put(&task).await?;
                self.publish(
                    owner,
                    project,
                    seq,
                    "task-completed",
                    serde_json::json!({
                        "task_id": task.id, "phase": "Evaluation", "pass": pass,
                    }),
                )
                .await?;
                // Command evaluators can't judge fixability: no abort verdict.
                Some(SlotOutcome::Product {
                    pass,
                    abort: false,
                    structured: eval_json,
                    output: slot_output,
                })
            }
            TaskKind::Agent { .. } => {
                // handle_submit_eval marks the task Done before the container
                // exits; the record we were passed is the pre-exit snapshot.
                let mut current = self
                    .tasks
                    .get(owner, project, seq, task.id)
                    .await?
                    .unwrap_or(task);
                // submit_eval could only self-report usage. Now that the
                // container is gone we have the CLI's measured figure — prefer
                // it, the same way the work path does.
                if let (Some(measured), Some(TaskResult::Agent { token_usage, .. })) =
                    (usage, current.result.as_mut())
                {
                    *token_usage = Some(measured);
                    self.tasks.put(&current).await?;
                }
                match &current.result {
                    Some(TaskResult::Agent {
                        pass,
                        abort,
                        structured,
                        ..
                    }) => Some(SlotOutcome::Product {
                        pass: *pass,
                        abort: *abort,
                        structured: structured.clone(),
                        output: None, // agents report through structured findings
                    }),
                    _ => {
                        // #167: an agent evaluator that ended without a
                        // `submit_eval` verdict produced no evidence — the same
                        // invalid-fail class as a Command with an empty stream.
                        // Route it through the no-output path (infra-loss
                        // semantics: no `eval_retries` burned, escalates
                        // `evaluator_no_output`) rather than a plain infra retry,
                        // so the reason distinguishes "no verdict" from a real
                        // infra loss and the round is never failed on nothing.
                        let mut failed = current;
                        failed.state = TaskState::Failed;
                        failed.infra_loss = true;
                        failed.completed_at = Some(Utc::now());
                        self.tasks.put(&failed).await?;
                        self.publish(
                            owner,
                            project,
                            seq,
                            "task-failed",
                            serde_json::json!({
                                "task_id": failed.id, "phase": "Evaluation",
                                "reason": EVAL_NO_OUTPUT_REASON,
                            }),
                        )
                        .await?;
                        return self
                            .eval_no_output_failure(owner, project, seq, failed, slot_idx)
                            .await;
                    }
                }
            }
            TaskKind::Human { .. } => None, // resolved via the inbox, not an exit
        };

        if let Some(outcome) = outcome {
            let round = self.active.get_mut(&key).unwrap().round.as_mut().unwrap();
            round.slots[slot_idx].outcome = Some(outcome);
            return self.stage_complete(owner, project, seq).await;
        }
        Ok(())
    }

    /// An eval slot failed for infra reasons — the agent produced no verdict, or
    /// its container never launched (§3.3). Retry per `eval_retries`; once the
    /// budget is spent, resolve the slot as [`SlotOutcome::Infra`] and run the
    /// reduce (a required infra failure escalates). The failed task is already
    /// persisted terminal by the caller.
    async fn eval_infra_failure(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        failed: Task,
        slot_idx: usize,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let eval_retries = self
            .active
            .get(&key)
            .and_then(|e| e.job_type.eval_retries)
            .unwrap_or(1);
        if failed.attempt <= eval_retries {
            let (evaluator, cycle) = {
                let exec = self.active.get(&key).expect("exec state");
                let slot = &exec.round.as_ref().unwrap().slots[slot_idx];
                (slot.evaluator.clone(), exec.cycle)
            };
            let branch = self.must_get(owner, project, seq)?.branch.clone();
            let new_id = self
                .launch_evaluator_task(
                    owner,
                    project,
                    seq,
                    TaskPhase::Evaluation,
                    &branch,
                    cycle,
                    &evaluator,
                    failed.attempt + 1,
                )
                .await?;
            let round = self.active.get_mut(&key).unwrap().round.as_mut().unwrap();
            round.slots[slot_idx].task_id = new_id;
            round.slots[slot_idx].attempt = failed.attempt + 1;
            return Ok(());
        }
        let round = self.active.get_mut(&key).unwrap().round.as_mut().unwrap();
        round.slots[slot_idx].outcome = Some(SlotOutcome::Infra);
        self.stage_complete(owner, project, seq).await
    }

    /// #167 (narrowed #198): an evaluator exited without ever delivering a
    /// verdict — a Command whose container died before judging (an abnormal
    /// signal/sentinel exit) with an empty captured stream, or an Agent ending
    /// without a `submit_eval` verdict. This is infrastructure loss, not a
    /// product verdict:
    /// relaunch the SAME attempt WITHOUT spending an `eval_retries` budget (the
    /// §3.6/#83 infra-loss semantics — no rework, no cycle consumed), bounded by
    /// [`INFRA_RELAUNCH_CAP`] over the evaluator's lineage. On exhaustion escalate
    /// with reason `evaluator_no_output`, so a human sees the evaluator cannot
    /// produce evidence rather than the round failing on nothing. The failed task
    /// is already persisted terminal (stamped `infra_loss`) by the caller.
    async fn eval_no_output_failure(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        failed: Task,
        slot_idx: usize,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        // Count evidence-free losses for this evaluator's lineage (same cycle +
        // evaluator): the freshly-stamped attempt is included, so the Nth loss
        // sees count N. Shares the `infra_loss` marker and cap with §3.6 restart
        // losses — both are infrastructure, both escalate to a human.
        let losses = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .iter()
            .filter(|t| {
                t.infra_loss
                    && t.phase == TaskPhase::Evaluation
                    && t.cycle == failed.cycle
                    && t.evaluator == failed.evaluator
            })
            .count();
        if losses > INFRA_RELAUNCH_CAP as usize {
            self.active.remove(&key);
            return self
                .escalate(
                    owner,
                    project,
                    seq,
                    EVAL_NO_OUTPUT_REASON,
                    format!(
                        "Job {seq}: evaluator '{}' exited without producing any output \
                         {losses} times — it cannot produce evidence of a verdict. A human \
                         should review the evaluator itself rather than the code.",
                        failed.evaluator.as_deref().unwrap_or("?")
                    ),
                    Some(failed.id),
                )
                .await;
        }
        let (evaluator, cycle) = {
            let exec = self.active.get(&key).expect("exec state");
            let slot = &exec.round.as_ref().unwrap().slots[slot_idx];
            (slot.evaluator.clone(), exec.cycle)
        };
        let branch = self.must_get(owner, project, seq)?.branch.clone();
        // Same attempt: `eval_retries` untouched (infra loss, not a real failure).
        let new_id = self
            .launch_evaluator_task(
                owner,
                project,
                seq,
                TaskPhase::Evaluation,
                &branch,
                cycle,
                &evaluator,
                failed.attempt,
            )
            .await?;
        let round = self.active.get_mut(&key).unwrap().round.as_mut().unwrap();
        round.slots[slot_idx].task_id = new_id;
        Ok(())
    }

    /// Called whenever a slot in the current stage resolves. A no-op while the
    /// stage is still in flight. Once every slot in the stage is terminal:
    /// advance to the next stage when every *required* evaluator passed and a
    /// later stage remains (§3.3 staged evaluation); otherwise run the reduce
    /// over every stage that ran. A short-circuited stage leaves the pending
    /// stages uncreated — they simply have no task records for this cycle.
    pub(crate) async fn stage_complete(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        // Draining (spec §3.6): don't advance to the next stage, run the reduce,
        // or open the merge gate. Restart reconciliation rebuilds the round from
        // the task log and replays this decision.
        if self.draining {
            return Ok(());
        }
        let key = (owner.to_string(), project.to_string(), seq);
        let (complete, advance) = {
            let Some(round) = self.active.get(&key).and_then(|e| e.round.as_ref()) else {
                return Ok(());
            };
            let complete = round.slots.iter().all(|s| s.outcome.is_some());
            (
                complete,
                complete && stage_passed(&round.slots) && !round.pending.is_empty(),
            )
        };
        if !complete {
            return Ok(());
        }
        if !advance {
            return self.reduce(owner, project, seq).await;
        }
        // Fold the finished stage into `done` and fan out the next one.
        let branch = self.must_get(owner, project, seq)?.branch.clone();
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let next = self
            .active
            .get_mut(&key)
            .unwrap()
            .round
            .as_mut()
            .unwrap()
            .pending
            .pop_front()
            .expect("advance implies a pending stage");
        let new_slots = self
            .launch_eval_stage(owner, project, seq, &branch, cycle, next)
            .await?;
        let round = self.active.get_mut(&key).unwrap().round.as_mut().unwrap();
        let finished = std::mem::replace(&mut round.slots, new_slots);
        round.done.extend(finished);
        Ok(())
    }

    /// §3.3 reduce, applied once all eval tasks resolved.
    async fn reduce(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let (
            results,
            required_infra_failure,
            overall_pass,
            aborted,
            cycle,
            reworks_used,
            work_type,
            budget,
        ) = {
            let exec = self.active.get(&key).expect("exec state");
            let round = exec.round.as_ref().expect("round");
            let mut results = Vec::new();
            let mut infra = false;
            let mut pass = true;
            // Required evaluators that declared the work unsalvageable
            // (design-lifecycle.md abort verdict). Advisory aborts are plain
            // advisory fails.
            let mut aborted: Vec<String> = Vec::new();
            // Every stage that ran: earlier stages that passed (`done`) plus the
            // final stage (`slots`). Stages that were never created — skipped by
            // a short-circuit — contribute nothing, exactly as intended.
            for slot in round.done.iter().chain(round.slots.iter()) {
                let required = slot.evaluator.required.unwrap_or(true);
                match slot.outcome.as_ref().expect("complete") {
                    SlotOutcome::Product {
                        pass: p,
                        abort,
                        structured,
                        output,
                    } => {
                        results.push(EvalResult {
                            evaluator: slot.evaluator.name.clone(),
                            pass: *p,
                            structured: structured.clone(),
                            output: output.clone(),
                        });
                        if required && !*p {
                            pass = false;
                        }
                        if required && *abort {
                            aborted.push(slot.evaluator.name.clone());
                        }
                    }
                    SlotOutcome::Infra => {
                        results.push(EvalResult {
                            evaluator: slot.evaluator.name.clone(),
                            pass: false,
                            structured: None,
                            output: None,
                        });
                        if required {
                            infra = true;
                        }
                    }
                }
            }
            (
                results,
                infra,
                pass,
                aborted,
                exec.cycle,
                exec.reworks_used,
                exec.job_type.work.r#type,
                exec.job_type.rework_budget.unwrap_or(0),
            )
        };

        if required_infra_failure {
            self.active.remove(&key);
            return self
                .escalate(
                    owner,
                    project,
                    seq,
                    "eval_infra_failure",
                    format!("Job {seq}: a required evaluator exhausted eval_retries"),
                    None,
                )
                .await;
        }
        if overall_pass {
            return self.finalize_pass(owner, project, seq).await;
        }

        // Abort verdict: rework can't fix this — skip the remaining budget and
        // hand the evaluators' findings to a human (design-lifecycle.md).
        if !aborted.is_empty() {
            let findings = results
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
                .join("\n\n");
            self.active.remove(&key);
            return self.escalate(owner, project, seq, "eval_abort",
                format!(
                    "Job {seq}: evaluator(s) {} declared cycle {cycle} not satisfiable by rework:\n\n{findings}",
                    aborted.join(", ")
                ), None)
                .await;
        }

        // Product failure: rework under budget, else escalate (§3.3).
        if work_type != WorkType::Command && reworks_used < budget {
            // enter_work preserves reworks_used from the existing state.
            self.active.get_mut(&key).unwrap().reworks_used = reworks_used + 1;
            self.publish(
                owner,
                project,
                seq,
                "job-rework-started",
                serde_json::json!({
                    "cycle": cycle + 1, "reason": "eval_failure", "eval_context": results,
                }),
            )
            .await?;
            self.enter_work(
                owner,
                project,
                seq,
                cycle + 1,
                results,
                None,
                Some(ReworkReason::EvalFailure),
            )
            .await
        } else {
            self.active.remove(&key);
            self.escalate(
                owner,
                project,
                seq,
                "rework_budget_exhausted",
                format!("Job {seq}: evaluation failed in cycle {cycle} with no rework budget left"),
                None,
            )
            .await
        }
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
        // `finalize: none` is a view input (design-lifecycle.md): the work's
        // effect is external and the branch is scratch.
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
            // Eval-pass IS the wrap-up; complete_done is the platform
            // bookkeeping every job gets.
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
            // Draining (spec §3.6): no gate starts, no landing. An Open
            // origin release holds the queue the same way (the post-merge
            // integration reset is lossless because of exactly this hold).
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
        // Was this landing's current cycle a gate-fix round (job #154)? Read
        // from the persisted task log so it holds across a restart.
        let tasks = self.tasks.list_for_job(owner, project, seq).await?;
        let force_gate = merge_gate::force_gate(&tasks, cycle);
        let mut summary = self
            .active
            .get(&key)
            .and_then(|e| e.work_submission.as_ref())
            .and_then(|s| s.summary.clone());
        // Audit trail (job #154): note the gate-fix round in the squash body so
        // the landed commit records that a mechanical compile fix was applied
        // after review, not that the branch was re-reviewed.
        if force_gate {
            let note = "Includes a gate-fix round (job #154): a compile-only merge-gate \
                        failure was repaired by a scoped fix task and re-gated, without \
                        re-review.";
            summary = Some(match summary {
                Some(prose) if !prose.is_empty() => format!("{prose}\n\n{note}"),
                _ => note.to_string(),
            });
        }
        // A batch lands as one squash that completes every member, so open the
        // commit body with the member list — otherwise git history records only
        // `job/{batchseq}: {type}` with no trace of which tickets it closed
        // (spec §2.1 batches; mirrors the create_batch auto-index).
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
        // Fold-local carry between rounds: the conflict files feeding the
        // context composition, and the candidate commit a gate round parks
        // (both arrive one round before the decision that consumes them).
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
            // An escalation out of the landing releases the exec slice BEFORE
            // its Escalate effect runs (parity with the pre-C2 order: the
            // escalation task's cycle must not read the dropped exec state).
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
                        // Compose the rework brief: the conflict context read
                        // plus the rebase outcome folded in (§3.2 step 12).
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
                        // Park the gate round: the candidate commit from this
                        // round, the head it was built against, and the stages
                        // beyond the launched first (same pure grouping the
                        // decider used).
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
            return Ok(()); // stale monitor
        };

        let pass = exit_code == 0;
        task.result = Some(TaskResult::Command {
            pass,
            exit_code,
            // The captured container output (compiler errors for a failed build
            // stage) is the record of why the gate failed — a compile-class
            // failure threads it into the gate-fix brief (job #154). A launch
            // failure has no container output, so its reason stands in instead.
            output: log_tail.clone().or(launch_error).unwrap_or_default(),
            structured: eval_json.clone(),
        });
        task.state = TaskState::Done;
        task.completed_at = Some(Utc::now());
        self.tasks.put(&task).await?;
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
            // The gate threads its captured compiler output into the gate-fix
            // brief from the task record directly (job #154), not via the slot.
            output: None,
        });
        if gate.round.slots.iter().any(|s| s.outcome.is_none()) {
            return Ok(());
        }
        // The current gate stage completed. If it passed and a later stage is
        // queued, launch it and keep waiting (job #154 staged gate). Only reduce
        // when a stage fails, or the last stage passes.
        if stage_passed(&gate.round.slots) && !gate.round.pending.is_empty() {
            let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
            let gate_branch = format!("merge-gate/{seq}");
            // Retire the passed stage into `done` and pull the next stage.
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
        // Same triage rule as pump_merges: a hard error in gate resolution
        // (promote, rework re-entry) escalates rather than wedging the queue.
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
        // The classification input (job #154): the first stage failing while a
        // distinct later stage was queued is the compile class. The decider
        // owns the classification itself.
        let first_stage_failed = gate.round.done.is_empty() && !gate.round.pending.is_empty();
        // The failed build stage's captured compiler output — the gate-fix
        // brief's evidence; only a first-stage failure can need it.
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
        // Scoped brief: the gate-fix framing, the exact compiler errors the gate
        // build stage emitted (job #154 requirement), then the rebase/conflict
        // context. Embedding the captured output means the agent sees the errors
        // without having to reproduce the build first.
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
        // Count this round against the gate-fix budget (in-memory; enter_work
        // preserves it, and it is rebuilt from the task log on restart).
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
        // 1. Reads feed the view — they are not effects.
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
        // 2. The decision, made purely.
        let (transitions, effects, step) = wrapup::decide(&view, event);
        // 3. Commit the decision: transitions first (§2.1 record is the source
        // of truth; the publish task and the announcements are its artifacts,
        // re-derived by restart reconciliation if a crash loses them).
        for mut t in transitions {
            self.set_state(&mut t.job, t.to).await?;
        }
        // The exec slice is released BEFORE the effects run (parity with the
        // pre-C3 order, and with C2's `CompletedDropExec`): neither the
        // escalation task's cycle nor the terminal announcement may read a
        // slice the decision just ended.
        if step.drops_exec() {
            self.active.remove(&key);
        }
        // 4. The artifacts of the decision.
        for effect in effects {
            self.interpret(effect).await?;
        }
        // 5. The bookkeeping the step names.
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
    pub(crate) async fn launch_wrapup_task(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        attempt: u32,
    ) -> Result<()> {
        // Draining (spec §3.6): launch no wrap-up publish. The squash has already
        // landed; restart reconciliation (recover_wrapup_command) relaunches it —
        // the command is idempotent by contract (§3.2).
        if self.draining {
            return Ok(());
        }
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        let job_type = self.active.get(&key).expect("exec state").job_type.clone();
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let run = job_type.wrap_up.run.clone().unwrap_or_default();
        // The publish ships merged main, so it runs against the default branch,
        // not the (now-landed) scratch job branch.
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
            // The wrap-up task carries its configured/derived label so it renders
            // as `Command · publish`, not a bare `Command` (job #146).
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
        self.tasks.put(&task).await?;
        self.publish(
            owner,
            project,
            seq,
            "task-created",
            serde_json::json!({
                "task_id": task_id, "phase": "WrapUp", "cycle": cycle, "attempt": attempt,
            }),
        )
        .await?;

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
        match self.backend.launch(config).await {
            Ok(id) => {
                task.container_id = Some(id.clone());
                self.tasks.put(&task).await?;
                self.spawn_logs_monitor(owner, project, seq, task_id, id);
            }
            // No free slot: queue the publish and retry when one frees (§3.5).
            Err(container::BackendError::NoCapacity(reason)) => {
                self.defer_launch(owner, project, seq, &mut task, reason)
                    .await?;
            }
            // Any other launch failure surfaces through the exit fan-in like
            // every other task (§3.2): `on_wrapup_exited` records it and escalates.
            Err(e) => {
                self.report_launch_failure(owner, project, seq, task_id, e);
            }
        }
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
    //! Unit coverage for the staged-evaluation decision core (spec §3.3): how
    //! evaluators partition into stages, and whether a completed stage lets the
    //! next one start. These are the pure fragments of the reduce path; the
    //! stateful reduce/advance flow is exercised end-to-end in Tier-2
    //! (`tests/execution.rs`).
    use super::*;
    use types::EvaluatorType;

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

    fn slot(name: &str, stage: u32, required: Option<bool>, outcome: SlotOutcome) -> EvalSlot {
        EvalSlot {
            evaluator: evaluator(name, stage, required),
            task_id: 0,
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
            slot("a", 0, None, product(true, false)),
            slot("b", 0, Some(true), product(true, false)),
        ];
        assert!(stage_passed(&slots));
    }

    #[test]
    fn stage_passed_required_fail_blocks() {
        let slots = vec![
            slot("a", 0, None, product(true, false)),
            slot("b", 0, None, product(false, false)),
        ];
        assert!(!stage_passed(&slots));
    }

    #[test]
    fn stage_passed_required_abort_blocks() {
        let slots = vec![slot("a", 0, None, product(false, true))];
        assert!(!stage_passed(&slots));
    }

    #[test]
    fn stage_passed_required_infra_blocks() {
        let slots = vec![slot("a", 0, None, SlotOutcome::Infra)];
        assert!(!stage_passed(&slots));
    }

    #[test]
    fn stage_passed_advisory_failures_never_block() {
        // Advisory fail, advisory abort, advisory infra — none stop the next
        // stage from starting.
        let slots = vec![
            slot("pass", 0, None, product(true, false)),
            slot("adv-fail", 0, Some(false), product(false, false)),
            slot("adv-abort", 0, Some(false), product(false, true)),
            slot("adv-infra", 0, Some(false), SlotOutcome::Infra),
        ];
        assert!(stage_passed(&slots));
    }

    // ── re-review context helpers (job #155) ────────────────────────────────

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
        // Only the first non-empty line of the work summary is kept.
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
        // Over the cap → truncated with a workspace pointer, and never split a
        // char boundary (the cut is byte-safe).
        let big = "x".repeat(DELTA_DIFF_MAX_BYTES + 500);
        let out = fenced_delta(&big);
        assert!(out.contains("truncated"), "{}", &out[out.len() - 80..]);
        assert!(out.len() < big.len() + 200);
    }
}

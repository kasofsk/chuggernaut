//! Evaluator fan-out and reduce (spec §3.3), and post-eval finalization
//! (§3.2 step 12): squash-merge, conflict re-entry, and the merge gate.
//!
//! All finalization flows through a per-project depth-1 merge queue. The fast
//! path (default HEAD unmoved, or no commits) merges immediately; a moved HEAD
//! parks the candidate squash commit on `merge-gate/{seq}` and re-runs the
//! required command evaluators against it before promoting — nothing reaches
//! the default branch untested against the exact tree that lands.

use crate::core::{Core, CoreError, EvalSubmission, Msg, Result, TaskExit};
use crate::exec::{ChannelRole, eval_image, task_timeout};
use agent::AgentRunConfig;
use chrono::Utc;
use container::{ContainerLaunchConfig, bootstrap_cmd};
use std::collections::VecDeque;
use types::{
    EvalResult, Evaluator, EvaluatorType, JobState, Task, TaskKind, TaskPhase, TaskResult,
    TaskState, WorkType, WrapUpMode,
};
use vcs::MergeOutcome;

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

/// Partition evaluators into stages in ascending `stage` order, preserving the
/// declared order within a stage (stable sort). One distinct stage → one group,
/// which is exactly today's single fan-out.
pub(crate) fn group_stages(mut evaluators: Vec<Evaluator>) -> VecDeque<Vec<Evaluator>> {
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

pub struct EvalSlot {
    pub evaluator: Evaluator,
    pub task_id: u64,
    pub attempt: u32,
    pub outcome: Option<SlotOutcome>,
}

#[derive(Clone)]
pub enum SlotOutcome {
    Product {
        pass: bool,
        /// "Not satisfiable by rework" (design-lifecycle.md): a required
        /// evaluator's abort escalates at reduce instead of consuming budget.
        abort: bool,
        structured: Option<serde_json::Value>,
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

enum FinalizeStep {
    /// Job reached Done or re-entered Work — the queue can advance.
    Completed,
    /// Gate tasks are running; the queue holds until they resolve.
    Gating,
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
        let key = (owner.to_string(), project.to_string(), seq);
        let (evaluators, cycle) = {
            let exec = self.active.get(&key).expect("exec state");
            (exec.job_type.eval.clone(), exec.cycle)
        };
        let mut job = self.must_get(owner, project, seq)?.clone();
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
        let base_ref = job.base_ref.clone().expect("base_ref set");
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
                        crate::exec::job_brief_block(&job)
                    ),
                },
                true,
            ),
        };
        // Agent evaluators get a transcript too — an eval that fails the job
        // is exactly the reasoning an operator wants to read back.
        let session_id = matches!(evaluator.r#type, EvaluatorType::Agent)
            .then(|| uuid::Uuid::new_v4().to_string());
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
            stage: evaluator.stage,
            performed_by: None,
            container_id: None,
            session_id: session_id.clone(),
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
        let env = self
            .container_env(
                owner,
                project,
                seq,
                branch,
                &job_type,
                &evaluator.secrets,
                ChannelRole::Eval { task_id },
                eval_timeout,
            )
            .await?;
        let tx = self.self_tx.clone().expect("spawned core");
        let (o, p) = (owner.to_string(), project.to_string());

        match evaluator.r#type {
            EvaluatorType::Command => {
                let run = evaluator.run.clone().unwrap_or_default();
                let launch = ContainerLaunchConfig {
                    image: eval_image(&job_type, evaluator),
                    cmd: bootstrap_cmd(&["sh".into(), "-c".into(), run]),
                    env,
                    files: self
                        .ssh_credential_files(
                            owner,
                            project,
                            seq,
                            ChannelRole::Eval { task_id },
                            eval_timeout,
                        )
                        .await?,
                    cpu_limit: job_type.resources.as_ref().and_then(|r| r.cpu),
                    memory_limit: job_type.resources.as_ref().and_then(|r| r.memory.clone()),
                };
                let id = self
                    .backend
                    .launch(launch)
                    .await
                    .map_err(|e| CoreError::NotFound(format!("launch failed: {e}")))?;
                task.container_id = Some(id.clone());
                self.tasks.put(&task).await?;
                let backend = self.backend.clone();
                let harvest = self.harvester();
                tokio::spawn(async move {
                    let exit_code = backend.wait(&id).await.unwrap_or(-1);
                    // §3.3: extract structured findings after exit.
                    let eval_json = backend
                        .copy_file(&id, "/workspace/eval-result.json")
                        .await
                        .ok()
                        .flatten()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    harvest.collect_logs(&o, &p, seq, task_id, &id).await;
                    // eval-result.json and logs are out — reclaim the overlay.
                    harvest.dispose(seq, task_id, &id).await;
                    let _ = tx
                        .send(Msg::TaskExited {
                            owner: o,
                            project: p,
                            seq,
                            task_id,
                            exit: TaskExit {
                                exit_code,
                                eval_json,
                                usage: None,
                                assessment: None,
                            },
                        })
                        .await;
                });
            }
            EvaluatorType::Agent => {
                let mut env = env;
                self.inject_platform_agent_secrets(&mut env).await?;
                // Evaluators judge against the same brief the author saw.
                let prompt = format!(
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
                    crate::exec::job_brief_block(&job)
                );
                let (mcp_servers, mut files) = self.channel_mcp(&env);
                files.extend(
                    self.ssh_credential_files(
                        owner,
                        project,
                        seq,
                        ChannelRole::Eval { task_id },
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
                    session_id: session_id.clone().unwrap_or_default(),
                };
                let provider = self.provider.clone();
                let harvest = self.harvester();
                tokio::spawn(async move {
                    let (exit_code, usage) = match provider.run(config).await {
                        Ok(out) => {
                            let usage = harvest.collect(&o, &p, seq, task_id, &out).await;
                            if let Some(id) = &out.container_id {
                                harvest.dispose(seq, task_id, id).await;
                            }
                            (out.exit_code, usage)
                        }
                        Err(e) => {
                            tracing::error!("eval agent run failed: {e}");
                            (-1, None)
                        }
                    };
                    let _ = tx
                        .send(Msg::TaskExited {
                            owner: o,
                            project: p,
                            seq,
                            task_id,
                            exit: TaskExit {
                                exit_code,
                                eval_json: None,
                                usage,
                                assessment: None,
                            },
                        })
                        .await;
                });
            }
            EvaluatorType::Human => unreachable!(),
        }
        Ok(task_id)
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

        let outcome = match &task.kind {
            TaskKind::Command { .. } => {
                let pass = exit_code == 0;
                task.result = Some(TaskResult::Command {
                    pass,
                    exit_code,
                    output: String::new(), // log capture: backend slice
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
                    }),
                    _ => {
                        // Infra error: no verdict recorded (§3.3).
                        let mut failed = current;
                        failed.state = TaskState::Failed;
                        failed.completed_at = Some(Utc::now());
                        self.tasks.put(&failed).await?;
                        self.publish(owner, project, seq, "task-failed", serde_json::json!({
                            "task_id": failed.id, "phase": "Evaluation", "reason": "no submit_eval",
                        }))
                        .await?;
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
                        Some(SlotOutcome::Infra)
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
                    } => {
                        results.push(EvalResult {
                            evaluator: slot.evaluator.name.clone(),
                            pass: *p,
                            structured: structured.clone(),
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
                ))
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
            self.enter_work(owner, project, seq, cycle + 1, results, None)
                .await
        } else {
            self.active.remove(&key);
            self.escalate(
                owner,
                project,
                seq,
                "rework_budget_exhausted",
                format!("Job {seq}: evaluation failed in cycle {cycle} with no rework budget left"),
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

    async fn finalize_pass(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        // `finalize: none` (design-lifecycle.md): nothing to land — the work's
        // effect is external, the branch is scratch. Eval-pass IS the wrap-up;
        // complete_done is the platform bookkeeping every job gets.
        let wrap_up = self
            .active
            .get(&(owner.to_string(), project.to_string(), seq))
            .map(|e| e.job_type.wrap_up.r#type)
            .unwrap_or_default();
        if wrap_up == WrapUpMode::None {
            // Nothing to land: eval-pass IS the wrap-up (Evaluation→Done).
            return self.complete_done(owner, project, seq).await;
        }
        // Eval passed; the job is now landing. Enter WrapUp (§2.1, §3.3) — the
        // merge queue, gate, and squash run in this state, not Evaluation.
        // refinalize (reconcile) re-enters while already WrapUp; skip the write.
        let mut job = self.must_get(owner, project, seq)?.clone();
        if job.state == JobState::Evaluation {
            self.set_state(&mut job, JobState::WrapUp).await?;
            self.publish(
                owner,
                project,
                seq,
                "job-wrapup-started",
                serde_json::json!({}),
            )
            .await?;
        }
        let slug = format!("{owner}/{project}");
        let q = self.merge_queue.entry(slug).or_default();
        if !q.contains(&seq) {
            q.push_back(seq);
        }
        self.pump_merges(owner, project).await
    }

    /// Advance the merge queue until it empties or a gate starts. Wrap-up is
    /// designed to be infallible; when a finalization step fails anyway (git
    /// plumbing, repo IO — not a Conflict, which has its own rework path), the
    /// job escalates and the queue moves on instead of wedging
    /// (design-lifecycle.md: unexpected wrap-up failure → triage).
    pub(crate) async fn pump_merges(&mut self, owner: &str, project: &str) -> Result<()> {
        let slug = format!("{owner}/{project}");
        // An Open origin release holds the queue: nothing lands on integration
        // until the release PR resolves (jobs still eval and enqueue). The
        // post-merge reset is lossless because of exactly this hold.
        if self.release_holds.contains(&slug) {
            return Ok(());
        }
        while !self.gating.contains_key(&slug) {
            let Some(&seq) = self.merge_queue.get(&slug).and_then(|q| q.front()) else {
                return Ok(());
            };
            self.merge_queue.get_mut(&slug).unwrap().pop_front();
            match self.try_finalize(owner, project, seq).await {
                Ok(FinalizeStep::Completed) => continue,
                Ok(FinalizeStep::Gating) => {
                    self.gating.insert(slug, seq);
                    return Ok(());
                }
                Err(e) => {
                    tracing::error!("finalizing {owner}/{project}#{seq}: {e}");
                    self.escalate_finalize_failure(owner, project, seq, &e)
                        .await;
                    continue;
                }
            }
        }
        Ok(())
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
            )
            .await
        {
            tracing::error!("escalating finalize failure for {owner}/{project}#{seq}: {e2}");
        }
    }

    /// One finalization attempt against the then-current default HEAD.
    async fn try_finalize(&mut self, owner: &str, project: &str, seq: u64) -> Result<FinalizeStep> {
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        if job.state != JobState::WrapUp {
            return Ok(FinalizeStep::Completed); // revoked while queued
        }
        let base_ref = job.base_ref.clone().expect("base_ref set");
        let summary = self
            .active
            .get(&key)
            .and_then(|e| e.work_submission.as_ref())
            .and_then(|s| s.summary.clone());

        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;

        if head == base_ref {
            // Fast path: evaluators already ran against exactly what lands.
            return match self
                .repos
                .squash_merge(
                    owner,
                    project,
                    seq,
                    &base_ref,
                    &job.r#type,
                    summary.as_deref(),
                )
                .await?
            {
                MergeOutcome::Merged { .. } | MergeOutcome::NoOp => {
                    self.complete_done(owner, project, seq).await?;
                    Ok(FinalizeStep::Completed)
                }
                // head == base_ref makes a conflict impossible by construction;
                // treat one as the conflict path anyway rather than crash.
                MergeOutcome::Conflict { files } => {
                    self.conflict_rework(owner, project, seq, &base_ref, &head, files)
                        .await?;
                    Ok(FinalizeStep::Completed)
                }
            };
        }

        // HEAD moved: build the candidate and open the gate (§3.3 Merge Gate).
        match self
            .repos
            .create_squash_candidate(
                owner,
                project,
                seq,
                &base_ref,
                &job.r#type,
                summary.as_deref(),
            )
            .await?
        {
            MergeOutcome::NoOp => {
                self.complete_done(owner, project, seq).await?;
                Ok(FinalizeStep::Completed)
            }
            MergeOutcome::Conflict { files } => {
                self.conflict_rework(owner, project, seq, &base_ref, &head, files)
                    .await?;
                Ok(FinalizeStep::Completed)
            }
            MergeOutcome::Merged { commit } => {
                let gate_evaluators: Vec<Evaluator> = self
                    .active
                    .get(&key)
                    .map(|e| {
                        e.job_type
                            .eval
                            .iter()
                            .filter(|ev| {
                                ev.r#type == EvaluatorType::Command && ev.required.unwrap_or(true)
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();

                if gate_evaluators.is_empty() {
                    // Nothing to re-run; the candidate promotes directly.
                    self.repos
                        .advance_default(owner, project, &commit, &head)
                        .await?;
                    let _ = self
                        .repos
                        .delete_branch(owner, project, &format!("merge-gate/{seq}"))
                        .await;
                    self.complete_done(owner, project, seq).await?;
                    return Ok(FinalizeStep::Completed);
                }

                let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
                self.publish(
                    owner,
                    project,
                    seq,
                    "job-merge-gate-started",
                    serde_json::json!({ "cycle": cycle }),
                )
                .await?;
                let gate_branch = format!("merge-gate/{seq}");
                let mut slots = Vec::new();
                for evaluator in gate_evaluators {
                    let task_id = self
                        .launch_evaluator_task(
                            owner,
                            project,
                            seq,
                            TaskPhase::MergeGate,
                            &gate_branch,
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
                self.active.get_mut(&key).expect("exec state").gate = Some(GateState {
                    commit,
                    old_head: head,
                    round: EvalRound::single(slots),
                });
                Ok(FinalizeStep::Gating)
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
        exit_code: i32,
        eval_json: Option<serde_json::Value>,
    ) -> Result<()> {
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
            output: String::new(),
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
        });
        if gate.round.slots.iter().any(|s| s.outcome.is_none()) {
            return Ok(());
        }
        // Same triage rule as pump_merges: a hard error in gate resolution
        // (promote, rework re-entry) escalates rather than wedging the queue.
        if let Err(e) = self.gate_reduce(owner, project, seq).await {
            tracing::error!("gate reduce for {owner}/{project}#{seq}: {e}");
            self.gating.remove(&format!("{owner}/{project}"));
            self.escalate_finalize_failure(owner, project, seq, &e)
                .await;
            return self.pump_merges(owner, project).await;
        }
        Ok(())
    }

    async fn gate_reduce(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let slug = format!("{owner}/{project}");
        let gate = self
            .active
            .get_mut(&key)
            .unwrap()
            .gate
            .take()
            .expect("gate state");
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
                }),
                _ => None,
            })
            .collect();

        self.gating.remove(&slug);
        let gate_branch = format!("merge-gate/{seq}");

        if failures.is_empty() {
            // Promote: the candidate commit IS the merge (§3.3). A failed CAS
            // means HEAD moved under the parked candidate (an origin-release
            // reset, or restart-race leftovers) — re-enqueue for finalization
            // against the new HEAD instead of escalating.
            if let Err(e) = self
                .repos
                .advance_default(owner, project, &gate.commit, &gate.old_head)
                .await
            {
                tracing::warn!(
                    "gate promote for {owner}/{project}#{seq}: HEAD moved under candidate ({e}); refinalizing"
                );
                let _ = self.repos.delete_branch(owner, project, &gate_branch).await;
                self.refinalize(owner, project, seq).await?;
                return Ok(());
            }
            let _ = self.repos.delete_branch(owner, project, &gate_branch).await;
            self.complete_done(owner, project, seq).await?;
        } else {
            // Integration failure: rework on the new base, budget NOT consumed
            // — same treatment as a merge conflict (§3.3).
            let _ = self.repos.delete_branch(owner, project, &gate_branch).await;
            let job = self.must_get(owner, project, seq)?.clone();
            let old_base = job.base_ref.clone().expect("base_ref set");
            let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
            let context = self
                .repos
                .conflict_context(owner, project, &old_base, &gate.old_head, &[])
                .await?;
            let mut job = job;
            job.base_ref = Some(gate.old_head.clone());
            self.jobs.put(&job).await?;
            self.graphs
                .entry(job.project.clone())
                .or_default()
                .insert(job.clone());
            self.publish(
                owner,
                project,
                seq,
                "job-rework-started",
                serde_json::json!({
                    "cycle": cycle + 1, "reason": "merge_gate_failure", "eval_context": failures,
                }),
            )
            .await?;
            self.enter_work(owner, project, seq, cycle + 1, failures, Some(context))
                .await?;
        }
        self.pump_merges(owner, project).await
    }

    /// Terminal success: branch cleanup, Done, dependents unblock.
    async fn complete_done(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let mut job = self.must_get(owner, project, seq)?.clone();
        let _ = self.repos.delete_branch(owner, project, &job.branch).await;
        self.set_state(&mut job, JobState::Done).await?;
        self.active.remove(&key);
        self.publish(owner, project, seq, "job-done", serde_json::json!({}))
            .await?;
        self.on_job_done(owner, project, seq).await
    }

    /// §3.2 step 12 conflict path: rebase-as-rework, budget NOT consumed.
    async fn conflict_rework(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        old_base: &str,
        head: &str,
        files: Vec<String>,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let mut job = self.must_get(owner, project, seq)?.clone();
        let cycle = self.active.get(&key).map(|e| e.cycle).unwrap_or(1);
        let context = self
            .repos
            .conflict_context(owner, project, old_base, head, &files)
            .await?;
        job.base_ref = Some(head.to_string());
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        self.publish(
            owner,
            project,
            seq,
            "job-rework-started",
            serde_json::json!({
                "cycle": cycle + 1, "reason": "merge_conflict", "eval_context": [],
            }),
        )
        .await?;
        self.enter_work(owner, project, seq, cycle + 1, Vec::new(), Some(context))
            .await
    }
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
}

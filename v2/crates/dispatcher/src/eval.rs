//! Evaluator fan-out and reduce (spec §3.3), squash-merge/conflict
//! finalization (§3.2 step 12). The merge-gate re-run against a candidate
//! commit (§3.3 Merge Gate) needs candidate-ref support in `vcs` and lands
//! next slice — until then a moved HEAD with a clean merge is logged and
//! merged ungated.

use crate::core::{Core, CoreError, EvalSubmission, Msg, Result};
use crate::exec::{eval_image, task_timeout};
use agent::AgentRunConfig;
use chrono::Utc;
use container::{ContainerLaunchConfig, bootstrap_cmd};
use types::{
    EvalResult, Evaluator, EvaluatorType, JobState, Task, TaskKind, TaskPhase, TaskResult,
    TaskState, WorkType,
};
use vcs::MergeOutcome;

pub struct EvalRound {
    pub slots: Vec<EvalSlot>,
}

pub struct EvalSlot {
    pub evaluator: Evaluator,
    pub task_id: u64,
    pub attempt: u32,
    pub outcome: Option<SlotOutcome>,
}

#[derive(Clone)]
pub enum SlotOutcome {
    Product { pass: bool, structured: Option<serde_json::Value> },
    /// Agent eval exhausted `eval_retries` without a `submit_eval` (§3.3).
    Infra,
}

impl Core {
    /// Work→Evaluation (§3.2 steps 9–10): one task per evaluator, fanned out.
    /// No evaluators → auto-pass straight to finalization.
    pub(crate) async fn enter_evaluation(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let (evaluators, cycle) = {
            let exec = self.active.get(&key).expect("exec state");
            (exec.job_type.eval.clone(), exec.cycle)
        };
        let mut job = self.must_get(owner, project, seq)?.clone();
        self.set_state(&mut job, JobState::Evaluation).await?;
        self.publish(owner, project, seq, "job-evaluation-started",
            serde_json::json!({ "cycle": cycle }))
            .await?;

        if evaluators.is_empty() {
            return self.finalize_pass(owner, project, seq).await;
        }

        let mut slots = Vec::new();
        for evaluator in evaluators {
            let task_id = self.launch_eval_task(owner, project, seq, cycle, &evaluator, 1).await?;
            slots.push(EvalSlot { evaluator, task_id, attempt: 1, outcome: None });
        }
        self.active.get_mut(&key).expect("exec state").round = Some(EvalRound { slots });
        Ok(())
    }

    /// Create + launch one eval task (§3.3 evaluator types). Returns task id.
    async fn launch_eval_task(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        cycle: u32,
        evaluator: &Evaluator,
        attempt: u32,
    ) -> Result<u64> {
        let key = (owner.to_string(), project.to_string(), seq);
        let job = self.must_get(owner, project, seq)?.clone();
        let job_type = self.active.get(&key).expect("exec state").job_type.clone();
        let base_ref = job.base_ref.clone().expect("base_ref set");
        let task_id = self.next_task_id(owner, project, seq).await?;

        let (kind, pending_human) = match evaluator.r#type {
            EvaluatorType::Command => {
                (TaskKind::Command { run: evaluator.run.clone().unwrap_or_default() }, false)
            }
            EvaluatorType::Agent => (
                TaskKind::Agent {
                    provider: evaluator
                        .provider
                        .map(|p| format!("{p:?}").to_lowercase())
                        .unwrap_or_else(|| "claude".into()),
                    model: evaluator.model.clone(),
                    prompt: evaluator.prompt.clone().unwrap_or_default(),
                },
                false,
            ),
            EvaluatorType::Human => {
                (TaskKind::Human { prompt: evaluator.prompt.clone().unwrap_or_default() }, true)
            }
        };
        let mut task = Task {
            id: task_id,
            job_seq: seq,
            project: job.project.clone(),
            phase: TaskPhase::Evaluation,
            cycle,
            kind,
            state: if pending_human { TaskState::Pending } else { TaskState::Running },
            attempt,
            container_id: None,
            result: None,
            created_at: Utc::now(),
            started_at: (!pending_human).then(Utc::now),
            completed_at: None,
        };
        self.tasks.put(&task).await?;
        self.publish(owner, project, seq, "task-created", serde_json::json!({
            "task_id": task_id, "phase": "Evaluation", "cycle": cycle,
            "attempt": attempt, "evaluator": evaluator.name,
        }))
        .await?;
        if pending_human {
            return Ok(task_id); // operator inbox (§3.3 human); resolve slice
        }

        // Eval containers get vars but only the evaluator's own secrets (§4.1).
        let mut eval_type = job_type.clone();
        eval_type.secrets = evaluator.secrets.clone();
        let env = self.container_env(owner, project, seq, &job.branch, &eval_type).await?;
        let tx = self.self_tx.clone().expect("spawned core");
        let (o, p) = (owner.to_string(), project.to_string());

        match evaluator.r#type {
            EvaluatorType::Command => {
                let run = evaluator.run.clone().unwrap_or_default();
                let launch = ContainerLaunchConfig {
                    image: eval_image(&job_type, evaluator),
                    cmd: bootstrap_cmd(&["sh".into(), "-c".into(), run]),
                    env,
                    files: vec![],
                    cpu_limit: job_type.resources.as_ref().and_then(|r| r.cpu),
                    memory_limit: job_type.resources.as_ref().and_then(|r| r.memory.clone()),
                };
                let id = self.backend.launch(launch).await
                    .map_err(|e| CoreError::NotFound(format!("launch failed: {e}")))?;
                task.container_id = Some(id.clone());
                self.tasks.put(&task).await?;
                let backend = self.backend.clone();
                tokio::spawn(async move {
                    let exit_code = backend.wait(&id).await.unwrap_or(-1);
                    // §3.3: extract structured findings after exit.
                    let eval_json = backend
                        .copy_file(&id, "/workspace/eval-result.json")
                        .await
                        .ok()
                        .flatten()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    let _ = tx
                        .send(Msg::TaskExited { owner: o, project: p, seq, task_id, exit_code, eval_json })
                        .await;
                });
            }
            EvaluatorType::Agent => {
                let prompt = self
                    .repos
                    .read_file_at(owner, project, &base_ref,
                        evaluator.prompt.as_deref().unwrap_or_default())
                    .await?
                    .unwrap_or_default();
                let config = AgentRunConfig {
                    image: eval_image(&job_type, evaluator),
                    prompt,
                    model: evaluator.model.clone(),
                    system_prompt: None,
                    mcp_servers: vec![],
                    env,
                    task_timeout: task_timeout(&job_type),
                    eval_context: vec![],
                    merge_conflict: None,
                };
                let provider = self.provider.clone();
                tokio::spawn(async move {
                    let exit_code = match provider.run(config).await {
                        Ok(out) => out.exit_code,
                        Err(e) => {
                            tracing::error!("eval agent run failed: {e}");
                            -1
                        }
                    };
                    let _ = tx
                        .send(Msg::TaskExited {
                            owner: o, project: p, seq, task_id, exit_code, eval_json: None,
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
        task.result = Some(TaskResult::Agent {
            pass: submission.pass,
            structured: submission.structured,
            token_usage: submission.token_usage,
        });
        task.state = TaskState::Done;
        task.completed_at = Some(Utc::now());
        self.tasks.put(&task).await?;
        self.publish(owner, project, seq, "task-completed", serde_json::json!({
            "task_id": task_id, "phase": "Evaluation", "pass": submission.pass,
        }))
        .await?;
        Ok(())
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
        exit_code: i32,
        eval_json: Option<serde_json::Value>,
    ) -> Result<()> {
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
                self.publish(owner, project, seq, "task-completed", serde_json::json!({
                    "task_id": task.id, "phase": "Evaluation", "pass": pass,
                }))
                .await?;
                Some(SlotOutcome::Product { pass, structured: eval_json })
            }
            TaskKind::Agent { .. } => {
                // handle_submit_eval marks the task Done before the container
                // exits; the record we were passed is the pre-exit snapshot.
                let current = self.tasks.get(owner, project, seq, task.id).await?.unwrap_or(task);
                match &current.result {
                    Some(TaskResult::Agent { pass, structured, .. }) => {
                        Some(SlotOutcome::Product { pass: *pass, structured: structured.clone() })
                    }
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
                            let new_id = self
                                .launch_eval_task(owner, project, seq, cycle, &evaluator, failed.attempt + 1)
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
            TaskKind::Human { .. } => None, // resolve slice
        };

        if let Some(outcome) = outcome {
            let round = self.active.get_mut(&key).unwrap().round.as_mut().unwrap();
            round.slots[slot_idx].outcome = Some(outcome);
            if round.slots.iter().all(|s| s.outcome.is_some()) {
                return self.reduce(owner, project, seq).await;
            }
        }
        Ok(())
    }

    /// §3.3 reduce, applied once all eval tasks resolved.
    async fn reduce(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let (results, required_infra_failure, overall_pass, cycle, reworks_used, work_type, budget) = {
            let exec = self.active.get(&key).expect("exec state");
            let round = exec.round.as_ref().expect("round");
            let mut results = Vec::new();
            let mut infra = false;
            let mut pass = true;
            for slot in &round.slots {
                let required = slot.evaluator.required.unwrap_or(true);
                match slot.outcome.as_ref().expect("complete") {
                    SlotOutcome::Product { pass: p, structured } => {
                        results.push(EvalResult {
                            evaluator: slot.evaluator.name.clone(),
                            pass: *p,
                            structured: structured.clone(),
                        });
                        if required && !*p {
                            pass = false;
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
                exec.cycle,
                exec.reworks_used,
                exec.job_type.work.r#type,
                exec.job_type.rework_budget.unwrap_or(0),
            )
        };

        if required_infra_failure {
            self.active.remove(&key);
            return self.escalate(owner, project, seq, "eval_infra_failure",
                format!("Job {seq}: a required evaluator exhausted eval_retries"))
                .await;
        }
        if overall_pass {
            return self.finalize_pass(owner, project, seq).await;
        }

        // Product failure: rework under budget, else escalate (§3.3).
        if work_type != WorkType::Command && reworks_used < budget {
            // enter_work preserves reworks_used from the existing state.
            self.active.get_mut(&key).unwrap().reworks_used = reworks_used + 1;
            self.publish(owner, project, seq, "job-rework-started", serde_json::json!({
                "cycle": cycle + 1, "reason": "eval_failure", "eval_context": results,
            }))
            .await?;
            self.enter_work(owner, project, seq, cycle + 1, results, None).await
        } else {
            self.active.remove(&key);
            self.escalate(owner, project, seq, "rework_budget_exhausted",
                format!("Job {seq}: evaluation failed in cycle {cycle} with no rework budget left"))
                .await
        }
    }

    /// §3.2 step 12: squash-merge, conflict re-entry, Done.
    async fn finalize_pass(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let mut job = self.must_get(owner, project, seq)?.clone();
        let base_ref = job.base_ref.clone().expect("base_ref set");
        let (summary, cycle) = {
            let exec = self.active.get(&key).expect("exec state");
            (
                exec.work_submission.as_ref().and_then(|s| s.summary.clone()),
                exec.cycle,
            )
        };

        // TODO(§3.3 Merge Gate): when default HEAD moved past base_ref and the
        // merge is clean, re-run required command evaluators against the
        // candidate commit before advancing. Needs candidate-ref support in
        // vcs; until then the merge proceeds ungated (logged).
        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self.repos.resolve_ref(owner, project, &default_branch).await?;
        if head != base_ref {
            tracing::warn!(
                "job {seq}: default branch moved past base_ref; merge gate not yet implemented — merging ungated"
            );
        }

        match self
            .repos
            .squash_merge(owner, project, seq, &base_ref, &job.r#type, summary.as_deref())
            .await?
        {
            MergeOutcome::Merged { .. } | MergeOutcome::NoOp => {
                let _ = self.repos.delete_branch(owner, project, &job.branch).await;
                self.set_state(&mut job, JobState::Done).await?;
                self.active.remove(&key);
                self.publish(owner, project, seq, "job-done", serde_json::json!({})).await?;
                self.on_job_done(owner, project, seq).await
            }
            MergeOutcome::Conflict { files } => {
                // §3.2 step 12: rebase-as-rework; budget NOT consumed.
                let old_base = base_ref;
                let context = self
                    .repos
                    .conflict_context(owner, project, &old_base, &head, &files)
                    .await?;
                job.base_ref = Some(head);
                self.jobs.put(&job).await?;
                self.graphs.entry(job.project.clone()).or_default().insert(job.clone());
                self.publish(owner, project, seq, "job-rework-started", serde_json::json!({
                    "cycle": cycle + 1, "reason": "merge_conflict", "eval_context": [],
                }))
                .await?;
                self.enter_work(owner, project, seq, cycle + 1, Vec::new(), Some(context)).await
            }
        }
    }
}

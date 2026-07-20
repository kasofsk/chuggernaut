//! Restart reconciliation (spec §3.6). Runs inside the actor task before the
//! message loop starts. The task log in `tasks.*` KV is the source of truth;
//! `Core::new` already rebuilt the rdeps index, graphs, and the Ready queue —
//! this pass recovers jobs that were mid-execution when the process died.

use crate::core::{Core, Msg, Result, TaskExit};
use crate::eval::{EvalRound, EvalSlot, SlotOutcome};
use chrono::Utc;
use types::{Job, JobState, Task, TaskPhase, TaskResult, TaskState};

impl Core {
    pub(crate) async fn reconcile(&mut self) -> Result<()> {
        let jobs: Vec<Job> = self
            .graphs
            .values()
            .flat_map(|g| g.jobs().cloned().collect::<Vec<_>>())
            .collect();

        // §3.6 step 3: Blocked jobs whose deps completed while we were down.
        for job in &jobs {
            let (owner, project) = split(&job.project);
            if job.state == JobState::Blocked
                && self.graphs.get(&job.project).is_some_and(|g| g.deps_done(job.id))
            {
                self.try_unblock(&owner, &project, job.id).await?;
            }
        }

        // §3.6 step 2: in-flight recovery.
        for job in &jobs {
            let (owner, project) = split(&job.project);
            match job.state {
                JobState::Work => self.recover_work(&owner, &project, job.id).await?,
                JobState::Evaluation => self.recover_evaluation(&owner, &project, job.id).await?,
                _ => {}
            }
        }
        Ok(())
    }

    async fn recover_work(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        self.ensure_exec_state(owner, project, seq).await?;
        let key = (owner.to_string(), project.to_string(), seq);
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let work_type = self.active.get(&key).expect("exec state").job_type.work.r#type;

        let latest = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .into_iter()
            .filter(|t| t.phase == TaskPhase::Work && t.cycle == cycle && t.evaluator.is_none())
            .max_by_key(|t| t.id);
        let Some(task) = latest else {
            // Crashed between Ready→Work and the first task write: relaunch.
            return self.launch_work_task(owner, project, seq, cycle, 1).await;
        };
        match (task.state, work_type) {
            // Pending human work waits on the inbox; nothing to recover.
            (TaskState::Pending, _) => Ok(()),
            (TaskState::Running, _) => self.settle_running(owner, project, seq, task).await,
            // Completed before the crash, transition lost: replay it.
            (TaskState::Done, _) => self.enter_evaluation(owner, project, seq).await,
            // Crashed between marking Failed and launching the retry: replay
            // the retry logic directly (the exit handler skips Failed tasks).
            (TaskState::Failed, _) => {
                self.retry_or_escalate_failed_work(owner, project, seq, &task).await
            }
        }
    }

    async fn recover_evaluation(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        self.ensure_exec_state(owner, project, seq).await?;
        let key = (owner.to_string(), project.to_string(), seq);
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let all = self.tasks.list_for_job(owner, project, seq).await?;

        // A gate was in flight (spec §3.3 Merge Gate, Restart): supersede it —
        // fail its Running tasks, drop the candidate, re-open the gate fresh.
        let gate_open = all
            .iter()
            .any(|t| t.phase == TaskPhase::MergeGate && t.cycle == cycle && t.state == TaskState::Running)
            || self
                .repos
                .resolve_ref(owner, project, &format!("merge-gate/{seq}"))
                .await
                .is_ok();
        let eval_done_and_gating = gate_open
            || all.iter().any(|t| t.phase == TaskPhase::MergeGate && t.cycle == cycle);
        if eval_done_and_gating {
            for t in all.iter().filter(|t| {
                t.phase == TaskPhase::MergeGate && t.cycle == cycle && t.state == TaskState::Running
            }) {
                let mut failed = t.clone();
                if let Some(cid) = &failed.container_id {
                    let _ = self.backend.kill(cid).await;
                }
                failed.state = TaskState::Failed;
                failed.completed_at = Some(Utc::now());
                self.tasks.put(&failed).await?;
            }
            let _ = self
                .repos
                .delete_branch(owner, project, &format!("merge-gate/{seq}"))
                .await;
            return self.refinalize(owner, project, seq).await;
        }

        // Rebuild the round: one slot per evaluator, latest task per name.
        let evaluators = self.active.get(&key).expect("exec state").job_type.eval.clone();
        if evaluators.is_empty() {
            // Auto-pass job caught between Evaluation and Done: re-finalize.
            return self.refinalize(owner, project, seq).await;
        }
        let mut slots = Vec::new();
        let mut running: Vec<Task> = Vec::new();
        for evaluator in evaluators {
            let latest = all
                .iter()
                .filter(|t| {
                    t.phase == TaskPhase::Evaluation
                        && t.cycle == cycle
                        && t.evaluator.as_deref() == Some(evaluator.name.as_str())
                })
                .max_by_key(|t| t.id)
                .cloned();
            match latest {
                None => {
                    // Crashed mid-fan-out: this evaluator never got a task.
                    let branch = self.must_get(owner, project, seq)?.branch.clone();
                    let task_id = self
                        .launch_evaluator_task(
                            owner, project, seq, TaskPhase::Evaluation, &branch, cycle,
                            &evaluator, 1,
                        )
                        .await?;
                    slots.push(EvalSlot { evaluator, task_id, attempt: 1, outcome: None });
                }
                Some(task) => {
                    let outcome = match (task.state, &task.result) {
                        (TaskState::Done, Some(r)) => Some(SlotOutcome::Product {
                            pass: result_pass(r),
                            abort: result_abort(r),
                            structured: result_structured(r),
                        }),
                        (TaskState::Failed, _) => Some(SlotOutcome::Infra),
                        _ => None, // Pending human or Running container
                    };
                    if task.state == TaskState::Running {
                        running.push(task.clone());
                    }
                    slots.push(EvalSlot {
                        evaluator,
                        task_id: task.id,
                        attempt: task.attempt,
                        outcome,
                    });
                }
            }
        }
        let complete = slots.iter().all(|s| s.outcome.is_some());
        self.active.get_mut(&key).expect("exec state").round = Some(EvalRound { slots });

        for task in running {
            self.settle_running(owner, project, seq, task).await?;
        }
        if complete {
            // No Running tasks and every slot resolved before the crash: the
            // reduce (and possibly the merge) was lost — replay it.
            return self.refinalize(owner, project, seq).await;
        }
        Ok(())
    }

    /// §3.6 step 2 rules for one Running task: persisted result wins; else ask
    /// the backend; not-found means failure/infra per task type. Resolution is
    /// delivered through the normal exit path.
    async fn settle_running(&mut self, owner: &str, project: &str, seq: u64, task: Task) -> Result<()> {
        // Persisted result + Running only happens for agent eval tasks whose
        // submit_eval landed but whose exit event was lost — on_eval_exited
        // reads the persisted verdict whatever exit code we synthesize.
        let backend_exit = match &task.container_id {
            Some(cid) => match self.backend.inspect(cid).await {
                Ok(Some(container::ContainerStatus::Running)) => {
                    // Still running: re-attach a monitor and resume.
                    let backend = self.backend.clone();
                    let tx = self.self_tx.clone().expect("spawned core");
                    let (o, p, cid) = (owner.to_string(), project.to_string(), cid.clone());
                    let task_id = task.id;
                    tokio::spawn(async move {
                        let exit_code = backend.wait(&cid).await.unwrap_or(-1);
                        let eval_json = backend
                            .copy_file(&cid, "/workspace/eval-result.json")
                            .await
                            .ok()
                            .flatten()
                            .and_then(|b| serde_json::from_slice(&b).ok());
                        let _ = tx
                            .send(Msg::TaskExited {
                                owner: o, project: p, seq, task_id,
                                // Re-attaching after a restart: the agent
                                // monitor that would have parsed usage is gone.
                                exit: TaskExit { exit_code, eval_json, usage: None },
                            })
                            .await;
                    });
                    return Ok(());
                }
                Ok(Some(container::ContainerStatus::Exited { exit_code })) => exit_code,
                // Not found: failure (work) / infra (agent eval) / failed
                // verdict (command eval) — all reachable via a -1 exit.
                Ok(None) | Err(_) => -1,
            },
            // Provider-run tasks don't record container ids yet: not found.
            None => -1,
        };
        self.on_task_exited(owner, project, seq, task.id, TaskExit::code(backend_exit)).await
    }

    async fn retry_or_escalate_failed_work(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task: &Task,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let work_retries = self
            .active
            .get(&key)
            .and_then(|e| e.job_type.work_retries)
            .unwrap_or(0);
        if task.attempt <= work_retries {
            let job = self.must_get(owner, project, seq)?.clone();
            let base_ref = job.base_ref.clone().expect("base_ref set in Work");
            self.repos.reset_branch(owner, project, &job.branch, &base_ref).await?;
            self.launch_work_task(owner, project, seq, task.cycle, task.attempt + 1).await
        } else {
            self.active.remove(&key);
            self.escalate(owner, project, seq, "work_retries_exhausted",
                format!("Job {seq}: work task failed with no retries left (found on restart)"))
                .await
        }
    }
}

fn split(slug: &str) -> (String, String) {
    let (o, p) = slug.split_once('/').expect("owner/project slug");
    (o.to_string(), p.to_string())
}

pub(crate) fn result_pass(result: &TaskResult) -> bool {
    match result {
        TaskResult::Command { pass, .. }
        | TaskResult::Agent { pass, .. }
        | TaskResult::Human { pass, .. } => *pass,
        TaskResult::Work { .. } => true,
    }
}

pub(crate) fn result_abort(result: &TaskResult) -> bool {
    match result {
        TaskResult::Agent { abort, .. } | TaskResult::Human { abort, .. } => *abort,
        TaskResult::Command { .. } | TaskResult::Work { .. } => false,
    }
}

pub(crate) fn result_structured(result: &TaskResult) -> Option<serde_json::Value> {
    match result {
        TaskResult::Command { structured, .. }
        | TaskResult::Agent { structured, .. }
        | TaskResult::Human { structured, .. }
        | TaskResult::Work { structured, .. } => structured.clone(),
    }
}

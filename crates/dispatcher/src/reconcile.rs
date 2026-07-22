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
                && self
                    .graphs
                    .get(&job.project)
                    .is_some_and(|g| g.deps_done(job.id))
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
                JobState::WrapUp => self.recover_wrapup(&owner, &project, job.id).await?,
                // Escalated/Stalled wait on the operator inbox; nothing to recover.
                _ => {}
            }
        }

        // §3.6: reclaim exited containers orphaned by the crash/restart. Runs
        // last so any container in-flight recovery still needs (Running tasks,
        // re-attached monitors) has been settled and protected first.
        self.sweep_exited_containers(&jobs).await;
        Ok(())
    }

    /// §3.6 startup sweep: remove exited `chuggernaut.managed` containers whose
    /// task is already terminal — the overlay is dead weight once its result is
    /// recorded. A container still bound to a live (Running/Pending) task is
    /// kept: recovery re-attaches to it. Orphans with no owning task are removed
    /// too — they are exactly the crash leftovers this catches.
    ///
    /// Best-effort: a Docker hiccup here must not block the dispatcher from
    /// starting, so every failure only warns.
    async fn sweep_exited_containers(&self, jobs: &[Job]) {
        let exited = match self.backend.list_managed_exited().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("startup container sweep: listing exited containers failed: {e}");
                return;
            }
        };
        if exited.is_empty() {
            return;
        }

        // Container ids we must not touch: those a live task may still resume.
        // Only active (non-terminal) jobs can hold live tasks, and those are
        // exactly the jobs in the graphs.
        let mut live = std::collections::HashSet::new();
        for job in jobs {
            let (owner, project) = split(&job.project);
            let tasks = match self.tasks.list_for_job(&owner, &project, job.id).await {
                Ok(tasks) => tasks,
                Err(e) => {
                    // Can't prove these containers are disposable — skip the
                    // whole sweep rather than risk removing a live one.
                    tracing::warn!(
                        "startup container sweep: listing tasks for job {} failed: {e}; skipping sweep",
                        job.id
                    );
                    return;
                }
            };
            for task in tasks {
                if matches!(task.state, TaskState::Running | TaskState::Pending)
                    && let Some(cid) = task.container_id
                {
                    live.insert(cid);
                }
            }
        }

        for id in exited {
            if live.contains(&id) {
                continue;
            }
            match self.backend.remove(&id).await {
                Ok(()) => tracing::info!("startup container sweep: removed exited container {id}"),
                Err(e) => {
                    tracing::warn!("startup container sweep: removing container {id} failed: {e}")
                }
            }
        }
    }

    async fn recover_work(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        self.ensure_exec_state(owner, project, seq).await?;
        let key = (owner.to_string(), project.to_string(), seq);
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let work_type = self
            .active
            .get(&key)
            .expect("exec state")
            .job_type
            .work
            .r#type;

        let latest = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .into_iter()
            .filter(|t| t.phase == TaskPhase::Work && t.cycle == cycle && t.evaluator.is_none())
            .max_by_key(|t| t.id);
        let Some(task) = latest else {
            // Crashed between Ready→Work and the first task write: relaunch. The
            // branch may already carry commits if the crash was later than it
            // looks — recover them rather than reset (§3.2).
            let job = self.must_get(owner, project, seq)?.clone();
            let base_ref = job.base_ref.clone().expect("base_ref set in Work");
            let resume = crate::exec::recover_or_reset_branch(
                &self.repos,
                owner,
                project,
                &job.branch,
                &base_ref,
            )
            .await?;
            return self
                .launch_work_task(owner, project, seq, cycle, 1, resume)
                .await;
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
                self.retry_or_escalate_failed_work(owner, project, seq, &task)
                    .await
            }
        }
    }

    /// Recover a job crashed while landing (§2.1 WrapUp; §3.3 Merge Gate,
    /// Restart; §3.2 wrap-up command). Two cases, discriminated by the task log:
    ///
    /// - A `WrapUp`-phase command task exists → the squash **already landed** and
    ///   only the `wrap_up.run` publish remains (§3.6). Re-driving the merge
    ///   queue would re-squash, so instead recover the publish: re-attach a live
    ///   container, replay a lost terminal transition, or relaunch a dead/
    ///   never-launched one (the command is idempotent by contract).
    /// - Otherwise the job was still merging: any in-flight gate is superseded —
    ///   its Running tasks fail and the candidate branch is dropped — and the job
    ///   re-enters the merge queue via `refinalize`, which re-opens the gate fresh
    ///   against current HEAD (a job merely parked in the queue simply re-enqueues).
    ///   `refinalize` → `finish_landing` re-launches the publish on the re-merge,
    ///   so a crash in the narrow window before the publish task was written still
    ///   ships it.
    async fn recover_wrapup(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        self.ensure_exec_state(owner, project, seq).await?;
        let key = (owner.to_string(), project.to_string(), seq);
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let all = self.tasks.list_for_job(owner, project, seq).await?;

        if let Some(task) = all
            .iter()
            .filter(|t| t.phase == TaskPhase::WrapUp && t.cycle == cycle)
            .max_by_key(|t| t.id)
            .cloned()
        {
            return self.recover_wrapup_command(owner, project, seq, task).await;
        }

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
        self.refinalize(owner, project, seq).await
    }

    /// Recover the `wrap_up.run` publish task after a restart (§3.2, §3.6). The
    /// merge is already landed; the publish is all that is left, and it must not
    /// be dropped.
    async fn recover_wrapup_command(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task: Task,
    ) -> Result<()> {
        match task.state {
            // Publish finished before the crash; only the Done transition was
            // lost. Land it.
            TaskState::Done => self.complete_done(owner, project, seq).await,
            // Publish ran and failed; the escalation was lost. Replay it — the
            // merge stays (design-lifecycle.md wrap-up failure).
            TaskState::Failed => {
                self.active
                    .remove(&(owner.to_string(), project.to_string(), seq));
                self.escalate(
                    owner,
                    project,
                    seq,
                    "wrap_up_failed",
                    format!(
                        "Job {seq}: the wrap-up publish command failed (found on restart). \
                         The squash already landed — the merge is final; only the publish \
                         did not run. Re-run the publish (jobs/web-publish.yaml) or resolve."
                    ),
                )
                .await
            }
            // In flight at crash time. If the container is still alive, re-attach
            // and let it finish (settle_running); otherwise it is dead or was
            // never launched — relaunch a fresh attempt, the command is
            // idempotent by contract (§3.2).
            TaskState::Running | TaskState::Pending => {
                let alive = match &task.container_id {
                    Some(cid) => matches!(
                        self.backend.inspect(cid).await,
                        Ok(Some(container::ContainerStatus::Running))
                    ),
                    None => false,
                };
                if alive {
                    return self.settle_running(owner, project, seq, task).await;
                }
                // Retire the orphaned record so it does not linger as Running,
                // then relaunch (the newer task's higher id wins any future scan).
                let mut dead = task.clone();
                dead.state = TaskState::Failed;
                dead.completed_at = Some(Utc::now());
                self.tasks.put(&dead).await?;
                self.launch_wrapup_task(owner, project, seq, task.attempt + 1)
                    .await
            }
        }
    }

    async fn recover_evaluation(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        self.ensure_exec_state(owner, project, seq).await?;
        let key = (owner.to_string(), project.to_string(), seq);
        let cycle = self.active.get(&key).expect("exec state").cycle;
        let all = self.tasks.list_for_job(owner, project, seq).await?;

        let evaluators = self
            .active
            .get(&key)
            .expect("exec state")
            .job_type
            .eval
            .clone();
        if evaluators.is_empty() {
            // Auto-pass job caught between Evaluation and Done: re-finalize.
            return self.refinalize(owner, project, seq).await;
        }

        // Rebuild the staged round from the task log (§3.3). Stages are launched
        // in order, so the started stages form a prefix: the last stage with any
        // task this cycle is the one in flight (`slots`), earlier stages are
        // `done`, later stages `pending` (no tasks yet). A single-stage job
        // collapses to one group — identical to the pre-staging rebuild.
        let stages: Vec<Vec<types::Evaluator>> =
            crate::eval::group_stages(evaluators).into_iter().collect();
        let latest = |ev: &types::Evaluator| -> Option<Task> {
            all.iter()
                .filter(|t| {
                    t.phase == TaskPhase::Evaluation
                        && t.cycle == cycle
                        && t.evaluator.as_deref() == Some(ev.name.as_str())
                })
                .max_by_key(|t| t.id)
                .cloned()
        };
        let current_idx = stages
            .iter()
            .rposition(|stage| stage.iter().any(|ev| latest(ev).is_some()))
            .unwrap_or(0);

        // Earlier stages passed before we advanced: rebuild their terminal
        // outcomes so the reduce sees the whole run. A non-terminal task here is
        // structurally impossible; treat one as infra rather than panic.
        let mut done: Vec<EvalSlot> = Vec::new();
        for stage in &stages[..current_idx] {
            for evaluator in stage {
                let (task_id, attempt, outcome) = match latest(evaluator) {
                    Some(task) => (
                        task.id,
                        task.attempt,
                        match (task.state, &task.result) {
                            (TaskState::Done, Some(r)) => SlotOutcome::Product {
                                pass: result_pass(r),
                                abort: result_abort(r),
                                structured: result_structured(r),
                            },
                            _ => SlotOutcome::Infra,
                        },
                    ),
                    None => (0, 1, SlotOutcome::Infra),
                };
                done.push(EvalSlot {
                    evaluator: evaluator.clone(),
                    task_id,
                    attempt,
                    outcome: Some(outcome),
                });
            }
        }

        // The stage in flight: rebuild each slot, relaunching any evaluator that
        // never got a task (crashed mid-fan-out).
        let mut slots = Vec::new();
        let mut running: Vec<Task> = Vec::new();
        for evaluator in stages[current_idx].clone() {
            match latest(&evaluator) {
                None => {
                    let branch = self.must_get(owner, project, seq)?.branch.clone();
                    let task_id = self
                        .launch_evaluator_task(
                            owner,
                            project,
                            seq,
                            TaskPhase::Evaluation,
                            &branch,
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
        let pending: std::collections::VecDeque<Vec<types::Evaluator>> =
            stages[current_idx + 1..].to_vec().into();
        self.active.get_mut(&key).expect("exec state").round = Some(EvalRound {
            slots,
            pending,
            done,
        });

        for task in running {
            self.settle_running(owner, project, seq, task).await?;
        }
        if complete {
            // No Running tasks and every slot in the stage resolved before the
            // crash: replay the advance-or-reduce decision — it may launch the
            // next stage or run the (lost) reduce and merge.
            return self.stage_complete(owner, project, seq).await;
        }
        Ok(())
    }

    /// §3.6 step 2 rules for one Running task: persisted result wins; else ask
    /// the backend; not-found means failure/infra per task type. Resolution is
    /// delivered through the normal exit path.
    async fn settle_running(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task: Task,
    ) -> Result<()> {
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
                        // Re-attached after a restart: reclaim the overlay once
                        // the verdict is out, same as the normal exit path.
                        if let Err(e) = backend.remove(&cid).await {
                            tracing::warn!(
                                "job {seq} task {task_id}: removing container failed: {e}"
                            );
                        }
                        let _ = tx
                            .send(Msg::TaskExited {
                                owner: o,
                                project: p,
                                seq,
                                task_id,
                                // Re-attaching after a restart: the agent
                                // monitor that would have parsed usage is gone.
                                exit: TaskExit {
                                    exit_code,
                                    eval_json,
                                    usage: None,
                                    assessment: None,
                                    launch_error: None,
                                },
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
        self.on_task_exited(owner, project, seq, task.id, TaskExit::code(backend_exit))
            .await
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
            // §3.2 crash recovery: a task found Failed on restart may have pushed
            // commits before dying — recover the branch instead of resetting.
            let job = self.must_get(owner, project, seq)?.clone();
            let base_ref = job.base_ref.clone().expect("base_ref set in Work");
            let resume = crate::exec::recover_or_reset_branch(
                &self.repos,
                owner,
                project,
                &job.branch,
                &base_ref,
            )
            .await?;
            self.launch_work_task(owner, project, seq, task.cycle, task.attempt + 1, resume)
                .await
        } else {
            self.active.remove(&key);
            self.escalate(
                owner,
                project,
                seq,
                "work_retries_exhausted",
                format!("Job {seq}: work task failed with no retries left (found on restart)"),
            )
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
        // Triage is advisory (§1.2) — never an eval verdict, so this is never
        // consulted; treat it as a non-failure for completeness.
        TaskResult::Work { .. } | TaskResult::Triage { .. } => true,
    }
}

pub(crate) fn result_abort(result: &TaskResult) -> bool {
    match result {
        TaskResult::Agent { abort, .. } | TaskResult::Human { abort, .. } => *abort,
        TaskResult::Command { .. } | TaskResult::Work { .. } | TaskResult::Triage { .. } => false,
    }
}

pub(crate) fn result_structured(result: &TaskResult) -> Option<serde_json::Value> {
    match result {
        TaskResult::Command { structured, .. }
        | TaskResult::Agent { structured, .. }
        | TaskResult::Human { structured, .. }
        | TaskResult::Work { structured, .. } => structured.clone(),
        // Triage carries prose, not structured findings.
        TaskResult::Triage { .. } => None,
    }
}

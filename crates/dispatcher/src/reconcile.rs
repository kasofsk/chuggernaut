//! Restart reconciliation (spec §3.6). Runs inside the actor task before the
//! message loop starts. The task log in `tasks.*` KV is the source of truth;
//! `Core::new` already rebuilt the rdeps index, graphs, and the Ready queue —
//! this pass recovers jobs that were mid-execution when the process died.
//!
//! - **Accepts:** the persisted `tasks.*` KV, read at startup before the
//!   message loop begins.
//! - **Emits:** recovery transitions for jobs left mid-execution (re-queue,
//!   re-attach, or fail per §3.6).
//! - **Guarantees:** runs inside the actor task before any message is
//!   processed; the task log stays the source of truth.
//! - **Spec:** §3.6.

use crate::core::{Core, Result, TaskExit};
use crate::eval::{EvalRound, EvalSlot, SlotOutcome};
use chrono::Utc;
use types::{Job, JobState, Task, TaskKind, TaskPhase, TaskResult, TaskState};

impl Core {
    pub(crate) async fn reconcile(&mut self) -> Result<()> {
        let jobs: Vec<Job> = self
            .graphs
            .values()
            .flat_map(|g| g.jobs().cloned().collect::<Vec<_>>())
            .collect();

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

        for job in &jobs {
            let (owner, project) = split(&job.project);
            match job.state {
                JobState::Work => self.recover_work(&owner, &project, job.id).await?,
                JobState::Evaluation => self.recover_evaluation(&owner, &project, job.id).await?,
                JobState::WrapUp => self.recover_wrapup(&owner, &project, job.id).await?,
                JobState::Escalated | JobState::Stalled => {
                    self.heal_missing_escalation_task(&owner, &project, job)
                        .await?
                }
                _ => {}
            }
        }

        self.launch_queue
            .make_contiguous()
            .sort_by_key(|q| q.queued_at);

        self.sweep_exited_containers(&jobs).await;
        self.sweep_orphan_running_containers(&jobs).await;
        Ok(())
    }

    /// C1 heal: an Escalated/Stalled job must always hold a Pending Human
    /// escalation task — that task *is* the operator inbox entry its state
    /// waits on. The decider shim commits the §2.1 transition before the
    /// PutTask/PublishEvent effects (the record is the decision; artifacts
    /// are downstream), so a crash between the two leaves the job parked with
    /// an empty inbox — and the same shape appears when a crash eats a
    /// resolution mid-flight. The job's stamped [`types::Escalation`] record
    /// carries everything the artifacts need, so recovery re-derives them
    /// here — the same `[PutTask, PublishEvent]` pair the decider emits,
    /// through the same interpreter — instead of the writes being carefully
    /// ordered and the window merely narrowed.
    async fn heal_missing_escalation_task(
        &mut self,
        owner: &str,
        project: &str,
        job: &Job,
    ) -> Result<()> {
        let tasks = self.tasks.list_for_job(owner, project, job.id).await?;
        if tasks
            .iter()
            .any(|t| t.phase == TaskPhase::Escalation && t.state == TaskState::Pending)
        {
            return Ok(());
        }
        let Some(esc) = &job.escalation else {
            tracing::warn!(
                "job {}#{} is {:?} with no pending escalation task and no \
                 escalation record; cannot heal — resolve via triage",
                job.project,
                job.id,
                job.state,
            );
            return Ok(());
        };

        let task_id = tasks.len() as u64 + 1;
        let cycle = tasks.iter().map(|t| t.cycle).max().unwrap_or(1);
        let task = crate::escalation::escalation_task(
            task_id,
            job.id,
            &job.project,
            cycle,
            esc.detail.clone(),
            esc.at,
        );
        let event_type = match job.state {
            JobState::Escalated => "job-escalated",
            _ => "job-stalled",
        };
        tracing::info!(
            "healing {}#{}: {:?} with no pending escalation task — re-creating \
             task {task_id} from the stamped record ({})",
            job.project,
            job.id,
            job.state,
            esc.reason,
        );
        self.interpret(crate::effects::Effect::PutTask {
            task: Box::new(task),
        })
        .await?;
        self.interpret(crate::effects::Effect::PublishEvent {
            owner: owner.to_string(),
            project: project.to_string(),
            seq: job.id,
            event_type: event_type.to_string(),
            extra: serde_json::json!({ "reason": esc.reason }),
        })
        .await?;
        Ok(())
    }

    /// §3.6 fleet sweep: kill running `chuggernaut.managed` containers that no
    /// live (`Running`) task owns. A crash-restart can fail an in-flight task
    /// (e.g. its container read as gone) while the container keeps running and
    /// holding a fleet slot; left alone, that slot leaks until an operator
    /// manually removes it, and every retry fails with `no free slots`. A
    /// container is kept only when it re-attaches to a live task — matched by
    /// its `(project, job, task)` identity labels or by a live task's recorded
    /// `container_id` — so recovery's monitor still lands. Anything else is
    /// reaped.
    ///
    /// A container carrying the marker but **no identity labels** is not ours to
    /// reap: every launch stamps the identity alongside the marker, so a bare
    /// marker means the container inherited it from its image (#268 — the
    /// `chug-worker` daemon did, and this sweep killed the fleet on every
    /// restart). It is logged and left running.
    ///
    /// Best-effort: a backend hiccup only warns and never blocks startup.
    #[allow(
        clippy::too_many_lines,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
    async fn sweep_orphan_running_containers(&self, jobs: &[Job]) {
        let running = match self.backend.list_managed_running().await {
            Ok(cs) => cs,
            Err(e) => {
                tracing::warn!("startup fleet sweep: listing running containers failed: {e}");
                return;
            }
        };
        if running.is_empty() {
            return;
        }

        let mut live_tasks = std::collections::HashSet::new();
        let mut live_cids = std::collections::HashSet::new();
        for job in jobs {
            let (owner, project) = split(&job.project);
            let tasks = match self.tasks.list_for_job(&owner, &project, job.id).await {
                Ok(tasks) => tasks,
                Err(e) => {
                    tracing::warn!(
                        "startup fleet sweep: listing tasks for job {} failed: {e}; skipping sweep",
                        job.id
                    );
                    return;
                }
            };
            for task in tasks {
                if task.state == TaskState::Running {
                    live_tasks.insert((job.project.clone(), job.id, task.id));
                    if let Some(cid) = task.container_id {
                        live_cids.insert(cid);
                    }
                }
            }
        }

        for rc in running {
            if live_cids.contains(&rc.id) {
                continue;
            }
            let (Some(project), Some(job), Some(task)) = (&rc.project, rc.job, rc.task) else {
                tracing::warn!(
                    "startup fleet sweep: container {} carries the managed marker but no \
                     identity labels — not a dispatcher launch, left running",
                    rc.id
                );
                continue;
            };
            if live_tasks.contains(&(project.clone(), job, task)) {
                continue;
            }
            match self.backend.kill(&rc.id).await {
                Ok(()) => {
                    let label = format!("{job}/{task}");
                    tracing::info!(
                        "startup fleet sweep: reaped orphan container {} for {label}",
                        rc.id
                    );
                    let (owner, project) = split(project);
                    let _ = self
                        .publish(
                            &owner,
                            &project,
                            job,
                            "container-reaped",
                            serde_json::json!({
                                "container_id": rc.id,
                                "task_id": task,
                                "detail": format!(
                                    "reaped orphan container {} for {label}",
                                    rc.id
                                ),
                            }),
                        )
                        .await;
                }
                Err(e) => tracing::warn!(
                    "startup fleet sweep: killing orphan container {} failed: {e}",
                    rc.id
                ),
            }
        }
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

        let mut live = std::collections::HashSet::new();
        for job in jobs {
            let (owner, project) = split(&job.project);
            let tasks = match self.tasks.list_for_job(&owner, &project, job.id).await {
                Ok(tasks) => tasks,
                Err(e) => {
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

    #[allow(
        clippy::expect_used,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
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
            (TaskState::Pending, _) => {
                if task.container_id.is_none()
                    && !matches!(task.kind, TaskKind::Human { .. })
                    && task.performed_by.is_none()
                {
                    self.enqueue_launch(crate::queue::QueuedLaunch {
                        owner: owner.to_string(),
                        project: project.to_string(),
                        seq,
                        task_id: task.id,
                        priority: crate::queue::launch_priority(task.phase),
                        queued_at: task.queued_at.unwrap_or_else(Utc::now),
                    });
                }
                Ok(())
            }
            (TaskState::Running, _) => self.settle_running(owner, project, seq, task).await,
            (TaskState::Done, _) => self.enter_evaluation(owner, project, seq).await,
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
    #[allow(
        clippy::expect_used,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
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
            t.phase == TaskPhase::MergeGate
                && t.cycle == cycle
                && matches!(t.state, TaskState::Running | TaskState::Pending)
        }) {
            let mut failed = t.clone();
            if let Some(cid) = &failed.container_id {
                let _ = self.backend.kill(cid).await;
            }
            failed.state = TaskState::Failed;
            failed.completed_at = Some(Utc::now());
            self.task_put(&failed).await?;
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
            TaskState::Done => self.complete_done(owner, project, seq).await,
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
                    Some(task.id),
                )
                .await
            }
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
                let mut dead = task.clone();
                dead.state = TaskState::Failed;
                dead.completed_at = Some(Utc::now());
                self.task_put(&dead).await?;
                self.launch_wrapup_task(owner, project, seq, task.attempt + 1)
                    .await
            }
        }
    }

    #[allow(
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
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
            return self.refinalize(owner, project, seq).await;
        }

        let stages: Vec<Vec<types::Evaluator>> =
            crate::decide::merge_gate::group_stages(evaluators)
                .into_iter()
                .collect();
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
                                output: result_output(r),
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
                            output: result_output(r),
                        }),
                        (TaskState::Failed, _) => Some(SlotOutcome::Infra),
                        _ => None,
                    };
                    if task.state == TaskState::Running {
                        running.push(task.clone());
                    }
                    if task.state == TaskState::Pending
                        && task.container_id.is_none()
                        && !matches!(task.kind, types::TaskKind::Human { .. })
                    {
                        self.enqueue_launch(crate::queue::QueuedLaunch {
                            owner: owner.to_string(),
                            project: project.to_string(),
                            seq,
                            task_id: task.id,
                            priority: crate::queue::launch_priority(task.phase),
                            queued_at: task.queued_at.unwrap_or_else(Utc::now),
                        });
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
            return self.eval_stage_settled(owner, project, seq).await;
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
        let exit = match &task.container_id {
            Some(cid) => match self.backend.inspect(cid).await {
                Ok(Some(status)) => {
                    let cid = cid.clone();
                    match (task.phase, status) {
                        (TaskPhase::Work | TaskPhase::WrapUp, _) => {
                            self.spawn_logs_monitor(owner, project, seq, task.id, cid);
                            return Ok(());
                        }
                        (_, container::ContainerStatus::Running) => {
                            self.spawn_eval_monitor(owner, project, seq, task.id, cid);
                            return Ok(());
                        }
                        (_, container::ContainerStatus::Exited { exit_code }) => {
                            TaskExit::code(exit_code)
                        }
                    }
                }
                Ok(None) | Err(_) => TaskExit::infra_loss(),
            },
            None => TaskExit::code(-1),
        };
        self.on_task_exited(owner, project, seq, task.id, exit)
            .await
    }

    #[allow(
        clippy::expect_used,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
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
                Some(task.id),
            )
            .await
        }
    }
}

#[allow(
    clippy::expect_used,
    reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
)]
fn split(slug: &str) -> (String, String) {
    let (o, p) = slug.split_once('/').expect("owner/project slug");
    (o.to_string(), p.to_string())
}

pub(crate) fn result_pass(result: &TaskResult) -> bool {
    match result {
        TaskResult::Command { pass, .. }
        | TaskResult::Agent { pass, .. }
        | TaskResult::Human { pass, .. } => *pass,
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
        TaskResult::Triage { .. } => None,
    }
}

/// A command evaluator's captured output tail (#167), for rebuilding a slot
/// outcome from the persisted task record on restart. Only `command` results
/// embed the tail; every other result reports through structured findings.
pub(crate) fn result_output(result: &TaskResult) -> Option<String> {
    match result {
        TaskResult::Command { output, .. } if !output.is_empty() => Some(output.clone()),
        _ => None,
    }
}

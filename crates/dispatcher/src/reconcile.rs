//! Restart reconciliation (spec §3.6). Runs inside the actor task before the
//! message loop starts. The task log in `tasks.*` KV is the source of truth;
//! `Core::new` already rebuilt the rdeps index, graphs, and the Ready queue —
//! this pass recovers jobs that were mid-execution when the process died.

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

        // §3.5: the in-flight recovery above re-queued capacity-deferred launches
        // as it walked jobs in graph order, not enqueue order. Sort the whole
        // launch queue by the persisted `queued_at` so FIFO fairness is restored
        // exactly as it stood before the restart — the launch that waited longest
        // resumes first, and the max-wait backstop stays honest.
        self.launch_queue
            .make_contiguous()
            .sort_by_key(|q| q.queued_at);

        // §3.6: reclaim exited containers orphaned by the crash/restart. Runs
        // last so any container in-flight recovery still needs (Running tasks,
        // re-attached monitors) has been settled and protected first.
        self.sweep_exited_containers(&jobs).await;
        // §3.6: reap *running* containers no live task owns. Runs after the
        // in-flight recovery above — which may itself have relaunched fresh
        // containers for the tasks it re-Ran — so the running set it reads
        // already reflects every task this boot will resume. All of this is
        // still before the message loop starts, so no concurrent launch can
        // race the reap (single-writer ordering).
        self.sweep_orphan_running_containers(&jobs).await;
        Ok(())
    }

    /// §3.6 fleet sweep: kill running `chuggernaut.managed` containers that no
    /// live (`Running`) task owns. A crash-restart can fail an in-flight task
    /// (e.g. its container read as gone) while the container keeps running and
    /// holding a fleet slot; left alone, that slot leaks until an operator
    /// manually removes it, and every retry fails with `no free slots`. A
    /// container is kept only when it re-attaches to a live task — matched by
    /// its `(project, job, task)` identity labels or by a live task's recorded
    /// `container_id` — so recovery's monitor still lands. Anything else,
    /// including a pre-labels container with no resolvable identity, is reaped.
    ///
    /// Best-effort: a backend hiccup only warns and never blocks startup.
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

        // The containers a live task will resume: their `(project, seq, task)`
        // identity and any recorded container id. Matching either keeps a
        // container for step-2 recovery to re-attach to.
        let mut live_tasks = std::collections::HashSet::new();
        let mut live_cids = std::collections::HashSet::new();
        for job in jobs {
            let (owner, project) = split(&job.project);
            let tasks = match self.tasks.list_for_job(&owner, &project, job.id).await {
                Ok(tasks) => tasks,
                Err(e) => {
                    // Can't prove these containers are disposable — skip the
                    // whole sweep rather than risk reaping a live one.
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
            let matched = live_cids.contains(&rc.id)
                || match (&rc.project, rc.job, rc.task) {
                    (Some(p), Some(j), Some(t)) => live_tasks.contains(&(p.clone(), j, t)),
                    _ => false,
                };
            if matched {
                continue;
            }
            // Orphan: kill it to free the slot. The task it belonged to was
            // already failed by step-2 recovery; the work is lost either way,
            // so reaping (not re-adoption) is the simple, safe choice.
            match self.backend.kill(&rc.id).await {
                Ok(()) => {
                    let label = match (&rc.project, rc.job, rc.task) {
                        (Some(_), Some(j), Some(t)) => format!("{j}/{t}"),
                        _ => "unknown job/task".to_string(),
                    };
                    tracing::info!(
                        "startup fleet sweep: reaped orphan container {} for {label}",
                        rc.id
                    );
                    // Attribute the reap to its job when the identity resolves.
                    if let (Some(p), Some(j)) = (&rc.project, rc.job) {
                        let (owner, project) = split(p);
                        let _ = self
                            .publish(
                                &owner,
                                &project,
                                j,
                                "container-reaped",
                                serde_json::json!({
                                    "container_id": rc.id,
                                    "task_id": rc.task,
                                    "detail": format!(
                                        "reaped orphan container {} for {label}",
                                        rc.id
                                    ),
                                }),
                            )
                            .await;
                    }
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
            (TaskState::Pending, _) => {
                // A command work task queued under capacity pressure (§3.5) —
                // Pending, no container, not human-performed — re-queues so the
                // launch resumes when a slot frees. A Pending human/claimed
                // attempt (kind Human, or performed_by Human) waits on the inbox.
                if task.container_id.is_none()
                    && !matches!(task.kind, TaskKind::Human { .. })
                    && task.performed_by.is_none()
                {
                    self.enqueue_launch(crate::queue::QueuedLaunch {
                        owner: owner.to_string(),
                        project: project.to_string(),
                        seq,
                        task_id: task.id,
                        priority: crate::launch_queue::launch_priority(task.phase),
                        // Restore the persisted enqueue time (§3.5) so the queue's
                        // FIFO order and the max-wait clock survive this restart;
                        // fall back to now for records written before it existed.
                        queued_at: task.queued_at.unwrap_or_else(Utc::now),
                    });
                }
                Ok(())
            }
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

        // A gate in flight is superseded, whether its task was Running or queued
        // under capacity pressure (§3.5): refinalize re-opens the gate fresh.
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
                    Some(task.id),
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
                            output: result_output(r),
                        }),
                        (TaskState::Failed, _) => Some(SlotOutcome::Infra),
                        _ => None, // Pending human, queued command, or Running container
                    };
                    if task.state == TaskState::Running {
                        running.push(task.clone());
                    }
                    // An evaluator queued under capacity pressure (§3.5): Pending,
                    // no container — re-queue so the launch resumes when a slot
                    // frees; the slot stays open (outcome None) meanwhile. Covers
                    // command and agent evaluators alike (#140); a Pending Human
                    // evaluator waits on the inbox, not the launch queue.
                    if task.state == TaskState::Pending
                        && task.container_id.is_none()
                        && !matches!(task.kind, types::TaskKind::Human { .. })
                    {
                        self.enqueue_launch(crate::queue::QueuedLaunch {
                            owner: owner.to_string(),
                            project: project.to_string(),
                            seq,
                            task_id: task.id,
                            priority: crate::launch_queue::launch_priority(task.phase),
                            // Persisted enqueue time (§3.5): stable FIFO + clock
                            // across restarts. See recover_work for the rationale.
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
        let exit = match &task.container_id {
            Some(cid) => match self.backend.inspect(cid).await {
                Ok(Some(container::ContainerStatus::Running)) => {
                    // Still running across the restart: re-attach the *same*
                    // monitor the launch path uses for this phase, so exit
                    // handling is byte-identical — not a bespoke inline monitor
                    // that drops the structured result. A command work / wrap-up
                    // task's `@chug:leg` deploy report (§3.6, #187) is then
                    // harvested from its logs at exit exactly as on the launch
                    // path (`spawn_logs_monitor`), instead of vanishing as
                    // `structured: None`. A SELF-deploy always spans its own
                    // dispatcher restart, so this re-attach is the only path its
                    // report can survive on — the report the Deploys page (#188)
                    // renders. Evaluators keep their eval-result.json monitor.
                    let cid = cid.clone();
                    match task.phase {
                        TaskPhase::Work | TaskPhase::WrapUp => {
                            self.spawn_logs_monitor(owner, project, seq, task.id, cid)
                        }
                        _ => self.spawn_eval_monitor(owner, project, seq, task.id, cid),
                    }
                    return Ok(());
                }
                Ok(Some(container::ContainerStatus::Exited { exit_code })) => {
                    // A real exit the crash lost: the code is authoritative and
                    // keeps burning budget, exactly as it would have live.
                    TaskExit::code(exit_code)
                }
                // The container is GONE — pruned, node rebooted, colima
                // restarted (or the backend can't answer). We recorded an id, so
                // the container did exist; its disappearance is an infrastructure
                // loss, not a real nonzero exit. Relaunch without spending retry
                // budget (§3.6), capped so a vanishing environment still
                // escalates (`infra_loss`) rather than looping forever.
                Ok(None) | Err(_) => TaskExit::infra_loss(),
            },
            // No recorded container id (Human task, or a launch that never
            // reported one): we can't prove a container ever ran, so keep the
            // failure semantics — a -1 exit that burns budget as before.
            None => TaskExit::code(-1),
        };
        self.on_task_exited(owner, project, seq, task.id, exit)
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
                Some(task.id),
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

/// A command evaluator's captured output tail (#167), for rebuilding a slot
/// outcome from the persisted task record on restart. Only `command` results
/// embed the tail; every other result reports through structured findings.
pub(crate) fn result_output(result: &TaskResult) -> Option<String> {
    match result {
        TaskResult::Command { output, .. } if !output.is_empty() => Some(output.clone()),
        _ => None,
    }
}

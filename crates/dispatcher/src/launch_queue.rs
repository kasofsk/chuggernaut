//! Capacity-aware launch queue (spec §3.5). When placement reports no free slot
//! on any node ([`BackendError::NoCapacity`]), a container launch is *queued*
//! rather than failed: the task is parked `Pending` (no retry budget consumed)
//! and re-attempted when a running container exits and frees a slot, drained by
//! `Core::run` after every message and by the periodic scan as a backstop. A
//! launch that outwaits [`MAX_QUEUE_WAIT`] escalates with a clear reason — the
//! genuinely-wedged-fleet backstop. Only the command launch paths (work,
//! evaluator, merge gate, wrap-up) queue; genuinely-unreachable-node and other
//! launch errors keep today's fail-the-task semantics. Agent launches run
//! through the provider and are out of scope here.
//!
//! Single-writer intact: the queue lives in the actor, the slot-freed signal
//! rides the existing container-exit fan-in, and every queue mutation happens
//! on the actor thread.

use crate::core::{Core, Msg, Result, TaskExit};
use crate::exec::{ChannelRole, eval_image, task_timeout};
use crate::queue::QueuedLaunch;
use chrono::Utc;
use container::{BackendError, ContainerLaunchConfig, bootstrap_cmd};
use std::time::Duration;
use types::{JobState, JobType, Task, TaskKind, TaskPhase, TaskResult, TaskState};

/// Maximum time a launch may sit in the capacity queue before it escalates as a
/// backstop (spec §3.5). Generous: capacity pressure is expected to clear in
/// minutes as running tasks exit, so this fires only on a genuinely stuck fleet.
pub(crate) const MAX_QUEUE_WAIT: Duration = Duration::from_secs(30 * 60);

/// Escalation reason for a launch that outwaited the queue (spec §3.5). A
/// stable, clear code a later structured-escalation pass (job #76) can adopt.
pub(crate) const QUEUE_TIMEOUT_REASON: &str = "no_free_slots_timeout";

/// The container monitor a resumed launch needs — mirrors the two command
/// monitor shapes the initial launch paths spawn.
#[derive(Clone, Copy)]
pub(crate) enum MonitorKind {
    /// Work / wrap-up: harvest logs, report the exit.
    Logs,
    /// Evaluation / merge gate: additionally extract `eval-result.json`.
    Eval,
}

enum ResumeOutcome {
    /// The launch succeeded (or was reported failed for a non-capacity reason);
    /// the queue entry is retired.
    Settled,
    /// The task no longer wants launching (revoked, superseded, job left
    /// execution); the queue entry is dropped.
    Discarded,
    /// The fleet is still full; the entry is re-queued unchanged.
    NoCapacity,
}

impl Core {
    /// Build the launch config shared by every command container (work,
    /// evaluator, merge gate, wrap-up). Used by the initial launch paths and by
    /// [`Core::resume_launch`], so a queued task launches identically to a fresh
    /// one.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn command_launch_config(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        branch: &str,
        job_type: &JobType,
        secrets: &[String],
        image: String,
        run: String,
        role: ChannelRole,
        timeout: Duration,
    ) -> Result<ContainerLaunchConfig> {
        let env = self
            .container_env(
                owner,
                project,
                seq,
                branch,
                job_type,
                secrets,
                role.clone(),
                timeout,
            )
            .await?;
        Ok(ContainerLaunchConfig {
            image,
            cmd: bootstrap_cmd(&["sh".into(), "-c".into(), run]),
            env,
            files: self
                .ssh_credential_files(owner, project, seq, role, timeout)
                .await?,
            cpu_limit: job_type.resources.as_ref().and_then(|r| r.cpu),
            memory_limit: job_type.resources.as_ref().and_then(|r| r.memory.clone()),
            node: job_type.placement_node().map(String::from),
        })
    }

    /// A command launch hit [`BackendError::NoCapacity`]: park the (already
    /// persisted, `Running`) task as `Pending` and queue it for retry when a
    /// slot frees. Not a failure — no retry budget is consumed, and the task
    /// stays visibly queued rather than Failed (§3.5).
    pub(crate) async fn defer_launch(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task: &mut Task,
        reason: String,
    ) -> Result<()> {
        let queued_at = Utc::now();
        task.state = TaskState::Pending;
        task.container_id = None;
        // The task-timeout clock starts when the container actually launches,
        // so a queued task must carry no start time (§3.5 excludes Pending).
        task.started_at = None;
        // Surface *why* it is Pending and *since when*, both persisted: the UI
        // shows a "queued" badge and queued-for duration, and the same
        // `queued_at` anchors the FIFO order and the max-wait backstop across a
        // dispatcher restart (§3.5).
        task.pending_reason = Some(types::PendingReason::QueuedForCapacity);
        task.queued_at = Some(queued_at);
        self.tasks.put(task).await?;
        self.publish(
            owner,
            project,
            seq,
            "task-queued",
            serde_json::json!({
                "task_id": task.id, "phase": format!("{:?}", task.phase), "reason": reason,
            }),
        )
        .await?;
        self.launch_queue.push_back(QueuedLaunch {
            owner: owner.to_string(),
            project: project.to_string(),
            seq,
            task_id: task.id,
            queued_at,
        });
        Ok(())
    }

    /// A read-only view of the launch queue scoped to one project (spec §3.5).
    /// `depth` and each `position` are fleet-wide (the queue is one global FIFO);
    /// `entries` carries only the requested project's launches so the reply never
    /// exposes other projects' coordinates. Cheap — a walk of the in-memory
    /// queue on the actor thread — and the source for the UI's "position N of M".
    pub(crate) fn queue_snapshot(&self, owner: &str, project: &str) -> types::QueueSnapshot {
        let entries = self
            .launch_queue
            .iter()
            .enumerate()
            .filter(|(_, q)| q.owner == owner && q.project == project)
            .map(|(i, q)| types::QueueEntry {
                seq: q.seq,
                task_id: q.task_id,
                position: i + 1,
                queued_at: q.queued_at,
            })
            .collect();
        types::QueueSnapshot {
            depth: self.launch_queue.len(),
            entries,
        }
    }

    /// Attempt every queued launch once, FIFO (spec §3.5). Called after each
    /// container exit (a freed slot) and by the periodic scan. A `NoCapacity`
    /// re-queue means the fleet is still full, so draining stops — the remaining
    /// entries would only re-queue; the freed slot is spoken for.
    pub(crate) async fn drain_launch_queue(&mut self) -> Result<()> {
        // Draining (spec §3.6): the launch queue simply holds its entries. They
        // are re-derived from the Pending task records on restart (#51), so a
        // held launch resumes then rather than starting a container as we exit.
        if self.draining {
            return Ok(());
        }
        let mut remaining = self.launch_queue.len();
        while remaining > 0 {
            remaining -= 1;
            let Some(q) = self.launch_queue.pop_front() else {
                break;
            };
            match self.resume_launch(&q).await? {
                ResumeOutcome::Settled | ResumeOutcome::Discarded => continue,
                ResumeOutcome::NoCapacity => {
                    // Preserve queued_at so the backstop measures the total wait.
                    self.launch_queue.push_back(q);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Re-attempt one queued command launch against the current fleet. Rebuilds
    /// the launch from the persisted task and live exec state, so a restart or a
    /// slot freeing drives it exactly like the initial attempt.
    async fn resume_launch(&mut self, q: &QueuedLaunch) -> Result<ResumeOutcome> {
        let (owner, project, seq, task_id) = (&q.owner, &q.project, q.seq, q.task_id);
        let Some(task) = self.tasks.get(owner, project, seq, task_id).await? else {
            return Ok(ResumeOutcome::Discarded); // task record vanished
        };
        // Resolved, revoked, or superseded while it waited.
        if task.state != TaskState::Pending {
            return Ok(ResumeOutcome::Discarded);
        }
        let key = (owner.clone(), project.clone(), seq);
        // The job left execution (escalated / revoked): drop the stale entry.
        if !self.active.contains_key(&key) {
            return Ok(ResumeOutcome::Discarded);
        }
        let run = match &task.kind {
            TaskKind::Command { run } => run.clone(),
            // Only command launches are ever queued; anything else is a bug or a
            // superseded record — drop it rather than mis-launch.
            _ => return Ok(ResumeOutcome::Discarded),
        };
        let job = self.must_get(owner, project, seq)?.clone();
        let job_type = self.active.get(&key).expect("checked").job_type.clone();

        let (branch, secrets, image, role, timeout, monitor) = match task.phase {
            TaskPhase::Work => (
                job.branch.clone(),
                job_type.work.secrets.clone(),
                job_type.image.clone().unwrap_or_default(),
                ChannelRole::Work { task_id },
                self.active.get(&key).expect("checked").work_timeout(),
                MonitorKind::Logs,
            ),
            TaskPhase::Evaluation | TaskPhase::MergeGate => {
                let Some(evaluator) = job_type
                    .eval
                    .iter()
                    .find(|e| Some(&e.name) == task.evaluator.as_ref())
                    .cloned()
                else {
                    return Ok(ResumeOutcome::Discarded); // evaluator no longer declared
                };
                let branch = if task.phase == TaskPhase::MergeGate {
                    format!("merge-gate/{seq}")
                } else {
                    job.branch.clone()
                };
                (
                    branch,
                    evaluator.secrets.clone(),
                    eval_image(&job_type, &evaluator),
                    ChannelRole::Eval {
                        task_id,
                        evaluator: evaluator.name.clone(),
                    },
                    task_timeout(&job_type),
                    MonitorKind::Eval,
                )
            }
            TaskPhase::WrapUp => (
                self.repos.default_branch(owner, project).await?,
                job_type.wrap_up.secrets.clone(),
                job_type
                    .wrap_up
                    .image
                    .clone()
                    .or_else(|| job_type.image.clone())
                    .unwrap_or_default(),
                ChannelRole::Work { task_id },
                task_timeout(&job_type),
                MonitorKind::Logs,
            ),
            TaskPhase::Triage => return Ok(ResumeOutcome::Discarded),
        };

        let config = self
            .command_launch_config(
                owner, project, seq, &branch, &job_type, &secrets, image, run, role, timeout,
            )
            .await?;
        self.launch_resumed(owner, project, seq, task, config, monitor)
            .await
    }

    /// Fire one resumed launch: on success flip the task back to `Running` and
    /// spawn its monitor; on `NoCapacity` report so drain re-queues; on any other
    /// launch error, fail the task through the normal exit fan-in (as today).
    async fn launch_resumed(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        mut task: Task,
        config: ContainerLaunchConfig,
        monitor: MonitorKind,
    ) -> Result<ResumeOutcome> {
        match self.backend.launch(config).await {
            Ok(id) => {
                task.container_id = Some(id.clone());
                task.state = TaskState::Running;
                task.started_at = Some(Utc::now());
                // The launch is off the queue: clear the queued markers so the
                // UI drops the "queued" badge live and no stale reason lingers.
                task.pending_reason = None;
                task.queued_at = None;
                self.tasks.put(&task).await?;
                self.publish(
                    owner,
                    project,
                    seq,
                    "task-launched",
                    serde_json::json!({
                        "task_id": task.id, "phase": format!("{:?}", task.phase),
                    }),
                )
                .await?;
                match monitor {
                    MonitorKind::Logs => self.spawn_logs_monitor(owner, project, seq, task.id, id),
                    MonitorKind::Eval => self.spawn_eval_monitor(owner, project, seq, task.id, id),
                }
                Ok(ResumeOutcome::Settled)
            }
            Err(BackendError::NoCapacity(_)) => Ok(ResumeOutcome::NoCapacity),
            Err(e) => {
                self.report_launch_failure(owner, project, seq, task.id, e);
                Ok(ResumeOutcome::Settled)
            }
        }
    }

    /// Monitor for a command work / wrap-up container: wait, harvest logs,
    /// reclaim the overlay, report the exit. Shared by the initial launch paths
    /// and the queue resume so both behave identically.
    pub(crate) fn spawn_logs_monitor(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        id: String,
    ) {
        let backend = self.backend.clone();
        let tx = self.self_tx.clone().expect("spawned core");
        let (o, p) = (owner.to_string(), project.to_string());
        let harvest = self.harvester();
        tokio::spawn(async move {
            let exit_code = backend.wait(&id).await.unwrap_or(-1);
            harvest.collect_logs(&o, &p, seq, task_id, &id).await;
            harvest.dispose(seq, task_id, &id).await;
            let _ = tx
                .send(Msg::TaskExited {
                    owner: o,
                    project: p,
                    seq,
                    task_id,
                    exit: TaskExit::code(exit_code),
                })
                .await;
        });
    }

    /// Monitor for a command evaluator / merge-gate container: like
    /// [`Core::spawn_logs_monitor`] plus extracting `/workspace/eval-result.json`
    /// as the structured verdict (§3.3).
    pub(crate) fn spawn_eval_monitor(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        id: String,
    ) {
        let backend = self.backend.clone();
        let tx = self.self_tx.clone().expect("spawned core");
        let (o, p) = (owner.to_string(), project.to_string());
        let harvest = self.harvester();
        tokio::spawn(async move {
            let exit_code = backend.wait(&id).await.unwrap_or(-1);
            let eval_json = backend
                .copy_file(&id, "/workspace/eval-result.json")
                .await
                .ok()
                .flatten()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            harvest.collect_logs(&o, &p, seq, task_id, &id).await;
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
                        launch_error: None,
                        infra_loss: false,
                    },
                })
                .await;
        });
    }

    /// Backstop for launches wedged in the queue past [`MAX_QUEUE_WAIT`] (§3.5):
    /// fail the parked task and escalate the job with a clear reason. Run from
    /// the periodic scan.
    pub(crate) async fn scan_launch_queue_timeouts(&mut self) -> Result<()> {
        let now = Utc::now();
        let max_wait = self.config.launch_queue_max_wait.unwrap_or(MAX_QUEUE_WAIT);
        // `queued_at` is the *persisted* enqueue time (`Task::queued_at`, restored
        // into the queue entry by reconciliation), so the wait accumulates across
        // dispatcher restarts. Under frequent auto-deploys a process-local clock
        // would reset every restart and this backstop might never fire (§3.5).
        let expired: Vec<QueuedLaunch> = self
            .launch_queue
            .iter()
            .filter(|q| (now - q.queued_at).to_std().unwrap_or_default() > max_wait)
            .cloned()
            .collect();
        for q in expired {
            self.launch_queue.retain(|e| e != &q);
            let Some(mut task) = self
                .tasks
                .get(&q.owner, &q.project, q.seq, q.task_id)
                .await?
            else {
                continue;
            };
            if task.state != TaskState::Pending {
                continue; // resumed or resolved between filter and here
            }
            // Only escalate a job still in an execution state; otherwise it was
            // superseded and the stale entry just drops.
            let Ok(job) = self.must_get(&q.owner, &q.project, q.seq) else {
                continue;
            };
            if !matches!(
                job.state,
                JobState::Work | JobState::Evaluation | JobState::WrapUp
            ) {
                continue;
            }
            let waited = (now - q.queued_at).to_std().unwrap_or_default();
            task.state = TaskState::Failed;
            task.completed_at = Some(now);
            // No longer queued — drop the markers so nothing reads it as waiting.
            task.pending_reason = None;
            task.queued_at = None;
            task.result = Some(TaskResult::Command {
                pass: false,
                exit_code: -1,
                output: format!(
                    "no free fleet slot became available after waiting {waited:?} in the launch queue"
                ),
                structured: None,
            });
            self.tasks.put(&task).await?;
            self.publish(
                &q.owner,
                &q.project,
                q.seq,
                "task-failed",
                serde_json::json!({
                    "task_id": q.task_id, "phase": format!("{:?}", task.phase),
                    "reason": QUEUE_TIMEOUT_REASON,
                }),
            )
            .await?;
            self.active
                .remove(&(q.owner.clone(), q.project.clone(), q.seq));
            self.escalate(
                &q.owner,
                &q.project,
                q.seq,
                QUEUE_TIMEOUT_REASON,
                format!(
                    "Job {}: a container launch waited over {max_wait:?} for a free fleet slot \
                     and none became available. The fleet is at capacity or wedged — free \
                     capacity (finish or revoke other jobs) or revoke this one.",
                    q.seq
                ),
                Some(q.task_id),
            )
            .await?;
        }
        Ok(())
    }
}

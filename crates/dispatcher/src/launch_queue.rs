//! Capacity-aware launch queue (spec §3.5). When placement reports no free slot
//! on any node ([`BackendError::NoCapacity`]), a container launch is *queued*
//! rather than failed: the task is parked `Pending` (no retry budget consumed)
//! and re-attempted when a running container exits and frees a slot, drained by
//! `Core::run` after every message and by the periodic scan as a backstop. A
//! launch that outwaits [`MAX_QUEUE_WAIT`] escalates with a clear reason — the
//! genuinely-wedged-fleet backstop. Every launch path queues on capacity
//! pressure: the command paths (work, evaluator, merge gate, wrap-up) inline,
//! and agent evaluators via [`Msg::LaunchDeferred`] — the provider erases
//! `NoCapacity`, so the spawned run signals it back for [`Core::defer_launch`]
//! (#140). Genuinely-unreachable-node and other launch errors keep today's
//! fail-the-task semantics.
//!
//! Single-writer intact: the queue lives in the actor, the slot-freed signal
//! rides the existing container-exit fan-in, and every queue mutation happens
//! on the actor thread.
//!
//! - **Accepts:** container launch requests that hit `NoCapacity`; the
//!   slot-freed signal from container exits; scan-tick drains.
//! - **Emits:** parked `Pending` tasks (no retry budget consumed), re-attempted
//!   launches, and escalation when a launch outwaits `MAX_QUEUE_WAIT`.
//! - **Guarantees:** every queue mutation on the actor thread;
//!   unreachable-node and other launch errors keep fail-the-task semantics.
//! - **Spec:** §3.5.

use crate::core::{Core, Msg, Result, TaskExit};
use crate::exec::{ChannelRole, eval_image, task_timeout};
use crate::queue::{QueuedLaunch, launch_priority};
use crate::triage::tail;
use chrono::Utc;
use container::{BackendError, ContainerLaunchConfig, bootstrap_cmd};
use std::time::Duration;
use types::{JobState, JobType, Task, TaskKind, TaskPhase, TaskResult, TaskState};

/// Maximum time a launch may sit in the capacity queue before it escalates as a
/// backstop (spec §3.5). Generous: capacity pressure is expected to clear in
/// minutes as running tasks exit, so this fires only on a genuinely stuck fleet.
pub(crate) const MAX_QUEUE_WAIT: Duration = Duration::from_secs(30 * 60);

/// How much of a command eval / merge-gate container's captured output to carry
/// back on the exit as `TaskExit::log_tail`. The tail is where a compiler's
/// error output lands; bounding it keeps the gate-fix brief (job #154) small.
const GATE_LOG_TAIL_BYTES: usize = 8_000;

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
    /// An agent evaluator was re-spawned through the provider (#140). The launch
    /// is asynchronous — capacity is claimed by the spawned run, whose own
    /// `NoCapacity` re-defers a fresh entry — so the drain retires this entry and
    /// stops, treating the freed slot as spoken for. The next freed slot (or the
    /// periodic scan) drains any remaining entries.
    SpawnedAgent,
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
        // Preserve the original enqueue time across *re-deferrals* (§3.5): when a
        // resumed agent eval loses the slot race and signals `NoCapacity` back
        // (its record still carries the first defer's `queued_at`), the wait must
        // keep accumulating from that first defer rather than restarting the
        // max-wait backstop clock — otherwise a churning-but-full fleet could
        // reset the clock on every resume and never escalate. First defer: the
        // task has no `queued_at` yet, so stamp now. (The command paths never
        // re-enter here — they re-queue the same entry directly — so this only
        // affects the agent re-defer path; command tasks always stamp now.)
        let queued_at = task.queued_at.unwrap_or_else(Utc::now);
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
        self.enqueue_launch(QueuedLaunch {
            owner: owner.to_string(),
            project: project.to_string(),
            seq,
            task_id: task.id,
            priority: launch_priority(task.phase),
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

    /// An agent evaluator launch came back [`BackendError::NoCapacity`] from the
    /// provider (which erases the variant), reported via [`Msg::LaunchDeferred`].
    /// Park and queue it exactly as the command paths do inline (§3.5, #140).
    /// Runs on the single-writer loop, so it never races the exit handler. A
    /// no-op if the task already left its launching state (resolved, superseded)
    /// or the job is no longer active.
    pub(crate) async fn on_launch_deferred(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        reason: String,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        if !self.active.contains_key(&key) {
            return Ok(()); // job escalated/revoked out from under the launch
        }
        let Some(mut task) = self.tasks.get(owner, project, seq, task_id).await? else {
            return Ok(()); // task record vanished
        };
        // Only a live launch attempt defers; a resolved/failed record is stale
        // (a duplicate signal, or a superseded round's leftover).
        if task.state != TaskState::Running {
            return Ok(());
        }
        self.defer_launch(owner, project, seq, &mut task, reason)
            .await
    }

    /// Insert a deferred launch respecting its [`LaunchPriority`] then FIFO
    /// (spec §3.5, #140): a finishing-phase launch lands ahead of every queued
    /// work launch but behind earlier finishing launches, so eval/wrap-up drain
    /// first and neither class starves the other's ordering.
    pub(crate) fn enqueue_launch(&mut self, entry: QueuedLaunch) {
        let pos = self
            .launch_queue
            .iter()
            .position(|q| q.priority > entry.priority)
            .unwrap_or(self.launch_queue.len());
        self.launch_queue.insert(pos, entry);
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
            // Backstop at resume time too (#202): an overdue entry escalates
            // instead of burning another optimistic relaunch — the scan alone
            // misses agent evals that are mid-relaunch whenever it fires.
            let now = Utc::now();
            let max_wait = self.config.launch_queue_max_wait.unwrap_or(MAX_QUEUE_WAIT);
            if q.is_expired(now, max_wait) {
                self.expire_queued_launch(&q, now, max_wait).await?;
                continue;
            }
            match self.resume_launch(&q).await? {
                ResumeOutcome::Settled | ResumeOutcome::Discarded => continue,
                // The agent launch is async and has claimed the freed slot; its
                // own NoCapacity re-defers a fresh entry, so retire this one and
                // stop draining (#140).
                ResumeOutcome::SpawnedAgent => break,
                ResumeOutcome::NoCapacity => {
                    // Preserve queued_at so the backstop measures the total wait,
                    // and re-insert by priority (#140) so a re-queued finishing
                    // launch stays ahead of queued work rather than falling behind.
                    self.enqueue_launch(q);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Re-attempt one queued command launch against the current fleet. Rebuilds
    /// the launch from the persisted task and live exec state, so a restart or a
    /// slot freeing drives it exactly like the initial attempt.
    // TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider.
    #[allow(clippy::expect_used, clippy::too_many_lines)]
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
        // An agent evaluator relaunches through the provider, not a command
        // container (#140). Only the Evaluation fan-out ever queues an agent —
        // the merge gate is command-only — so rebuild it from the persisted
        // evaluator + session id against the job branch and re-spawn. The
        // spawned run re-defers if the fleet is still full.
        if let TaskKind::Agent { .. } = &task.kind {
            let job = self.must_get(owner, project, seq)?.clone();
            let job_type = self.active.get(&key).expect("checked").job_type.clone();
            let Some(evaluator) = job_type
                .eval
                .iter()
                .find(|e| Some(&e.name) == task.evaluator.as_ref())
                .cloned()
            else {
                return Ok(ResumeOutcome::Discarded); // evaluator no longer declared
            };
            let mut task = task;
            let session_id = task.session_id.clone();
            task.state = TaskState::Running;
            task.started_at = Some(Utc::now());
            // Off the queue → drop the "queued" badge the instant it launches
            // (§3.5), matching the command path (`launch_resumed`). The launch is
            // optimistic — the spawned run may re-hit `NoCapacity` and re-defer —
            // so keep `queued_at` on the record: it is the anchor `defer_launch`
            // preserves so the backstop clock accumulates rather than resetting.
            task.pending_reason = None;
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
            self.spawn_eval_agent(
                owner,
                project,
                seq,
                task.id,
                session_id,
                &job.branch,
                &evaluator,
            )
            .await?;
            return Ok(ResumeOutcome::SpawnedAgent);
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
            // Neither ever launches a queued command container: triage runs
            // through the agent provider, escalation tasks are Human.
            TaskPhase::Triage | TaskPhase::Escalation => {
                return Ok(ResumeOutcome::Discarded);
            }
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
    // TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider.
    #[allow(clippy::expect_used)]
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
            // Harvest the logs, and from them any structured deploy report the
            // command emitted on stdout (ticket #187) — `@chug:leg` lines and
            // the `@chug:report` envelope — so a deploy's outcome rides the exit
            // into the task's structured result instead of an opaque log.
            let structured = harvest
                .collect_logs(&o, &p, seq, task_id, &id)
                .await
                .and_then(|bytes| {
                    crate::harvest::parse_deploy_report(&String::from_utf8_lossy(&bytes))
                })
                .and_then(|report| serde_json::to_value(report).ok());
            harvest.dispose(seq, task_id, &id).await;
            let _ = tx
                .send(Msg::TaskExited {
                    owner: o,
                    project: p,
                    seq,
                    task_id,
                    exit: TaskExit {
                        structured,
                        ..TaskExit::code(exit_code)
                    },
                })
                .await;
        });
    }

    /// Monitor for a command evaluator / merge-gate container: like
    /// [`Core::spawn_logs_monitor`] plus extracting `/workspace/eval-result.json`
    /// as the structured verdict (§3.3).
    // TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider.
    #[allow(clippy::expect_used)]
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
            // Keep a tail of the container output on the exit so a failing gate
            // build stage's compiler errors can ride into the gate-fix brief
            // (job #154) — collect_logs already fetches (and stores) the bytes.
            let log_tail = harvest
                .collect_logs(&o, &p, seq, task_id, &id)
                .await
                .map(|bytes| tail(&String::from_utf8_lossy(&bytes), GATE_LOG_TAIL_BYTES))
                .filter(|s| !s.trim().is_empty());
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
                        log_tail,
                        infra_loss: false,
                        structured: None,
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
            .filter(|q| q.is_expired(now, max_wait))
            .cloned()
            .collect();
        for q in expired {
            self.launch_queue.retain(|e| e != &q);
            self.expire_queued_launch(&q, now, max_wait).await?;
        }
        Ok(())
    }

    /// Fail + escalate one launch that outwaited the queue backstop (§3.5).
    /// Shared by the periodic scan and the resume path: a starved *agent* eval
    /// ping-pongs between the queue and an optimistic re-spawn (the spawn
    /// re-defers on `NoCapacity`), so at scan time it is often mid-flight and
    /// invisible to the queue walk — the #140/#202 starvation. Checking at
    /// resume time closes the gap: however the entry is observed, an overdue
    /// wait escalates instead of burning another doomed relaunch.
    async fn expire_queued_launch(
        &mut self,
        q: &QueuedLaunch,
        now: chrono::DateTime<Utc>,
        max_wait: std::time::Duration,
    ) -> Result<()> {
        let Some(mut task) = self
            .tasks
            .get(&q.owner, &q.project, q.seq, q.task_id)
            .await?
        else {
            return Ok(());
        };
        if task.state != TaskState::Pending {
            return Ok(()); // resumed or resolved in the meantime
        }
        // Only escalate a job still in an execution state; otherwise it was
        // superseded and the stale entry just drops.
        let Ok(job) = self.must_get(&q.owner, &q.project, q.seq) else {
            return Ok(());
        };
        if !matches!(
            job.state,
            JobState::Work | JobState::Evaluation | JobState::WrapUp
        ) {
            return Ok(());
        }
        let waited = q.waited(now);
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
        Ok(())
    }
}

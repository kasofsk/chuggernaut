//! The work-execution sequence (spec §3.2): Ready→Work, container launch,
//! retry with branch recover-or-reset (§3.2 crash recovery), and the
//! rework/conflict re-entry paths. Evaluation
//! lives in `eval.rs`; both are `impl Core` blocks — the core stays the single
//! writer, these files are its execution verbs.

use crate::core::{Core, CoreError, Msg, Result, TaskExit, WorkSubmission};
use crate::escalation;
use crate::queue::QueuedJob;
use crate::release;
use agent::{AgentRunConfig, McpServerConfig};
use chrono::Utc;
use container::{ContainerLaunchConfig, bootstrap_cmd};
use std::collections::HashMap;
use std::time::Duration;
use types::{
    EscalationAction, EvalResult, Evaluator, JobState, JobType, Task, TaskKind, TaskPhase,
    TaskResolution, TaskResult, TaskState, WorkType, parse_duration,
};

/// Working memory for a job in Work/Evaluation. Restart rebuild is the
/// reconcile slice; until then a dispatcher restart drops in-flight jobs.
pub struct ExecState {
    pub job_type: JobType,
    pub cycle: u32,
    /// Eval-failure reworks consumed (`rework_budget` accounting). Conflict
    /// cycles increment `cycle` but not this counter (spec §2.1).
    pub reworks_used: u32,
    /// Latest `submit_result` payload — commit-message summary + rework context.
    pub work_submission: Option<WorkSubmission>,
    pub round: Option<crate::eval::EvalRound>,
    /// Parked candidate + gate tasks while the merge gate runs (§3.3).
    pub gate: Option<crate::eval::GateState>,
    /// §4.3 context for the current cycle's work task.
    pub eval_context: Vec<EvalResult>,
    pub merge_conflict: Option<String>,
    /// Per-job work-task timeout override (`Job.timeout`, §1.1), resolved once
    /// at Work entry. Applies to Work-phase tasks only — evaluators keep the
    /// type default. None → the type's `resources.task_timeout` applies.
    pub work_timeout: Option<Duration>,
}

impl ExecState {
    /// The timeout governing this job's Work-phase tasks: the per-job override
    /// if set, else the job type's `resources.task_timeout` (§1.1, §3.5).
    pub(crate) fn work_timeout(&self) -> Duration {
        self.work_timeout
            .unwrap_or_else(|| task_timeout(&self.job_type))
    }
}

impl Core {
    /// Ready→Work entry (§3.2 steps 1–6).
    pub(crate) async fn start_job(&mut self, q: QueuedJob) -> Result<()> {
        let job = self.must_get(&q.owner, &q.project, q.seq)?.clone();
        if job.state != JobState::Ready {
            return Ok(()); // revoked or escalated while queued
        }
        self.enter_work(&q.owner, &q.project, q.seq, 1, Vec::new(), None)
            .await
    }

    /// Shared Work entry for cycle 1, retries, rework, and conflict re-entry.
    /// Creates or resets `job/{seq}` at `base_ref`, then launches attempt 1 of
    /// the cycle's work task.
    pub(crate) async fn enter_work(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        cycle: u32,
        eval_context: Vec<EvalResult>,
        merge_conflict: Option<String>,
    ) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        let base_ref = job.base_ref.clone().ok_or_else(|| {
            CoreError::NotFound(format!("{owner}/{project}#{seq} has no base_ref"))
        })?;

        // Load the contract at base_ref; failure here is a launch-time problem.
        let job_type = match release::load_job_type(
            &self.repos,
            owner,
            project,
            &base_ref,
            &job.r#type,
            Some(seq),
        )
        .await
        .and_then(|jt| release::with_job_evaluators(jt, &job))
        {
            Ok(jt) => jt,
            Err(errs) => {
                let detail = errs
                    .iter()
                    .map(|e| format!("- {}: {}", e.field, e.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                return self
                    .escalate(
                        owner,
                        project,
                        seq,
                        "launch_validation_failed",
                        format!("Job {seq} failed launch-time validation:\n{detail}"),
                    )
                    .await;
            }
        };

        // §2.2 launch-time pass: secrets and vars re-checked before injection.
        let kv = self.kv_names(owner, project).await?;
        let missing: Vec<String> = job_type
            .work
            .secrets
            .iter()
            .filter(|s| !kv.secrets.contains(*s))
            .map(|s| format!("secret '{s}'"))
            .chain(
                job_type
                    .vars
                    .iter()
                    .filter(|v| !kv.vars.contains(*v))
                    .map(|v| format!("var '{v}'")),
            )
            .collect();
        if !missing.is_empty() {
            return self
                .escalate(
                    owner,
                    project,
                    seq,
                    "launch_validation_failed",
                    format!("Job {seq}: missing at launch: {}", missing.join(", ")),
                )
                .await;
        }

        // Create the branch on first entry; reset it on re-entry.
        if self
            .repos
            .resolve_ref(owner, project, &job.branch)
            .await
            .is_ok()
        {
            self.repos
                .reset_branch(owner, project, &job.branch, &base_ref)
                .await?;
        } else {
            self.repos
                .create_branch(owner, project, &job.branch, &base_ref)
                .await?;
        }

        if job.state != JobState::Work {
            self.set_state(&mut job, JobState::Work).await?;
        }
        if cycle == 1 {
            self.publish(
                owner,
                project,
                seq,
                "job-started",
                serde_json::json!({ "cycle": cycle }),
            )
            .await?;
        }

        // §1.1 per-job override: parseability is validated at release, so a
        // malformed string here is a stale record — fall back to the type
        // default rather than failing the launch.
        let work_timeout = job.timeout.as_deref().and_then(|s| parse_duration(s).ok());
        self.active.insert(
            (owner.to_string(), project.to_string(), seq),
            ExecState {
                job_type,
                cycle,
                reworks_used: self
                    .active
                    .get(&(owner.to_string(), project.to_string(), seq))
                    .map(|e| e.reworks_used)
                    .unwrap_or(0),
                work_submission: None,
                round: None,
                gate: None,
                eval_context,
                merge_conflict,
                work_timeout,
            },
        );
        self.launch_work_task(owner, project, seq, cycle, 1, false)
            .await
    }

    /// Create + launch one work task record (§1.2 creation rules). `resume` is
    /// set by the crash-recovery paths when the job branch was kept (not reset)
    /// because a previous attempt left commits on it; it injects a resume note
    /// into the agent prompt (§3.2 crash recovery).
    pub(crate) async fn launch_work_task(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        cycle: u32,
        attempt: u32,
        resume: bool,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let exec = self.active.get(&key).expect("exec state");
        let job_type = exec.job_type.clone();
        // §1.1 per-job override for Work-phase tasks (else type default). Drives
        // the agent run timeout and the §7.4 credential TTLs so creds outlive a
        // longer override.
        let work_timeout = exec.work_timeout();
        let (eval_context, merge_conflict) =
            (exec.eval_context.clone(), exec.merge_conflict.clone());
        let job = self.must_get(owner, project, seq)?.clone();
        let base_ref = job.base_ref.clone().expect("base_ref set in Work");

        let task_id = self.next_task_id(owner, project, seq).await?;
        let (kind, pending_human) = match job_type.work.r#type {
            WorkType::Agent => (
                TaskKind::Agent {
                    provider: provider_name(
                        job_type.work.provider,
                        self.config.agent_provider_default.as_deref(),
                    ),
                    // §12.4 model resolution for the Work agent: per-job
                    // override → job type → project default (folded into
                    // work.model by `with_defaults`) → platform default.
                    model: job
                        .model
                        .clone()
                        .or_else(|| job_type.work.model.clone())
                        .or_else(|| self.config.agent_model_default.clone()),
                    prompt: job_type.work.prompt.clone().unwrap_or_default(),
                },
                false,
            ),
            WorkType::Command => (
                TaskKind::Command {
                    run: job_type.work.run.clone().unwrap_or_default(),
                },
                false,
            ),
            WorkType::Human => (
                TaskKind::Human {
                    prompt: format!(
                        "{}{}",
                        job_type.work.prompt.clone().unwrap_or_default(),
                        job_brief_block(&job)
                    ),
                },
                true,
            ),
        };
        // §1.2 claims: a pending claim parks this attempt for the human
        // instead of launching. Consulted here — inside the single serialized
        // launch path — so an attempt is either launched or parked, never
        // both. The task keeps its DECLARED kind; the claim is recorded as
        // the performer.
        let claimed = job.claim_next;
        let parked = pending_human || claimed;
        // Minted before launch and persisted with the task, so the transcript
        // stays addressable even if the dispatcher restarts mid-run. A claimed
        // attempt runs no agent, so it gets no session.
        let session_id = (matches!(job_type.work.r#type, WorkType::Agent) && !claimed)
            .then(|| uuid::Uuid::new_v4().to_string());
        let mut task = Task {
            id: task_id,
            job_seq: seq,
            project: job.project.clone(),
            phase: TaskPhase::Work,
            cycle,
            kind,
            state: if parked {
                TaskState::Pending
            } else {
                TaskState::Running
            },
            attempt,
            evaluator: None,
            stage: 0,
            performed_by: claimed.then_some(types::Performer::Human),
            container_id: None,
            session_id: session_id.clone(),
            result: None,
            created_at: Utc::now(),
            // A claimed attempt starts now, humanly: the claim was the "I'm
            // starting" declaration (§1.2), so the parked task reads as
            // in-progress-by-human, not idle.
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
                "task_id": task_id, "phase": "Work", "cycle": cycle, "attempt": attempt,
                "performed_by": claimed.then_some("human"),
            }),
        )
        .await?;
        if claimed {
            // Consume the claim — it covers exactly this one attempt. The next
            // attempt (retry, rework) launches per the declared kind unless
            // the human claims again.
            let mut job = self.must_get(owner, project, seq)?.clone();
            job.claim_next = false;
            self.jobs.put(&job).await?;
            self.graphs
                .entry(job.project.clone())
                .or_default()
                .insert(job);
            return Ok(()); // operator resolves it via the inbox, like human work
        }
        if pending_human {
            return Ok(()); // operator inbox drives it from here (§1.2)
        }

        let env = self
            .container_env(
                owner,
                project,
                seq,
                &job.branch,
                &job_type,
                &job_type.work.secrets,
                ChannelRole::Work,
                work_timeout,
            )
            .await?;
        match job_type.work.r#type {
            WorkType::Agent => {
                let mut env = env;
                self.inject_platform_agent_secrets(&mut env).await?;
                let prompt = self
                    .build_prompt(
                        owner,
                        project,
                        &base_ref,
                        job_type.work.prompt.as_deref().unwrap_or_default(),
                        &job_brief_block(&job),
                        &eval_context,
                        merge_conflict.as_deref(),
                        resume,
                    )
                    .await?;
                let (mcp_servers, mut files) = self.channel_mcp(&env);
                files.extend(
                    self.ssh_credential_files(owner, project, seq, ChannelRole::Work, work_timeout)
                        .await?,
                );
                let config = AgentRunConfig {
                    image: job_type.image.clone().unwrap_or_default(),
                    prompt,
                    // §12.4 model resolution for the Work agent: per-job
                    // override → job type → project default (folded into
                    // work.model by `with_defaults`) → platform default.
                    model: job
                        .model
                        .clone()
                        .or_else(|| job_type.work.model.clone())
                        .or_else(|| self.config.agent_model_default.clone()),
                    // §4.4 upfront injection: tagged knowledge (tags/{tag}.md
                    // at base_ref) rides the system prompt, work agents only.
                    system_prompt: self
                        .knowledge_block(owner, project, &base_ref, &job_type, &job)
                        .await?,
                    mcp_servers,
                    files,
                    env,
                    task_timeout: work_timeout,
                    eval_context,
                    merge_conflict,
                    session_id: session_id.clone().unwrap_or_default(),
                };
                let provider = self.provider.clone();
                let tx = self.self_tx.clone().expect("spawned core");
                let (o, p) = (owner.to_string(), project.to_string());
                let harvest = self.harvester();
                tokio::spawn(async move {
                    let (exit_code, usage) = match provider.run(config).await {
                        Ok(out) => {
                            // Harvest before reporting the exit: once the task
                            // completes the job may advance, and the artifacts
                            // are the only record of how it got here.
                            let usage = harvest.collect(&o, &p, seq, task_id, &out).await;
                            // Reclaim the overlay now that logs/transcript are
                            // captured — otherwise every task leaks its build.
                            if let Some(id) = &out.container_id {
                                harvest.dispose(seq, task_id, id).await;
                            }
                            (out.exit_code, usage)
                        }
                        Err(e) => {
                            tracing::error!("agent run failed: {e}");
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
            WorkType::Command => {
                let run = job_type.work.run.clone().unwrap_or_default();
                let launch = ContainerLaunchConfig {
                    image: job_type.image.clone().unwrap_or_default(),
                    cmd: bootstrap_cmd(&["sh".into(), "-c".into(), run]),
                    env,
                    files: self
                        .ssh_credential_files(owner, project, seq, ChannelRole::Work, work_timeout)
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
                let tx = self.self_tx.clone().expect("spawned core");
                let (o, p) = (owner.to_string(), project.to_string());
                let harvest = self.harvester();
                tokio::spawn(async move {
                    let exit_code = backend.wait(&id).await.unwrap_or(-1);
                    // Logs are the only record of what a command task printed —
                    // TaskResult::Command.output has never carried it.
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
            WorkType::Human => unreachable!(),
        }
        Ok(())
    }

    /// Container exit fan-in: route by the task's phase.
    /// Handles for collecting artifacts off the actor thread, inside the
    /// per-task monitor.
    pub(crate) fn harvester(&self) -> crate::harvest::Harvester {
        crate::harvest::Harvester::new(self.backend.clone(), self.artifacts.clone())
    }

    pub(crate) async fn on_task_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        exit: TaskExit,
    ) -> Result<()> {
        let Some(task) = self.tasks.get(owner, project, seq, task_id).await? else {
            return Ok(());
        };
        match task.phase {
            TaskPhase::Work => {
                // Stale monitors (revoke, rework) may report exits for tasks
                // that already resolved; their exits are noise.
                if task.state != TaskState::Running {
                    return Ok(());
                }
                self.on_work_exited(owner, project, seq, task, exit).await
            }
            // Eval tasks can legitimately be Done already — submit_eval lands
            // before the container exits, and the exit completes the slot.
            // on_eval_exited drops anything not in the current round.
            TaskPhase::Evaluation => self.on_eval_exited(owner, project, seq, task, exit).await,
            TaskPhase::MergeGate => {
                self.on_gate_exited(owner, project, seq, task, exit.exit_code, exit.eval_json)
                    .await
            }
            // Advisory triage (§1.2): record the assessment; never touch job state.
            TaskPhase::Triage => {
                if task.state != TaskState::Running {
                    return Ok(());
                }
                self.on_triage_exited(owner, project, seq, task, exit).await
            }
        }
    }

    async fn on_work_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        mut task: Task,
        exit: TaskExit,
    ) -> Result<()> {
        let TaskExit {
            exit_code, usage, ..
        } = exit;
        let key = (owner.to_string(), project.to_string(), seq);
        task.completed_at = Some(Utc::now());
        if exit_code == 0 {
            task.state = TaskState::Done;
            // Normally already written by handle_submit_result; this covers an
            // agent that exited 0 without submitting.
            if task.result.is_none() {
                let sub = self
                    .active
                    .get(&key)
                    .and_then(|e| e.work_submission.clone());
                task.result = Some(TaskResult::Work {
                    summary: sub.as_ref().and_then(|s| s.summary.clone()),
                    structured: sub.as_ref().and_then(|s| s.structured.clone()),
                    token_usage: sub.and_then(|s| s.token_usage),
                });
            }
            // Measured usage from the CLI's own JSON result wins over the
            // agent's self-report, which it may omit or invent.
            if let (Some(measured), Some(TaskResult::Work { token_usage, .. })) =
                (usage, task.result.as_mut())
            {
                *token_usage = Some(measured);
            }
            self.tasks.put(&task).await?;
            self.publish(
                owner,
                project,
                seq,
                "task-completed",
                serde_json::json!({
                    "task_id": task.id, "phase": "Work",
                }),
            )
            .await?;
            return self.enter_evaluation(owner, project, seq).await;
        }

        task.state = TaskState::Failed;
        self.tasks.put(&task).await?;
        self.publish(
            owner,
            project,
            seq,
            "task-failed",
            serde_json::json!({
                "task_id": task.id, "phase": "Work", "exit_code": exit_code,
            }),
        )
        .await?;

        let work_retries = self
            .active
            .get(&key)
            .and_then(|e| e.job_type.work_retries)
            .unwrap_or(0);
        if task.attempt <= work_retries {
            // §2.1/§3.2: new task record, attempt++. The branch is recovered if
            // the crashed attempt pushed commits, else reset to base_ref.
            let job = self.must_get(owner, project, seq)?.clone();
            let base_ref = job.base_ref.clone().expect("base_ref set in Work");
            let resume =
                recover_or_reset_branch(&self.repos, owner, project, &job.branch, &base_ref)
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
                format!("Job {seq}: work task failed (exit {exit_code}) with no retries left"),
            )
            .await
        }
    }

    /// `req.work.submit.*` (spec §4.2): optional structured context. Idempotent
    /// once the task is Done; the exit code still decides the outcome.
    pub(crate) async fn handle_submit_result(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        submission: WorkSubmission,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let Some(exec) = self.active.get_mut(&key) else {
            return Ok(()); // job already past Work — late duplicate, ack it
        };
        exec.work_submission = Some(submission.clone());
        let cycle = exec.cycle;

        // Persist it, not just cache it: the submission arrives while the
        // container is still running (§4.2 ack-then-exit), so a dispatcher
        // restart in that window used to lose the agent's summary entirely —
        // ExecState rebuilds as None, and the commit message reads from it.
        // The task is still Running; the exit handler fills in the rest.
        if let Some(mut task) = self.running_work_task(owner, project, seq, cycle).await? {
            task.result = Some(TaskResult::Work {
                summary: submission.summary,
                structured: submission.structured,
                token_usage: submission.token_usage,
            });
            self.tasks.put(&task).await?;
        }
        Ok(())
    }

    /// The Running work task of the current cycle, if any.
    async fn running_work_task(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        cycle: u32,
    ) -> Result<Option<Task>> {
        Ok(self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .into_iter()
            .filter(|t| {
                t.phase == TaskPhase::Work
                    && t.cycle == cycle
                    && t.evaluator.is_none()
                    && t.state == TaskState::Running
            })
            .max_by_key(|t| t.id))
    }

    /// Operator resolution of a Pending Human task (§1.2): human work tasks,
    /// human evaluator tasks, and escalation tasks — the valid `kind` depends
    /// on the job state, wrong kinds are rejected (the API layer's 400).
    pub(crate) async fn handle_resolve_task(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        resolution: TaskResolution,
        operator: &str,
    ) -> Result<()> {
        let Some(mut task) = self.tasks.get(owner, project, seq, task_id).await? else {
            return Err(CoreError::NotFound(format!("task {task_id}")));
        };
        // Resolvable: Pending Human-kind tasks (declared human work, human
        // evaluators, escalations) and Pending claimed attempts of ANY kind —
        // the claim made the human the performer without changing the kind
        // (§1.2 claims).
        let human_performed = matches!(task.kind, TaskKind::Human { .. })
            || task.performed_by == Some(types::Performer::Human);
        if task.state != TaskState::Pending || !human_performed {
            return Err(CoreError::InvalidResolution(
                "only Pending Human or claimed tasks can be resolved".into(),
            ));
        }
        let job = self.must_get(owner, project, seq)?.clone();

        let complete_task = |task: &mut Task,
                             pass: bool,
                             abort: bool,
                             structured: Option<serde_json::Value>,
                             action| {
            task.result = Some(TaskResult::Human {
                pass,
                abort,
                structured,
                action,
                operator: operator.to_string(),
                resolved_at: Utc::now(),
            });
            task.state = TaskState::Done;
            task.completed_at = Some(Utc::now());
        };

        match (job.state, resolution) {
            // Post-work escalation task (§1.2): work executed, automation ran
            // out. Retry re-enters Work; Resolve re-enters Evaluation.
            (JobState::Escalated, TaskResolution::Escalation { action, structured }) => {
                complete_task(&mut task, true, false, structured, Some(action));
                self.tasks.put(&task).await?;
                self.publish(
                    owner,
                    project,
                    seq,
                    "job-escalation-resolved",
                    serde_json::json!({ "action": format!("{action:?}") }),
                )
                .await?;
                match action {
                    EscalationAction::Retry => self.escalation_retry(owner, project, seq).await,
                    EscalationAction::Resolve => {
                        // §1.2: operator did the work; submit the current
                        // branch for evaluation as-is.
                        self.ensure_exec_state(owner, project, seq).await?;
                        self.enter_evaluation(owner, project, seq).await
                    }
                    EscalationAction::Revoke => {
                        self.revoke_job(owner, project, seq).await.map(|_| ())
                    }
                }
            }
            // Pre-work escalation task (§1.2): no work task exists. Retry
            // re-runs the failed step (re-validation / re-enqueue) via
            // prework_retry; Resolve is rejected — there is nothing to submit.
            (JobState::Stalled, TaskResolution::Escalation { action, structured }) => {
                if matches!(action, EscalationAction::Resolve) {
                    return Err(CoreError::InvalidResolution(
                        "pre-work escalations accept only Retry and Revoke".into(),
                    ));
                }
                complete_task(&mut task, true, false, structured, Some(action));
                self.tasks.put(&task).await?;
                self.publish(
                    owner,
                    project,
                    seq,
                    "job-escalation-resolved",
                    serde_json::json!({ "action": format!("{action:?}") }),
                )
                .await?;
                match action {
                    EscalationAction::Retry => self.prework_retry(owner, project, seq).await,
                    EscalationAction::Revoke => {
                        self.revoke_job(owner, project, seq).await.map(|_| ())
                    }
                    EscalationAction::Resolve => unreachable!("rejected above"),
                }
            }
            (JobState::Escalated | JobState::Stalled, _) => Err(CoreError::InvalidResolution(
                "escalation tasks require kind: Escalation".into(),
            )),
            (_, TaskResolution::Escalation { .. }) => Err(CoreError::InvalidResolution(
                "kind: Escalation is only valid on escalation tasks".into(),
            )),

            // Human-performed work attempt: declared human work (§1.1
            // work.type: human) or a claimed attempt of any kind (§1.2
            // claims). `abort` on Fail is an evaluator concept — ignored here.
            (
                JobState::Work,
                TaskResolution::Pass {
                    structured,
                    summary,
                },
            ) => {
                complete_task(&mut task, true, false, structured.clone(), None);
                self.tasks.put(&task).await?;
                self.ensure_exec_state(owner, project, seq).await?;
                // The human's summary is this attempt's submit_result (§1.2
                // claims): it flows into the squash-merge commit body exactly
                // like an agent's submission.
                if summary.is_some() || structured.is_some() {
                    let key = (owner.to_string(), project.to_string(), seq);
                    if let Some(exec) = self.active.get_mut(&key) {
                        exec.work_submission = Some(WorkSubmission {
                            summary,
                            structured,
                            token_usage: None,
                        });
                    }
                }
                self.enter_evaluation(owner, project, seq).await
            }
            // Fail consumes the attempt through the normal work-failure path
            // (§1.2 claims): retries remaining → the next attempt launches per
            // the DECLARED kind (an agent picks the work right back up — no
            // un-conversion), else escalation.
            (JobState::Work, TaskResolution::Fail { structured, .. }) => {
                complete_task(&mut task, false, false, Some(structured), None);
                task.state = TaskState::Failed;
                self.tasks.put(&task).await?;
                self.publish(
                    owner,
                    project,
                    seq,
                    "task-failed",
                    serde_json::json!({
                        "task_id": task.id, "phase": "Work", "declined_by": operator,
                    }),
                )
                .await?;
                self.ensure_exec_state(owner, project, seq).await?;
                let key = (owner.to_string(), project.to_string(), seq);
                let work_retries = self
                    .active
                    .get(&key)
                    .and_then(|e| e.job_type.work_retries)
                    .unwrap_or(0);
                if task.attempt <= work_retries {
                    // §2.1: an operator fail-out is a deliberate rejection, not a
                    // crash — hard-reset to base_ref so the next attempt starts
                    // clean rather than resuming the declined work.
                    let job = self.must_get(owner, project, seq)?.clone();
                    let base_ref = job.base_ref.clone().expect("base_ref set in Work");
                    self.repos
                        .reset_branch(owner, project, &job.branch, &base_ref)
                        .await?;
                    self.launch_work_task(owner, project, seq, task.cycle, task.attempt + 1, false)
                        .await
                } else {
                    self.active.remove(&key);
                    self.escalate(
                        owner,
                        project,
                        seq,
                        "work_retries_exhausted",
                        format!(
                            "Job {seq}: work attempt failed (declined by operator) \
                             with no retries left"
                        ),
                    )
                    .await
                }
            }

            // Human evaluator task (§3.3 human).
            (JobState::Evaluation, TaskResolution::Pass { structured, .. }) => {
                complete_task(&mut task, true, false, structured.clone(), None);
                self.tasks.put(&task).await?;
                self.resolve_eval_slot(owner, project, seq, task_id, true, false, structured)
                    .await
            }
            (JobState::Evaluation, TaskResolution::Fail { structured, abort }) => {
                complete_task(&mut task, false, abort, Some(structured.clone()), None);
                self.tasks.put(&task).await?;
                self.resolve_eval_slot(owner, project, seq, task_id, false, abort, Some(structured))
                    .await
            }

            (state, _) => Err(CoreError::InvalidResolution(format!(
                "no resolvable Human task in job state {state:?}"
            ))),
        }
    }

    /// §1.2 escalation Retry: new work task, same cycle, attempt++, branch
    /// used AS-IS — the operator may have modified it. `work_retries` budget
    /// is not reset.
    async fn escalation_retry(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
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
        let last_attempt = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .iter()
            .filter(|t| {
                t.phase == TaskPhase::Work
                    && t.cycle == cycle
                    && kind_matches_work(&t.kind, work_type)
            })
            .map(|t| t.attempt)
            .max()
            .unwrap_or(0);
        let mut job = self.must_get(owner, project, seq)?.clone();
        // Branch used as-is (§1.2); if it carries commits from the escalated
        // attempt, tell the retry it is resuming so it builds on them (§3.2).
        let base_ref = job.base_ref.clone().ok_or_else(|| {
            CoreError::NotFound(format!("{owner}/{project}#{seq} has no base_ref"))
        })?;
        let resume = self
            .repos
            .has_commits_beyond(owner, project, &base_ref, &job.branch)
            .await?;
        self.set_state(&mut job, JobState::Work).await?;
        self.launch_work_task(owner, project, seq, cycle, last_attempt + 1, resume)
            .await
    }

    /// §1.2 pre-work escalation Retry: re-run Ready-transition re-validation
    /// at current HEAD. Pass → Ready + enqueue; fail → a fresh escalation
    /// task, job stays Escalated.
    async fn prework_retry(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;

        let revalidation = match release::load_job_type(
            &self.repos,
            owner,
            project,
            &head,
            &job.r#type,
            Some(seq),
        )
        .await
        .and_then(|jt| release::with_job_evaluators(jt, &job))
        {
            Ok(jt) => release::static_errors(&self.repos, owner, project, &head, &job, &jt, None)
                .await
                .and_then(|errs| if errs.is_empty() { Ok(()) } else { Err(errs) }),
            Err(errs) => Err(errs),
        };
        match revalidation {
            Ok(()) => {
                job.base_ref = Some(head);
                job.ready_at.get_or_insert_with(Utc::now);
                self.set_state(&mut job, JobState::Ready).await?;
                self.queue.enqueue(QueuedJob {
                    owner: owner.into(),
                    project: project.into(),
                    seq,
                });
                self.publish(owner, project, seq, "job-unblocked", serde_json::json!({}))
                    .await
            }
            Err(errs) => {
                let detail = errs
                    .iter()
                    .map(|e| format!("- {}: {}", e.field, e.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                let task_id = self.next_task_id(owner, project, seq).await?;
                let task = escalation::escalation_task(
                    task_id,
                    seq,
                    &job.project,
                    1,
                    format!("Job {seq} still fails re-validation at {head}:\n{detail}"),
                );
                self.tasks.put(&task).await?;
                Ok(())
            }
        }
    }

    /// Rebuild working memory for a job whose ExecState was dropped (post-
    /// escalation resume; dispatcher restart is the reconcile slice).
    /// `reworks_used` restarts at 0 — after a human owned the escalation, the
    /// budget question is theirs (TODO: derive from the event stream instead).
    pub(crate) async fn ensure_exec_state(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        if self.active.contains_key(&key) {
            return Ok(());
        }
        let job = self.must_get(owner, project, seq)?.clone();
        let base_ref = job.base_ref.clone().ok_or_else(|| {
            CoreError::InvalidResolution(format!("job {seq} has not entered execution"))
        })?;
        let job_type = release::load_job_type(
            &self.repos,
            owner,
            project,
            &base_ref,
            &job.r#type,
            Some(seq),
        )
        .await?;
        let job_type = release::with_job_evaluators(job_type, &job)?;
        let cycle = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .iter()
            .map(|t| t.cycle)
            .max()
            .unwrap_or(1);
        // Recover the submission from the task log rather than starting blank:
        // it is what the squash-merge commit message is built from.
        let work_submission = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .iter()
            .filter(|t| t.phase == TaskPhase::Work && t.cycle == cycle && t.evaluator.is_none())
            .max_by_key(|t| t.id)
            .and_then(|t| match &t.result {
                Some(TaskResult::Work {
                    summary,
                    structured,
                    token_usage,
                }) => Some(WorkSubmission {
                    summary: summary.clone(),
                    structured: structured.clone(),
                    token_usage: *token_usage,
                }),
                _ => None,
            });
        self.active.insert(
            key,
            ExecState {
                job_type,
                cycle,
                reworks_used: 0,
                work_submission,
                round: None,
                gate: None,
                eval_context: vec![],
                merge_conflict: None,
                work_timeout: job.timeout.as_deref().and_then(|s| parse_duration(s).ok()),
            },
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_prompt(
        &self,
        owner: &str,
        project: &str,
        base_ref: &str,
        prompt_path: &str,
        brief: &str,
        eval_context: &[EvalResult],
        merge_conflict: Option<&str>,
        resume: bool,
    ) -> Result<String> {
        let mut prompt = self
            .repos
            .read_file_at(owner, project, base_ref, prompt_path)
            .await?
            .unwrap_or_default();
        prompt.push_str(brief);
        if !eval_context.is_empty() || merge_conflict.is_some() {
            prompt.push_str(&rework_context_block(eval_context, merge_conflict));
        }
        if resume {
            prompt.push_str(RESUME_NOTE_BLOCK);
        }
        Ok(prompt)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn container_env(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        branch: &str,
        job_type: &JobType,
        secrets_declared: &[String],
        role: ChannelRole,
        // §7.4 credential TTL: the resolved timeout of the task these creds
        // serve (work override or type default), so a longer-running task's
        // credentials outlive it.
        creds_ttl: Duration,
    ) -> Result<HashMap<String, String>> {
        let mut env = HashMap::from([
            ("JOB_ID".into(), seq.to_string()),
            ("JOB_PROJECT".into(), format!("{owner}/{project}")),
            ("JOB_BRANCH".into(), branch.to_string()),
            (
                "BASE_BRANCH".into(),
                self.repos.default_branch(owner, project).await?,
            ),
            (
                "REPO_URL".into(),
                format!("{}/{owner}/{project}.git", self.config.repo_url_base),
            ),
            ("NATS_URL".into(), self.config.nats_url.clone()),
        ]);
        match role {
            ChannelRole::Work => {
                env.insert("CHANNEL_ROLE".into(), "work".into());
            }
            ChannelRole::Eval { task_id } => {
                env.insert("CHANNEL_ROLE".into(), "eval".into());
                env.insert("JOB_TASK_ID".into(), task_id.to_string());
            }
        }
        self.inject_git_ssh_command(&mut env);
        // §7.4: scoped credentials valid for task_timeout, minted per launch.
        if let Some(seed) = &self.config.nats_account_seed {
            let signer = auth::nats::NatsUserSigner::from_account_seed(seed)
                .map_err(|e| CoreError::Config(format!("nats account seed: {e}")))?;
            let perms = match role {
                ChannelRole::Work => auth::nats::work_container_permissions(owner, project, seq),
                ChannelRole::Eval { task_id } => {
                    auth::nats::eval_container_permissions(owner, project, seq, task_id)
                }
            };
            let ttl = chrono::Duration::from_std(creds_ttl)
                .unwrap_or_else(|_| chrono::Duration::hours(1));
            let creds = signer
                .mint_creds(
                    &format!(
                        "{owner}-{project}-{seq}-{}",
                        match role {
                            ChannelRole::Work => "work".to_string(),
                            ChannelRole::Eval { task_id } => format!("eval-{task_id}"),
                        }
                    ),
                    &perms,
                    Some(ttl),
                )
                .map_err(|e| CoreError::Config(format!("minting container creds: {e}")))?;
            env.insert("NATS_CREDS".into(), creds);
        }
        let vars = self.store.raw_bucket(store::buckets::VARS).await?;
        for name in &job_type.vars {
            if let Some(value) = vars
                .get_json::<String>(&format!("{owner}.{project}.{name}"))
                .await?
            {
                env.insert(name.clone(), value);
            }
        }
        // Reserved names (origin credentials) are rejected at release
        // validation; skipping them here too keeps them out of containers even
        // if a job record predates the rule.
        let injectable = secrets_declared
            .iter()
            .filter(|n| !n.starts_with(crate::origin::RESERVED_SECRET_PREFIX));
        match &self.secrets {
            // §8.2: age-decrypted immediately before injection.
            Some(secrets) => {
                use store::secrets::SecretStore;
                for name in injectable {
                    if let Some(value) = secrets.get(owner, project, name).await? {
                        env.insert(name.clone(), value);
                    }
                }
            }
            // Dev mode without an identity: values injected as stored.
            None => {
                let secrets = self.store.raw_bucket(store::buckets::SECRETS).await?;
                for name in injectable {
                    if let Some(value) = secrets
                        .get_json::<String>(&format!("{owner}.{project}.{name}"))
                        .await?
                    {
                        env.insert(name.clone(), value);
                    }
                }
            }
        }
        Ok(env)
    }

    /// §4.4 upfront knowledge injection, repo-versioned form: the union of
    /// the type's `knowledge:` defaults and the job's tags, each resolved to
    /// `tags/{tag}.md` at `base_ref` and concatenated into the work agent's
    /// system prompt. Tags without a file are skipped — the tag may predate
    /// its write-up. None when nothing resolves.
    pub(crate) async fn knowledge_block(
        &self,
        owner: &str,
        project: &str,
        base_ref: &str,
        job_type: &JobType,
        job: &types::Job,
    ) -> Result<Option<String>> {
        let mut tags: Vec<&str> = job_type
            .knowledge
            .iter()
            .chain(job.knowledge_tags.iter())
            .map(String::as_str)
            .collect();
        tags.dedup_by(|a, b| a == b);
        let mut seen = std::collections::HashSet::new();
        let mut block = String::new();
        for tag in tags {
            if !seen.insert(tag) {
                continue;
            }
            match self
                .repos
                .read_file_at(owner, project, base_ref, &format!("tags/{tag}.md"))
                .await?
            {
                Some(content) => {
                    block.push_str(&format!("\n### {tag}\n{content}\n"));
                }
                None => tracing::debug!("knowledge tag '{tag}' has no tags/{tag}.md at {base_ref}"),
            }
        }
        if block.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!("## Project Knowledge\n{block}")))
    }

    /// Platform agent credentials (§8.2): every secret under the reserved
    /// `global/agents` scope is injected into every *agent* container — work
    /// agents and agent evaluators — env-named by the secret. Declared
    /// (project/evaluator) secrets win on name collision. Command containers
    /// never receive them: the provider credential is agent-CLI plumbing,
    /// not task input.
    pub(crate) async fn inject_platform_agent_secrets(
        &self,
        env: &mut HashMap<String, String>,
    ) -> Result<()> {
        const SCOPE: &str = "agents";
        let owner = store::keys::RESERVED_OWNER;
        match &self.secrets {
            Some(secrets) => {
                use store::secrets::SecretStore;
                for name in secrets.list(owner, SCOPE).await? {
                    if name.starts_with(crate::origin::RESERVED_SECRET_PREFIX) {
                        continue;
                    }
                    if let Some(value) = secrets.get(owner, SCOPE, &name).await? {
                        env.entry(name).or_insert(value);
                    }
                }
            }
            // Dev mode without an identity: values injected as stored.
            None => {
                let bucket = self.store.raw_bucket(store::buckets::SECRETS).await?;
                let prefix = format!("{owner}.{SCOPE}.");
                for key in bucket.keys_with_prefix(&prefix).await? {
                    if let (Some(name), Some(value)) = (
                        key.strip_prefix(&prefix),
                        bucket.get_json::<String>(&key).await?,
                    ) && !name.starts_with(crate::origin::RESERVED_SECRET_PREFIX)
                    {
                        env.entry(name.to_string()).or_insert(value);
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn ssh_front_active(&self) -> bool {
        self.config.ssh_ca.is_some() && self.config.repo_url_base.starts_with("ssh://")
    }

    /// §5.2: repos behind the SSH front need the injected cert (paths are fixed
    /// — the credential itself rides in via `ssh_credential_files`). No SendEnv
    /// needed for git protocol v2 — git appends `-o SendEnv=GIT_PROTOCOL` itself
    /// once it detects an OpenSSH variant (the server half `AcceptEnv
    /// GIT_PROTOCOL` is sshd's config). No-op when the SSH front is inactive.
    pub(crate) fn inject_git_ssh_command(&self, env: &mut HashMap<String, String>) {
        if self.ssh_front_active() {
            env.insert(
                "GIT_SSH_COMMAND".into(),
                format!(
                    "ssh -i {SSH_ID_PATH} -o CertificateFile={SSH_CERT_PATH} \
                     -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
                     -o UserKnownHostsFile=/dev/null"
                ),
            );
        }
    }

    /// §7.4 per-job SSH credential as injected files (work rw, eval ro).
    /// Empty when the SSH front isn't configured (file:// dev repos, tests).
    pub(crate) async fn ssh_credential_files(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        role: ChannelRole,
        // §7.4 credential TTL: the resolved timeout of the task these creds
        // serve (work override or type default).
        creds_ttl: Duration,
    ) -> Result<Vec<container::InjectedFile>> {
        if !self.ssh_front_active() {
            return Ok(vec![]);
        }
        let ca = auth::ssh::SshCa::new(self.config.ssh_ca.as_ref().expect("checked"));
        let access = match role {
            ChannelRole::Work => auth::ssh::CertAccess::ReadWrite,
            ChannelRole::Eval { .. } => auth::ssh::CertAccess::ReadOnly,
        };
        let ttl =
            chrono::Duration::from_std(creds_ttl).unwrap_or_else(|_| chrono::Duration::hours(1));
        let cred = ca
            .issue_job_credential(owner, project, seq, access, ttl)
            .await
            .map_err(|e| CoreError::Config(format!("issuing job ssh cert: {e}")))?;
        Ok(vec![
            container::InjectedFile {
                container_path: SSH_ID_PATH.into(),
                contents: cred.private_key.into_bytes(),
                mode: 0o600,
                artifact: None,
            },
            container::InjectedFile {
                container_path: SSH_CERT_PATH.into(),
                contents: cred.certificate.into_bytes(),
                mode: 0o644,
                artifact: None,
            },
        ])
    }

    /// The channel MCP server entry + injected binary for agent launches
    /// (spec §4.2). Empty when no binary is configured (tests, degraded dev).
    pub(crate) fn channel_mcp(
        &self,
        env: &HashMap<String, String>,
    ) -> (Vec<McpServerConfig>, Vec<container::InjectedFile>) {
        let Some(bytes) = &self.channel_binary else {
            return (vec![], vec![]);
        };
        let path = "/usr/local/bin/chuggernaut-channel";
        // §4.2: the binary connects using NATS_URL/NATS_CREDS from
        // McpServerConfig.env (the rest of its context rides on container env).
        let mcp_env: HashMap<String, String> = ["NATS_URL", "NATS_CREDS"]
            .iter()
            .filter_map(|k| env.get(*k).map(|v| (k.to_string(), v.clone())))
            .collect();
        (
            vec![McpServerConfig {
                name: "chuggernaut-channel".into(),
                command: path.into(),
                args: vec![],
                env: mcp_env,
            }],
            vec![container::InjectedFile {
                container_path: path.into(),
                contents: bytes.clone(),
                mode: 0o755,
                // Worker nodes hold their own (worker-arch) channel binary;
                // the fleet backend sends this name instead of the bytes.
                artifact: Some(types::worker::ARTIFACT_CHANNEL.into()),
            }],
        )
    }
}

/// Selects the channel tool set (§4.2) and the §7.4 credential scope.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ChannelRole {
    Work,
    Eval { task_id: u64 },
}

/// Fixed container paths for the injected §7.4 SSH credential.
pub(crate) const SSH_ID_PATH: &str = "/chuggernaut/ssh/id";
pub(crate) const SSH_CERT_PATH: &str = "/chuggernaut/ssh/id-cert.pub";

fn kind_matches_work(kind: &TaskKind, work_type: WorkType) -> bool {
    matches!(
        (kind, work_type),
        (TaskKind::Agent { .. }, WorkType::Agent)
            | (TaskKind::Command { .. }, WorkType::Command)
            | (TaskKind::Human { .. }, WorkType::Human)
    )
}

/// §12.4 fallback chain: declaration → platform default → `claude` (tests
/// construct `CoreConfig` without a default; production always sets one).
pub(crate) fn provider_name(
    declared: Option<types::job_type::Provider>,
    platform_default: Option<&str>,
) -> String {
    declared
        .map(|p| format!("{p:?}").to_lowercase())
        .or_else(|| platform_default.map(String::from))
        .unwrap_or_else(|| "claude".into())
}

pub(crate) fn task_timeout(job_type: &JobType) -> Duration {
    job_type
        .resources
        .as_ref()
        .and_then(|r| r.task_timeout.as_deref())
        .and_then(|s| parse_duration(s).ok())
        .unwrap_or(Duration::from_secs(3600))
}

/// §4.3 job brief: the instance's ticket (title/description from job
/// creation), appended to the type's prompt for the work agent, every agent
/// evaluator, and human task prompts. Empty when the job carries neither.
pub(crate) fn job_brief_block(job: &types::Job) -> String {
    if job.title.is_empty() && job.description.is_empty() {
        return String::new();
    }
    let mut block = String::from("\n\n---\n## Job Brief\n");
    if !job.title.is_empty() {
        block.push_str(&format!("**{}**\n", job.title));
    }
    if !job.description.is_empty() {
        block.push('\n');
        block.push_str(&job.description);
        block.push('\n');
    }
    block
}

/// §3.2 crash-recovery note: appended to the work agent's prompt when the job
/// branch was recovered from an interrupted attempt rather than reset, so the
/// agent builds on the pushed commits instead of redoing them.
pub(crate) const RESUME_NOTE_BLOCK: &str = "\n\n---\n## Resuming a Previous Attempt\n\
    A previous attempt at this job was interrupted (crash, node loss, or dispatcher restart) \
    after pushing commits to your branch. Those commits have been preserved — your branch is \
    **not** a fresh checkout of the base. Review what is already there (e.g. `git log`, \
    `git diff`) before continuing, and build on it rather than redoing or duplicating the work.\n";

/// §3.2 crash recovery: prepare the deterministic job branch (`job/{seq}`) for
/// a fresh work attempt, choosing between recovering a crashed attempt's work
/// and the clean-slate reset. Deterministic naming makes this a pure lookup:
///
/// - Branch absent → create it at `base_ref`; a job with no prior attempt
///   behaves exactly as before. Returns `false` (fresh start).
/// - Branch present with commits beyond `base_ref` → a previous attempt pushed
///   before it was interrupted; keep the branch untouched so the retry resumes
///   that work. Returns `true` (the prompt should note the resume). The branch
///   may be behind the moved default branch; that stale-behind case is left for
///   the pre-eval rebase / merge gate to resolve, exactly as for a solo job.
/// - Branch present with nothing beyond `base_ref` → nothing to recover; hard-
///   reset to `base_ref` (the §2.1 clean-slate retry, a no-op here). Returns
///   `false`.
pub(crate) async fn recover_or_reset_branch(
    repos: &vcs::RepoManager,
    owner: &str,
    project: &str,
    branch: &str,
    base_ref: &str,
) -> Result<bool> {
    if repos.resolve_ref(owner, project, branch).await.is_err() {
        repos
            .create_branch(owner, project, branch, base_ref)
            .await?;
        return Ok(false);
    }
    if repos
        .has_commits_beyond(owner, project, base_ref, branch)
        .await?
    {
        Ok(true)
    } else {
        repos.reset_branch(owner, project, branch, base_ref).await?;
        Ok(false)
    }
}

/// §4.3 rework-context block appended to the prompt file content.
pub(crate) fn rework_context_block(
    eval_context: &[EvalResult],
    merge_conflict: Option<&str>,
) -> String {
    let mut block = String::from("\n\n---\n## Rework Context\n");
    if !eval_context.is_empty() {
        block.push_str("\n### Previous Evaluation Findings\n");
        for r in eval_context {
            let findings = r
                .structured
                .as_ref()
                .and_then(|v| serde_json::to_string_pretty(v).ok())
                .unwrap_or_else(|| "(no structured findings)".into());
            block.push_str(&format!(
                "**{}** (pass: {}):\n{findings}\n\n",
                r.evaluator, r.pass
            ));
        }
    }
    if let Some(conflict) = merge_conflict {
        block.push_str("\n### Merge Conflict\n");
        block.push_str(conflict);
        block.push('\n');
    }
    block
}

pub(crate) fn eval_image(job_type: &JobType, evaluator: &Evaluator) -> String {
    evaluator
        .image
        .clone()
        .or_else(|| job_type.image.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the §3.2 crash-recovery branch decision — the pure
    //! recover-or-reset lookup over a real bare repo. The end-to-end resume
    //! (crash-after-push → retry recovers + prompt notes it) is exercised in
    //! Tier-2 (`tests/execution.rs`).
    use super::recover_or_reset_branch;
    use test_utils::repo::TempRepo;

    async fn tip(repo: &TempRepo, branch: &str) -> String {
        repo.manager
            .resolve_ref(&repo.owner, &repo.project, branch)
            .await
            .expect("resolve branch")
    }

    /// Branch does not exist → created at `base_ref`, no resume. A job with no
    /// prior attempt behaves exactly as before.
    #[tokio::test]
    async fn absent_branch_is_created_fresh_no_resume() {
        let repo = TempRepo::create("acme", "api").await;
        let base = repo.head().await;

        let resume = recover_or_reset_branch(&repo.manager, "acme", "api", "job/1", &base)
            .await
            .unwrap();

        assert!(!resume, "no prior branch → fresh start, no resume note");
        assert_eq!(
            tip(&repo, "job/1").await,
            base,
            "branch created at base_ref"
        );
    }

    /// Branch exists with commits beyond `base_ref` (a crashed attempt pushed) →
    /// kept untouched and resume requested.
    #[tokio::test]
    async fn branch_with_commits_is_recovered_with_resume() {
        let repo = TempRepo::create("acme", "api").await;
        let base = repo.head().await;
        repo.create_job_branch(1, &base).await;
        let clone = repo.clone_branch("job/1").await;
        clone.commit_file("wip.rs", b"partial", "wip").await;
        clone.push("job/1").await;
        let crashed_tip = tip(&repo, "job/1").await;
        assert_ne!(crashed_tip, base, "the attempt pushed a commit");

        let resume = recover_or_reset_branch(&repo.manager, "acme", "api", "job/1", &base)
            .await
            .unwrap();

        assert!(resume, "commits beyond base_ref are recovered");
        assert_eq!(
            tip(&repo, "job/1").await,
            crashed_tip,
            "the recovered branch is kept, not reset"
        );
    }

    /// Branch exists but carries nothing beyond `base_ref` → reset (a no-op
    /// here), no resume. This is the clean-slate retry.
    #[tokio::test]
    async fn empty_branch_is_reset_no_resume() {
        let repo = TempRepo::create("acme", "api").await;
        let base = repo.head().await;
        repo.create_job_branch(1, &base).await; // created, nothing pushed

        let resume = recover_or_reset_branch(&repo.manager, "acme", "api", "job/1", &base)
            .await
            .unwrap();

        assert!(!resume, "an empty branch has nothing to recover");
        assert_eq!(tip(&repo, "job/1").await, base);
    }

    /// The recovered branch may be behind a moved default branch (a deploy
    /// landed after `base_ref` was pinned). Policy: recover as-is, do not rebase
    /// — the pre-eval rebase and merge gate handle the stale stacking later.
    #[tokio::test]
    async fn stale_behind_main_is_recovered_not_rebased() {
        let repo = TempRepo::create("acme", "api").await;
        let base = repo.head().await;

        // Default branch moves on after base_ref was pinned.
        let main_clone = repo.clone_branch("main").await;
        main_clone
            .commit_file("landed.rs", b"deploy", "deploy")
            .await;
        main_clone.push("main").await;
        let new_main = repo.head().await;
        assert_ne!(new_main, base);

        // The crashed attempt's branch, built off the OLD base_ref.
        repo.create_job_branch(1, &base).await;
        let job_clone = repo.clone_branch("job/1").await;
        job_clone.commit_file("wip.rs", b"partial", "wip").await;
        job_clone.push("job/1").await;
        let crashed_tip = tip(&repo, "job/1").await;

        let resume = recover_or_reset_branch(&repo.manager, "acme", "api", "job/1", &base)
            .await
            .unwrap();

        assert!(resume, "a branch behind the moved main is still recovered");
        assert_eq!(
            tip(&repo, "job/1").await,
            crashed_tip,
            "recovery must not rebase onto the moved main"
        );
        assert_ne!(tip(&repo, "job/1").await, new_main);
    }
}

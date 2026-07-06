//! The work-execution sequence (spec §3.2): Ready→Work, container launch,
//! retry-with-branch-reset, and the rework/conflict re-entry paths. Evaluation
//! lives in `eval.rs`; both are `impl Core` blocks — the core stays the single
//! writer, these files are its execution verbs.

use crate::core::{Core, CoreError, Msg, Result, WorkSubmission};
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
}

impl Core {
    /// Ready→Work entry (§3.2 steps 1–6).
    pub(crate) async fn start_job(&mut self, q: QueuedJob) -> Result<()> {
        let job = self.must_get(&q.owner, &q.project, q.seq)?.clone();
        if job.state != JobState::Ready {
            return Ok(()); // revoked or escalated while queued
        }
        self.enter_work(&q.owner, &q.project, q.seq, 1, Vec::new(), None).await
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
            &self.repos, owner, project, &base_ref, &job.r#type, Some(seq),
        )
        .await
        {
            Ok(jt) => jt,
            Err(errs) => {
                let detail = errs
                    .iter()
                    .map(|e| format!("- {}: {}", e.field, e.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                return self
                    .escalate(owner, project, seq, "launch_validation_failed",
                        format!("Job {seq} failed launch-time validation:\n{detail}"))
                    .await;
            }
        };

        // §2.2 launch-time pass: secrets and vars re-checked before injection.
        let kv = self.kv_names(owner, project).await?;
        let missing: Vec<String> = job_type
            .secrets
            .iter()
            .filter(|s| !kv.secrets.contains(*s))
            .map(|s| format!("secret '{s}'"))
            .chain(job_type.vars.iter().filter(|v| !kv.vars.contains(*v)).map(|v| format!("var '{v}'")))
            .collect();
        if !missing.is_empty() {
            return self
                .escalate(owner, project, seq, "launch_validation_failed",
                    format!("Job {seq}: missing at launch: {}", missing.join(", ")))
                .await;
        }

        // Create the branch on first entry; reset it on re-entry.
        if self.repos.resolve_ref(owner, project, &job.branch).await.is_ok() {
            self.repos.reset_branch(owner, project, &job.branch, &base_ref).await?;
        } else {
            self.repos.create_branch(owner, project, &job.branch, &base_ref).await?;
        }

        if job.state != JobState::Work {
            self.set_state(&mut job, JobState::Work).await?;
        }
        if cycle == 1 {
            self.publish(owner, project, seq, "job-started", serde_json::json!({ "cycle": cycle }))
                .await?;
        }

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
            },
        );
        self.launch_work_task(owner, project, seq, cycle, 1).await
    }

    /// Create + launch one work task record (§1.2 creation rules).
    pub(crate) async fn launch_work_task(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        cycle: u32,
        attempt: u32,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        let exec = self.active.get(&key).expect("exec state");
        let job_type = exec.job_type.clone();
        let (eval_context, merge_conflict) = (exec.eval_context.clone(), exec.merge_conflict.clone());
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
                    model: job_type
                        .work
                        .model
                        .clone()
                        .or_else(|| self.config.agent_model_default.clone()),
                    prompt: job_type.work.prompt.clone().unwrap_or_default(),
                },
                false,
            ),
            WorkType::Command => (
                TaskKind::Command { run: job_type.work.run.clone().unwrap_or_default() },
                false,
            ),
            WorkType::Human => (
                TaskKind::Human { prompt: job_type.work.prompt.clone().unwrap_or_default() },
                true,
            ),
        };
        let mut task = Task {
            id: task_id,
            job_seq: seq,
            project: job.project.clone(),
            phase: TaskPhase::Work,
            cycle,
            kind,
            state: if pending_human { TaskState::Pending } else { TaskState::Running },
            attempt,
            evaluator: None,
            container_id: None,
            result: None,
            created_at: Utc::now(),
            started_at: (!pending_human).then(Utc::now),
            completed_at: None,
        };
        self.tasks.put(&task).await?;
        self.publish(owner, project, seq, "task-created", serde_json::json!({
            "task_id": task_id, "phase": "Work", "cycle": cycle, "attempt": attempt,
        }))
        .await?;
        if pending_human {
            return Ok(()); // operator inbox drives it from here (§1.2)
        }

        let env = self
            .container_env(owner, project, seq, &job.branch, &job_type, ChannelRole::Work)
            .await?;
        match job_type.work.r#type {
            WorkType::Agent => {
                let prompt = self
                    .build_prompt(owner, project, &base_ref,
                        job_type.work.prompt.as_deref().unwrap_or_default(),
                        &eval_context, merge_conflict.as_deref())
                    .await?;
                let (mcp_servers, mut files) = self.channel_mcp(&env);
                files.extend(
                    self.ssh_credential_files(owner, project, seq, ChannelRole::Work, &job_type)
                        .await?,
                );
                let config = AgentRunConfig {
                    image: job_type.image.clone().unwrap_or_default(),
                    prompt,
                    model: job_type
                        .work
                        .model
                        .clone()
                        .or_else(|| self.config.agent_model_default.clone()),
                    system_prompt: None, // KO injection: knowledge slice
                    mcp_servers,
                    files,
                    env,
                    task_timeout: task_timeout(&job_type),
                    eval_context,
                    merge_conflict,
                };
                let provider = self.provider.clone();
                let tx = self.self_tx.clone().expect("spawned core");
                let (o, p) = (owner.to_string(), project.to_string());
                tokio::spawn(async move {
                    let exit_code = match provider.run(config).await {
                        Ok(out) => out.exit_code,
                        Err(e) => {
                            tracing::error!("agent run failed: {e}");
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
            WorkType::Command => {
                let run = job_type.work.run.clone().unwrap_or_default();
                let launch = ContainerLaunchConfig {
                    image: job_type.image.clone().unwrap_or_default(),
                    cmd: bootstrap_cmd(&["sh".into(), "-c".into(), run]),
                    env,
                    files: self
                        .ssh_credential_files(owner, project, seq, ChannelRole::Work, &job_type)
                        .await?,
                    cpu_limit: job_type.resources.as_ref().and_then(|r| r.cpu),
                    memory_limit: job_type.resources.as_ref().and_then(|r| r.memory.clone()),
                };
                let id = self.backend.launch(launch).await
                    .map_err(|e| CoreError::NotFound(format!("launch failed: {e}")))?;
                task.container_id = Some(id.clone());
                self.tasks.put(&task).await?;
                let backend = self.backend.clone();
                let tx = self.self_tx.clone().expect("spawned core");
                let (o, p) = (owner.to_string(), project.to_string());
                tokio::spawn(async move {
                    let exit_code = backend.wait(&id).await.unwrap_or(-1);
                    let _ = tx
                        .send(Msg::TaskExited {
                            owner: o, project: p, seq, task_id, exit_code, eval_json: None,
                        })
                        .await;
                });
            }
            WorkType::Human => unreachable!(),
        }
        Ok(())
    }

    /// Container exit fan-in: route by the task's phase.
    pub(crate) async fn on_task_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        exit_code: i32,
        eval_json: Option<serde_json::Value>,
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
                self.on_work_exited(owner, project, seq, task, exit_code).await
            }
            // Eval tasks can legitimately be Done already — submit_eval lands
            // before the container exits, and the exit completes the slot.
            // on_eval_exited drops anything not in the current round.
            TaskPhase::Evaluation => {
                self.on_eval_exited(owner, project, seq, task, exit_code, eval_json).await
            }
            TaskPhase::MergeGate => {
                self.on_gate_exited(owner, project, seq, task, exit_code, eval_json).await
            }
        }
    }

    async fn on_work_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        mut task: Task,
        exit_code: i32,
    ) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        task.completed_at = Some(Utc::now());
        if exit_code == 0 {
            task.state = TaskState::Done;
            if task.result.is_none() {
                let sub = self.active.get(&key).and_then(|e| e.work_submission.clone());
                task.result = Some(TaskResult::Work {
                    summary: sub.as_ref().and_then(|s| s.summary.clone()),
                    structured: sub.as_ref().and_then(|s| s.structured.clone()),
                    token_usage: sub.and_then(|s| s.token_usage),
                });
            }
            self.tasks.put(&task).await?;
            self.publish(owner, project, seq, "task-completed", serde_json::json!({
                "task_id": task.id, "phase": "Work",
            }))
            .await?;
            return self.enter_evaluation(owner, project, seq).await;
        }

        task.state = TaskState::Failed;
        self.tasks.put(&task).await?;
        self.publish(owner, project, seq, "task-failed", serde_json::json!({
            "task_id": task.id, "phase": "Work", "exit_code": exit_code,
        }))
        .await?;

        let work_retries = self
            .active
            .get(&key)
            .and_then(|e| e.job_type.work_retries)
            .unwrap_or(0);
        if task.attempt <= work_retries {
            // §2.1: hard-reset to base_ref, new task record, attempt++.
            let job = self.must_get(owner, project, seq)?.clone();
            let base_ref = job.base_ref.clone().expect("base_ref set in Work");
            self.repos.reset_branch(owner, project, &job.branch, &base_ref).await?;
            self.launch_work_task(owner, project, seq, task.cycle, task.attempt + 1).await
        } else {
            self.active.remove(&key);
            self.escalate(owner, project, seq, "work_retries_exhausted",
                format!("Job {seq}: work task failed (exit {exit_code}) with no retries left"))
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
        exec.work_submission = Some(submission);
        Ok(())
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
        if task.state != TaskState::Pending || !matches!(task.kind, TaskKind::Human { .. }) {
            return Err(CoreError::InvalidResolution(
                "only Pending Human tasks can be resolved".into(),
            ));
        }
        let job = self.must_get(owner, project, seq)?.clone();

        let complete_task =
            |task: &mut Task, pass: bool, structured: Option<serde_json::Value>, action| {
                task.result = Some(TaskResult::Human {
                    pass,
                    structured,
                    action,
                    operator: operator.to_string(),
                    resolved_at: Utc::now(),
                });
                task.state = TaskState::Done;
                task.completed_at = Some(Utc::now());
            };

        match (job.state, resolution) {
            // Escalation task (§1.2 escalation resolution).
            (JobState::Escalated, TaskResolution::Escalation { action, structured }) => {
                complete_task(&mut task, true, structured, Some(action));
                self.tasks.put(&task).await?;
                self.publish(owner, project, seq, "job-escalation-resolved",
                    serde_json::json!({ "action": format!("{action:?}") }))
                    .await?;
                match action {
                    EscalationAction::Retry if job.base_ref.is_none() => {
                        // Pre-work escalation (§1.2): re-run the failed step —
                        // Ready-transition re-validation — not a work task.
                        self.prework_retry(owner, project, seq).await
                    }
                    EscalationAction::Retry => self.escalation_retry(owner, project, seq).await,
                    EscalationAction::Resolve => {
                        if job.base_ref.is_none() {
                            return Err(CoreError::InvalidResolution(
                                "pre-work escalations accept only Retry and Revoke".into(),
                            ));
                        }
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
            (JobState::Escalated, _) => Err(CoreError::InvalidResolution(
                "escalation tasks require kind: Escalation".into(),
            )),
            (_, TaskResolution::Escalation { .. }) => Err(CoreError::InvalidResolution(
                "kind: Escalation is only valid on escalation tasks".into(),
            )),

            // Human work task (§1.1 work.type: human).
            (JobState::Work, TaskResolution::Pass { structured }) => {
                complete_task(&mut task, true, structured, None);
                self.tasks.put(&task).await?;
                self.ensure_exec_state(owner, project, seq).await?;
                self.enter_evaluation(owner, project, seq).await
            }
            (JobState::Work, TaskResolution::Fail { structured }) => {
                complete_task(&mut task, false, Some(structured), None);
                self.tasks.put(&task).await?;
                self.active.remove(&(owner.to_string(), project.to_string(), seq));
                self.escalate(owner, project, seq, "human_work_failed",
                    format!("Job {seq}: operator declined the human work task"))
                    .await
            }

            // Human evaluator task (§3.3 human).
            (JobState::Evaluation, TaskResolution::Pass { structured }) => {
                complete_task(&mut task, true, structured.clone(), None);
                self.tasks.put(&task).await?;
                self.resolve_eval_slot(owner, project, seq, task_id, true, structured).await
            }
            (JobState::Evaluation, TaskResolution::Fail { structured }) => {
                complete_task(&mut task, false, Some(structured.clone()), None);
                self.tasks.put(&task).await?;
                self.resolve_eval_slot(owner, project, seq, task_id, false, Some(structured)).await
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
        let work_type = self.active.get(&key).expect("exec state").job_type.work.r#type;
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
        self.set_state(&mut job, JobState::Work).await?;
        self.launch_work_task(owner, project, seq, cycle, last_attempt + 1).await
    }

    /// §1.2 pre-work escalation Retry: re-run Ready-transition re-validation
    /// at current HEAD. Pass → Ready + enqueue; fail → a fresh escalation
    /// task, job stays Escalated.
    async fn prework_retry(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self.repos.resolve_ref(owner, project, &default_branch).await?;

        let revalidation = match release::load_job_type(
            &self.repos, owner, project, &head, &job.r#type, Some(seq),
        )
        .await
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
                self.publish(owner, project, seq, "job-unblocked", serde_json::json!({})).await
            }
            Err(errs) => {
                let detail = errs
                    .iter()
                    .map(|e| format!("- {}: {}", e.field, e.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                let task_id = self.next_task_id(owner, project, seq).await?;
                let task = escalation::escalation_task(task_id, seq, &job.project, 1,
                    format!("Job {seq} still fails re-validation at {head}:\n{detail}"));
                self.tasks.put(&task).await?;
                Ok(())
            }
        }
    }

    /// Rebuild working memory for a job whose ExecState was dropped (post-
    /// escalation resume; dispatcher restart is the reconcile slice).
    /// `reworks_used` restarts at 0 — after a human owned the escalation, the
    /// budget question is theirs (TODO: derive from the event stream instead).
    pub(crate) async fn ensure_exec_state(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let key = (owner.to_string(), project.to_string(), seq);
        if self.active.contains_key(&key) {
            return Ok(());
        }
        let job = self.must_get(owner, project, seq)?.clone();
        let base_ref = job.base_ref.clone().ok_or_else(|| {
            CoreError::InvalidResolution(format!("job {seq} has not entered execution"))
        })?;
        let job_type = release::load_job_type(
            &self.repos, owner, project, &base_ref, &job.r#type, Some(seq),
        )
        .await?;
        let cycle = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .iter()
            .map(|t| t.cycle)
            .max()
            .unwrap_or(1);
        self.active.insert(key, ExecState {
            job_type,
            cycle,
            reworks_used: 0,
            work_submission: None,
            round: None,
            gate: None,
            eval_context: vec![],
            merge_conflict: None,
        });
        Ok(())
    }

    async fn build_prompt(
        &self,
        owner: &str,
        project: &str,
        base_ref: &str,
        prompt_path: &str,
        eval_context: &[EvalResult],
        merge_conflict: Option<&str>,
    ) -> Result<String> {
        let mut prompt = self
            .repos
            .read_file_at(owner, project, base_ref, prompt_path)
            .await?
            .unwrap_or_default();
        if !eval_context.is_empty() || merge_conflict.is_some() {
            prompt.push_str(&rework_context_block(eval_context, merge_conflict));
        }
        Ok(prompt)
    }

    pub(crate) async fn container_env(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        branch: &str,
        job_type: &JobType,
        role: ChannelRole,
    ) -> Result<HashMap<String, String>> {
        let mut env = HashMap::from([
            ("JOB_ID".into(), seq.to_string()),
            ("JOB_PROJECT".into(), format!("{owner}/{project}")),
            ("JOB_BRANCH".into(), branch.to_string()),
            ("BASE_BRANCH".into(), self.repos.default_branch(owner, project).await?),
            ("REPO_URL".into(), format!("{}/{owner}/{project}.git", self.config.repo_url_base)),
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
        // §5.2: repos behind the SSH front need the injected cert (paths are
        // fixed — the credential itself rides in via ssh_credential_files).
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
            let ttl = chrono::Duration::from_std(task_timeout(job_type))
                .unwrap_or_else(|_| chrono::Duration::hours(1));
            let creds = signer
                .mint_creds(
                    &format!("{owner}-{project}-{seq}-{}", match role {
                        ChannelRole::Work => "work".to_string(),
                        ChannelRole::Eval { task_id } => format!("eval-{task_id}"),
                    }),
                    &perms,
                    Some(ttl),
                )
                .map_err(|e| CoreError::Config(format!("minting container creds: {e}")))?;
            env.insert("NATS_CREDS".into(), creds);
        }
        let vars = self.store.raw_bucket(store::buckets::VARS).await?;
        for name in &job_type.vars {
            if let Some(value) = vars.get_json::<String>(&format!("{owner}.{project}.{name}")).await? {
                env.insert(name.clone(), value);
            }
        }
        match &self.secrets {
            // §8.2: age-decrypted immediately before injection.
            Some(secrets) => {
                use store::secrets::SecretStore;
                for name in &job_type.secrets {
                    if let Some(value) = secrets.get(owner, project, name).await? {
                        env.insert(name.clone(), value);
                    }
                }
            }
            // Dev mode without an identity: values injected as stored.
            None => {
                let secrets = self.store.raw_bucket(store::buckets::SECRETS).await?;
                for name in &job_type.secrets {
                    if let Some(value) =
                        secrets.get_json::<String>(&format!("{owner}.{project}.{name}")).await?
                    {
                        env.insert(name.clone(), value);
                    }
                }
            }
        }
        Ok(env)
    }

    fn ssh_front_active(&self) -> bool {
        self.config.ssh_ca.is_some() && self.config.repo_url_base.starts_with("ssh://")
    }

    /// §7.4 per-job SSH credential as injected files (work rw, eval ro).
    /// Empty when the SSH front isn't configured (file:// dev repos, tests).
    pub(crate) async fn ssh_credential_files(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        role: ChannelRole,
        job_type: &JobType,
    ) -> Result<Vec<container::InjectedFile>> {
        if !self.ssh_front_active() {
            return Ok(vec![]);
        }
        let ca = auth::ssh::SshCa::new(self.config.ssh_ca.as_ref().expect("checked"));
        let access = match role {
            ChannelRole::Work => auth::ssh::CertAccess::ReadWrite,
            ChannelRole::Eval { .. } => auth::ssh::CertAccess::ReadOnly,
        };
        let ttl = chrono::Duration::from_std(task_timeout(job_type))
            .unwrap_or_else(|_| chrono::Duration::hours(1));
        let cred = ca
            .issue_job_credential(owner, project, seq, access, ttl)
            .await
            .map_err(|e| CoreError::Config(format!("issuing job ssh cert: {e}")))?;
        Ok(vec![
            container::InjectedFile {
                container_path: SSH_ID_PATH.into(),
                contents: cred.private_key.into_bytes(),
                mode: 0o600,
            },
            container::InjectedFile {
                container_path: SSH_CERT_PATH.into(),
                contents: cred.certificate.into_bytes(),
                mode: 0o644,
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
            block.push_str(&format!("**{}** (pass: {}):\n{findings}\n\n", r.evaluator, r.pass));
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

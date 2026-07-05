//! The work-execution sequence (spec §3.2): Ready→Work, container launch,
//! retry-with-branch-reset, and the rework/conflict re-entry paths. Evaluation
//! lives in `eval.rs`; both are `impl Core` blocks — the core stays the single
//! writer, these files are its execution verbs.

use crate::core::{Core, CoreError, Msg, Result, WorkSubmission};
use crate::queue::QueuedJob;
use crate::release;
use agent::AgentRunConfig;
use chrono::Utc;
use container::{ContainerLaunchConfig, bootstrap_cmd};
use std::collections::HashMap;
use std::time::Duration;
use types::{
    EvalResult, Evaluator, JobState, JobType, Task, TaskKind, TaskPhase, TaskResult, TaskState,
    WorkType, parse_duration,
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
                    provider: provider_name(&job_type),
                    model: job_type.work.model.clone(),
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

        let env = self.container_env(owner, project, seq, &job.branch, &job_type).await?;
        match job_type.work.r#type {
            WorkType::Agent => {
                let prompt = self
                    .build_prompt(owner, project, &base_ref,
                        job_type.work.prompt.as_deref().unwrap_or_default(),
                        &eval_context, merge_conflict.as_deref())
                    .await?;
                let config = AgentRunConfig {
                    image: job_type.image.clone().unwrap_or_default(),
                    prompt,
                    model: job_type.work.model.clone(),
                    system_prompt: None, // KO injection: knowledge slice
                    mcp_servers: vec![], // channel/ko wiring: agent-provider slice
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
                    files: vec![],
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
            TaskPhase::Evaluation | TaskPhase::MergeGate => {
                self.on_eval_exited(owner, project, seq, task, exit_code, eval_json).await
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
    ) -> Result<HashMap<String, String>> {
        let mut env = HashMap::from([
            ("JOB_ID".into(), seq.to_string()),
            ("JOB_PROJECT".into(), format!("{owner}/{project}")),
            ("JOB_BRANCH".into(), branch.to_string()),
            ("BASE_BRANCH".into(), self.repos.default_branch(owner, project).await?),
            ("REPO_URL".into(), format!("{}/{owner}/{project}.git", self.config.repo_url_base)),
            ("NATS_URL".into(), self.config.nats_url.clone()),
            // TODO(§7.4): short-lived scoped JWT — auth slice.
            ("NATS_TOKEN".into(), String::new()),
        ]);
        let vars = self.store.raw_bucket(store::buckets::VARS).await?;
        for name in &job_type.vars {
            if let Some(value) = vars.get_json::<String>(&format!("{owner}.{project}.{name}")).await? {
                env.insert(name.clone(), value);
            }
        }
        // TODO(§8.2): age-decrypt — SecretStore dispatcher-side construction.
        let secrets = self.store.raw_bucket(store::buckets::SECRETS).await?;
        for name in &job_type.secrets {
            if let Some(value) =
                secrets.get_json::<String>(&format!("{owner}.{project}.{name}")).await?
            {
                env.insert(name.clone(), value);
            }
        }
        Ok(env)
    }
}

pub(crate) fn provider_name(job_type: &JobType) -> String {
    job_type
        .work
        .provider
        .map(|p| format!("{p:?}").to_lowercase())
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

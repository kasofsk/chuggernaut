//! Operator-dispatched triage (spec §1.2): an advisory agent run over a job's
//! full state that produces a written assessment + recommendation, recorded as
//! a `TaskPhase::Triage` task in the job.
//!
//! **Purely advisory** — triage never drives a job transition (no `state.rs`
//! change): the operator still decides Retry / Resolve / Revoke. It is available
//! only while the job is Escalated or Stalled, and may be repeated. The triage
//! agent runs in a platform-level image (`TRIAGE_IMAGE`) so it works uniformly
//! on any job type — agent, command, or human. The prompt embeds the job state
//! the dispatcher already holds (brief, escalation reason, every task's result,
//! and captured stdout logs); there is no channel MCP, so the assessment is
//! read back from the CLI's own JSON result on stdout ([`agent::claude::parse_result`]).

use crate::core::{Core, CoreError, Msg, Result, TaskExit};
use crate::exec::{ChannelRole, job_brief_block, provider_name};
use crate::release::ValidationError;
use agent::AgentRunConfig;
use chrono::Utc;
use std::collections::HashMap;
use std::time::Duration;
use store::ArtifactKind;
use types::{Job, JobState, Task, TaskKind, TaskPhase, TaskResult, TaskState};

/// Wall-clock budget for a triage run. Triage reads and reasons over the job
/// state; it is not the fallible product work, so a fixed, generous bound is
/// enough. Enforced by the provider (it kills the container on elapse).
const TRIAGE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How much of each captured stdout log to embed in the prompt — the tail,
/// which is where a failure's error output lands. Keeps the prompt bounded when
/// a job accumulated many verbose tasks.
const STDOUT_TAIL_BYTES: usize = 4_000;

/// The built-in triage instruction prompt, prepended to the assembled job-state
/// context. Platform-owned (not repo-versioned): triage is a platform
/// capability, not a per-project contract.
pub(crate) const TRIAGE_PROMPT: &str = "\
You are a triage agent for the Chuggernaut job orchestrator. A job has landed in \
an operator-intervention state (Escalated or Stalled) and a human operator has \
asked for help understanding what went wrong.

Below is the complete state of the job: its brief, why it stopped, every task \
that ran with its result, and the captured stdout of those tasks. The project \
repository has been cloned into your working directory as further context. You \
cannot act on the job or change its state — you are purely advisory.

Produce a concise written assessment with three parts:
1. What happened — the most likely root cause of the failure, grounded in the \
evidence below.
2. Recommendation — whether the operator should Retry, Resolve (submit the \
current branch for evaluation), or Revoke, and why. If the job is Stalled there \
is no work to submit, so only Retry or Revoke apply.
3. Caveats — anything you could not determine from the available evidence.

Write for a human operator who will make the final call. Do not attempt to use \
any tools to change the job; your final message is the assessment.

---
";

impl Core {
    /// Handle `req.jobs.triage.*` (spec §1.2): launch an advisory triage agent
    /// over the job. Guards on Escalated/Stalled and a configured `TRIAGE_IMAGE`;
    /// creates a `TaskPhase::Triage` task and spawns the run. Never changes job
    /// state, so it is not routed through `set_state`/`assert_transition`.
    pub async fn triage_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let job = self.must_get(owner, project, seq)?.clone();
        // §1.2: triage is an operator aid for the two intervention states.
        if !matches!(job.state, JobState::Escalated | JobState::Stalled) {
            return Err(CoreError::Conflict(format!(
                "triage is only available while a job is Escalated or Stalled; \
                 {owner}/{project}#{seq} is {:?}",
                job.state
            )));
        }
        // Platform-level image (§1.2). 422 when unset — the action is unavailable.
        let image = self.config.triage_image.clone().ok_or_else(|| {
            CoreError::Validation(vec![ValidationError::new(
                Some(seq),
                "triage",
                "TRIAGE_IMAGE is not configured; triage is unavailable",
            )])
        })?;

        let prompt = self.build_triage_prompt(owner, project, &job).await?;

        // Provider/model reuse the platform agent defaults (§1.2, §12.4).
        let provider = provider_name(None, self.config.agent_provider_default.as_deref());
        let model = self.config.agent_model_default.clone();
        let session_id = uuid::Uuid::new_v4().to_string();

        // Advisory tag: pin to the latest cycle so the task sorts with the run
        // it is triaging. Triage is not part of the rework loop.
        let existing = self.tasks.list_for_job(owner, project, seq).await?;
        let cycle = existing.iter().map(|t| t.cycle).max().unwrap_or(1);
        let task_id = existing.len() as u64 + 1;

        let task = Task {
            id: task_id,
            job_seq: seq,
            project: job.project.clone(),
            phase: TaskPhase::Triage,
            cycle,
            kind: TaskKind::Agent {
                provider,
                model: model.clone(),
                prompt: prompt.clone(),
            },
            state: TaskState::Running,
            attempt: 1,
            evaluator: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            session_id: Some(session_id.clone()),
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        };
        self.tasks.put(&task).await?;
        self.publish(
            owner,
            project,
            seq,
            "task-created",
            serde_json::json!({
                "task_id": task_id, "phase": "Triage", "cycle": cycle,
            }),
        )
        .await?;

        // Minimal launch env: clone the default branch (always present — a
        // Stalled job may have no job branch) read-only for code context, plus
        // the platform agent credentials so the CLI can authenticate. No channel
        // MCP, no NATS credentials — the prompt is self-contained (§1.2).
        let default_branch = self.repos.default_branch(owner, project).await?;
        let mut env = HashMap::from([
            ("JOB_ID".to_string(), seq.to_string()),
            ("JOB_PROJECT".to_string(), format!("{owner}/{project}")),
            ("JOB_BRANCH".to_string(), default_branch),
            (
                "REPO_URL".to_string(),
                format!("{}/{owner}/{project}.git", self.config.repo_url_base),
            ),
        ]);
        self.inject_git_ssh_command(&mut env);
        self.inject_platform_agent_secrets(&mut env).await?;
        // Read-only SSH credential for the clone (empty when no SSH front).
        let files = self
            .ssh_credential_files(
                owner,
                project,
                seq,
                ChannelRole::Eval { task_id },
                TRIAGE_TIMEOUT,
            )
            .await?;

        let config = AgentRunConfig {
            image,
            prompt,
            model,
            system_prompt: None,
            mcp_servers: vec![], // no channel MCP (§1.2)
            files,
            env,
            task_timeout: TRIAGE_TIMEOUT,
            eval_context: vec![],
            merge_conflict: None,
            session_id: session_id.clone(),
            node: None, // triage runs on the platform image; no per-type pin
        };
        let provider = self.provider.clone();
        let tx = self.self_tx.clone().expect("spawned core");
        let (o, p) = (owner.to_string(), project.to_string());
        let harvest = self.harvester();
        tokio::spawn(async move {
            let (exit_code, assessment, usage) = match provider.run(config).await {
                Ok(out) => {
                    // The assessment rides the CLI's JSON result on stdout, so
                    // harvest it before reporting the exit.
                    let (assessment, usage) =
                        harvest.collect_agent(&o, &p, seq, task_id, &out).await;
                    if let Some(id) = &out.container_id {
                        harvest.dispose(seq, task_id, id).await;
                    }
                    (out.exit_code, assessment, usage)
                }
                Err(e) => {
                    tracing::error!("triage agent run failed: {e}");
                    (-1, None, None)
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
                        assessment,
                        launch_error: None,
                    },
                })
                .await;
        });
        Ok(())
    }

    /// Record a finished triage run (spec §1.2). Writes `TaskResult::Triage` and
    /// marks the task Done (assessment captured) or Failed (none). **Never**
    /// changes job state — triage is advisory; the job stays Escalated/Stalled.
    pub(crate) async fn on_triage_exited(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        mut task: Task,
        exit: TaskExit,
    ) -> Result<()> {
        task.completed_at = Some(Utc::now());
        match exit.assessment {
            Some(assessment) => {
                task.state = TaskState::Done;
                task.result = Some(TaskResult::Triage {
                    assessment,
                    token_usage: exit.usage,
                });
                self.tasks.put(&task).await?;
                self.publish(
                    owner,
                    project,
                    seq,
                    "task-completed",
                    serde_json::json!({
                        "task_id": task.id, "phase": "Triage",
                    }),
                )
                .await?;
            }
            None => {
                // Container died or produced no parseable result. Record the
                // attempt so the operator sees it, rather than a silent no-op.
                task.state = TaskState::Failed;
                task.result = Some(TaskResult::Triage {
                    assessment:
                        "Triage produced no assessment — the agent exited without a result. \
                         Check the task's captured stdout, and retry."
                            .to_string(),
                    token_usage: exit.usage,
                });
                self.tasks.put(&task).await?;
                self.publish(
                    owner,
                    project,
                    seq,
                    "task-failed",
                    serde_json::json!({
                        "task_id": task.id, "phase": "Triage",
                    }),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Assemble the triage prompt from the job state the dispatcher already
    /// holds: the built-in instructions, the brief, why the job stopped (the
    /// pending escalation task), and every prior task with its result and
    /// captured stdout. Session transcripts are omitted by design (§1.2).
    async fn build_triage_prompt(&self, owner: &str, project: &str, job: &Job) -> Result<String> {
        let mut p = String::from(TRIAGE_PROMPT);
        p.push_str(&format!(
            "## Job {}#{} — state {:?}, type `{}`\n",
            job.project, job.id, job.state, job.r#type
        ));
        let brief = job_brief_block(job);
        if brief.is_empty() {
            p.push_str("\n(no job brief)\n");
        } else {
            p.push_str(&brief);
            p.push('\n');
        }

        let tasks = self.tasks.list_for_job(owner, project, job.id).await?;

        // Why the job stopped: the escalation/stall task summoning the human is
        // the one still Pending (the operator dispatched triage instead of
        // resolving it).
        if let Some(TaskKind::Human { prompt }) = tasks
            .iter()
            .filter(|t| t.state == TaskState::Pending && matches!(t.kind, TaskKind::Human { .. }))
            .max_by_key(|t| t.id)
            .map(|t| &t.kind)
        {
            p.push_str("\n---\n## Why the job stopped\n");
            p.push_str(prompt);
            p.push('\n');
        }

        p.push_str("\n---\n## Task log\n");
        for t in &tasks {
            // Skip prior triage tasks: an earlier assessment is not evidence,
            // and feeding it back invites drift.
            if t.phase == TaskPhase::Triage {
                continue;
            }
            let evaluator = t
                .evaluator
                .as_deref()
                .map(|e| format!(" [{e}]"))
                .unwrap_or_default();
            p.push_str(&format!(
                "\n### Task {} — {:?}{} (cycle {}, attempt {}) — {:?}\n",
                t.id, t.phase, evaluator, t.cycle, t.attempt, t.state
            ));
            p.push_str(&format!("kind: {}\n", task_kind_label(&t.kind)));
            if let Some(result) = &t.result {
                p.push_str(&format!("result: {}\n", render_task_result(result)));
            }
            if let Some(artifacts) = &self.artifacts
                && let Ok(Some(bytes)) = artifacts
                    .get(owner, project, job.id, t.id, ArtifactKind::Stdout)
                    .await
            {
                let text = String::from_utf8_lossy(&bytes);
                p.push_str(&format!(
                    "stdout (tail):\n```\n{}\n```\n",
                    tail(&text, STDOUT_TAIL_BYTES)
                ));
            }
        }
        Ok(p)
    }
}

fn task_kind_label(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::Agent { .. } => "agent",
        TaskKind::Command { .. } => "command",
        TaskKind::Human { .. } => "human",
    }
}

fn render_task_result(result: &TaskResult) -> String {
    let compact = |v: &serde_json::Value| serde_json::to_string(v).unwrap_or_else(|_| "…".into());
    let verdict = |pass: bool| if pass { "pass" } else { "fail" };
    match result {
        TaskResult::Work {
            summary,
            structured,
            ..
        } => {
            let mut s = format!(
                "work done; summary: {}",
                summary.as_deref().unwrap_or("(none)")
            );
            if let Some(st) = structured {
                s.push_str(&format!("; structured: {}", compact(st)));
            }
            s
        }
        TaskResult::Command {
            pass,
            exit_code,
            output,
            ..
        } => format!(
            "command {} (exit {}); output tail:\n{}",
            verdict(*pass),
            exit_code,
            tail(output, STDOUT_TAIL_BYTES)
        ),
        TaskResult::Agent {
            pass,
            abort,
            structured,
            ..
        } => format!(
            "agent eval {}{}; findings: {}",
            verdict(*pass),
            if *abort {
                " [abort: not satisfiable by rework]"
            } else {
                ""
            },
            structured
                .as_ref()
                .map(compact)
                .unwrap_or_else(|| "(none)".into())
        ),
        TaskResult::Human {
            pass,
            abort,
            structured,
            action,
            operator,
            ..
        } => format!(
            "human {}{}{}; operator {}; notes: {}",
            verdict(*pass),
            if *abort { " [abort]" } else { "" },
            action.map(|a| format!(" [{a:?}]")).unwrap_or_default(),
            operator,
            structured
                .as_ref()
                .map(compact)
                .unwrap_or_else(|| "(none)".into())
        ),
        // Skipped from the log above; here only for exhaustiveness.
        TaskResult::Triage { .. } => "(prior triage assessment)".into(),
    }
}

/// The last `max` bytes of `s`, on a char boundary, prefixed with an elision
/// marker when it was truncated.
fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…(truncated)…\n{}", &s[start..])
}

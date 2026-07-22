//! Task-timeout and one-shot job-deadline scans (spec §3.5). Driven by the
//! ticker in `core::spawn` (and `CoreHandle::trigger_scan` in tests); both
//! scans run inside the single-writer loop like any other message.

use crate::core::{Core, Result, TaskExit};
use crate::exec::task_timeout;
use crate::release;
use chrono::Utc;
use types::{JobState, TaskKind, TaskPhase, TaskResult, TaskState, parse_duration};

/// Prompt marker identifying deadline escalation tasks — the one-shot rule
/// (§3.5) excludes jobs whose task log contains a *resolved* one.
pub(crate) const DEADLINE_MARKER: &str = "[deadline]";

impl Core {
    pub(crate) async fn run_scans(&mut self) -> Result<()> {
        self.scan_task_timeouts().await?;
        // Backstop for launches wedged in the capacity queue (§3.5). The
        // periodic drain that retries them rides `Core::run` after this scan
        // message, like every other slot-freed retry.
        self.scan_launch_queue_timeouts().await?;
        self.scan_job_deadlines().await?;
        // Keep the platform config snapshot fresh: republish only when live
        // fleet state or deploy drift moved (spec §3.1, CD plan C). Best-effort
        // — never fails the scan.
        self.refresh_config_snapshot().await;
        Ok(())
    }

    /// Running non-Human tasks past `task_timeout`: kill the container and
    /// deliver a timeout exit through the normal failure paths (work retry /
    /// agent-eval infra / command-eval fail).
    async fn scan_task_timeouts(&mut self) -> Result<()> {
        let keys: Vec<(String, String, u64)> = self.active.keys().cloned().collect();
        let now = Utc::now();
        for (owner, project, seq) in keys {
            // The per-job override is Work-scoped (§1.1, §3.5): Work-phase tasks
            // use it, every other phase uses the type default. Enforcing the
            // split here — at kill time — is what keeps the override work-scoped.
            let (work_timeout, type_timeout) =
                match self.active.get(&(owner.clone(), project.clone(), seq)) {
                    Some(e) => (e.work_timeout(), task_timeout(&e.job_type)),
                    None => continue,
                };
            let expired: Vec<_> = self
                .tasks
                .list_for_job(&owner, &project, seq)
                .await?
                .into_iter()
                .filter(|t| {
                    let timeout = if t.phase == TaskPhase::Work {
                        work_timeout
                    } else {
                        type_timeout
                    };
                    t.state == TaskState::Running
                        && !matches!(t.kind, TaskKind::Human { .. })
                        && t.started_at
                            .is_some_and(|s| (now - s).to_std().unwrap_or_default() > timeout)
                })
                .collect();
            for task in expired {
                let timeout = if task.phase == TaskPhase::Work {
                    work_timeout
                } else {
                    type_timeout
                };
                tracing::warn!("task {}#{} timed out after {timeout:?}", seq, task.id);
                if let Some(cid) = &task.container_id {
                    let _ = self.backend.kill(cid).await;
                }
                self.on_task_exited(&owner, &project, seq, task.id, TaskExit::code(-1))
                    .await?;
            }
        }
        Ok(())
    }

    /// Jobs in Ready/Work/Evaluation past `job_deadline` (anchored at
    /// `ready_at`): kill containers, escalate once (§3.5 one-shot rule).
    async fn scan_job_deadlines(&mut self) -> Result<()> {
        let now = Utc::now();
        let candidates: Vec<(String, u64)> = self
            .graphs
            .iter()
            .flat_map(|(slug, g)| {
                g.jobs()
                    .filter(|j| {
                        matches!(
                            j.state,
                            JobState::Ready | JobState::Work | JobState::Evaluation
                        ) && j.ready_at.is_some()
                    })
                    .map(|j| (slug.clone(), j.id))
                    .collect::<Vec<_>>()
            })
            .collect();

        for (slug, seq) in candidates {
            let (owner, project) = slug.split_once('/').expect("slug");
            let (owner, project) = (owner.to_string(), project.to_string());
            let job = self.must_get(&owner, &project, seq)?.clone();

            // Deadline comes from the job type: exec state if active, else
            // loaded at base_ref (Ready jobs).
            let key = (owner.clone(), project.clone(), seq);
            let deadline_str = match self.active.get(&key) {
                Some(e) => e.job_type.job_deadline.clone(),
                None => {
                    let Some(base_ref) = job.base_ref.clone() else {
                        continue;
                    };
                    match release::load_job_type(
                        &self.repos,
                        &owner,
                        &project,
                        &base_ref,
                        &job.r#type,
                        Some(seq),
                    )
                    .await
                    {
                        Ok(jt) => jt.job_deadline,
                        Err(_) => continue, // launch path surfaces this itself
                    }
                }
            };
            let Some(deadline) = deadline_str.as_deref().and_then(|d| parse_duration(d).ok())
            else {
                continue;
            };
            let ready_at = job.ready_at.expect("filtered on ready_at");
            if (now - ready_at).to_std().unwrap_or_default() <= deadline {
                continue;
            }

            // One-shot: a resolved deadline escalation disables enforcement.
            let tasks = self.tasks.list_for_job(&owner, &project, seq).await?;
            let already_resolved = tasks.iter().any(|t| {
                matches!(&t.kind, TaskKind::Human { prompt } if prompt.starts_with(DEADLINE_MARKER))
                    && matches!(
                        &t.result,
                        Some(TaskResult::Human {
                            action: Some(_),
                            ..
                        })
                    )
            });
            if already_resolved {
                continue;
            }

            tracing::warn!("job {slug}#{seq} exceeded job_deadline {deadline:?}");
            self.kill_running_containers(&owner, &project, seq).await;
            self.queue.remove(&crate::queue::QueuedJob {
                owner: owner.clone(),
                project: project.clone(),
                seq,
            });
            self.active.remove(&key);
            let dl = deadline_str.unwrap_or_default();
            // Deadline from Ready is a pre-work escalation (no work task):
            // Stalled, resolved Retry/Revoke only. From Work/Evaluation it is
            // post-work: Escalated, where Resolve is also available (§1.2, §3.5).
            if job.state == JobState::Ready {
                self.stall(
                    &owner,
                    &project,
                    seq,
                    "job_deadline_exceeded",
                    format!(
                        "{DEADLINE_MARKER} Job {seq} exceeded its job_deadline ({dl}) \
                         before starting. Retry to re-enable pacing under your control."
                    ),
                    None,
                )
                .await?;
            } else {
                self.escalate(
                    &owner,
                    &project,
                    seq,
                    "job_deadline_exceeded",
                    format!(
                        "{DEADLINE_MARKER} Job {seq} exceeded its job_deadline ({dl}). \
                         Resolve to re-enable pacing under your control."
                    ),
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }
}

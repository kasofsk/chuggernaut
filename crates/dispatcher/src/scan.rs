//! Task-timeout and one-shot job-deadline scans (spec §3.5). Driven by the
//! ticker in `core::spawn` (and `CoreHandle::trigger_scan` in tests); both
//! scans run inside the single-writer loop like any other message.
//!
//! The schedule tick rides the same message (design #310 Decision 8): a
//! separate timer would still have to send into the actor, and riding
//! `run_scans` is what makes a restart unable to double-fire and what makes a
//! §3.6 drain suppress origination by construction.
//!
//! - **Accepts:** the periodic scan tick (or `CoreHandle::trigger_scan` in
//!   tests).
//! - **Emits:** task-timeout escalations and one-shot job-deadline
//!   transitions, and the jobs due schedules originate; also drives the
//!   launch-queue drain and config republish.
//! - **Guarantees:** every scan runs inside the single-writer loop; every wait
//!   is bounded.
//! - **Spec:** §3.5, §1.1 (schedules).

use crate::core::{Core, Result, TaskExit};
use crate::exec::task_timeout;
use crate::release;
use crate::schedules::SCHEDULE_REFRESH_TICKS;
use chrono::{DateTime, Utc};
use chuggernaut_domain::decide::schedule::{
    ScheduleDecision, ScheduleLatest, ScheduleVerdict, ScheduleView,
};
use types::{CreateSpec, JobState, TaskKind, TaskPhase, TaskResult, TaskState, parse_duration};

/// Prompt marker identifying deadline escalation tasks — the one-shot rule
/// (§3.5) excludes jobs whose task log contains a *resolved* one.
pub(crate) const DEADLINE_MARKER: &str = "[deadline]";

/// How long a dynamically-announced worker may go without an announce heartbeat
/// before the scan marks it unschedulable (spec §3.1 dynamic registration).
/// Generous relative to the daemon's 15s announce interval so a couple of
/// dropped heartbeats never trip a spurious deregistration.
pub(crate) const WORKER_HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long after the dispatcher starts a reachable worker may go without ever
/// reporting its capacity before the §8 never-observed warning fires. A few
/// minutes: long enough that a daemon coming up behind the dispatcher, or a
/// fleet mid-refresh, is not warned about, short enough that the operator hears
/// about it in the same sitting as the deploy.
pub(crate) const CAPACITY_OBSERVE_GRACE: std::time::Duration =
    std::time::Duration::from_secs(3 * 60);

/// Bounded cadence for the §8 warning (STYLE.md Tier 2 principle 3: everything
/// is bounded). One line per node per interval — loud enough to be seen in the
/// logs of an idle night, quiet enough that a fleet left in this state does not
/// bury everything else.
pub(crate) const CAPACITY_WARN_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// Is the §8 never-observed warning due for this node on this tick? Pure over
/// its inputs so the cadence is unit-tested without a fleet, a backend, or a
/// fifteen-minute wait.
///
/// Three conditions, all necessary. The node must be a *worker* — a
/// docker-endpoint node carries no capacity observation and `DOCKER_NODES`
/// legitimately owns its number. It must be **reachable**: an unreachable node
/// is already loud through the ping path, and the signature this warning names
/// is specifically "RPC works, announce does not". And it must never have
/// reported — once it has, provenance reads `node` and there is nothing to warn
/// about.
fn capacity_warning_due(
    node: &container::NodeStatus,
    now: DateTime<Utc>,
    started_at: DateTime<Utc>,
    last_warned: Option<DateTime<Utc>>,
) -> bool {
    let Some(capacity) = node.capacity else {
        return false;
    };
    if !node.available || capacity.observed_at.is_some() {
        return false;
    }
    let elapsed = |from: DateTime<Utc>| (now - from).to_std().unwrap_or_default();
    if elapsed(started_at) < CAPACITY_OBSERVE_GRACE {
        return false;
    }
    match last_warned {
        None => true,
        Some(at) => elapsed(at) >= CAPACITY_WARN_INTERVAL,
    }
}

/// The most recent job carrying `name`'s provenance, by creation — the value
/// design #310 Decision 5 derives the anchor from, so no last-fired record
/// exists to drift from the job records.
fn latest_scheduled_job(graph: &crate::graph::JobGraph, name: &str) -> Option<ScheduleLatest> {
    graph
        .jobs()
        .filter(|job| job.schedule.as_deref() == Some(name))
        .max_by_key(|job| (job.created_at, job.id))
        .map(|job| ScheduleLatest {
            seq: job.id,
            state: job.state,
            created_at: job.created_at,
            completed_at: job.completed_at,
        })
}

/// What one occurrence asks `Core::create_job` for. Every occurrence of a
/// schedule asks for the same job — a static title and ticket body, no deps and
/// no inputs (design #310 Decision 10 keeps parameterization out of v1).
fn schedule_create_spec(owner: &str, project: &str, schedule: &types::Schedule) -> CreateSpec {
    CreateSpec {
        owner: owner.to_string(),
        project: project.to_string(),
        r#type: schedule.job_type.clone(),
        title: schedule.job_title().to_string(),
        description: schedule.description.clone().unwrap_or_default(),
        cover_html: None,
        deps: vec![],
        members: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        inputs: std::collections::BTreeMap::new(),
        groups: vec![],
        factory: None,
        schedule: Some(schedule.name.clone()),
        draft: false,
    }
}

impl Core {
    pub(crate) async fn run_scans(&mut self) -> Result<()> {
        self.scan_task_timeouts().await?;
        self.scan_launch_queue_timeouts().await?;
        self.scan_worker_heartbeats();
        self.scan_never_observed_capacity();
        self.reconcile_capacity_intent();
        self.scan_job_deadlines().await?;
        self.scan_schedules().await?;
        self.refresh_config_snapshot().await;
        Ok(())
    }

    /// Originate the jobs due schedules ask for (spec §1.1 schedules): gather
    /// one view per loaded schedule, call the pure decider, perform what it
    /// decided. Nothing fires while draining (§3.6), and the reload every
    /// [`SCHEDULE_REFRESH_TICKS`] is the table's backstop.
    async fn scan_schedules(&mut self) -> Result<()> {
        if self.draining {
            return Ok(());
        }
        self.schedule_ticks = self.schedule_ticks.wrapping_add(1);
        if self.schedule_ticks.is_multiple_of(SCHEDULE_REFRESH_TICKS) {
            self.refresh_schedules().await;
        }

        let now = Utc::now();
        let slugs: Vec<String> = self.schedules.keys().cloned().collect();
        for slug in slugs {
            let Some((owner, project)) = slug.split_once('/') else {
                continue;
            };
            let (owner, project) = (owner.to_string(), project.to_string());
            let (decisions, effects) = self.decide_schedules(&slug, &owner, &project, now);
            for effect in effects {
                self.interpret(effect).await?;
            }
            for decision in decisions {
                match decision.verdict {
                    ScheduleVerdict::Skip => self.record_schedule_skip(&slug, &decision),
                    ScheduleVerdict::Fire => {
                        self.fire_schedule(&owner, &project, &decision).await;
                    }
                }
            }
        }
        Ok(())
    }

    /// Gather one project's schedule views off the in-memory table and graph,
    /// and decide them. Split out so the borrows the view holds end before the
    /// origination that follows writes anything.
    fn decide_schedules(
        &self,
        slug: &str,
        owner: &str,
        project: &str,
        now: DateTime<Utc>,
    ) -> (Vec<ScheduleDecision>, Vec<chuggernaut_domain::Effect>) {
        let Some(table) = self.schedules.get(slug) else {
            return (Vec::new(), Vec::new());
        };
        let graph = self.graphs.get(slug);
        let views: Vec<ScheduleView<'_>> = table
            .values()
            .map(|entry| ScheduleView {
                schedule: &entry.schedule,
                latest: graph.and_then(|g| latest_scheduled_job(g, &entry.schedule.name)),
                first_seen_at: entry.first_seen_at,
                last_skipped_occurrence: entry.last_skipped_occurrence,
            })
            .collect();
        chuggernaut_domain::decide::schedule::decide(owner, project, &views, now)
    }

    /// Remember the occurrence a `schedule-skipped` just reported, so a blocked
    /// schedule reports it once rather than once every 30 seconds.
    fn record_schedule_skip(&mut self, slug: &str, decision: &ScheduleDecision) {
        if let Some(entry) = self
            .schedules
            .get_mut(slug)
            .and_then(|table| table.get_mut(&decision.schedule))
        {
            entry.last_skipped_occurrence = Some(decision.occurrence_at);
        }
    }

    /// Create and release the job one due occurrence asks for, publishing
    /// `schedule-fired` on it. Every failure here is logged and left for the
    /// next occurrence — a broken project must not wedge the scan.
    async fn fire_schedule(&mut self, owner: &str, project: &str, decision: &ScheduleDecision) {
        let slug = format!("{owner}/{project}");
        let Some(schedule) = self
            .schedules
            .get(&slug)
            .and_then(|table| table.get(&decision.schedule))
            .map(|entry| entry.schedule.clone())
        else {
            return;
        };
        debug_assert!(
            !self.graphs.get(&slug).is_some_and(|g| {
                g.jobs().any(|j| {
                    j.schedule.as_deref() == Some(&schedule.name) && !j.state.is_terminal()
                })
            }),
            "schedule '{}' fired with a live job already in flight",
            schedule.name
        );

        let job = match self
            .create_job(schedule_create_spec(owner, project, &schedule))
            .await
        {
            Ok(job) => job,
            Err(e) => {
                tracing::warn!(
                    "schedule '{}' for {slug}: creating the job failed: {e}",
                    schedule.name
                );
                return;
            }
        };
        assert_eq!(
            job.schedule.as_deref(),
            Some(schedule.name.as_str()),
            "a scheduled job is created carrying its provenance"
        );

        if let Err(e) = self
            .publish(
                owner,
                project,
                job.id,
                "schedule-fired",
                serde_json::json!({
                    "schedule": schedule.name,
                    "occurrence_at": decision.occurrence_at,
                }),
            )
            .await
        {
            tracing::warn!("schedule '{}' for {slug}: {e}", schedule.name);
        }
        if let Err(e) = self.release_job(owner, project, job.id).await {
            tracing::warn!(
                "schedule '{}' for {slug}: job {} stays Frozen — release failed: {e}",
                schedule.name,
                job.id
            );
        }
    }

    /// Mark dynamically-announced workers whose announce heartbeat lapsed past
    /// the timeout unschedulable (spec §3.1 dynamic registration): the backend
    /// stops placing on them and the roster shows them down, but their running
    /// containers are untouched — `route` still reaches them and the poll-based
    /// `wait` re-attaches. Seed (`DOCKER_NODES`) nodes are never gated here; they
    /// use the ping-based health path. A fresh announce re-admits the node.
    fn scan_worker_heartbeats(&mut self) {
        let now = Utc::now();
        let timeout = self
            .config
            .worker_heartbeat_timeout
            .unwrap_or(crate::scan::WORKER_HEARTBEAT_TIMEOUT);
        let stale: Vec<String> = self
            .announced_workers
            .iter()
            .filter(|(name, _)| !self.seed_node_names.contains(*name))
            .filter(|(_, last)| (now - **last).to_std().unwrap_or_default() > timeout)
            .map(|(name, _)| name.clone())
            .collect();
        for name in stale {
            if self
                .fleet_roster
                .iter()
                .any(|n| n.name == name && n.available)
            {
                tracing::warn!(
                    node = %name,
                    "worker announce heartbeat lapsed — marking unschedulable (running containers keep running)"
                );
            }
            self.backend.mark_worker_unschedulable(&name);
            if let Some(n) = self.fleet_roster.iter_mut().find(|n| n.name == name) {
                n.available = false;
            }
        }
    }

    /// Warn about worker nodes that answer their RPCs but have never reported
    /// capacity (design #293 §8). That pairing — ping works, announce does not —
    /// *is* the denied-publish bug of 2026-07-26, under which both prod nodes
    /// ran for weeks on a `DOCKER_NODES` seed nothing had confirmed while
    /// looking perfectly healthy.
    ///
    /// This is the loud half of the §5a trade: narrowing the startup gate turns
    /// a crash into a warning, which is only the right trade while the warning
    /// is real. Bounded on both axes — one line per node per
    /// [`CAPACITY_WARN_INTERVAL`], and nothing at all until
    /// [`CAPACITY_OBSERVE_GRACE`] after the dispatcher started, so a daemon that
    /// simply came up second is never accused.
    fn scan_never_observed_capacity(&mut self) {
        let now = Utc::now();
        let live = self.backend.fleet_status();
        for node in &live {
            if !capacity_warning_due(
                node,
                now,
                self.started_at,
                self.capacity_warned_at.get(&node.name).copied(),
            ) {
                continue;
            }
            tracing::warn!(
                node = %node.name,
                slots = node.slots.unwrap_or(0),
                "worker node answers its RPCs but has NEVER reported capacity — the slot \
                 count in force is the DOCKER_NODES boot seed, unconfirmed by the node \
                 (spec §3.1 slot source). Check the daemon's event.worker.announce publish \
                 grant and that it is on a build that reports slots"
            );
            self.capacity_warned_at.insert(node.name.clone(), now);
        }
        let warned: std::collections::HashSet<&str> = live
            .iter()
            .filter(|n| n.capacity.is_some_and(|c| c.observed_at.is_none()))
            .map(|n| n.name.as_str())
            .collect();
        self.capacity_warned_at
            .retain(|name, _| warned.contains(name.as_str()));
    }

    /// Running non-Human tasks past `task_timeout`: kill the container and
    /// deliver a timeout exit through the normal failure paths (work retry /
    /// agent-eval infra / command-eval fail).
    async fn scan_task_timeouts(&mut self) -> Result<()> {
        let keys: Vec<(String, String, u64)> = self.active.keys().cloned().collect();
        let now = Utc::now();
        for (owner, project, seq) in keys {
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
                    self.spawn_timeout_harvest(&owner, &project, seq, task.id, cid.clone());
                }
                self.on_task_exited(&owner, &project, seq, task.id, TaskExit::code(-1))
                    .await?;
            }
        }
        Ok(())
    }

    /// Store a timed-out task's container logs as its `stdout.log` artifact
    /// (ticket #270), off the actor thread.
    ///
    /// A killed container's launch-path monitor normally harvests at exit — but
    /// that monitor is parked in `backend.wait`, and when the thing that broke IS
    /// the node holding the container (deploy #267: the worker daemons died
    /// mid-deploy, so every `inspect` poll answered with a transport error) the
    /// wait never returns and the task's only record is lost. This is the second,
    /// independent attempt: it runs on the exit path the timeout scan itself
    /// owns, so a task the dispatcher gives up on still leaves the log an
    /// operator reads the failure out of.
    ///
    /// Spawned, never awaited: `logs` on an unreachable node must not block the
    /// single-writer loop. The harvest writes no job/task state, and re-storing
    /// the same artifact if the monitor later wakes and harvests too is a
    /// same-key overwrite. Disposal is deliberately left to the monitor — the
    /// container may still be draining, and reclaiming it here would race the
    /// harvest we just started.
    fn spawn_timeout_harvest(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        id: container::ContainerId,
    ) {
        let harvest = self.harvester();
        let (o, p) = (owner.to_string(), project.to_string());
        tokio::spawn(async move {
            harvest.collect_logs(&o, &p, seq, task_id, &id).await;
        });
    }

    /// Jobs in Ready/Work/Evaluation past `job_deadline` (anchored at
    /// `ready_at`): kill containers, escalate once (§3.5 one-shot rule).
    #[allow(
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
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
                        Err(_) => continue,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn node(
        available: bool,
        capacity: Option<types::worker::ObservedCapacity>,
    ) -> container::NodeStatus {
        container::NodeStatus {
            name: "air".into(),
            available,
            version: Some("0.1.0+air".into()),
            refresh_outcome: None,
            slots: Some(2),
            capacity,
        }
    }

    /// The §8 never-observed warning fires for exactly one signature — a worker
    /// that answers its RPCs and has never reported capacity — and only at a
    /// bounded cadence. This is the mechanism that makes §5a's narrowed startup
    /// gate a correct trade instead of a regression, so its silence has to be
    /// as precise as its noise.
    #[test]
    fn never_observed_capacity_warning_cadence() {
        let started = Utc::now();
        let past_grace = started + chrono::Duration::from_std(CAPACITY_OBSERVE_GRACE).unwrap();
        let never = Some(types::worker::ObservedCapacity::default());

        assert!(!capacity_warning_due(
            &node(true, never),
            started,
            started,
            None
        ));
        assert!(capacity_warning_due(
            &node(true, never),
            past_grace,
            started,
            None
        ));

        let just_warned = past_grace;
        assert!(!capacity_warning_due(
            &node(true, never),
            past_grace + chrono::Duration::minutes(1),
            started,
            Some(just_warned)
        ));
        assert!(capacity_warning_due(
            &node(true, never),
            past_grace + chrono::Duration::from_std(CAPACITY_WARN_INTERVAL).unwrap(),
            started,
            Some(just_warned)
        ));

        assert!(!capacity_warning_due(
            &node(false, never),
            past_grace,
            started,
            None
        ));

        let reported = Some(types::worker::ObservedCapacity {
            mark: (1_000, 1),
            slots_max: Some(6),
            observed_at: Some(started),
        });
        assert!(!capacity_warning_due(
            &node(true, reported),
            past_grace,
            started,
            None
        ));

        assert!(!capacity_warning_due(
            &node(true, None),
            past_grace,
            started,
            None
        ));
    }
}

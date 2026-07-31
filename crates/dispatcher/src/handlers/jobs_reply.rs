//! The reply bodies the `req.jobs.*` family serves: the job record with its
//! derived fields, the resolved evaluation criteria, and the channel-progress
//! join the jobs list carries.
//!
//! Everything here is derived on read and never stored — the same rule the
//! retry/rework counts follow (§1.1). A derived field that disagreed with the
//! record would be a second source of truth, so each one is recomputed from KV
//! on every reply.
//!
//! - **Accepts:** a job's coordinates plus the store and repo ports.
//! - **Emits:** reply bytes for `req.jobs.{get,update,list,criteria,…}`.
//! - **Guarantees:** best-effort enrichment never fails a reply — an unreadable
//!   task log or channels bucket drops the derived field, and a broken job type
//!   degrades to the job's own evaluators plus an error list.
//! - **Spec:** §1.1, §4.2, §6.2; design-lifecycle.md (additive evaluators).

use super::reply::{NOT_FOUND, error_reply, ok_reply};
use crate::core::CoreError;
use store::NatsStore;
use vcs::RepoManager;

/// Resolved evaluation criteria for one job: the type's evaluators (with
/// project defaults merged, §1.1) plus the job's additive ones
/// (design-lifecycle.md), each annotated with its source. Resolved at the
/// job's pinned `base_ref`, or default-branch HEAD before Ready — the same
/// ref execution will use. Type-load failures degrade to the job's own
/// evaluators plus the error list rather than a hard error, so the UI can
/// still render something for a job whose type YAML is currently broken.
pub(super) async fn job_criteria(
    store: &NatsStore,
    repos: &RepoManager,
    owner: &str,
    project: &str,
    seq: u64,
) -> Vec<u8> {
    let job = match store.jobs().await {
        Ok(jobs) => match jobs.get(owner, project, seq).await {
            Ok(Some(job)) => job,
            Ok(None) => return NOT_FOUND.to_vec(),
            Err(e) => return error_reply(&e.into()),
        },
        Err(e) => return error_reply(&e.into()),
    };
    let reference = match &job.base_ref {
        Some(r) => r.clone(),
        None => match repos.default_branch(owner, project).await {
            Ok(branch) => match repos.resolve_ref(owner, project, &branch).await {
                Ok(head) => head,
                Err(e) => return error_reply(&CoreError::Vcs(e)),
            },
            Err(e) => return error_reply(&CoreError::Vcs(e)),
        },
    };

    let annotate = |evals: &[types::Evaluator], source: &str| -> Vec<serde_json::Value> {
        evals
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .map(|mut v| {
                v["source"] = serde_json::json!(source);
                v
            })
            .collect()
    };
    let mut evaluators = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut wrap_up = None;
    match crate::release::load_job_type(repos, owner, project, &reference, &job.r#type, Some(seq))
        .await
    {
        Ok(jt) => {
            wrap_up = Some(format!("{:?}", jt.wrap_up.r#type).to_lowercase());
            evaluators.extend(annotate(&jt.eval, "type"));
            if let Err(errs) = crate::release::with_job_evaluators(jt, &job) {
                errors.extend(
                    errs.into_iter()
                        .map(|e| format!("{}: {}", e.field, e.message)),
                );
            }
        }
        Err(errs) => {
            errors.extend(
                errs.into_iter()
                    .map(|e| format!("{}: {}", e.field, e.message)),
            );
        }
    }
    evaluators.extend(annotate(&job.eval, "job"));
    ok_reply(&serde_json::json!({
        "ref": reference,
        "wrap_up": wrap_up,
        "evaluators": evaluators,
        "errors": errors,
    }))
}

/// Each job's latest channel post, keyed by job seq (spec §4.2).
///
/// The `channels` bucket already holds exactly this — one entry per job,
/// overwritten in place — so the whole project costs one prefix scan. Attaching
/// it to the jobs list is what lets the operator UI show a live job's progress
/// line without opening an SSE replay of the project's entire event history.
///
/// Best-effort by design: the progress line is a nicety, and an unreadable
/// channels bucket must never fail the jobs list. A miss just drops the line.
pub(super) async fn latest_channel_updates(
    store: &NatsStore,
    owner: &str,
    project: &str,
) -> std::collections::HashMap<u64, types::ChannelUpdate> {
    let Ok(bucket) = store.raw_bucket(store::buckets::CHANNELS).await else {
        return std::collections::HashMap::new();
    };
    let prefix = format!("{owner}.{project}.jobs.");
    let entries: Vec<(String, types::ChannelEntry)> = match bucket.list_prefix_keyed(&prefix).await
    {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(%owner, %project, error = %e, "channel scan failed; jobs list drops progress lines");
            return std::collections::HashMap::new();
        }
    };
    entries
        .into_iter()
        .filter_map(|(key, entry)| {
            let seq: u64 = key.strip_prefix(&prefix)?.parse().ok()?;
            Some((seq, entry.update?))
        })
        .collect()
}

pub(super) async fn fetch_job(store: &NatsStore, owner: &str, project: &str, seq: u64) -> Vec<u8> {
    let jobs = match store.jobs().await {
        Ok(j) => j,
        Err(e) => return error_reply(&e.into()),
    };
    let job = match jobs.get(owner, project, seq).await {
        Ok(Some(job)) => job,
        Ok(None) => return NOT_FOUND.to_vec(),
        Err(e) => return error_reply(&e.into()),
    };
    let tasks = match store.tasks().await {
        Ok(t) => t
            .list_for_job(owner, project, seq)
            .await
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    job_reply_with_awaiting(&job, &tasks)
}

/// Serialize a job with the derived `awaiting_human` field (fix #3): the first
/// Pending Human task in the log, if any, with its kind inferred from the job
/// state. Makes "is a human being asked to do something?" answerable from the
/// job payload — a Pending human task can sit in Work (human work), Evaluation
/// (human evaluator), or an escalation state (Escalated/Stalled). Derived on
/// read, never stored, like the retry/rework counts (§1.1).
fn job_reply_with_awaiting(job: &types::Job, tasks: &[types::Task]) -> Vec<u8> {
    use types::JobState;
    let awaiting = (!job.state.is_terminal())
        .then(|| {
            tasks.iter().find(|t| {
                t.state == types::TaskState::Pending
                    && (matches!(t.kind, types::TaskKind::Human { .. })
                        || t.performed_by == Some(types::Performer::Human))
            })
        })
        .flatten()
        .map(|t| {
            let kind = match job.state {
                JobState::Work => "work",
                JobState::Evaluation => "eval",
                _ => "escalation",
            };
            serde_json::json!({
                "task_id": t.id,
                "kind": kind,
                "claimed": t.performed_by == Some(types::Performer::Human),
            })
        });
    let mut value = serde_json::to_value(job).unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(map) = &mut value {
        map.insert(
            "awaiting_human".into(),
            awaiting.unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| br#"{"error":{"status":500}}"#.to_vec())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::job_reply_with_awaiting;
    use chrono::Utc;
    use types::{Job, JobState, Task, TaskKind, TaskPhase, TaskState};

    fn job(state: JobState) -> Job {
        Job {
            state,
            ..test_utils::fixture::job("acme/api", 1)
        }
    }

    fn human_task(id: u64, phase: TaskPhase, state: TaskState) -> Task {
        Task {
            id,
            job_seq: 1,
            project: "acme/api".into(),
            phase,
            cycle: 1,
            kind: TaskKind::Human {
                prompt: "do it".into(),
            },
            state,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            session_id: None,
            pending_reason: None,
            queued_at: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    fn awaiting(job: &Job, tasks: &[Task]) -> serde_json::Value {
        let bytes = job_reply_with_awaiting(job, tasks);
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["awaiting_human"].clone()
    }

    #[test]
    fn awaiting_human_kind_follows_state() {
        let v = awaiting(
            &job(JobState::Escalated),
            &[human_task(3, TaskPhase::Work, TaskState::Pending)],
        );
        assert_eq!(v["task_id"], 3);
        assert_eq!(v["kind"], "escalation");
        let v = awaiting(
            &job(JobState::Stalled),
            &[human_task(3, TaskPhase::Work, TaskState::Pending)],
        );
        assert_eq!(v["kind"], "escalation");
        let v = awaiting(
            &job(JobState::Work),
            &[human_task(1, TaskPhase::Work, TaskState::Pending)],
        );
        assert_eq!(v["kind"], "work");
        let v = awaiting(
            &job(JobState::Evaluation),
            &[human_task(2, TaskPhase::Evaluation, TaskState::Pending)],
        );
        assert_eq!(v["kind"], "eval");
    }

    #[test]
    fn awaiting_human_null_without_pending_human_task() {
        let v = awaiting(
            &job(JobState::Evaluation),
            &[human_task(1, TaskPhase::Evaluation, TaskState::Done)],
        );
        assert!(v.is_null());
        let v = awaiting(&job(JobState::Work), &[]);
        assert!(v.is_null());
    }

    #[test]
    fn awaiting_human_null_on_terminal_job() {
        for state in [JobState::Revoked, JobState::Done] {
            let v = awaiting(
                &job(state),
                &[human_task(3, TaskPhase::Work, TaskState::Pending)],
            );
            assert!(v.is_null(), "{state:?} should not await a human");
        }
    }
}

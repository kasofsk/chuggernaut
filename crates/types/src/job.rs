//! Job record and state machine states (spec §1.1, §2.1).

use crate::job_type::Evaluator;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A node in the project DAG. Stored in NATS KV at `jobs.{owner}.{project}.{seq}`;
/// the dispatcher is its sole writer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Sequential per project; maintained via counter in NATS KV.
    pub id: u64,
    /// `"{owner}/{repo}"` slug.
    pub project: String,
    /// Job type name; references `jobs/{type}.yaml` at `base_ref`.
    pub r#type: String,
    /// Ticket-style instance identity: what this particular run is for.
    /// The type carries the *how* (prompts, evaluators); title/description
    /// carry the *what*, and are injected into work and eval prompts as the
    /// job brief (§4.3). Empty for jobs whose prompt is self-contained.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Upstream job ids this job depends on. Edges are ordering: upstreams
    /// must be Done first (their work is in this job's base, their structured
    /// results are available to it). Plain ids, no named roles — picked at
    /// creation, validated (existence, no cycles) at release.
    #[serde(default)]
    pub deps: Vec<u64>,
    pub state: JobState,
    /// `"job/{id}"`; set at creation; actual git branch created when job enters Work.
    pub branch: String,
    /// Exact HEAD of default branch; set/updated at every Ready-transition and on
    /// squash-merge conflict; None until job first enters Ready.
    pub base_ref: Option<String>,
    /// Union of job type defaults and operator-supplied tags at creation.
    pub knowledge_tags: Vec<String>,
    /// Additive per-job evaluators (design-lifecycle.md): layered on top of the
    /// type's `eval:` list at execution. The type's evaluators are a floor —
    /// creation can add criteria, never remove or override them. Name
    /// collisions with the type's evaluators are a release-time error.
    #[serde(default)]
    pub eval: Vec<Evaluator>,
    /// Optional per-job work-task timeout override (duration string, §1.1).
    /// Layers over the job type's `resources.task_timeout` exactly like [`eval`]
    /// layers over the type's evaluators — but for Work tasks only; evaluators
    /// keep the type default. Any valid duration (shorter or longer). Parseability
    /// is validated at release, consistent with "wiring validated at release, not
    /// creation". None means the type default applies.
    #[serde(default)]
    pub timeout: Option<String>,
    /// Optional per-job model override for the Work agent (spec §1.1, §12.4).
    /// The most specific choice an operator can make, so it wins over every
    /// other layer: the job type's `work.model`, the project default
    /// (`jobs/_defaults.yaml`), and the platform default (`AGENT_MODEL_DEFAULT`).
    /// Applies to Work-phase agent tasks only — evaluators keep the
    /// type/project/platform resolution, exactly as [`Job::timeout`] scopes to
    /// Work. None → the resolution chain applies. Defaulted for records written
    /// before per-job model selection existed.
    #[serde(default)]
    pub model: Option<String>,
    /// A human has claimed the job's NEXT work attempt (spec §1.2 claims):
    /// instead of launching a container, the dispatcher parks that attempt as
    /// a Pending task with the declared kind and `performed_by: human`, then
    /// clears this flag — a claim covers exactly one attempt. Defaults false
    /// so records that predate claims deserialize.
    #[serde(default)]
    pub claim_next: bool,
    /// Structured record of the job's most recent escalation or stall (spec
    /// §1.2, §3.4): the reason code, a human-readable detail, the failing task
    /// (when one exists), and when it happened — so operators see WHY on the
    /// record instead of reconstructing it from dispatcher logs. Written at
    /// every escalate/stall call site; advisory, no transition consults it.
    /// None until the job first escalates; defaulted so older records
    /// deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<Escalation>,
    /// Factory name when created by a factory triage agent (spec §13); None for
    /// operator-created jobs.
    pub factory: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set once (immutably) when job first enters Ready; anchor for `job_deadline`.
    pub ready_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobState {
    /// Editable pre-release draft (spec §2.1): the job's definition can be
    /// iterated on (PATCH .../jobs/{seq}) before it enters the DAG for real.
    /// Invisible to scheduling, holds no branch, cannot be claimed. Leaves via
    /// release (→ Ready/Blocked) or revoke; a Frozen never-released job may be
    /// moved back here (`POST .../draft`). Once released, never editable again.
    Draft,
    Frozen,
    Blocked,
    Ready,
    Work,
    Evaluation,
    /// Eval passed; the job is landing (merge queue → merge gate → squash).
    /// Only `wrap_up: merge` jobs enter here; `wrap_up: none` goes
    /// Evaluation→Done directly (spec §2.1, §3.3).
    WrapUp,
    /// Post-work human intervention: automation ran out after work executed.
    /// Resolved Retry/Resolve/Revoke.
    Escalated,
    /// Pre-work human intervention: the job could not start or become ready
    /// (config re-validation failed, or its deadline elapsed while still
    /// Ready). No work task exists. Resolved Retry/Revoke only — Resolve is
    /// rejected (spec §1.2 pre-Work escalations).
    Stalled,
    Done,
    Revoked,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Done | JobState::Revoked)
    }
}

/// Why the dispatcher escalated (→Escalated) or stalled (→Stalled) a job,
/// carried on the job record so operators diagnose from what the API serves
/// rather than from dispatcher logs (spec §1.2, §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Escalation {
    /// Machine reason code, matching the `job-escalated`/`job-stalled` event
    /// reason (e.g. `launch_validation_failed`, `work_retries_exhausted`,
    /// `eval_abort`, `job_deadline_exceeded`).
    pub reason: String,
    /// Human-readable explanation — the same text shown in the operator's
    /// intervention task prompt.
    pub detail: String,
    /// The task whose failure triggered the escalation, when one exists. None
    /// for pre-work escalations that fail before any task runs (launch
    /// validation, a deadline that elapsed while still Ready) and for
    /// evaluation-phase escalations with no single culprit task. Defaulted so
    /// records without it deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_task: Option<u64>,
    /// When the escalation was recorded.
    pub at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_round_trips_spec_example() {
        let json = r#"{
          "id": 42,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [11, 22],
          "state": "Frozen",
          "branch": "job/42",
          "base_ref": null,
          "knowledge_tags": ["rust", "rest-api", "payments/stripe-integration"],
          "factory": null,
          "created_at": "2026-04-05T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.id, 42);
        assert_eq!(job.state, JobState::Frozen);
        assert_eq!(job.deps, vec![11, 22]);
        // `timeout` is optional and defaults to None on records that predate it.
        assert_eq!(job.timeout, None);
        // `model` defaults to None on records that predate per-job model selection.
        assert_eq!(job.model, None);
        // `claim_next` defaults false on records that predate claims.
        assert!(!job.claim_next);
        let back = serde_json::to_string(&job).unwrap();
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }

    #[test]
    fn job_round_trips_with_timeout_override() {
        let json = r#"{
          "id": 7,
          "project": "acme/api",
          "type": "deploy",
          "deps": [],
          "state": "Frozen",
          "branch": "job/7",
          "base_ref": null,
          "knowledge_tags": [],
          "timeout": "45m",
          "factory": null,
          "created_at": "2026-07-20T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.timeout.as_deref(), Some("45m"));
        let back = serde_json::to_string(&job).unwrap();
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }

    #[test]
    fn job_round_trips_in_draft_state() {
        // Draft is a first-class serde variant, distinct from Frozen.
        let json = r#"{
          "id": 9,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Draft",
          "branch": "job/9",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.state, JobState::Draft);
        assert!(!job.state.is_terminal());
        let back = serde_json::to_string(&job).unwrap();
        assert!(back.contains(r#""state":"Draft""#));
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }

    #[test]
    fn job_round_trips_with_escalation_and_stays_backward_compat() {
        // Old records (no escalation key) deserialize to None and omit it on
        // the wire.
        let json = r#"{
          "id": 9,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Frozen",
          "branch": "job/9",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.escalation, None);
        assert!(!serde_json::to_string(&job).unwrap().contains("escalation"));

        // A populated escalation round-trips, with and without a failing task.
        let at = "2026-07-22T11:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let mut escalated = job.clone();
        escalated.escalation = Some(Escalation {
            reason: "launch_validation_failed".into(),
            detail: "Job 9 failed launch-time validation: missing secret".into(),
            failing_task: None,
            at,
        });
        let back = serde_json::to_string(&escalated).unwrap();
        assert!(back.contains("launch_validation_failed"));
        assert!(!back.contains("failing_task")); // None is skipped
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), escalated);

        escalated.escalation.as_mut().unwrap().failing_task = Some(4);
        let back = serde_json::to_string(&escalated).unwrap();
        assert!(back.contains("\"failing_task\":4"));
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), escalated);
    }

    #[test]
    fn job_round_trips_with_model_override() {
        let json = r#"{
          "id": 8,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Frozen",
          "branch": "job/8",
          "base_ref": null,
          "knowledge_tags": [],
          "model": "claude-fable-5",
          "factory": null,
          "created_at": "2026-07-21T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.model.as_deref(), Some("claude-fable-5"));
        let back = serde_json::to_string(&job).unwrap();
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }
}

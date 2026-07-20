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
    /// Factory name when created by a factory triage agent (spec §13); None for
    /// operator-created jobs.
    pub factory: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set once (immutably) when job first enters Ready; anchor for `job_deadline`.
    pub ready_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobState {
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
}

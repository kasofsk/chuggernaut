//! Job record and state machine states (spec §1.1, §2.1).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Input name → upstream job id; empty if job type declares no inputs.
    pub inputs: HashMap<String, u64>,
    pub state: JobState,
    /// `"job/{id}"`; set at creation; actual git branch created when job enters Work.
    pub branch: String,
    /// Exact HEAD of default branch; set/updated at every Ready-transition and on
    /// squash-merge conflict; None until job first enters Ready.
    pub base_ref: Option<String>,
    /// Union of job type defaults and operator-supplied tags at creation.
    pub knowledge_tags: Vec<String>,
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
    Escalated,
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
          "inputs": { "spec": 11, "codebase": 22 },
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
        assert_eq!(job.inputs["spec"], 11);
        let back = serde_json::to_string(&job).unwrap();
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }
}

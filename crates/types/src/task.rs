//! Task log entries (spec §1.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unit of execution within a job's Work and Evaluation phases. Chronological log,
/// no task graph. Stored at `tasks.{owner}.{project}.{job_seq}.{task_id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Sequential within job, 1-indexed.
    pub id: u64,
    pub job_seq: u64,
    pub project: String,
    pub phase: TaskPhase,
    pub cycle: u32,
    pub kind: TaskKind,
    pub state: TaskState,
    /// 1-indexed; each retry is a new task record with attempt+1.
    pub attempt: u32,
    /// Evaluator name for Evaluation/MergeGate tasks; None for work and
    /// escalation tasks. Ties the task to its `eval:` declaration — restart
    /// reconciliation and the UI both need the mapping.
    #[serde(default)]
    pub evaluator: Option<String>,
    /// Backend-assigned container ID (Docker or k8s); None for Human tasks.
    pub container_id: Option<String>,
    /// Agent tasks only: the session id handed to the agent CLI, which names
    /// its transcript. Recorded at task creation so the artifact stays
    /// addressable across a dispatcher restart, and so a later cycle can resume
    /// the conversation.
    #[serde(default)]
    pub session_id: Option<String>,
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPhase {
    Work,
    Evaluation,
    /// Merge-gate re-run of required command evaluators against the candidate
    /// squash commit (spec §3.3 Merge Gate). Only present when the default
    /// branch HEAD moved past `base_ref` while the job was in flight.
    MergeGate,
    /// Operator-dispatched triage (spec §1.2): an advisory agent run over the
    /// whole job state that produces a written assessment + recommendation.
    /// Purely advisory — it never drives a job transition. Only created while
    /// the job is Escalated or Stalled, and may be repeated.
    Triage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TaskKind {
    Command {
        run: String,
    },
    Agent {
        provider: String,
        model: Option<String>,
        prompt: String,
    },
    Human {
        prompt: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TaskResult {
    Work {
        summary: Option<String>,
        structured: Option<serde_json::Value>,
        token_usage: Option<TokenUsage>,
    },
    Command {
        pass: bool,
        exit_code: i32,
        output: String,
        structured: Option<serde_json::Value>,
    },
    Agent {
        pass: bool,
        /// Eval verdict "not satisfiable by rework" (design-lifecycle.md):
        /// implies `pass: false`; a required evaluator's abort skips the
        /// remaining rework budget and escalates.
        #[serde(default)]
        abort: bool,
        structured: Option<serde_json::Value>,
        token_usage: Option<TokenUsage>,
    },
    Human {
        pass: bool,
        /// Same abort semantics as `Agent`; set via `TaskResolution::Fail`.
        #[serde(default)]
        abort: bool,
        structured: Option<serde_json::Value>,
        action: Option<EscalationAction>,
        operator: String,
        resolved_at: DateTime<Utc>,
    },
    /// Result of an operator-dispatched triage task (spec §1.2). The agent's
    /// written assessment + recommendation, captured from the CLI JSON result
    /// text (triage runs without the channel MCP, so there is no `submit_result`).
    /// Advisory only — never consulted by any state transition.
    Triage {
        assessment: String,
        token_usage: Option<TokenUsage>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscalationAction {
    Retry,
    Resolve,
    Revoke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

/// Operator submission for Human tasks (spec §1.2). Adjacent tagging: `kind`
/// discriminates. `structured` is required (non-null) on `Fail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TaskResolution {
    Pass {
        structured: Option<serde_json::Value>,
    },
    Fail {
        structured: serde_json::Value,
        /// Human evaluator's "not satisfiable by rework" (design-lifecycle.md);
        /// only meaningful on evaluator tasks, ignored elsewhere.
        #[serde(default)]
        abort: bool,
    },
    Escalation {
        action: EscalationAction,
        structured: Option<serde_json::Value>,
    },
}

/// Rework context passed to the next work cycle (spec §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    pub evaluator: String,
    pub pass: bool,
    pub structured: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_resolution_matches_spec_wire_format() {
        let cases = [
            r#"{ "kind": "Pass", "structured": null }"#,
            r#"{ "kind": "Fail", "structured": { "notes": "auth check failed" } }"#,
            r#"{ "kind": "Escalation", "action": "Retry", "structured": null }"#,
        ];
        for c in cases {
            let _: TaskResolution = serde_json::from_str(c).unwrap();
        }
        // abort defaults false on the wire; explicit abort round-trips.
        let r: TaskResolution =
            serde_json::from_str(r#"{ "kind": "Fail", "structured": {} }"#).unwrap();
        assert_eq!(
            r,
            TaskResolution::Fail {
                structured: serde_json::json!({}),
                abort: false
            }
        );
        let r: TaskResolution =
            serde_json::from_str(r#"{ "kind": "Fail", "structured": {}, "abort": true }"#).unwrap();
        assert_eq!(
            r,
            TaskResolution::Fail {
                structured: serde_json::json!({}),
                abort: true
            }
        );

        let r: TaskResolution = serde_json::from_str(
            r#"{ "kind": "Escalation", "action": "Revoke", "structured": null }"#,
        )
        .unwrap();
        assert_eq!(
            r,
            TaskResolution::Escalation {
                action: EscalationAction::Revoke,
                structured: None
            }
        );
    }

    #[test]
    fn triage_phase_and_result_round_trip() {
        // TaskPhase::Triage round-trips.
        let p: TaskPhase = serde_json::from_str(r#""Triage""#).unwrap();
        assert_eq!(p, TaskPhase::Triage);
        assert_eq!(serde_json::to_string(&p).unwrap(), r#""Triage""#);

        // TaskResult::Triage round-trips, with and without token usage.
        let with_usage = TaskResult::Triage {
            assessment: "Work failed on a missing migration; recommend Revoke.".into(),
            token_usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
        };
        let json = serde_json::to_string(&with_usage).unwrap();
        assert!(json.contains(r#""kind":"Triage""#));
        assert_eq!(
            serde_json::from_str::<TaskResult>(&json).unwrap(),
            with_usage
        );

        let no_usage: TaskResult = serde_json::from_str(
            r#"{ "kind": "Triage", "assessment": "insufficient signal", "token_usage": null }"#,
        )
        .unwrap();
        assert_eq!(
            no_usage,
            TaskResult::Triage {
                assessment: "insufficient signal".into(),
                token_usage: None
            }
        );
    }
}

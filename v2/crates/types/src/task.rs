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
    /// Backend-assigned container ID (Docker or k8s); None for Human tasks.
    pub container_id: Option<String>,
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPhase {
    Work,
    Evaluation,
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
        structured: Option<serde_json::Value>,
        token_usage: Option<TokenUsage>,
    },
    Human {
        pass: bool,
        structured: Option<serde_json::Value>,
        action: Option<EscalationAction>,
        operator: String,
        resolved_at: DateTime<Utc>,
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
}

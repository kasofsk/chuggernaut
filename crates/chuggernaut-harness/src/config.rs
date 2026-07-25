//! Harness config, injected by the dispatcher at /chuggernaut/harness.json
//! (spec §4.5). Command strings are composed by the dispatcher-side provider —
//! the harness never builds CLI flags itself, which is what keeps it
//! provider-agnostic (v1: only ClaudeProvider composes these).

use serde::{Deserialize, Serialize};

pub const HARNESS_CONFIG_PATH: &str = "/chuggernaut/harness.json";
pub const REVIEW_RESULT_PATH: &str = "/chuggernaut/review-result.json";
pub const WORK_RESULT_PATH: &str = "/chuggernaut/work-result.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HarnessConfig {
    /// Job identity for step reporting subjects.
    pub owner: String,
    pub project: String,
    pub job_seq: u64,
    pub task_id: u64,
    /// Shell command for author iteration 1 (standard prompt invocation).
    pub author_cmd: String,
    /// Shell command template for author iterations > 1; the harness replaces
    /// `{findings}` with the reviewer's findings block. For Claude this wraps
    /// `claude -p --continue` so the author session is resumed.
    pub author_continue_cmd: String,
    /// Shell command for each review invocation (always a fresh session).
    pub reviewer_cmd: String,
    /// Max author↔reviewer rounds (JobType review.iterations, default 5).
    pub iterations: u32,
}

/// Reviewer verdict, written locally by the channel MCP server's
/// `submit_review` tool (spec §4.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewResult {
    pub pass: bool,
    pub findings: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn harness_config_round_trips() {
        let json = r#"{
            "owner": "acme", "project": "api", "job_seq": 42, "task_id": 1,
            "author_cmd": "claude -p \"$(cat /chuggernaut/prompt.md)\"",
            "author_continue_cmd": "claude -p --continue {findings}",
            "reviewer_cmd": "claude -p \"$(cat /chuggernaut/review-prompt.md)\"",
            "iterations": 5
        }"#;
        let cfg: HarnessConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.iterations, 5);
        assert_eq!(cfg.job_seq, 42);
    }
}

use std::collections::HashMap;
use std::env;

use chuggernaut_types::{AllowedClaudeFlag, default_allowed_claude_flags};

#[derive(Debug, Clone)]
pub struct Config {
    pub nats_url: String,
    /// NATS URL passed to action workers (may differ from nats_url when
    /// workers run inside Docker containers that need host.docker.internal).
    /// Defaults to nats_url if not set.
    pub nats_worker_url: String,
    pub http_listen: String,
    pub lease_secs: u64,
    pub default_timeout_secs: u64,
    pub cas_max_retries: u32,
    pub monitor_scan_interval_secs: u64,
    pub job_retention_secs: u64,
    pub activity_limit: usize,
    pub forgejo_url: Option<String>,
    pub forgejo_token: Option<String>,
    pub action_workflow: String,
    pub action_runner_label: String,
    pub max_concurrent_actions: usize,
    pub review_workflow: String,
    pub review_runner_label: String,
    pub max_continuations: u32,
    pub ci_poll_timeout_secs: u64,
    pub rework_limit: u32,
    pub human_login: String,
    /// Which Claude CLI flags jobs are allowed to set via `claude_args`.
    /// Override with CHUGGERNAUT_ALLOWED_CLAUDE_FLAGS as a JSON array.
    pub allowed_claude_flags: Vec<AllowedClaudeFlag>,
    /// When true, pause dispatching new jobs when workers report API rate
    /// limit overage (isUsingOverage from Claude CLI's rate_limit_event).
    pub pause_on_overage: bool,
    /// Maps job capabilities to runner labels for routing jobs to specialized
    /// runners. E.g. `{"flutter": "flutter", "rust": "rust"}`. If a job has
    /// a capability matching a key, that label is used instead of the default.
    pub runner_label_map: HashMap<String, String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            nats_url: env::var("CHUGGERNAUT_NATS_URL")
                .unwrap_or_else(|_| "nats://localhost:4222".to_string()),
            nats_worker_url: env::var("CHUGGERNAUT_NATS_WORKER_URL").unwrap_or_else(|_| {
                env::var("CHUGGERNAUT_NATS_URL")
                    .unwrap_or_else(|_| "nats://localhost:4222".to_string())
            }),
            http_listen: env::var("CHUGGERNAUT_HTTP_LISTEN")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            lease_secs: parse_env("CHUGGERNAUT_LEASE_SECS", 60),
            default_timeout_secs: parse_env("CHUGGERNAUT_DEFAULT_TIMEOUT_SECS", 3600),
            cas_max_retries: parse_env("CHUGGERNAUT_CAS_MAX_RETRIES", 5),
            monitor_scan_interval_secs: parse_env("CHUGGERNAUT_MONITOR_SCAN_INTERVAL_SECS", 10),
            job_retention_secs: parse_env("CHUGGERNAUT_JOB_RETENTION_SECS", 86400),
            activity_limit: parse_env("CHUGGERNAUT_ACTIVITY_LIMIT", 50),
            forgejo_url: env::var("CHUGGERNAUT_FORGEJO_URL").ok(),
            forgejo_token: env::var("CHUGGERNAUT_FORGEJO_TOKEN").ok(),
            action_workflow: env::var("CHUGGERNAUT_ACTION_WORKFLOW")
                .unwrap_or_else(|_| "work.yml".to_string()),
            action_runner_label: env::var("CHUGGERNAUT_ACTION_RUNNER_LABEL")
                .unwrap_or_else(|_| "ubuntu-latest".to_string()),
            max_concurrent_actions: parse_env("CHUGGERNAUT_MAX_CONCURRENT_ACTIONS", 2),
            review_workflow: env::var("CHUGGERNAUT_REVIEW_WORKFLOW")
                .unwrap_or_else(|_| "review.yml".to_string()),
            review_runner_label: env::var("CHUGGERNAUT_REVIEW_RUNNER_LABEL")
                .unwrap_or_else(|_| "ubuntu-latest".to_string()),
            max_continuations: parse_env("CHUGGERNAUT_MAX_CONTINUATIONS", 3),
            ci_poll_timeout_secs: parse_env("CHUGGERNAUT_CI_POLL_TIMEOUT_SECS", 120),
            rework_limit: parse_env("CHUGGERNAUT_REWORK_LIMIT", 3),
            human_login: env::var("CHUGGERNAUT_HUMAN_LOGIN").unwrap_or_else(|_| "you".to_string()),
            allowed_claude_flags: env::var("CHUGGERNAUT_ALLOWED_CLAUDE_FLAGS")
                .ok()
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_else(default_allowed_claude_flags),
            pause_on_overage: parse_env("CHUGGERNAUT_PAUSE_ON_OVERAGE", true),
            runner_label_map: env::var("CHUGGERNAUT_RUNNER_LABEL_MAP")
                .ok()
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default(),
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

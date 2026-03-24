use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub nats_url: String,
    pub forgejo_url: String,
    pub forgejo_token: String,
    pub dispatcher_url: String,
    pub human_login: String,
    pub delay_secs: u64,
    pub workflow: String,
    pub runner: String,
    pub action_timeout_secs: u64,
    pub poll_secs: u64,
    pub escalation_poll_secs: u64,
    pub rework_limit: u32,
    pub merge_lock_ttl_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            nats_url: env::var("CHUGGERNAUT_NATS_URL")
                .unwrap_or_else(|_| "nats://localhost:4222".to_string()),
            forgejo_url: env::var("CHUGGERNAUT_FORGEJO_URL")
                .expect("CHUGGERNAUT_FORGEJO_URL is required"),
            forgejo_token: env::var("CHUGGERNAUT_REVIEWER_FORGEJO_TOKEN")
                .expect("CHUGGERNAUT_REVIEWER_FORGEJO_TOKEN is required"),
            dispatcher_url: env::var("CHUGGERNAUT_DISPATCHER_URL")
                .unwrap_or_else(|_| "http://localhost:8080".to_string()),
            human_login: env::var("CHUGGERNAUT_REVIEWER_HUMAN_LOGIN")
                .unwrap_or_else(|_| "you".to_string()),
            delay_secs: parse_env("CHUGGERNAUT_REVIEWER_DELAY_SECS", 3),
            workflow: env::var("CHUGGERNAUT_REVIEWER_WORKFLOW")
                .unwrap_or_else(|_| "review-work.yml".to_string()),
            runner: env::var("CHUGGERNAUT_REVIEWER_RUNNER")
                .unwrap_or_else(|_| "ubuntu-latest".to_string()),
            action_timeout_secs: parse_env("CHUGGERNAUT_REVIEWER_ACTION_TIMEOUT_SECS", 600),
            poll_secs: parse_env("CHUGGERNAUT_REVIEWER_POLL_SECS", 10),
            escalation_poll_secs: parse_env("CHUGGERNAUT_REVIEWER_ESCALATION_POLL_SECS", 30),
            rework_limit: parse_env("CHUGGERNAUT_REVIEWER_REWORK_LIMIT", 3),
            merge_lock_ttl_secs: parse_env("CHUGGERNAUT_REVIEWER_MERGE_LOCK_TTL_SECS", 300),
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

use std::env;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub nats_url: String,
    pub forgejo_url: String,
    pub forgejo_token: String,
    pub heartbeat_interval_secs: u64,
    pub reregister_interval_secs: u64,
    pub capabilities: Vec<String>,
    pub worker_type: String,
    pub platform: Vec<String>,
}

impl WorkerConfig {
    pub fn from_env() -> Self {
        Self {
            worker_id: env::var("CHUGGERNAUT_WORKER_ID").expect("CHUGGERNAUT_WORKER_ID is required"),
            nats_url: env::var("CHUGGERNAUT_NATS_URL")
                .unwrap_or_else(|_| "nats://localhost:4222".to_string()),
            forgejo_url: env::var("CHUGGERNAUT_FORGEJO_URL").expect("CHUGGERNAUT_FORGEJO_URL is required"),
            forgejo_token: env::var("CHUGGERNAUT_FORGEJO_TOKEN")
                .expect("CHUGGERNAUT_FORGEJO_TOKEN is required"),
            heartbeat_interval_secs: parse_env("CHUGGERNAUT_HEARTBEAT_INTERVAL_SECS", 10),
            reregister_interval_secs: parse_env("CHUGGERNAUT_REREGISTER_INTERVAL_SECS", 15),
            capabilities: env::var("CHUGGERNAUT_CAPABILITIES")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            worker_type: env::var("CHUGGERNAUT_WORKER_TYPE").unwrap_or_else(|_| "sim".to_string()),
            platform: vec![std::env::consts::OS.to_string()],
        }
    }

    /// Create a config with explicit values (for CLI subcommands).
    pub fn new(
        worker_id: String,
        nats_url: String,
        forgejo_url: String,
        forgejo_token: String,
        worker_type: String,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            worker_id,
            nats_url,
            forgejo_url,
            forgejo_token,
            heartbeat_interval_secs: 10,
            reregister_interval_secs: 15,
            capabilities,
            worker_type,
            platform: vec![std::env::consts::OS.to_string()],
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub nats_url: String,
    pub http_listen: String,
    pub lease_secs: u64,
    pub default_timeout_secs: u64,
    pub cas_max_retries: u32,
    pub monitor_scan_interval_secs: u64,
    pub job_retention_secs: u64,
    pub activity_limit: usize,
    pub blacklist_ttl_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            nats_url: env::var("CHUGGERNAUT_NATS_URL")
                .unwrap_or_else(|_| "nats://localhost:4222".to_string()),
            http_listen: env::var("CHUGGERNAUT_HTTP_LISTEN")
                .unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            lease_secs: parse_env("CHUGGERNAUT_LEASE_SECS", 60),
            default_timeout_secs: parse_env("CHUGGERNAUT_DEFAULT_TIMEOUT_SECS", 3600),
            cas_max_retries: parse_env("CHUGGERNAUT_CAS_MAX_RETRIES", 5),
            monitor_scan_interval_secs: parse_env("CHUGGERNAUT_MONITOR_SCAN_INTERVAL_SECS", 10),
            job_retention_secs: parse_env("CHUGGERNAUT_JOB_RETENTION_SECS", 86400),
            activity_limit: parse_env("CHUGGERNAUT_ACTIVITY_LIMIT", 50),
            blacklist_ttl_secs: parse_env("CHUGGERNAUT_BLACKLIST_TTL_SECS", 3600),
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

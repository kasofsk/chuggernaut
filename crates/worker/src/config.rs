//! `chuggernaut worker` configuration — env-derived, mirroring the dispatcher
//! pattern (crates/dispatcher/src/config.rs).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Node name — must match the `DOCKER_NODES` entry on the dispatcher
    /// (`{name}|worker|{slots}`) and be subject-safe.
    pub node: String,
    pub nats_url: String,
    /// `.creds` file minted by `chuggernaut admin worker-creds`; None connects
    /// plain (open dev server).
    pub nats_creds: Option<PathBuf>,
    /// The local Docker daemon.
    pub docker_endpoint: String,
    /// Node-local copy of the channel MCP binary, substituted into launches
    /// that reference the `"channel"` artifact.
    pub channel_binary: PathBuf,
}

#[derive(Debug, thiserror::Error)]
#[error("worker config: {0}")]
pub struct ConfigError(String);

impl WorkerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let node = std::env::var("WORKER_NODE")
            .map_err(|_| ConfigError("WORKER_NODE is required".into()))?;
        if !is_subject_safe(&node) {
            return Err(ConfigError(format!(
                "WORKER_NODE {node:?} must be [A-Za-z0-9_-]+ (rides in NATS subjects)"
            )));
        }
        let nats_url =
            std::env::var("NATS_URL").map_err(|_| ConfigError("NATS_URL is required".into()))?;
        let nats_creds = std::env::var("NATS_CREDS")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                let default = PathBuf::from("/data/keys/worker.creds");
                default.exists().then_some(default)
            });
        Ok(Self {
            node,
            nats_url,
            nats_creds,
            docker_endpoint: std::env::var("WORKER_DOCKER_ENDPOINT")
                .unwrap_or_else(|_| "unix:///var/run/docker.sock".into()),
            channel_binary: std::env::var("WORKER_CHANNEL_BINARY")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    PathBuf::from("/usr/local/lib/chuggernaut/chuggernaut-channel")
                }),
        })
    }
}

/// Node names ride in NATS subjects — same charset the dispatcher enforces at
/// `DOCKER_NODES` parse time.
pub fn is_subject_safe(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_safety() {
        assert!(is_subject_safe("nuc"));
        assert!(is_subject_safe("gumbo-nuc-0"));
        assert!(!is_subject_safe(""));
        assert!(!is_subject_safe("nuc.0"));
        assert!(!is_subject_safe("nuc 0"));
        assert!(!is_subject_safe("nuc>"));
    }
}

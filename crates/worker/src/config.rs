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
    /// Host path of the node-local build cache (`WORKER_CACHE_DIR`). `Some`
    /// bind-mounts it into every launched container and turns on sccache;
    /// `None` (unset) disables caching — no bind, no env, no behavior change.
    /// A node property, provisioned entirely worker-side: it never rides the
    /// wire or the dispatcher's launch config (spec §3.1).
    pub cache_dir: Option<PathBuf>,
    /// Node-local script that rebuilds the three node images at a given SHA and
    /// swaps the daemon (`worker-refresh.sh build <sha> <tag>` / `swap <tag>`),
    /// invoked when a `refresh` RPC arrives (spec §3.1). `None` ⇒ self-refresh
    /// is not wired and refresh requests are rejected. Set from
    /// `WORKER_REFRESH_SCRIPT`, defaulting to the image's bundled copy when it
    /// exists.
    pub refresh_script: Option<PathBuf>,
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
            cache_dir: parse_cache_dir(std::env::var("WORKER_CACHE_DIR").ok()),
            refresh_script: resolve_refresh_script(std::env::var("WORKER_REFRESH_SCRIPT").ok()),
        })
    }
}

/// Resolve `WORKER_REFRESH_SCRIPT`: an explicit path wins; unset falls back to
/// the image's bundled copy only if it is actually present, so a node without
/// the script cleanly reports self-refresh as unconfigured rather than failing
/// at swap time. Pure over its input for unit testing.
fn resolve_refresh_script(raw: Option<String>) -> Option<PathBuf> {
    match raw.filter(|s| !s.is_empty()) {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            let default = PathBuf::from("/usr/local/lib/chuggernaut/worker-refresh.sh");
            default.exists().then_some(default)
        }
    }
}

/// Parse `WORKER_CACHE_DIR` into the optional node-local cache path. Absent or
/// empty ⇒ `None` (caching disabled). Pure over its input so the present/absent
/// behavior is unit-tested without mutating the process environment.
fn parse_cache_dir(raw: Option<String>) -> Option<PathBuf> {
    raw.filter(|s| !s.is_empty()).map(PathBuf::from)
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
    fn cache_dir_parses_present_and_absent() {
        // Absent ⇒ caching disabled.
        assert_eq!(parse_cache_dir(None), None);
        // Present ⇒ the host path, verbatim.
        assert_eq!(
            parse_cache_dir(Some("/var/cache/chuggernaut/sccache".into())),
            Some(PathBuf::from("/var/cache/chuggernaut/sccache"))
        );
        // An empty value is treated as unset, not a bind on `/`.
        assert_eq!(parse_cache_dir(Some(String::new())), None);
    }

    #[test]
    fn refresh_script_resolves_explicit_and_absent() {
        // Explicit path is taken verbatim, even if it does not (yet) exist.
        assert_eq!(
            resolve_refresh_script(Some("/opt/refresh.sh".into())),
            Some(PathBuf::from("/opt/refresh.sh"))
        );
        // Empty is treated as unset; the bundled default is absent in tests, so
        // self-refresh is cleanly unconfigured rather than pointing at nothing.
        assert_eq!(resolve_refresh_script(Some(String::new())), None);
        assert_eq!(resolve_refresh_script(None), None);
    }

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

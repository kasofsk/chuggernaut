//! `chuggernaut worker` configuration — env-derived, mirroring the dispatcher
//! pattern (crates/dispatcher/src/config.rs).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Node name — must match the `DOCKER_NODES` entry on the dispatcher
    /// (`{name}|worker|{slots}`) and be subject-safe.
    pub node: String,
    /// Concurrent-container capacity this node advertises for itself in its
    /// announce heartbeat (`WORKER_SLOTS`, spec §3.1 dynamic registration).
    /// Default 4. The dispatcher's live fleet uses it as the node's slot cap;
    /// re-announcing with a new value changes capacity at runtime, no restart.
    pub slots: u32,
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
    /// Git URL the refresh script fetches the build context from over the ssh
    /// front (`WORKER_REFRESH_GIT_URL`, spec §3.1). `None` (unset/empty) ⇒ the
    /// node has no git credential and reports refresh requests as *skipped* in
    /// the RPC reply rather than accepting and silently no-oping in the
    /// background — so a deploy surfaces the missing credential loudly.
    pub refresh_git_url: Option<String>,
    /// The node's git private key (`WORKER_GIT_KEY`, default
    /// `/data/keys/worker_git`). Its absence is the *other* half of "no git
    /// credential" (the key file the refresh fetch would `ssh -i`), also
    /// reported as a skip.
    pub refresh_git_key: PathBuf,
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
        let slots = parse_slots(std::env::var("WORKER_SLOTS").ok())?;
        Ok(Self {
            node,
            slots,
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
            refresh_git_url: parse_git_url(std::env::var("WORKER_REFRESH_GIT_URL").ok()),
            refresh_git_key: std::env::var("WORKER_GIT_KEY")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/data/keys/worker_git")),
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

/// Parse `WORKER_SLOTS` into the node's advertised capacity. Absent or empty ⇒
/// the default 4; a non-numeric value is a hard config error. Pure over its
/// input for unit testing without mutating the process environment.
fn parse_slots(raw: Option<String>) -> Result<u32, ConfigError> {
    match raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(s) => s
            .parse()
            .map_err(|_| ConfigError(format!("WORKER_SLOTS must be a number, got {s:?}"))),
        None => Ok(4),
    }
}

/// Parse `WORKER_REFRESH_GIT_URL` into the optional git URL. Absent, empty, or
/// whitespace-only ⇒ `None` (no git credential — refresh requests are reported
/// as skipped). `build-worker.sh`'s `REFRESH_ENV` passthrough injects an empty
/// string when the operator's shell has the var unset, so an empty/blank value
/// must normalize to unset here rather than being taken as a configured URL the
/// refresh then dies on. Pure over its input for unit testing.
fn parse_git_url(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]
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
    fn git_url_parses_present_and_absent() {
        // Absent ⇒ no credential (refresh reported as skipped).
        assert_eq!(parse_git_url(None), None);
        // An empty value is treated as unset, the exact prod condition (#114:
        // WORKER_REFRESH_GIT_URL empty) that must surface as a skip.
        assert_eq!(parse_git_url(Some(String::new())), None);
        // Whitespace-only is likewise unset — build-worker.sh's REFRESH_ENV can
        // inject a blank value when the operator's shell has the var unset, and
        // a blank string must not be taken as a configured URL.
        assert_eq!(parse_git_url(Some("   ".into())), None);
        assert_eq!(parse_git_url(Some("\t\n".into())), None);
        // Present ⇒ the URL, verbatim.
        assert_eq!(
            parse_git_url(Some("ssh://git@front:2222/acme/chug.git".into())),
            Some("ssh://git@front:2222/acme/chug.git".to_string())
        );
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
    fn slots_parses_default_and_value() {
        // Absent / empty ⇒ the default capacity.
        assert_eq!(parse_slots(None).unwrap(), 4);
        assert_eq!(parse_slots(Some(String::new())).unwrap(), 4);
        assert_eq!(parse_slots(Some("  ".into())).unwrap(), 4);
        // A number is taken verbatim (the air 4→5 re-announce case).
        assert_eq!(parse_slots(Some("5".into())).unwrap(), 5);
        assert_eq!(parse_slots(Some(" 2 ".into())).unwrap(), 2);
        // Non-numeric is a hard config error.
        assert!(parse_slots(Some("lots".into())).is_err());
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

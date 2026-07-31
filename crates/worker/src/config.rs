//! `chuggernaut worker` configuration — env-derived, mirroring the dispatcher
//! pattern (crates/dispatcher/src/config.rs).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Node name — must match the `DOCKER_NODES` entry on the dispatcher
    /// (`{name}|worker|{slots}`) and be subject-safe.
    pub node: String,
    /// Concurrent-container capacity this node starts at (`WORKER_SLOTS`, spec
    /// §3.1 dynamic registration). Default [`SLOTS_DEFAULT`]. It is the node's
    /// **first-boot value only** — an operator changes capacity at runtime with
    /// the `set_slots` op (spec §3.1 operator capacity control), never by
    /// recreating the container. Kept so a fresh node, or one whose dispatcher is
    /// down, boots at a sane number before any operator intent exists.
    pub slots: u32,
    /// Ceiling on the slot count this node will adopt (`WORKER_SLOTS_MAX`, spec
    /// §3.1). Defaults to the node's CPU count and is enforced **only here, at
    /// the daemon** — the enforcement point is the only place that actually
    /// knows what the node can serve. Below it the operator is trusted: no
    /// memory or disk heuristics. Also clamps [`Self::slots`], so the node never
    /// advertises capacity its own `set_slots` would refuse.
    pub slots_max: u32,
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
        let slots_max = parse_slots_max(std::env::var("WORKER_SLOTS_MAX").ok(), node_cpu_count())?;
        Ok(Self {
            node,
            slots,
            slots_max,
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

/// The node's first-boot slot count when `WORKER_SLOTS` is unset — the value of
/// last resort for a node brought up with no env at all, and the fallback
/// ceiling when the platform cannot report a CPU count.
pub const SLOTS_DEFAULT: u32 = 4;

/// Parse `WORKER_SLOTS` into the node's first-boot capacity. Absent or empty ⇒
/// [`SLOTS_DEFAULT`]; a non-numeric value is a hard config error. Pure over its
/// input for unit testing without mutating the process environment.
fn parse_slots(raw: Option<String>) -> Result<u32, ConfigError> {
    match raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(s) => s
            .parse()
            .map_err(|_| ConfigError(format!("WORKER_SLOTS must be a number, got {s:?}"))),
        None => Ok(SLOTS_DEFAULT),
    }
}

/// Parse `WORKER_SLOTS_MAX` into the node's runtime ceiling. Absent or empty ⇒
/// `cpus` (the node's CPU count). A value *above* the CPU count is allowed on
/// purpose — the env exists for nodes that know better than their core count, in
/// either direction (air's colima VM has 6 CPUs but serves 2 concurrent Rust
/// builds; an IO-bound node may serve more than it has cores).
///
/// Zero is a hard config error: a node whose ceiling is zero is pinned at a full
/// drain forever and could never be raised from the operator UI again — the
/// unrecoverable state runtime capacity control exists to avoid (design #293
/// §5a). Pure over its inputs for unit testing.
fn parse_slots_max(raw: Option<String>, cpus: u32) -> Result<u32, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(cpus);
    };
    let value: u32 = raw
        .parse()
        .map_err(|_| ConfigError(format!("WORKER_SLOTS_MAX must be a number, got {raw:?}")))?;
    if value == 0 {
        return Err(ConfigError(
            "WORKER_SLOTS_MAX must be at least 1 — a node with a zero ceiling can never be \
             raised again from the operator UI"
                .into(),
        ));
    }
    Ok(value)
}

/// The node's own CPU count, the default ceiling (spec §3.1). A platform that
/// cannot report one falls back to [`SLOTS_DEFAULT`], so an unknown CPU count
/// never clamps the node's own default boot value.
fn node_cpu_count() -> u32 {
    match std::thread::available_parallelism() {
        Ok(cpus) => u32::try_from(cpus.get()).unwrap_or(u32::MAX),
        Err(_) => SLOTS_DEFAULT,
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
        assert_eq!(parse_cache_dir(None), None);
        assert_eq!(
            parse_cache_dir(Some("/var/cache/chuggernaut/sccache".into())),
            Some(PathBuf::from("/var/cache/chuggernaut/sccache"))
        );
        assert_eq!(parse_cache_dir(Some(String::new())), None);
    }

    #[test]
    fn git_url_parses_present_and_absent() {
        assert_eq!(parse_git_url(None), None);
        assert_eq!(parse_git_url(Some(String::new())), None);
        assert_eq!(parse_git_url(Some("   ".into())), None);
        assert_eq!(parse_git_url(Some("\t\n".into())), None);
        assert_eq!(
            parse_git_url(Some("ssh://git@front:2222/acme/chug.git".into())),
            Some("ssh://git@front:2222/acme/chug.git".to_string())
        );
    }

    #[test]
    fn refresh_script_resolves_explicit_and_absent() {
        assert_eq!(
            resolve_refresh_script(Some("/opt/refresh.sh".into())),
            Some(PathBuf::from("/opt/refresh.sh"))
        );
        assert_eq!(resolve_refresh_script(Some(String::new())), None);
        assert_eq!(resolve_refresh_script(None), None);
    }

    #[test]
    fn slots_parses_default_and_value() {
        assert_eq!(parse_slots(None).unwrap(), 4);
        assert_eq!(parse_slots(Some(String::new())).unwrap(), 4);
        assert_eq!(parse_slots(Some("  ".into())).unwrap(), 4);
        assert_eq!(parse_slots(Some("5".into())).unwrap(), 5);
        assert_eq!(parse_slots(Some(" 2 ".into())).unwrap(), 2);
        assert!(parse_slots(Some("lots".into())).is_err());
    }

    /// The ceiling `set_slots` is validated against (spec §3.1): the node's CPU
    /// count by default, overridable in either direction, and never zero.
    #[test]
    fn slots_max_parses_default_and_value() {
        assert_eq!(parse_slots_max(None, 6).unwrap(), 6);
        assert_eq!(parse_slots_max(Some(String::new()), 6).unwrap(), 6);
        assert_eq!(parse_slots_max(Some(" \t".into()), 6).unwrap(), 6);
        assert_eq!(parse_slots_max(Some(" 2 ".into()), 6).unwrap(), 2);
        assert_eq!(parse_slots_max(Some("12".into()), 6).unwrap(), 12);
        assert!(parse_slots_max(Some("0".into()), 6).is_err());
        assert!(parse_slots_max(Some("some".into()), 6).is_err());
        assert_eq!(parse_slots_max(None, SLOTS_DEFAULT).unwrap(), SLOTS_DEFAULT);
        assert!(node_cpu_count() >= 1);
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

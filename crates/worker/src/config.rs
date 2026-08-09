//! `chuggernaut worker` configuration — env-derived, mirroring the dispatcher
//! pattern (crates/dispatcher/src/config.rs).

use container::docker::DockerGrantEntry;
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
    /// Execution runtimes this node offers (`WORKER_MODES`, design #322),
    /// defaulting to [`WorkerMode::Container`]. A node property provisioned
    /// exactly as [`Self::cache_dir`] is — worker-side, never on the wire or the
    /// dispatcher's launch config (spec §3.1).
    pub modes: Vec<WorkerMode>,
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
    /// Root of this node's host task directories (`WORKER_HOST_ROOT`, design
    /// #309 P0), defaulting to [`container::host::HOST_ROOT_DEFAULT`]. Read only
    /// when [`Self::modes`] names [`WorkerMode::Host`]; a container-only node
    /// never touches it.
    pub host_root: PathBuf,
    /// Projects this node runs **host** work for (`WORKER_HOST_PROJECTS`,
    /// `owner/project` entries, design #309 §10). Empty ⇒ nobody, so serving
    /// `host` at all and declaring whose work it is for are two separate acts;
    /// a container launch on the same node is untouched by it.
    pub host_projects: Vec<String>,
    /// The KVM device this node passes through (`WORKER_KVM`); `None` (unset) ⇒
    /// no passthrough at all. A node property provisioned exactly as
    /// [`Self::cache_dir`] is — worker-side, never on the wire or the launch
    /// config (design #367 A1) — and the daemon refuses to start when it is set
    /// and the device node is absent.
    pub kvm_device: Option<PathBuf>,
    /// Projects allowed the KVM device and the read-only toolchain mounts that
    /// travel with it (`WORKER_KVM_PROJECTS`, `owner/project` entries). Empty ⇒
    /// nobody, so enabling KVM on a node and granting it to a project are two
    /// separate acts (design #367 §2.3).
    pub kvm_projects: Vec<String>,
    /// Host path of the docker socket this node binds into an allow-listed
    /// launch (`WORKER_DOCKER_SOCKET`); `None` (unset) ⇒ no bind at all. A node
    /// property provisioned exactly as [`Self::kvm_device`] is — worker-side,
    /// never on the wire or the launch config (design #517 D3) — and the daemon
    /// refuses to start when it is set and the socket is absent from the
    /// daemon's own view.
    pub docker_socket: Option<PathBuf>,
    /// The `(project, job type)` pairs allowed that socket
    /// (`WORKER_DOCKER_GRANTS`, `owner/project:job_type` entries). Empty ⇒
    /// nobody, so binding a socket on a node and granting it to a workload are
    /// two separate acts (design #517 D3) — and a granted launch holds node
    /// root, which is why this is node config and never a job-type field.
    pub docker_grants: Vec<DockerGrantEntry>,
    /// The node's Android SDK path (`WORKER_ANDROID_SDK_DIR`), mounted read-only
    /// for an allow-listed launch. It names the operator's activation-maintained
    /// **stable** path, which is why the parse rejects a nix store hash (design
    /// #367 §3.5).
    pub android_sdk_dir: PathBuf,
    /// The node's Flutter SDK path (`WORKER_FLUTTER_DIR`), mounted read-only for
    /// an allow-listed launch as a second, independent toolchain leaf; `None`
    /// (unset) ⇒ no mount and no `FLUTTER_ROOT`. Held to the same stable-path
    /// rule as [`Self::android_sdk_dir`], and no default: a node that does not
    /// provision Flutter must not name a path that is not there.
    pub flutter_dir: Option<PathBuf>,
    /// The node's JDK path (`WORKER_JDK_DIR`), mounted read-only for an
    /// allow-listed launch as a third, independent toolchain leaf; `None`
    /// (unset) ⇒ no mount and no `JAVA_HOME`. Held to the same stable-path rule
    /// as [`Self::android_sdk_dir`], and no default: a node that does not
    /// provision a JDK must not name a path that is not there.
    pub jdk_dir: Option<PathBuf>,
    /// Worker-writable directory the node's per-task nix GC roots are written to
    /// (`WORKER_NIX_GCROOTS_DIR`); `None` (unset) ⇒ no realise and no roots at
    /// all. A runtime precondition provisioned with the node, so the daemon
    /// refuses to start when it is set and the directory is absent (design #373
    /// Decision 4).
    pub nix_gcroots_dir: Option<PathBuf>,
    /// Projects whose job types may have this node realise their declared
    /// `runtime.env` (`WORKER_NIX_PROJECTS`, `owner/project` entries, design
    /// #373 Decision 2 rule 3). Empty ⇒ nobody, and granting it grants
    /// **evaluation** of that project's flake inside `chug-worker` (design #373
    /// 3b).
    pub nix_projects: Vec<String>,
    /// The flake-aware client a declared `runtime.env` is built with
    /// (`WORKER_NIX_FLAKE_CLIENT`), defaulting to [`NIX_FLAKE_CLIENT_DEFAULT`].
    /// Reached through the node's profiles for the same reason
    /// [`Self::nix_client`] is.
    pub nix_flake_client: PathBuf,
    /// The nix client the realise runs (`WORKER_NIX_CLIENT`), defaulting to
    /// [`NIX_CLIENT_DEFAULT`]. It resolves through the node's *profiles*, which
    /// are themselves a GC root, so the long-lived worker's own client cannot be
    /// collected by an old-generation GC (design #373 3b).
    pub nix_client: PathBuf,
    /// The node's nix daemon socket (`WORKER_NIX_DAEMON_SOCKET`), defaulting to
    /// [`NIX_DAEMON_SOCKET_DEFAULT`]. The realise is the daemon's work, not the
    /// worker's: builders stay sandboxed as `nixbld` users (design #373 3b).
    pub nix_daemon_socket: PathBuf,
    /// The store prefix a realise target must resolve into
    /// (`WORKER_NIX_STORE_DIR`, nix's own `NIX_STORE_DIR`), defaulting to
    /// [`NIX_STORE_DIR_DEFAULT`]. The boot check refuses a toolchain that lands
    /// outside it, because the nix client refuses a non-store path.
    pub nix_store_dir: PathBuf,
    /// Bound on one pre-launch realise in seconds
    /// (`WORKER_NIX_REALISE_TIMEOUT_SECS`), defaulting to
    /// [`NIX_REALISE_TIMEOUT_SECS_DEFAULT`]. The realise runs before execution
    /// begins, so no `task_timeout` covers it and an unbounded one would hang the
    /// launch path (design #373 3c, docs/reference/style.md Tier 2 rule 3).
    pub nix_realise_timeout_secs: u64,
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

/// One execution runtime a worker node can offer (design #322 W1). The node
/// declares the list it serves in `WORKER_MODES`; `container` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerMode {
    /// Containers on the node's local Docker daemon.
    Container,
    /// Host processes on the node itself (design #322 W2).
    Host,
}

impl WorkerMode {
    /// The canonical name, as it is written in `WORKER_MODES` and reported back
    /// in errors.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Host => "host",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "container" => Some(Self::Container),
            "host" => Some(Self::Host),
            _ => None,
        }
    }
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
        let modes = parse_modes(std::env::var("WORKER_MODES").ok())?;
        let nix = NixEnv::from_env()?;
        Ok(Self {
            node,
            slots,
            slots_max,
            modes,
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
            host_root: parse_host_root(std::env::var("WORKER_HOST_ROOT").ok())?,
            host_projects: parse_projects(
                "WORKER_HOST_PROJECTS",
                std::env::var("WORKER_HOST_PROJECTS").ok(),
            )?,
            kvm_device: parse_kvm_device(std::env::var("WORKER_KVM").ok())?,
            kvm_projects: parse_projects(
                "WORKER_KVM_PROJECTS",
                std::env::var("WORKER_KVM_PROJECTS").ok(),
            )?,
            docker_socket: parse_docker_socket(std::env::var("WORKER_DOCKER_SOCKET").ok())?,
            docker_grants: parse_docker_grants(std::env::var("WORKER_DOCKER_GRANTS").ok())?,
            android_sdk_dir: parse_android_sdk_dir(std::env::var("WORKER_ANDROID_SDK_DIR").ok())?,
            flutter_dir: parse_flutter_dir(std::env::var("WORKER_FLUTTER_DIR").ok())?,
            jdk_dir: parse_jdk_dir(std::env::var("WORKER_JDK_DIR").ok())?,
            nix_gcroots_dir: nix.gcroots_dir,
            nix_projects: nix.projects,
            nix_flake_client: nix.flake_client,
            nix_client: nix.client,
            nix_daemon_socket: nix.daemon_socket,
            nix_store_dir: nix.store_dir,
            nix_realise_timeout_secs: nix.realise_timeout_secs,
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

/// The node's nix settings, parsed as one group (design #373): they are one
/// feature, and grouping them keeps [`WorkerConfig::from_env`] inside the line
/// bound.
struct NixEnv {
    gcroots_dir: Option<PathBuf>,
    projects: Vec<String>,
    flake_client: PathBuf,
    client: PathBuf,
    daemon_socket: PathBuf,
    store_dir: PathBuf,
    realise_timeout_secs: u64,
}

impl NixEnv {
    fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            gcroots_dir: parse_nix_gcroots_dir(std::env::var("WORKER_NIX_GCROOTS_DIR").ok())?,
            projects: parse_projects(
                "WORKER_NIX_PROJECTS",
                std::env::var("WORKER_NIX_PROJECTS").ok(),
            )?,
            flake_client: parse_nix_path(
                "WORKER_NIX_FLAKE_CLIENT",
                std::env::var("WORKER_NIX_FLAKE_CLIENT").ok(),
                NIX_FLAKE_CLIENT_DEFAULT,
            )?,
            client: parse_nix_path(
                "WORKER_NIX_CLIENT",
                std::env::var("WORKER_NIX_CLIENT").ok(),
                NIX_CLIENT_DEFAULT,
            )?,
            daemon_socket: parse_nix_path(
                "WORKER_NIX_DAEMON_SOCKET",
                std::env::var("WORKER_NIX_DAEMON_SOCKET").ok(),
                NIX_DAEMON_SOCKET_DEFAULT,
            )?,
            store_dir: parse_nix_path(
                "WORKER_NIX_STORE_DIR",
                std::env::var("WORKER_NIX_STORE_DIR").ok(),
                NIX_STORE_DIR_DEFAULT,
            )?,
            realise_timeout_secs: parse_nix_realise_timeout_secs(
                std::env::var("WORKER_NIX_REALISE_TIMEOUT_SECS").ok(),
            )?,
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

/// Parse `WORKER_HOST_ROOT` into the node's host task root (design #309 P0);
/// absent or empty ⇒ [`container::host::HOST_ROOT_DEFAULT`]. Held to
/// [`parse_stable_path`]'s rule: the root is worker-writable node state that
/// outlives a task, never a store path.
fn parse_host_root(raw: Option<String>) -> Result<PathBuf, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(PathBuf::from(container::host::HOST_ROOT_DEFAULT));
    };
    parse_stable_path("WORKER_HOST_ROOT", &raw)
}

/// The node's Android SDK path when `WORKER_ANDROID_SDK_DIR` is unset (design
/// #367 §3.5). Under `/var/lib` deliberately: a symlink under `/tmp` is not
/// stat-able by the docker daemon at container create, so the mount fails.
pub const ANDROID_SDK_DIR_DEFAULT: &str = "/var/lib/chuggernaut/android-sdk";

/// Parse `WORKER_KVM` into the device this node passes through (design #367
/// §2.3): absent, empty, `0`, `false` or `off` ⇒ `None`; `1`, `true` or `on` ⇒
/// the default device; an absolute path names another device node. Anything
/// else is a hard config error — a capability must never be silently off.
fn parse_kvm_device(raw: Option<String>) -> Result<Option<PathBuf>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match raw.as_str() {
        "0" | "false" | "off" => Ok(None),
        "1" | "true" | "on" => Ok(Some(PathBuf::from(container::docker::KVM_DEVICE_PATH))),
        path if path.starts_with('/') => Ok(Some(PathBuf::from(path))),
        other => Err(ConfigError(format!(
            "WORKER_KVM must be 1/0 or an absolute device path, got {other:?}"
        ))),
    }
}

/// Parse one node-side project allow-list — the device and its toolchain mounts
/// (`WORKER_KVM_PROJECTS`, design #367 §2.3), project-declared toolchains
/// (`WORKER_NIX_PROJECTS`, design #373 Decision 2 rule 3), or the node's host
/// tenancy (`WORKER_HOST_PROJECTS`, design #309 §10). Absent or empty ⇒
/// **nobody**, and a malformed or repeated `owner/project` entry is a hard
/// config error rather than a grant that silently never matches a `JOB_PROJECT`.
fn parse_projects(var: &str, raw: Option<String>) -> Result<Vec<String>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut projects = Vec::new();
    for entry in raw.split(',') {
        let project = entry.trim();
        let (owner, name) = project.split_once('/').unwrap_or_default();
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return Err(ConfigError(format!(
                "{var} entry {project:?} is not an owner/project pair"
            )));
        }
        if projects.iter().any(|p| p == project) {
            return Err(ConfigError(format!(
                "{var} lists {project:?} more than once"
            )));
        }
        projects.push(project.to_string());
    }
    debug_assert!(
        !projects.is_empty(),
        "a parsed allow-list names at least one project"
    );
    Ok(projects)
}

/// Parse `WORKER_DOCKER_SOCKET` into the socket this node binds into an
/// allow-listed launch (design #517 D3); absent or empty ⇒ `None`, which binds
/// nothing. Held to [`parse_stable_path`]'s rule, so a relative path is a hard
/// config error rather than a bind source the engine resolves somewhere
/// unintended.
fn parse_docker_socket(raw: Option<String>) -> Result<Option<PathBuf>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    parse_stable_path("WORKER_DOCKER_SOCKET", &raw).map(Some)
}

/// Parse `WORKER_DOCKER_GRANTS` into the `(project, job type)` pairs allowed
/// the node's docker socket (design #517 D3). Absent or empty ⇒ **nobody**, and
/// a malformed or repeated entry is a hard config error rather than a grant
/// that silently never matches a launch — the socket is root-equivalent, so
/// every failure here fails closed and says so.
fn parse_docker_grants(raw: Option<String>) -> Result<Vec<DockerGrantEntry>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let mut allowed: Vec<DockerGrantEntry> = Vec::new();
    for entry in raw.split(',') {
        let parsed = DockerGrantEntry::parse(entry)
            .map_err(|e| ConfigError(format!("WORKER_DOCKER_GRANTS {e}")))?;
        if allowed.contains(&parsed) {
            return Err(ConfigError(format!(
                "WORKER_DOCKER_GRANTS lists {:?} more than once",
                entry.trim()
            )));
        }
        allowed.push(parsed);
    }
    debug_assert!(
        !allowed.is_empty(),
        "a parsed allow-list names at least one pair"
    );
    Ok(allowed)
}

/// Parse `WORKER_ANDROID_SDK_DIR` into the node's SDK path; absent or empty ⇒
/// [`ANDROID_SDK_DIR_DEFAULT`]. A relative path, or one carrying a nix store
/// hash, is a hard config error (design #367 §3.5): a store path changes on
/// every SDK bump, so pinning one here keeps testing the *previous* SDK until
/// garbage collection turns it into an unattributable `ENOENT`.
fn parse_android_sdk_dir(raw: Option<String>) -> Result<PathBuf, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(PathBuf::from(ANDROID_SDK_DIR_DEFAULT));
    };
    parse_stable_path("WORKER_ANDROID_SDK_DIR", &raw)
}

/// Parse `WORKER_FLUTTER_DIR` into the node's Flutter SDK path; absent or empty
/// ⇒ `None`, which mounts nothing and injects no `FLUTTER_ROOT`. Optional rather
/// than defaulted because Flutter is a second, independent leaf: a node that
/// provisions only the Android SDK stays exactly as it is.
fn parse_flutter_dir(raw: Option<String>) -> Result<Option<PathBuf>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    parse_stable_path("WORKER_FLUTTER_DIR", &raw).map(Some)
}

/// Parse `WORKER_JDK_DIR` into the node's JDK path; absent or empty ⇒ `None`,
/// which mounts nothing and injects no `JAVA_HOME`. Optional for the same reason
/// Flutter is: a node that provisions only the Android SDK stays exactly as it
/// is.
fn parse_jdk_dir(raw: Option<String>) -> Result<Option<PathBuf>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    parse_stable_path("WORKER_JDK_DIR", &raw).map(Some)
}

/// One absolute host path out of operator-typed config, refusing a nix store
/// hash. A content hash goes silently wrong at the next `nixos-rebuild` (design
/// #367 §3.5) and, unrooted, is collectable garbage the moment a GC runs — which
/// is why the worker's own nix client is held to the same rule (design #373 3b).
fn parse_stable_path(var: &str, raw: &str) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(ConfigError(format!(
            "{var} must be an absolute host path, got {raw:?}"
        )));
    }
    if path
        .components()
        .any(|c| is_store_hash(&c.as_os_str().to_string_lossy()))
    {
        return Err(ConfigError(format!(
            "{var} {raw:?} names a nix store path — chug config names a stable, \
             activation-maintained path, never a content hash (design #367 §3.5, #373 3b)"
        )));
    }
    Ok(path)
}

/// The node's nix client when `WORKER_NIX_CLIENT` is unset (design #373 3b): the
/// multi-call `nix` binary reached *through the profiles*, which are themselves
/// a GC root — a client resolved out of a generation's store path can be
/// collected out from under the long-lived worker.
pub const NIX_CLIENT_DEFAULT: &str = "/nix/var/nix/profiles/system/sw/bin/nix-store";

/// The node's flake-aware client when `WORKER_NIX_FLAKE_CLIENT` is unset
/// (design #373 P2): the multi-call `nix` binary beside [`NIX_CLIENT_DEFAULT`]
/// in the same profiles tree, because `nix-store --realise` takes store paths
/// and a project declares a flake ref.
pub const NIX_FLAKE_CLIENT_DEFAULT: &str = "/nix/var/nix/profiles/system/sw/bin/nix";

/// The node's nix daemon socket when `WORKER_NIX_DAEMON_SOCKET` is unset — nix's
/// own default location, mode `0666` on a stock node, so the worker needs no uid
/// mapping to use it (design #373 3b).
pub const NIX_DAEMON_SOCKET_DEFAULT: &str = "/nix/var/nix/daemon-socket/socket";

/// The store prefix when `WORKER_NIX_STORE_DIR` is unset — nix's own default,
/// and the path `build-worker.sh` mounts read-only. A node that relocated its
/// store (nix's `NIX_STORE_DIR`) names the new prefix here.
pub const NIX_STORE_DIR_DEFAULT: &str = "/nix/store";

/// Headroom the `launch` RPC keeps for everything the realise precedes — the
/// container create and start, and the reply's own trip home. Subtracted from
/// the dispatcher's launch budget to give the realise its ceiling.
const NIX_REALISE_RESERVE_SECS: u64 = 15;

/// The largest realise bound the `launch` RPC can actually contain, since the
/// realise runs inside it and the dispatcher abandons the call at
/// [`store::worker::OP_TIMEOUT`]. Past this the caller has already failed the
/// task on transport, so the named refusal design #373 3c asks for could never
/// be reached.
pub const NIX_REALISE_TIMEOUT_SECS_MAX: u64 =
    store::worker::OP_TIMEOUT.as_secs() - NIX_REALISE_RESERVE_SECS;

/// The realise bound when `WORKER_NIX_REALISE_TIMEOUT_SECS` is unset (design
/// #373 3c), inside [`NIX_REALISE_TIMEOUT_SECS_MAX`] with room left for the
/// container create the same RPC still has to do. A closure that cannot be
/// substituted in this long fails the launch loudly rather than hanging the
/// launch path until the caller gives up on transport.
pub const NIX_REALISE_TIMEOUT_SECS_DEFAULT: u64 = 30;

/// Parse `WORKER_NIX_GCROOTS_DIR` into the node's GC-roots directory; absent or
/// empty ⇒ `None`, which turns the realise and its roots off entirely (design
/// #373 Decision 4). Held to [`parse_stable_path`]'s rule: the roots directory is
/// worker-writable node state, never a store path.
fn parse_nix_gcroots_dir(raw: Option<String>) -> Result<Option<PathBuf>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    parse_stable_path("WORKER_NIX_GCROOTS_DIR", &raw).map(Some)
}

/// Parse one of the node's nix paths (the client, the daemon socket); absent or
/// empty ⇒ `default`. Same stable-path rule as every other operator-typed path.
fn parse_nix_path(var: &str, raw: Option<String>, default: &str) -> Result<PathBuf, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(PathBuf::from(default));
    };
    parse_stable_path(var, &raw)
}

/// Parse `WORKER_NIX_REALISE_TIMEOUT_SECS` into the realise bound; absent or
/// empty ⇒ [`NIX_REALISE_TIMEOUT_SECS_DEFAULT`]. Zero, non-numeric and anything
/// over [`NIX_REALISE_TIMEOUT_SECS_MAX`] are hard config errors: a zero bound
/// refuses every launch, and a bound the `launch` RPC cannot contain fails on
/// transport instead of naming itself, which is what design #373 3c forbids.
fn parse_nix_realise_timeout_secs(raw: Option<String>) -> Result<u64, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(NIX_REALISE_TIMEOUT_SECS_DEFAULT);
    };
    let value: u64 = raw.parse().map_err(|_| {
        ConfigError(format!(
            "WORKER_NIX_REALISE_TIMEOUT_SECS must be a number of seconds, got {raw:?}"
        ))
    })?;
    if value == 0 {
        return Err(ConfigError(
            "WORKER_NIX_REALISE_TIMEOUT_SECS must be at least 1 — a zero bound would refuse \
             every launch it covers"
                .into(),
        ));
    }
    if value > NIX_REALISE_TIMEOUT_SECS_MAX {
        return Err(ConfigError(format!(
            "WORKER_NIX_REALISE_TIMEOUT_SECS={value} is over the ceiling of \
             {NIX_REALISE_TIMEOUT_SECS_MAX}s — the realise runs inside the `launch` RPC, which \
             the dispatcher abandons after {}s, so a longer bound is never reached: the task \
             fails on worker transport while this node goes on to launch a container nobody is \
             waiting for (design #373 3c)",
            store::worker::OP_TIMEOUT.as_secs()
        )));
    }
    Ok(value)
}

/// Whether one path component is a nix store entry: 32 characters of nix's
/// base32 alphabet, then `-` and the derivation name.
fn is_store_hash(component: &str) -> bool {
    let Some((hash, name)) = component.split_once('-') else {
        return false;
    };
    !name.is_empty()
        && hash.len() == 32
        && hash
            .chars()
            .all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c))
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

/// The runtimes a node offers when `WORKER_MODES` is unset — containers and
/// nothing else, which is today's behavior on every node in the fleet.
pub fn default_modes() -> Vec<WorkerMode> {
    vec![WorkerMode::Container]
}

/// Parse `WORKER_MODES` into the runtimes this node offers, in the order
/// declared; absent or empty ⇒ `[WorkerMode::Container]`, today's behavior. An
/// unknown name, a blank entry, or a repeat is a hard config error rather than a
/// silently dropped mode — pure over its input for unit testing.
fn parse_modes(raw: Option<String>) -> Result<Vec<WorkerMode>, ConfigError> {
    let Some(raw) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(default_modes());
    };
    let mut modes = Vec::new();
    for entry in raw.split(',') {
        let name = entry.trim();
        let mode = WorkerMode::parse(name).ok_or_else(|| {
            ConfigError(format!(
                "WORKER_MODES entry {name:?} is not a mode (expected container | host)"
            ))
        })?;
        if modes.contains(&mode) {
            return Err(ConfigError(format!(
                "WORKER_MODES lists {} more than once",
                mode.as_str()
            )));
        }
        modes.push(mode);
    }
    debug_assert!(
        !modes.is_empty(),
        "a parsed mode list always names at least one runtime"
    );
    Ok(modes)
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

    /// `WORKER_KVM` (design #367 §2.3): unset is off — today's behavior — a
    /// boolean turns on the default device, an explicit path names another, and
    /// a value that is neither is refused rather than read as "off".
    #[test]
    fn kvm_device_parses_default_path_and_rejections() {
        assert_eq!(parse_kvm_device(None).unwrap(), None);
        assert_eq!(parse_kvm_device(Some(String::new())).unwrap(), None);
        assert_eq!(parse_kvm_device(Some(" \t".into())).unwrap(), None);
        assert_eq!(parse_kvm_device(Some("0".into())).unwrap(), None);
        assert_eq!(parse_kvm_device(Some("false".into())).unwrap(), None);
        assert_eq!(
            parse_kvm_device(Some(" 1 ".into())).unwrap(),
            Some(PathBuf::from("/dev/kvm"))
        );
        assert_eq!(
            parse_kvm_device(Some("true".into())).unwrap(),
            Some(PathBuf::from(container::docker::KVM_DEVICE_PATH))
        );
        assert_eq!(
            parse_kvm_device(Some("/dev/kvm1".into())).unwrap(),
            Some(PathBuf::from("/dev/kvm1"))
        );
        let err = parse_kvm_device(Some("yes".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("WORKER_KVM"), "{err}");
    }

    /// `WORKER_KVM_PROJECTS` (design #367 §2.3): unset grants NOBODY — the
    /// fail-closed rule that makes enabling KVM on a node and granting it to a
    /// project two separate acts — and every malformed entry is refused rather
    /// than kept as a grant that can never match a `JOB_PROJECT`.
    #[test]
    fn kvm_projects_parse_empty_list_and_rejections() {
        assert_eq!(
            parse_projects("WORKER_KVM_PROJECTS", None).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_projects("WORKER_KVM_PROJECTS", Some(String::new())).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_projects("WORKER_KVM_PROJECTS", Some("  ".into())).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_projects(
                "WORKER_KVM_PROJECTS",
                Some(" acme/beacon , acme/api ".into())
            )
            .unwrap(),
            vec!["acme/beacon".to_string(), "acme/api".to_string()]
        );

        for bad in ["beacon", "acme/", "/beacon", "acme/b/c", "acme/beacon,", ""] {
            assert!(
                parse_projects("WORKER_KVM_PROJECTS", Some(format!("acme/api,{bad}"))).is_err(),
                "must reject {bad:?}"
            );
        }
        let err = parse_projects("WORKER_KVM_PROJECTS", Some("acme/api,acme/api".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than once"), "{err}");
    }

    /// `WORKER_DOCKER_SOCKET` (design #517 D3): unset binds nothing — today's
    /// behavior on every node — an absolute path names the socket, and a
    /// relative one or a store hash is refused rather than read as "off".
    #[test]
    fn docker_socket_parses_absent_present_and_rejections() {
        assert_eq!(parse_docker_socket(None).unwrap(), None);
        assert_eq!(parse_docker_socket(Some("  ".into())).unwrap(), None);
        assert_eq!(
            parse_docker_socket(Some(" /var/run/docker.sock ".into())).unwrap(),
            Some(PathBuf::from("/var/run/docker.sock"))
        );
        let err = parse_docker_socket(Some("docker.sock".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("WORKER_DOCKER_SOCKET"), "{err}");
    }

    /// `WORKER_DOCKER_GRANTS` (design #517 D3): unset grants NOBODY — the
    /// fail-closed rule that makes binding a socket on a node and granting it
    /// to a workload two separate acts — and every malformed or repeated entry
    /// is refused rather than kept as a grant that can never match a launch.
    #[test]
    fn docker_grants_parse_empty_list_and_rejections() {
        for empty in [None, Some(String::new()), Some("  ".into())] {
            assert_eq!(parse_docker_grants(empty).unwrap(), Vec::new());
        }
        assert_eq!(
            parse_docker_grants(Some(" acme/beacon:build-image , acme/api:code ".into())).unwrap(),
            vec![
                DockerGrantEntry {
                    project: "acme/beacon".into(),
                    job_type: "build-image".into(),
                },
                DockerGrantEntry {
                    project: "acme/api".into(),
                    job_type: "code".into(),
                },
            ]
        );

        for bad in ["acme/beacon", "acme:code", "acme/beacon:", "code", ""] {
            let err = parse_docker_grants(Some(format!("acme/api:code,{bad}")))
                .unwrap_err()
                .to_string();
            assert!(err.contains("WORKER_DOCKER_GRANTS"), "{bad:?}: {err}");
        }
        let err = parse_docker_grants(Some("acme/api:code,acme/api:code".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than once"), "{err}");
    }

    /// `WORKER_ANDROID_SDK_DIR` (design #367 §3.5): unset is the documented
    /// stable default, an absolute path is taken as given, and a nix store path
    /// is REFUSED — no content hash may enter chug-side config, because it goes
    /// silently wrong at the next `nixos-rebuild`.
    #[test]
    fn android_sdk_dir_rejects_a_store_hash() {
        assert_eq!(
            parse_android_sdk_dir(None).unwrap(),
            PathBuf::from(ANDROID_SDK_DIR_DEFAULT)
        );
        assert_eq!(
            parse_android_sdk_dir(Some(String::new())).unwrap(),
            PathBuf::from(ANDROID_SDK_DIR_DEFAULT)
        );
        assert_eq!(
            parse_android_sdk_dir(Some(" /var/lib/chug/android-sdk ".into())).unwrap(),
            PathBuf::from("/var/lib/chug/android-sdk")
        );

        let store = "/nix/store/3zr1pgwpc00zrj8qc8d631bdfw1z9c5y-androidsdk/libexec/android-sdk";
        let err = parse_android_sdk_dir(Some(store.into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("store path"), "{err}");
        assert!(
            parse_android_sdk_dir(Some(
                "/var/lib/nix/j92gsy8k9mzqz9zjw6h1mp6nxbfnlqhx-android-sdk-emulator".into()
            ))
            .is_err(),
            "a hash is a hash wherever it is written"
        );
        assert!(
            parse_android_sdk_dir(Some("relative/android-sdk".into())).is_err(),
            "a bind source must be an absolute host path"
        );
    }

    /// `WORKER_FLUTTER_DIR`: unset is OFF — no mount, no `FLUTTER_ROOT`, today's
    /// behavior on every node that has only the Android SDK — and the same
    /// no-store-hash rule holds, because a Flutter store path goes stale at the
    /// next `nixos-rebuild` exactly as an SDK one does.
    #[test]
    fn flutter_dir_is_optional_and_rejects_a_store_hash() {
        assert_eq!(parse_flutter_dir(None).unwrap(), None);
        assert_eq!(parse_flutter_dir(Some(String::new())).unwrap(), None);
        assert_eq!(parse_flutter_dir(Some(" \t".into())).unwrap(), None);
        assert_eq!(
            parse_flutter_dir(Some(" /var/lib/chuggernaut/toolchain/flutter ".into())).unwrap(),
            Some(PathBuf::from("/var/lib/chuggernaut/toolchain/flutter"))
        );

        let store = "/nix/store/cshk8jsnfmrh0f8asaash8qwm8lygikc-flutter-wrapped-3.41.2-sdk-links";
        let err = parse_flutter_dir(Some(store.into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("store path"), "{err}");
        assert!(
            err.contains("WORKER_FLUTTER_DIR"),
            "the refusal names the setting: {err}"
        );
        assert!(
            parse_flutter_dir(Some("toolchain/flutter".into())).is_err(),
            "a bind source must be an absolute host path"
        );
    }

    /// `WORKER_JDK_DIR`: unset is OFF — no mount, no `JAVA_HOME`, today's
    /// behavior on every node — and the same no-store-hash rule holds, which
    /// matters most here: the node's JDK lives at a content-addressed
    /// `openjdk-17.0.20+2` store path an operator would otherwise be tempted to
    /// paste in.
    #[test]
    fn jdk_dir_is_optional_and_rejects_a_store_hash() {
        assert_eq!(parse_jdk_dir(None).unwrap(), None);
        assert_eq!(parse_jdk_dir(Some(String::new())).unwrap(), None);
        assert_eq!(parse_jdk_dir(Some(" \t".into())).unwrap(), None);
        assert_eq!(
            parse_jdk_dir(Some(" /var/lib/chuggernaut/toolchain/jdk ".into())).unwrap(),
            Some(PathBuf::from("/var/lib/chuggernaut/toolchain/jdk"))
        );

        let store = "/nix/store/hgz4vjw1x8k6qy0mprsz9d3fabn7lc5m-openjdk-17.0.20+2";
        let err = parse_jdk_dir(Some(store.into())).unwrap_err().to_string();
        assert!(err.contains("store path"), "{err}");
        assert!(
            err.contains("WORKER_JDK_DIR"),
            "the refusal names the setting: {err}"
        );
        assert!(
            parse_jdk_dir(Some("toolchain/jdk".into())).is_err(),
            "a bind source must be an absolute host path"
        );
    }

    /// The nix node settings (design #373 P1): unset gcroots ⇒ the whole
    /// mechanism is off, the client and socket carry their documented defaults,
    /// and every one of them is held to the no-store-hash rule — a hashed client
    /// is precisely the path an old-generation GC collects out from under a
    /// long-lived worker.
    #[test]
    fn nix_settings_parse_defaults_and_reject_store_paths() {
        assert_eq!(parse_nix_gcroots_dir(None).unwrap(), None);
        assert_eq!(parse_nix_gcroots_dir(Some("  ".into())).unwrap(), None);
        assert_eq!(
            parse_nix_gcroots_dir(Some(" /var/lib/chuggernaut/gcroots ".into())).unwrap(),
            Some(PathBuf::from("/var/lib/chuggernaut/gcroots"))
        );
        assert!(
            parse_nix_gcroots_dir(Some("gcroots".into())).is_err(),
            "a roots dir is an absolute host path"
        );

        assert_eq!(
            parse_nix_path("WORKER_NIX_CLIENT", None, NIX_CLIENT_DEFAULT).unwrap(),
            PathBuf::from(NIX_CLIENT_DEFAULT)
        );
        assert_eq!(
            parse_nix_path(
                "WORKER_NIX_DAEMON_SOCKET",
                Some(String::new()),
                NIX_DAEMON_SOCKET_DEFAULT
            )
            .unwrap(),
            PathBuf::from(NIX_DAEMON_SOCKET_DEFAULT)
        );
        assert!(
            NIX_CLIENT_DEFAULT.starts_with("/nix/var/nix/profiles/"),
            "the default client must resolve through the profiles, which are a GC root"
        );
        assert_eq!(
            parse_nix_path("WORKER_NIX_STORE_DIR", None, NIX_STORE_DIR_DEFAULT).unwrap(),
            PathBuf::from("/nix/store")
        );
        assert_eq!(
            parse_nix_path(
                "WORKER_NIX_STORE_DIR",
                Some(" /mnt/nix/store ".into()),
                NIX_STORE_DIR_DEFAULT
            )
            .unwrap(),
            PathBuf::from("/mnt/nix/store")
        );

        let store = "/nix/store/h2zwqsnfmrh0f8asaash8qwm8lygikcw-nix-2.34.7/bin/nix";
        let err = parse_nix_path("WORKER_NIX_CLIENT", Some(store.into()), NIX_CLIENT_DEFAULT)
            .unwrap_err()
            .to_string();
        assert!(err.contains("store path"), "{err}");
        assert!(
            parse_nix_gcroots_dir(Some(store.into())).is_err(),
            "a hash is a hash wherever it is written"
        );
    }

    /// `WORKER_NIX_PROJECTS` (design #373 Decision 2 rule 3): the same
    /// fail-closed allow-list `WORKER_KVM_PROJECTS` is, refusals named after the
    /// setting the operator typed, and the flake client defaults through the
    /// profiles beside the store-path client.
    #[test]
    fn nix_projects_are_fail_closed_and_the_flake_client_defaults() {
        assert_eq!(
            parse_projects("WORKER_NIX_PROJECTS", None).unwrap(),
            Vec::<String>::new(),
            "unset grants nobody"
        );
        assert_eq!(
            parse_projects("WORKER_NIX_PROJECTS", Some(" acme/beacon ".into())).unwrap(),
            vec!["acme/beacon".to_string()]
        );
        let err = parse_projects("WORKER_NIX_PROJECTS", Some("beacon".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("WORKER_NIX_PROJECTS"), "{err}");

        assert_eq!(
            parse_nix_path("WORKER_NIX_FLAKE_CLIENT", None, NIX_FLAKE_CLIENT_DEFAULT).unwrap(),
            PathBuf::from(NIX_FLAKE_CLIENT_DEFAULT)
        );
        assert!(
            NIX_FLAKE_CLIENT_DEFAULT.starts_with("/nix/var/nix/profiles/"),
            "the flake client must resolve through the profiles, which are a GC root"
        );
    }

    /// The realise bound (design #373 3c): unset is the documented default, a
    /// number is taken as given, and zero or a typo is REFUSED — the launch path
    /// must never end up unbounded by a value that read as "off".
    #[test]
    fn nix_realise_timeout_parses_default_and_rejections() {
        assert_eq!(
            parse_nix_realise_timeout_secs(None).unwrap(),
            NIX_REALISE_TIMEOUT_SECS_DEFAULT
        );
        assert_eq!(
            parse_nix_realise_timeout_secs(Some(" \t".into())).unwrap(),
            NIX_REALISE_TIMEOUT_SECS_DEFAULT
        );
        assert_eq!(
            parse_nix_realise_timeout_secs(Some(" 40 ".into())).unwrap(),
            40
        );
        let err = parse_nix_realise_timeout_secs(Some("0".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("at least 1"), "{err}");
        assert!(parse_nix_realise_timeout_secs(Some("ages".into())).is_err());
    }

    /// The bound is REFUSED at parse time when it cannot fit inside the `launch`
    /// RPC the realise runs in (design #373 3c): past the dispatcher's
    /// [`store::worker::OP_TIMEOUT`] the task has already failed on transport, so
    /// a longer bound buys nothing and hides the failure it was meant to name.
    #[test]
    fn nix_realise_timeout_is_refused_over_the_launch_rpc_budget() {
        const {
            assert!(
                NIX_REALISE_TIMEOUT_SECS_MAX < store::worker::OP_TIMEOUT.as_secs(),
                "the ceiling must leave the RPC room for the container create"
            );
        }
        assert_eq!(
            parse_nix_realise_timeout_secs(Some(NIX_REALISE_TIMEOUT_SECS_DEFAULT.to_string()))
                .unwrap(),
            NIX_REALISE_TIMEOUT_SECS_DEFAULT,
            "the default must be a value the parse itself accepts"
        );
        assert_eq!(
            parse_nix_realise_timeout_secs(Some(NIX_REALISE_TIMEOUT_SECS_MAX.to_string())).unwrap(),
            NIX_REALISE_TIMEOUT_SECS_MAX
        );
        let err = parse_nix_realise_timeout_secs(Some("600".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("over the ceiling"), "{err}");
        assert!(
            err.contains("launch"),
            "the refusal names the coupling: {err}"
        );
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

    /// `WORKER_HOST_ROOT` (design #309 P0): unset is the documented default, an
    /// absolute path is taken as given, and the same no-store-hash rule holds —
    /// the root is worker-writable state that outlives a task, so a collectable
    /// store path would be exactly wrong.
    #[test]
    fn host_root_parses_default_and_rejects_a_store_hash() {
        assert_eq!(
            parse_host_root(None).unwrap(),
            PathBuf::from(container::host::HOST_ROOT_DEFAULT)
        );
        assert_eq!(
            parse_host_root(Some(" \t".into())).unwrap(),
            PathBuf::from(container::host::HOST_ROOT_DEFAULT)
        );
        assert_eq!(
            parse_host_root(Some(" /data/chug/host ".into())).unwrap(),
            PathBuf::from("/data/chug/host")
        );
        let store = "/nix/store/3zr1pgwpc00zrj8qc8d631bdfw1z9c5y-host-tasks";
        let err = parse_host_root(Some(store.into())).unwrap_err().to_string();
        assert!(err.contains("WORKER_HOST_ROOT"), "{err}");
        assert!(parse_host_root(Some("host-tasks".into())).is_err());
    }

    /// `WORKER_HOST_PROJECTS` (design #309 §10): unset runs host work for
    /// NOBODY, the fail-closed answer §10 asserts and the tree did not have —
    /// and a malformed or repeated entry is refused rather than kept as a
    /// tenancy that can never match a `JOB_PROJECT`.
    #[test]
    fn host_projects_are_fail_closed_when_unset() {
        for empty in [None, Some(String::new()), Some("  ".into())] {
            assert_eq!(
                parse_projects("WORKER_HOST_PROJECTS", empty).unwrap(),
                Vec::<String>::new(),
                "an undeclared host node serves no project"
            );
        }
        assert_eq!(
            parse_projects(
                "WORKER_HOST_PROJECTS",
                Some(" acme/beacon , acme/api ".into())
            )
            .unwrap(),
            vec!["acme/beacon".to_string(), "acme/api".to_string()]
        );

        for bad in ["beacon", "acme/", "/beacon", "acme/b/c", "acme/beacon,", ""] {
            let err = parse_projects("WORKER_HOST_PROJECTS", Some(format!("acme/api,{bad}")))
                .unwrap_err()
                .to_string();
            assert!(err.contains("WORKER_HOST_PROJECTS"), "{bad:?}: {err}");
        }
        let err = parse_projects("WORKER_HOST_PROJECTS", Some("acme/api,acme/api".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than once"), "{err}");
    }

    /// `WORKER_MODES` (design #322 W1): unset is container-only — today's
    /// behavior — a list is taken in the order declared, and every malformed
    /// shape is refused rather than partially adopted.
    #[test]
    fn modes_parse_default_list_and_rejections() {
        assert_eq!(parse_modes(None).unwrap(), vec![WorkerMode::Container]);
        assert_eq!(
            parse_modes(Some(String::new())).unwrap(),
            vec![WorkerMode::Container]
        );
        assert_eq!(
            parse_modes(Some(" \t".into())).unwrap(),
            vec![WorkerMode::Container]
        );
        assert_eq!(
            parse_modes(Some("host".into())).unwrap(),
            vec![WorkerMode::Host]
        );
        assert_eq!(
            parse_modes(Some(" container , host ".into())).unwrap(),
            vec![WorkerMode::Container, WorkerMode::Host]
        );
        assert_eq!(
            parse_modes(Some("host,container".into())).unwrap(),
            vec![WorkerMode::Host, WorkerMode::Container]
        );

        for bad in [
            "vm",
            "Container",
            "container,",
            ",container",
            "container,,host",
        ] {
            assert!(
                parse_modes(Some(bad.into())).is_err(),
                "must reject {bad:?}"
            );
        }
        let err = parse_modes(Some("container,container".into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than once"), "{err}");
        let err = parse_modes(Some("kvm".into())).unwrap_err().to_string();
        assert!(err.contains("container | host"), "{err}");
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

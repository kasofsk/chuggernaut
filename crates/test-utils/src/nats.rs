//! NATS server for tier-2 tests (testing.md), managed by the
//! [`testcontainers`] crate (#206).
//!
//! ## One instance per test process (principle 2)
//!
//! [`NatsTestServer::shared`] starts a single JetStream-enabled `nats` container
//! the first time any test in a binary asks for it and hands every later caller
//! the same running instance. The container handle is owned by a process-wide
//! `OnceCell`, so it lives for the whole test binary (testcontainers stops a
//! container on `Drop`, and a `static` is never dropped — the testcontainers
//! reaper cleans it up after the process exits). Combined with per-test
//! namespacing ([`store::NatsStore::connect_namespaced`]) this lets the whole
//! tier-2 suite share one server.
//!
//! [`NatsTestServer::spawn`] is the per-test-isolation escape hatch: a private
//! container just for the caller (torn down on drop), for tests that genuinely
//! need their own server.
//!
//! ## Skip semantics
//!
//! When Docker is unavailable the container cannot start, so `spawn`/`shared`
//! return `None` and callers skip — exactly as the previous hand-rolled harness
//! did (see the `require_nats!` macro). Setting the [`URL_ENV`] environment
//! variable points tests at an already-running NATS instead (CI with a baked
//! server, or a shared dev NATS); no container — and no Docker — is then needed.

use std::time::Duration;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tokio::sync::OnceCell;

/// The `nats` image + tag started for tier-2. JetStream is enabled via the
/// `-js` flag (plain server) or a mounted config (the operator-mode variant).
const IMAGE_NAME: &str = "nats";
const IMAGE_TAG: &str = "2.10-alpine";
/// The server's own readiness log line (printed on stderr).
const READY_LOG: &str = "Server is ready";
/// Env override: reuse an already-running NATS at this URL instead of starting
/// a container. When set, Docker is not required. Also the escape valve for a
/// resource-constrained host: point every binary at one externally-managed NATS
/// (e.g. `docker run -d -p 4222:4222 nats:2.10-alpine -js`) so a whole-workspace
/// run stands up a single server instead of one container per binary.
pub const URL_ENV: &str = "CHUG_TEST_NATS_URL";

/// Docker label stamped on every harness-started NATS container, so the
/// stale-container sweep only ever touches our own.
const LEAK_LABEL: &str = "chug.test.nats";

/// Best-effort reap of harness containers older than 30 minutes (see the
/// comment at the sweep call site). Shells out to `docker` — the harness
/// already requires Docker, and a subprocess keeps this synchronous-simple.
fn sweep_stale_containers() {
    let Ok(out) = std::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            concat!("label=", "chug.test.nats"),
            "--format",
            "{{.ID}}\t{{.RunningFor}}",
        ])
        .output()
    else {
        return;
    };
    let stale: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (id, age) = l.split_once('\t')?;
            let stale = age.contains("hour")
                || age.contains("day")
                || age
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n >= 30 && age.contains("minute"));
            stale.then(|| id.to_string())
        })
        .collect();
    if stale.is_empty() {
        return;
    }
    eprintln!(
        "test-utils: reaping {} stale NATS test container(s) (no ryuk in testcontainers-rs)",
        stale.len()
    );
    let _ = std::process::Command::new("docker")
        .arg("rm")
        .arg("-f")
        .args(&stale)
        .output();
}

/// A NATS server available to a tier-2 test: either a testcontainers-managed
/// container (torn down on drop) or an externally-provided URL ([`URL_ENV`]).
pub struct NatsTestServer {
    url: String,
    /// Owns the container so it is stopped on drop. `None` when reusing an
    /// external URL, or for the process-shared instance held in a `static`.
    _container: Option<ContainerAsync<GenericImage>>,
}

/// Process-wide shared server (principle 2). `Option` so a Docker-less run
/// resolves to `None` once and every caller skips.
static SHARED: OnceCell<Option<NatsTestServer>> = OnceCell::const_new();

impl NatsTestServer {
    /// The single per-process server (principle 2). The first caller in a test
    /// binary starts the container; later callers get the running instance.
    /// `None` when NATS is unavailable (Docker down and no [`URL_ENV`]).
    pub async fn shared() -> Option<&'static NatsTestServer> {
        SHARED
            .get_or_init(|| async { NatsTestServer::start(None, true).await })
            .await
            .as_ref()
    }

    /// A private, isolated server just for the caller (the escape hatch for
    /// tests that need their own instance — e.g. the worker daemon and cli-init
    /// tests, which connect with the empty prefix and cannot be namespaced onto
    /// the shared server). Torn down when the returned value drops. `None` when
    /// unavailable.
    pub async fn spawn() -> Option<Self> {
        Self::start(None, false).await
    }

    /// Spawn a private server with an extra config body — the operator/resolver
    /// stanza used by the §7.4 auth tests. The harness owns JetStream (a
    /// `store_dir` is prepended), so the body must NOT set `jetstream {}`; the
    /// listen port is the image default (4222), mapped to an ephemeral host
    /// port by testcontainers.
    pub async fn spawn_with_config(config: &str) -> Option<Self> {
        Self::start(Some(config), false).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed."
    )]
    async fn start(config: Option<&str>, shared: bool) -> Option<Self> {
        if shared
            && config.is_none()
            && let Ok(url) = std::env::var(URL_ENV)
            && !url.is_empty()
        {
            if let Some(hostport) = url.strip_prefix("nats://")
                && tokio::net::TcpStream::connect(hostport).await.is_ok()
            {
                return Some(Self {
                    url,
                    _container: None,
                });
            }
            eprintln!(
                "test-utils: {URL_ENV}={url} is UNREACHABLE — falling back to a per-process container"
            );
        }

        sweep_stale_containers();
        let build = || {
            let image = GenericImage::new(IMAGE_NAME, IMAGE_TAG)
                .with_exposed_port(4222.tcp())
                .with_wait_for(WaitFor::message_on_stderr(READY_LOG))
                .with_labels([(LEAK_LABEL, "1")]);
            match config {
                None => image.with_cmd(["-js"]),
                Some(body) => image
                    .with_copy_to("/etc/nats-test/nats.conf", full_config(body).into_bytes())
                    .with_cmd(["-c", "/etc/nats-test/nats.conf"]),
            }
        };

        let mut attempt = 0;
        let container = loop {
            attempt += 1;
            match build().start().await {
                Ok(c) => break c,
                Err(e) if attempt < 5 => {
                    tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                    let _ = e;
                }
                Err(e) => {
                    eprintln!(
                        "skipping: NATS testcontainer could not start (is Docker running?): {e}"
                    );
                    return None;
                }
            }
        };

        let host = match container.get_host().await {
            Ok(h) => h.to_string(),
            Err(e) => {
                eprintln!("skipping: NATS container host unavailable: {e}");
                return None;
            }
        };
        let port = match container.get_host_port_ipv4(4222.tcp()).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skipping: NATS container port unmapped: {e}");
                return None;
            }
        };
        let url = format!("nats://{host}:{port}");

        if !await_accept(&url).await {
            eprintln!("skipping: NATS at {url} never accepted a connection");
            return None;
        }

        Some(Self {
            url,
            _container: Some(container),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Probe the client port until a real NATS connection succeeds (bounded).
/// Goes through `store` so test-utils keeps no direct `async-nats` dependency.
async fn await_accept(url: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if store::NatsStore::connect(url).await.is_ok() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wrap a caller-supplied config body (auth/resolver stanza only) with the
/// harness-owned JetStream store so the body stays free of infra details. The
/// store dir is inside the container's own filesystem.
fn full_config(body: &str) -> String {
    format!("jetstream {{ store_dir: \"/tmp/js\" }}\n{body}")
}

/// Skip guard: binds a shared [`NatsTestServer`] or returns early (test
/// skipped). Uses the process-shared instance (#206), so a whole test binary
/// starts at most one container.
#[macro_export]
macro_rules! require_nats {
    () => {
        match $crate::nats::NatsTestServer::shared().await {
            Some(server) => server,
            None => return,
        }
    };
}

/// Skip guard for a **private** server booted with an extra config body
/// (operator-mode auth tests). Not shareable — the config is test-specific.
#[macro_export]
macro_rules! require_nats_config {
    ($config:expr) => {
        match $crate::nats::NatsTestServer::spawn_with_config($config).await {
            Some(server) => server,
            None => return,
        }
    };
}

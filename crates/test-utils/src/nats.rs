//! NATS server for tier-2 tests (testing.md), backed by a local `nats-server`
//! process or the [`testcontainers`] crate (#206, #408).
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
//! server just for the caller (torn down on drop), for tests that genuinely
//! need their own instance.
//!
//! ## How a private server is obtained (#408)
//!
//! A private server is a `nats-server -js` **process** when the binary is on
//! `PATH` — an OS-chosen port (`-p -1`) and a fresh store directory per caller,
//! killed and removed on drop — and a container otherwise. The process path is
//! what makes the five private-server files run in a job container, which has a
//! baked `nats-server` and no Docker socket ([#309](docs/design/309-host-native-execution.md) §10).
//! It is deliberately *not* used for [`NatsTestServer::shared`]: that handle
//! lives in a `static` that is never dropped, so a child process there would
//! outlive the test binary with no reaper to collect it. [`LOCAL_ENV`]`=0` forces
//! the container path.
//!
//! ## Skip semantics
//!
//! When neither a local binary nor Docker can serve, `spawn`/`shared` return
//! `None` and callers skip — exactly as the previous hand-rolled harness did
//! (see the `require_nats!` macro). Setting the [`URL_ENV`] environment variable
//! points *shared* tests at an already-running NATS instead (CI with a baked
//! server, or a shared dev NATS); no container — and no Docker — is then needed.
//!
//! An *impossible* start is answered once, not once per caller: a client-init
//! failure means no runtime exists to retry against, so the verdict is recorded
//! process-wide (`RUNTIME_UNREACHABLE`), and a missing local binary is recorded
//! the same way (`LOCAL_UNAVAILABLE`), so every later caller skips instantly.
//! Only a transient container failure is retried with backoff (#407).

use std::path::Path;
use std::process::Child;
use std::sync::{Once, OnceLock};
use std::time::Duration;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{
        IntoContainerPort, WaitFor,
        error::{ClientError, TestcontainersError},
    },
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

/// Env override: set to `0` to force a private server onto the container path
/// even when a local `nats-server` is on `PATH`. The `#[cfg(test)]` regression
/// test for #407's fast skip uses it to reach the container retry loop.
pub const LOCAL_ENV: &str = "CHUG_TEST_NATS_LOCAL";

/// The binary a private server is started from when it is on `PATH` —
/// `deploy/prod/Dockerfile.agent-rust` bakes it at `/usr/local/bin/nats-server`.
const LOCAL_BINARY: &str = "nats-server";

/// The server's own line announcing the port it was given by `-p -1`.
const LOCAL_LISTEN_LOG: &str = "Listening for client connections on";

/// Readiness polls before a local server is given up on (principle 3: every
/// loop is bounded). 25 ms apart, so 5 s — a `nats-server` starts in well
/// under one.
const LOCAL_POLLS_MAX: u32 = 200;

/// Docker label stamped on every harness-started NATS container, so the
/// stale-container sweep only ever touches our own.
const LEAK_LABEL: &str = "chug.test.nats";

/// Start attempts before a *transient* container failure is given up on
/// (principle 3: every loop is bounded).
const ATTEMPTS_MAX: u64 = 5;

/// Best-effort reap of harness containers older than 30 minutes, run at most
/// once per test process. Shells out to `docker` — the harness already requires
/// Docker, and a subprocess keeps this synchronous-simple.
fn sweep_stale_containers() {
    static SWEPT: Once = Once::new();
    SWEPT.call_once(sweep_stale_containers_now);
}

fn sweep_stale_containers_now() {
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

/// A NATS server available to a tier-2 test: a local `nats-server` process, a
/// testcontainers-managed container (both torn down on drop), or an
/// externally-provided URL ([`URL_ENV`]).
pub struct NatsTestServer {
    url: String,
    /// Owns the local process and its store dir so both are reclaimed on drop.
    /// `None` unless this server came from [`LOCAL_BINARY`].
    _local: Option<LocalServer>,
    /// Owns the container so it is stopped on drop. `None` when reusing an
    /// external URL, or for the process-shared instance held in a `static`.
    _container: Option<ContainerAsync<GenericImage>>,
}

/// A private `nats-server` process and the temp dir holding its JetStream store,
/// its config and its log. Dropped in declaration order: the process is killed
/// before the directory under it is removed.
struct LocalServer {
    child: Child,
    _store: tempfile::TempDir,
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Process-wide shared server (principle 2). `Option` so a Docker-less run
/// resolves to `None` once and every caller skips.
static SHARED: OnceCell<Option<NatsTestServer>> = OnceCell::const_new();

/// Set the first time a start attempt proves no container runtime is reachable,
/// so every later caller in the process skips without attempting again.
static RUNTIME_UNREACHABLE: OnceLock<()> = OnceLock::new();

/// Set the first time an exec proves there is no [`LOCAL_BINARY`] on `PATH`, so
/// every later private spawn goes straight to the container path (#407's
/// "an impossible start is answered once", one level down).
static LOCAL_UNAVAILABLE: OnceLock<()> = OnceLock::new();

/// A start failure that no retry can fix: the Docker client could not be built,
/// configured, or pointed at a host. Anything else may be transient.
fn is_runtime_unreachable(error: &TestcontainersError) -> bool {
    matches!(
        error,
        TestcontainersError::Client(
            ClientError::Init(_)
                | ClientError::Configuration(_)
                | ClientError::InvalidDockerHost(_)
        )
    )
}

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
    /// `store_dir` is prepended) and the listen port, so the body must NOT set
    /// `jetstream {}` or `port`.
    pub async fn spawn_with_config(config: &str) -> Option<Self> {
        Self::start(Some(config), false).await
    }

    async fn start(config: Option<&str>, shared: bool) -> Option<Self> {
        if shared
            && config.is_none()
            && let Some(server) = Self::start_external().await
        {
            return Some(server);
        }
        if !shared && let Some(server) = Self::start_local(config).await {
            return Some(server);
        }
        Self::start_container(config).await
    }

    /// Reuse the already-running NATS named by [`URL_ENV`], when it answers.
    async fn start_external() -> Option<Self> {
        let url = std::env::var(URL_ENV).ok().filter(|u| !u.is_empty())?;
        if let Some(hostport) = url.strip_prefix("nats://")
            && tokio::net::TcpStream::connect(hostport).await.is_ok()
        {
            return Some(Self {
                url,
                _local: None,
                _container: None,
            });
        }
        eprintln!(
            "test-utils: {URL_ENV}={url} is UNREACHABLE — falling back to a per-process container"
        );
        None
    }

    /// Start a private `nats-server -js` process on an OS-chosen port with its
    /// own store directory. `None` when the binary is absent or the server never
    /// became ready, so the caller falls back to a container.
    async fn start_local(config: Option<&str>) -> Option<Self> {
        if LOCAL_UNAVAILABLE.get().is_some()
            || std::env::var(LOCAL_ENV).is_ok_and(|opt_out| opt_out == "0")
        {
            return None;
        }
        let store = tempfile::Builder::new()
            .prefix("chug-test-nats-")
            .tempdir()
            .ok()?;
        let log_path = store.path().join("server.log");
        let log = std::fs::File::create(&log_path).ok()?;
        let mut command = std::process::Command::new(LOCAL_BINARY);
        command.args(["-a", "127.0.0.1", "-p", "-1"]);
        match config {
            None => {
                command.arg("-js").arg("-sd").arg(store.path());
            }
            Some(body) => {
                let conf_path = store.path().join("nats.conf");
                let store_dir = store.path().join("js");
                let conf = full_config(&store_dir.to_string_lossy(), body);
                std::fs::write(&conf_path, conf).ok()?;
                command.arg("-c").arg(&conf_path);
            }
        }
        command.stdout(log.try_clone().ok()?).stderr(log);

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound && LOCAL_UNAVAILABLE.set(()).is_ok() {
                    eprintln!(
                        "test-utils: no `{LOCAL_BINARY}` on PATH — private servers fall back to a container"
                    );
                }
                return None;
            }
        };
        let Some(port) = start_local_port(&log_path, &mut child).await else {
            start_local_diagnose(&log_path);
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        Some(Self {
            url: format!("nats://127.0.0.1:{port}"),
            _local: Some(LocalServer {
                child,
                _store: store,
            }),
            _container: None,
        })
    }

    async fn start_container(config: Option<&str>) -> Option<Self> {
        if RUNTIME_UNREACHABLE.get().is_some() {
            return None;
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
                    .with_copy_to(
                        "/etc/nats-test/nats.conf",
                        full_config("/tmp/js", body).into_bytes(),
                    )
                    .with_cmd(["-c", "/etc/nats-test/nats.conf"]),
            }
        };

        let mut attempt = 0;
        let container = loop {
            attempt += 1;
            match build().start().await {
                Ok(c) => break c,
                Err(e) if is_runtime_unreachable(&e) => {
                    if RUNTIME_UNREACHABLE.set(()).is_ok() {
                        eprintln!(
                            "skipping: no container runtime is reachable ({e}) — every later NATS test in this binary skips without retrying"
                        );
                    }
                    return None;
                }
                Err(_) if attempt < ATTEMPTS_MAX => {
                    tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
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
            _local: None,
            _container: Some(container),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Wait for a local server to log the port `-p -1` gave it, giving up early if
/// the process died instead (bounded by [`LOCAL_POLLS_MAX`]).
async fn start_local_port(log_path: &Path, child: &mut Child) -> Option<u16> {
    for _ in 0..LOCAL_POLLS_MAX {
        let log = std::fs::read_to_string(log_path).unwrap_or_default();
        if log.contains(READY_LOG)
            && let Some(port) = start_local_port_from_log(&log)
        {
            return Some(port);
        }
        if child.try_wait().ok().flatten().is_some() {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

fn start_local_port_from_log(log: &str) -> Option<u16> {
    log.lines()
        .find_map(|line| line.split_once(LOCAL_LISTEN_LOG))
        .and_then(|(_, addr)| addr.trim().rsplit_once(':'))
        .and_then(|(_, port)| port.trim().parse().ok())
}

/// Show the server's own words rather than a generic timeout, the way
/// `.chug/tasks/ci.sh`'s `start_gate_nats_local` does.
fn start_local_diagnose(log_path: &Path) {
    eprintln!(
        "test-utils: the local {LOCAL_BINARY} did not come up — falling back to a container. It said:"
    );
    for line in std::fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .take(20)
    {
        eprintln!("    {line}");
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
/// store dir is the container's own filesystem or the local server's temp dir.
fn full_config(store_dir: &str, body: &str) -> String {
    format!("jetstream {{ store_dir: \"{store_dir}\" }}\n{body}")
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

#[cfg(test)]
mod tests {
    use super::{
        Duration, LOCAL_ENV, NatsTestServer, RUNTIME_UNREACHABLE, start_local_port_from_log,
    };

    #[test]
    fn a_local_servers_port_is_read_from_its_own_log() {
        let log = "[1] [INF] Listening for client connections on 127.0.0.1:39461\n[1] [INF] Server is ready\n";
        assert_eq!(start_local_port_from_log(log), Some(39461));
        assert_eq!(
            start_local_port_from_log("[1] [INF] Starting nats-server"),
            None
        );
    }

    /// A start that cannot succeed is answered once per process, not once per
    /// caller — before #407 each of these paid the 5 s retry backoff.
    #[tokio::test]
    async fn an_unreachable_runtime_costs_one_attempt_for_the_whole_process() {
        const SPAWNS: usize = 20;
        const BUDGET: Duration = Duration::from_secs(2);
        unsafe { std::env::set_var(LOCAL_ENV, "0") };
        unsafe { std::env::set_var("DOCKER_HOST", "unix:///nonexistent/chug-test.sock") };

        let started = std::time::Instant::now();
        for _ in 0..SPAWNS {
            assert!(NatsTestServer::spawn().await.is_none());
        }

        let elapsed = started.elapsed();
        assert!(RUNTIME_UNREACHABLE.get().is_some(), "verdict not recorded");
        assert!(
            elapsed < BUDGET,
            "{SPAWNS} impossible spawns took {elapsed:?}, over the {BUDGET:?} budget"
        );
    }
}

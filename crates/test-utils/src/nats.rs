//! Ephemeral NATS server for tier-2 tests (testing.md).
//!
//! Two mechanisms, tried in order so tier-2 runs wherever *any* of them is
//! available:
//!
//! 1. **Local `nats-server` binary** on `PATH` — a JetStream-enabled process
//!    on an OS-assigned port, killed on drop. This is what the CI `agent-rust`
//!    image bakes in (a ~15 MB static binary), so the merge-gate actually
//!    executes this tier instead of silently skipping it.
//! 2. **`nats:2-alpine` in Docker** on an ephemeral host port, removed on
//!    drop — the local dev fallback when the binary is absent but Docker is up.
//!
//! Neither available → [`NatsTestServer::spawn`] returns `None` so callers can
//! skip (see the `require_nats!` macro).

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const IMAGE: &str = "nats:2-alpine";
// Generous enough to tolerate a heavily-loaded CI box (a full `cargo test
// --workspace` spawns many of these in parallel while the machine still
// compiles).
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The running server's control mechanism; [`Drop`] tears it down.
enum Backend {
    /// A local `nats-server` child process.
    Local(Child),
    /// A Docker container id.
    Docker(String),
}

pub struct NatsTestServer {
    backend: Backend,
    url: String,
    /// Local-mode JetStream store, removed with the server.
    _store_dir: Option<tempfile::TempDir>,
    /// Keeps a mounted/loaded config file alive for the server's lifetime.
    _config_dir: Option<tempfile::TempDir>,
}

impl NatsTestServer {
    /// Spawn a JetStream-enabled NATS server. Returns `None` when no mechanism
    /// (local binary or Docker) is available so callers can skip. Panics on
    /// unexpected failures — a half-working environment should be loud.
    pub fn spawn() -> Option<Self> {
        Self::spawn_inner(None)
    }

    /// Spawn with an extra config body — the operator/resolver stanza used by
    /// the §7.4 auth tests. The harness owns the listen port and JetStream
    /// store, so the body must NOT set `port:` or `jetstream {}`.
    pub fn spawn_with_config(config: &str) -> Option<Self> {
        Self::spawn_inner(Some(config))
    }

    fn spawn_inner(config: Option<&str>) -> Option<Self> {
        if local_nats_available() {
            return Some(Self::spawn_local(config));
        }
        if docker_available() {
            return Some(Self::spawn_docker(config));
        }
        eprintln!("skipping: no NATS available (no `nats-server` binary, Docker daemon down)");
        None
    }

    // -- Local `nats-server` process -------------------------------------

    fn spawn_local(config: Option<&str>) -> Self {
        let store_dir = tempfile::tempdir().expect("tempdir (jetstream store)");
        let log_dir = tempfile::tempdir().expect("tempdir (nats log)");
        let log_path = log_dir.path().join("nats.log");
        let log = std::fs::File::create(&log_path).expect("create nats log");
        let log_err = log.try_clone().expect("clone nats log handle");

        // `-p -1` lets the server pick a free port atomically and write the
        // chosen address to `<ports_dir>/nats-server_<pid>.ports` — no
        // bind-then-release race between many parallel test servers.
        let mut cmd = Command::new("nats-server");
        cmd.args(["-a", "127.0.0.1", "-p", "-1"])
            .args(["--ports_file_dir", &log_dir.path().display().to_string()]);
        let config_dir = match config {
            Some(body) => {
                let dir = tempfile::tempdir().expect("tempdir (nats.conf)");
                let conf = dir.path().join("nats.conf");
                std::fs::write(&conf, full_config(body, store_dir.path()))
                    .expect("write nats.conf");
                cmd.args(["-c".into(), conf.display().to_string()]);
                Some(dir)
            }
            None => {
                cmd.args(["-js", "-sd", &store_dir.path().display().to_string()]);
                None
            }
        };
        let mut child = cmd
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .expect("spawn nats-server");

        let url = await_local_ready(&mut child, &log_path, log_dir.path());
        Self {
            backend: Backend::Local(child),
            url,
            _store_dir: Some(store_dir),
            _config_dir: config_dir,
        }
    }

    // -- Docker `nats:2-alpine` container --------------------------------

    fn spawn_docker(config: Option<&str>) -> Self {
        let config_dir = config.map(|body| {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(
                dir.path().join("nats.conf"),
                full_config(body, std::path::Path::new("/tmp/js")),
            )
            .expect("write nats.conf");
            dir
        });

        // 127.0.0.1:0 → Docker assigns an ephemeral host port. The config
        // variant skips --rm so a config error's logs survive the crash
        // (Drop still removes the container).
        let mut args: Vec<String> = ["run", "-d", "-p", "127.0.0.1:0:4222"]
            .map(String::from)
            .to_vec();
        if config_dir.is_none() {
            args.insert(2, "--rm".into());
        }
        match &config_dir {
            Some(dir) => {
                args.push("-v".into());
                args.push(format!("{}:/etc/nats-test", dir.path().display()));
                args.push(IMAGE.into());
                args.extend(["-c".into(), "/etc/nats-test/nats.conf".into()]);
            }
            None => args.extend([IMAGE.into(), "-js".into()]),
        }
        let run = Command::new("docker")
            .args(&args)
            .output()
            .expect("docker run");
        assert!(
            run.status.success(),
            "docker run failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let container_id = String::from_utf8_lossy(&run.stdout).trim().to_string();

        let port_out = Command::new("docker")
            .args(["port", &container_id, "4222/tcp"])
            .output()
            .expect("docker port");
        let mapping = String::from_utf8_lossy(&port_out.stdout);
        let port = mapping
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .unwrap_or_else(|| {
                // Config errors kill the container instantly; surface its logs
                // (the --rm may not have collected it yet).
                let logs = Command::new("docker")
                    .args(["logs", &container_id])
                    .output()
                    .map(|o| {
                        format!(
                            "{}{}",
                            String::from_utf8_lossy(&o.stdout),
                            String::from_utf8_lossy(&o.stderr)
                        )
                    })
                    .unwrap_or_default();
                let _ = Command::new("docker")
                    .args(["rm", "-f", &container_id])
                    .output();
                panic!("no port mapping for {container_id}; container logs:\n{logs}");
            })
            .trim()
            .to_string();

        let server = Self {
            backend: Backend::Docker(container_id),
            url: format!("nats://127.0.0.1:{port}"),
            _store_dir: None,
            _config_dir: config_dir,
        };
        server.await_docker_ready();
        server
    }

    fn await_docker_ready(&self) {
        let Backend::Docker(container_id) = &self.backend else {
            unreachable!("await_docker_ready on a non-Docker backend");
        };
        // The docker-proxy accepts TCP as soon as the port maps, well before
        // nats-server inside is up — so readiness is the server's own log line.
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let logs = Command::new("docker")
                .args(["logs", container_id])
                .output()
                .expect("docker logs");
            if String::from_utf8_lossy(&logs.stderr).contains("Server is ready") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "NATS container {container_id} not ready within {READY_TIMEOUT:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for NatsTestServer {
    fn drop(&mut self) {
        match &mut self.backend {
            Backend::Local(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            Backend::Docker(container_id) => {
                let _ = Command::new("docker")
                    .args(["rm", "-f", container_id])
                    .output();
            }
        }
    }
}

/// Wait for a local `nats-server` to report ready, returning its client URL
/// (read from the ports file the server writes once it is listening). Fails
/// fast — and loud — if the process exits early instead of blocking the whole
/// timeout on an empty log.
fn await_local_ready(
    child: &mut Child,
    log_path: &std::path::Path,
    ports_dir: &std::path::Path,
) -> String {
    let ports_file = ports_dir.join(format!("nats-server_{}.ports", child.id()));
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "nats-server exited early ({status}); log:\n{}",
                read_to_string_lossy(log_path)
            );
        }
        let log = read_to_string_lossy(log_path);
        if log.contains("Server is ready")
            && let Some(url) = read_ports_url(&ports_file)
        {
            return url;
        }
        assert!(
            Instant::now() < deadline,
            "local nats-server not ready within {READY_TIMEOUT:?}; log:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_to_string_lossy(path: &std::path::Path) -> String {
    let mut buf = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut buf);
    }
    buf
}

/// Extract the client URL from a `nats-server` ports file, whose body is
/// `{"nats":["nats://127.0.0.1:PORT"], ...}`. Returns `None` until the file
/// exists and holds a `nats://` URL.
fn read_ports_url(ports_file: &std::path::Path) -> Option<String> {
    let body = std::fs::read_to_string(ports_file).ok()?;
    let start = body.find("nats://")?;
    let rest = &body[start..];
    let end = rest.find('"').unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// True when a `nats-server` binary is on `PATH`.
fn local_nats_available() -> bool {
    Command::new("nats-server")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True when the Docker daemon answers.
fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Wrap a caller-supplied config body (auth/resolver stanza only) with the
/// harness-owned JetStream store so the body stays free of infra details. The
/// listen port is owned by the harness too: `-p -1` for a local process, and
/// the default 4222 inside Docker (mapped out to an ephemeral host port).
fn full_config(body: &str, store_dir: &std::path::Path) -> String {
    format!(
        "jetstream {{ store_dir: \"{}\" }}\n{body}",
        store_dir.display()
    )
}

/// Skip guard: binds a [`NatsTestServer`] or returns early (test skipped).
#[macro_export]
macro_rules! require_nats {
    () => {
        match $crate::nats::NatsTestServer::spawn() {
            Some(server) => server,
            None => return,
        }
    };
}

/// Skip guard for a server booted with an extra config body (operator-mode
/// auth tests).
#[macro_export]
macro_rules! require_nats_config {
    ($config:expr) => {
        match $crate::nats::NatsTestServer::spawn_with_config($config) {
            Some(server) => server,
            None => return,
        }
    };
}

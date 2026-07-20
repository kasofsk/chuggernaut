//! Embedded NATS server for tier-2 tests (testing.md): a JetStream-enabled
//! `nats-server` in a Docker container on an ephemeral host port, removed on
//! drop. Docker unavailable → tests skip via [`NatsTestServer::spawn`]
//! returning `None` (see the `require_nats!` macro).

use std::process::Command;
use std::time::{Duration, Instant};

const IMAGE: &str = "nats:2-alpine";
const READY_TIMEOUT: Duration = Duration::from_secs(30);

pub struct NatsTestServer {
    container_id: String,
    url: String,
    /// Keeps a mounted config file alive for the container's lifetime.
    _config_dir: Option<tempfile::TempDir>,
}

impl NatsTestServer {
    /// Spawn a JetStream-enabled NATS server. Returns `None` when the Docker
    /// daemon is unavailable (CI without Docker, etc.) so callers can skip.
    /// Panics on unexpected failures — a half-working Docker should be loud.
    pub fn spawn() -> Option<Self> {
        Self::spawn_inner(None)
    }

    /// Spawn with a full `nats-server` config body (operator-mode auth tests).
    /// The config must define its own listen port 4222 and jetstream settings.
    pub fn spawn_with_config(config: &str) -> Option<Self> {
        Self::spawn_inner(Some(config))
    }

    fn spawn_inner(config: Option<&str>) -> Option<Self> {
        let daemon_up = Command::new("docker")
            .args(["info", "--format", "{{.ServerVersion}}"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !daemon_up {
            eprintln!("skipping: Docker daemon unavailable");
            return None;
        }

        let config_dir = config.map(|body| {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join("nats.conf"), body).expect("write nats.conf");
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
                let _ = Command::new("docker").args(["rm", "-f", &container_id]).output();
                panic!("no port mapping for {container_id}; container logs:\n{logs}");
            })
            .trim()
            .to_string();

        let server = Self {
            container_id,
            url: format!("nats://127.0.0.1:{port}"),
            _config_dir: config_dir,
        };
        server.await_ready();
        Some(server)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn await_ready(&self) {
        // The docker-proxy accepts TCP as soon as the port maps, well before
        // nats-server inside is up — so readiness is the server's own log line.
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let logs = Command::new("docker")
                .args(["logs", &self.container_id])
                .output()
                .expect("docker logs");
            if String::from_utf8_lossy(&logs.stderr).contains("Server is ready") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "NATS container {} not ready within {READY_TIMEOUT:?}",
                self.container_id
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for NatsTestServer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container_id])
            .output();
    }
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

/// Skip guard for a server booted with a full config body (operator-mode
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

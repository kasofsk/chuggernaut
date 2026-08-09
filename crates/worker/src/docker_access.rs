//! Node-side docker-reachability discovery (design #517 D4).
//!
//! accepts: the daemon's own view of docker — `DOCKER_HOST`, the active CLI
//! context under the daemon's `HOME`, and the node's configured
//! `WORKER_DOCKER_ENDPOINT`; emits: whether this node's daemon reached a docker
//! endpoint, advertised on `NodeCapabilities` for both execution modes;
//! guarantees: the probe only reads (a `GET /_ping`, never a create, start or
//! stop), is bounded by a per-candidate timeout over a fixed candidate list, and
//! never refuses a boot — a node with no docker is a normal node.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one candidate endpoint has to answer before the probe calls it
/// unreachable (docs/reference/style.md Tier 2 rule 3). Per candidate, so the
/// whole boot-time discovery is bounded by the candidate list times this.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Ceiling on a docker config or context file the resolution reads, so walking
/// the daemon's `HOME` can never hand the probe an unbounded file.
const CONFIG_BYTES_MAX: u64 = 1 << 20;

/// The daemon's own view of where docker is, as the environment presents it:
/// the three variables the docker CLI reads, plus the `HOME` it reads its
/// context out of. Taken as a value rather than read inline, so the resolution
/// is tested against a fixture tree instead of the machine running the tests.
#[derive(Debug, Clone, Default)]
pub struct DockerEnv {
    /// `DOCKER_HOST`, the CLI's first rule and the only one that needs no file.
    pub docker_host: Option<String>,
    /// `DOCKER_CONTEXT`, which overrides `config.json`'s `currentContext`.
    pub docker_context: Option<String>,
    /// `DOCKER_CONFIG` — the CLI's config *directory*, not a file.
    pub docker_config: Option<PathBuf>,
    /// The daemon's `HOME`, which is also the `HOME` a host task inherits.
    pub home: Option<PathBuf>,
}

impl DockerEnv {
    /// The daemon's own environment, which is what a host task's docker CLI
    /// resolves against too: `container::host` carries `PATH` and `HOME` from
    /// the daemon, so the two views share the context file.
    pub fn from_env() -> Self {
        Self {
            docker_host: non_empty(std::env::var("DOCKER_HOST").ok()),
            docker_context: non_empty(std::env::var("DOCKER_CONTEXT").ok()),
            docker_config: non_empty(std::env::var("DOCKER_CONFIG").ok()).map(PathBuf::from),
            home: non_empty(std::env::var("HOME").ok()).map(PathBuf::from),
        }
    }

    /// Every endpoint this node's daemon might reach docker at, in the order the
    /// CLI resolves them and with the node's configured container endpoint last.
    /// Deduplicated, so a node whose config names the socket its context also
    /// names is asked once.
    pub fn candidates(&self, configured: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for candidate in [
            self.docker_host.clone(),
            self.context_endpoint(),
            non_empty(Some(configured.to_string())),
        ]
        .into_iter()
        .flatten()
        {
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
        out
    }

    /// The directory the CLI keeps `config.json` and its contexts in.
    fn config_dir(&self) -> Option<PathBuf> {
        self.docker_config
            .clone()
            .or_else(|| self.home.as_ref().map(|home| home.join(".docker")))
    }

    /// The endpoint the **active context** names, resolved the way the CLI
    /// resolves it: `DOCKER_CONTEXT` or `config.json`'s `currentContext`, then
    /// that name's `meta.json` under the sha256 of the name. `default` is the
    /// built-in context and carries no metadata file, so it resolves nothing.
    fn context_endpoint(&self) -> Option<String> {
        let dir = self.config_dir()?;
        let name = match &self.docker_context {
            Some(name) => name.clone(),
            None => json_at(
                &read_bounded(&dir.join("config.json"))?,
                &["currentContext"],
            )?,
        };
        if name.is_empty() || name == "default" {
            return None;
        }
        let meta = dir
            .join("contexts")
            .join("meta")
            .join(format!("{:x}", Sha256::digest(name.as_bytes())))
            .join("meta.json");
        non_empty(json_at(
            &read_bounded(&meta)?,
            &["Endpoints", "docker", "Host"],
        ))
    }
}

/// What this node's daemon found when it looked for a docker endpoint: the one
/// that answered, and every endpoint it asked. A node fact assembled at boot
/// from the machine itself, never operator-typed.
#[derive(Debug, Clone, Default)]
pub struct DockerAccess {
    endpoint: Option<String>,
    searched: Vec<String>,
}

impl DockerAccess {
    /// Ask each candidate in turn and stop at the first that answers, which is
    /// exactly what the node advertises. A candidate that refuses is recorded
    /// rather than dropped, so the boot log names what was asked.
    pub async fn probe(env: &DockerEnv, configured: &str, timeout: Duration) -> Self {
        let mut access = Self::default();
        for candidate in env.candidates(configured) {
            access.searched.push(candidate.clone());
            match container::docker::endpoint_answers(&candidate, timeout).await {
                Ok(()) => {
                    access.endpoint = Some(candidate);
                    break;
                }
                Err(e) => tracing::debug!(endpoint = %candidate, "no docker endpoint here: {e}"),
            }
        }
        access
    }

    /// Whether the daemon reached a docker endpoint, which is the whole of what
    /// [`types::worker::NodeCapabilities::docker_reachable`] claims — never that
    /// a given launch is handed the socket.
    pub fn reachable(&self) -> bool {
        self.endpoint.is_some()
    }

    /// The endpoint that answered, for the boot log and the operator.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// A record reporting `endpoint` as the one that answered, so a test of what
    /// a node ADVERTISES needs no socket to serve; every test of the probe
    /// itself runs against a fixture socket instead.
    #[cfg(test)]
    pub fn fixture_reached(endpoint: &str) -> Self {
        Self {
            endpoint: Some(endpoint.to_string()),
            searched: vec![endpoint.to_string()],
        }
    }

    /// The endpoints the probe actually asked, as the boot log prints them: a
    /// node whose environment named none is reported as such rather than as an
    /// empty string.
    pub fn searched_display(&self) -> String {
        if self.searched.is_empty() {
            return "none".to_string();
        }
        self.searched.join(", ")
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// A string at a path of object keys, or `None` — malformed JSON and a missing
/// key are both "this file names nothing".
fn json_at(body: &str, keys: &[&str]) -> Option<String> {
    let mut node: serde_json::Value = serde_json::from_str(body).ok()?;
    for key in keys {
        node = node.get(key)?.clone();
    }
    node.as_str().map(str::to_string)
}

/// One small config file the CLI would read, or `None` — absent, unreadable and
/// past [`CONFIG_BYTES_MAX`] are all "this node names no context".
fn read_bounded(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > CONFIG_BYTES_MAX {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test fixture that cannot be built fails the test"
)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chug-docker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A `~/.docker` naming `context` as current and pointing it at `host`,
    /// laid out the way the CLI lays it out.
    fn context_fixture(root: &Path, context: &str, host: &str) -> PathBuf {
        let dir = root.join(".docker");
        let meta = dir
            .join("contexts")
            .join("meta")
            .join(format!("{:x}", Sha256::digest(context.as_bytes())));
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            dir.join("config.json"),
            format!(r#"{{"currentContext":"{context}"}}"#),
        )
        .unwrap();
        std::fs::write(
            meta.join("meta.json"),
            format!(r#"{{"Name":"{context}","Endpoints":{{"docker":{{"Host":"{host}"}}}}}}"#),
        )
        .unwrap();
        dir
    }

    /// The route the #516 measurement found on `gumbo-air-0`: no `DOCKER_HOST`,
    /// and the socket named by the active context under the daemon's own `HOME`
    /// — which is the `HOME` a host task's CLI resolves against too.
    #[test]
    fn the_active_context_resolves_the_socket_the_cli_would_use() {
        let root = temp_dir("context");
        context_fixture(&root, "colima", "unix:///colima/docker.sock");

        let env = DockerEnv {
            home: Some(root.clone()),
            ..Default::default()
        };
        assert_eq!(
            env.context_endpoint().as_deref(),
            Some("unix:///colima/docker.sock")
        );

        let overridden = DockerEnv {
            docker_context: Some("default".into()),
            ..env.clone()
        };
        assert_eq!(
            overridden.context_endpoint(),
            None,
            "the built-in context carries no metadata file"
        );
        assert_eq!(
            DockerEnv::default().context_endpoint(),
            None,
            "an environment naming no HOME resolves nothing, and never panics"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The candidate list is the CLI's own order with the node's configured
    /// container endpoint last, deduplicated — so a node whose config already
    /// names its socket is asked once, not twice.
    #[test]
    fn candidates_are_the_daemons_own_view_in_the_clis_order() {
        let root = temp_dir("candidates");
        context_fixture(&root, "colima", "unix:///colima/docker.sock");
        let env = DockerEnv {
            docker_host: Some("unix:///env/docker.sock".into()),
            home: Some(root.clone()),
            ..Default::default()
        };
        assert_eq!(
            env.candidates("unix:///var/run/docker.sock"),
            vec![
                "unix:///env/docker.sock",
                "unix:///colima/docker.sock",
                "unix:///var/run/docker.sock",
            ]
        );

        let plain = DockerEnv::default();
        assert_eq!(
            plain.candidates("unix:///var/run/docker.sock"),
            vec!["unix:///var/run/docker.sock"],
            "a node whose environment names nothing still has its configured endpoint"
        );
        assert_eq!(
            DockerEnv {
                docker_host: Some("unix:///var/run/docker.sock".into()),
                ..Default::default()
            }
            .candidates("unix:///var/run/docker.sock")
            .len(),
            1,
            "the same endpoint twice is one probe"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A node with no reachable endpoint advertises false, and says what it
    /// asked (design #517 D4) — the probe is never a boot refusal, so this is
    /// the whole of what a docker-less node does differently.
    #[tokio::test]
    async fn a_node_with_no_reachable_endpoint_advertises_false() {
        let root = temp_dir("absent");
        let env = DockerEnv {
            docker_host: Some(format!("unix://{}", root.join("nothing.sock").display())),
            ..Default::default()
        };
        let access = DockerAccess::probe(&env, "unix:///chug/absent.sock", PROBE_TIMEOUT).await;
        assert!(!access.reachable());
        assert_eq!(access.endpoint(), None);
        assert!(
            access
                .searched_display()
                .contains("unix:///chug/absent.sock"),
            "{}",
            access.searched_display()
        );
        assert_eq!(DockerAccess::default().searched_display(), "none");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The probe is a probe, not a grant: the only request it puts on the socket
    /// is the API's read-only `GET /_ping`, so discovery creates, starts and
    /// stops nothing.
    #[tokio::test]
    async fn the_probe_asks_one_read_only_ping_and_nothing_else() {
        let root = temp_dir("ping");
        let socket = root.join("docker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let served = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = String::new();
            let mut buf = [0u8; 512];
            loop {
                let read = stream.read(&mut buf).await.unwrap();
                if read == 0 {
                    break;
                }
                request.push_str(&String::from_utf8_lossy(&buf[..read]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .await
                .unwrap();
            request
        });

        let env = DockerEnv {
            docker_host: Some(format!("unix://{}", socket.display())),
            ..Default::default()
        };
        let access = DockerAccess::probe(&env, "unix:///chug/absent.sock", PROBE_TIMEOUT).await;
        assert!(access.reachable(), "the fixture endpoint answered");
        assert_eq!(
            access.endpoint(),
            Some(format!("unix://{}", socket.display())).as_deref()
        );

        let request = served.await.unwrap();
        let line = request.lines().next().unwrap_or_default().to_string();
        assert!(line.starts_with("GET "), "{request}");
        assert!(line.contains("_ping"), "{request}");
        for verb in ["POST", "PUT", "DELETE"] {
            assert!(!request.contains(verb), "{verb} in {request}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}

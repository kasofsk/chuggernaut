//! Dispatcher configuration (spec §12.4). AGENT_PROVIDER_DEFAULT is required —
//! the dispatcher refuses to start without it.

use crate::core::{CoreConfig, CoreError, Result};
use container::docker::DockerNodeConfig;
use std::path::PathBuf;

/// Everything `chuggernaut dispatcher` needs, read from environment variables
/// (spec §12.4). Paths default to the §12.3 deployment layout.
#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    /// `NATS_URL` (default `nats://localhost:4222`).
    pub nats_url: String,
    /// `REPOS_ROOT` (default `/data/repos`) — bare repos volume (§12.2).
    pub repos_root: PathBuf,
    /// `REPO_URL_BASE` — base for the `REPO_URL` injected into containers;
    /// must be reachable from every fleet node (§3.1). Defaults to
    /// `file://{repos_root}` until the SSH front (§5.2/auth) lands.
    pub repo_url_base: String,
    /// `KEYS_DIR` (default `/data/keys`) — where `chuggernaut init` wrote the
    /// keypairs (§12.1). The dispatcher reads `age_private.key` from here.
    pub keys_dir: PathBuf,
    /// `NATS_URL_CONTAINER` — the NATS URL injected into containers, when it
    /// differs from the dispatcher's own (e.g. dispatcher on the Docker host
    /// uses `localhost`, containers need `host.docker.internal`). Defaults to
    /// `NATS_URL`.
    pub nats_url_container: Option<String>,
    /// `CHANNEL_BINARY` — path to the chuggernaut-channel binary injected into
    /// agent containers (§4.2). Unset → agents run without the channel MCP.
    pub channel_binary: Option<PathBuf>,
    /// `AGENT_PROVIDER_DEFAULT` (required, §12.4): `claude` | `codex`.
    pub agent_provider_default: String,
    /// `AGENT_MODEL_DEFAULT` (optional, §12.4).
    pub agent_model_default: Option<String>,
    /// `DOCKER_NODES` — comma-separated `name|endpoint|slots` entries.
    /// Unset → single local-socket node with `DOCKER_SLOTS` slots (default 4).
    pub docker_nodes: Vec<DockerNodeConfig>,
    /// `HOOK_BIN` — chuggernaut binary path baked into new repos' pre-receive
    /// hooks (§5.2), as seen from the SSH host (e.g.
    /// `/usr/local/bin/chuggernaut` inside the sshd container). Unset → this
    /// process's own path.
    pub hook_bin: Option<PathBuf>,
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl DispatcherConfig {
    pub fn from_env() -> Result<Self> {
        let repos_root =
            PathBuf::from(env_opt("REPOS_ROOT").unwrap_or_else(|| "/data/repos".into()));
        let repo_url_base = env_opt("REPO_URL_BASE")
            .unwrap_or_else(|| format!("file://{}", repos_root.display()));

        let agent_provider_default = env_opt("AGENT_PROVIDER_DEFAULT").ok_or_else(|| {
            CoreError::Config(
                "AGENT_PROVIDER_DEFAULT is required (claude | codex) — see spec §12.4".into(),
            )
        })?;
        if !matches!(agent_provider_default.as_str(), "claude" | "codex") {
            return Err(CoreError::Config(format!(
                "AGENT_PROVIDER_DEFAULT must be claude or codex, got {agent_provider_default:?}"
            )));
        }

        let docker_nodes = match env_opt("DOCKER_NODES") {
            Some(spec) => parse_docker_nodes(&spec)?,
            None => {
                let slots = match env_opt("DOCKER_SLOTS") {
                    Some(s) => s.parse().map_err(|_| {
                        CoreError::Config(format!("DOCKER_SLOTS must be a number, got {s:?}"))
                    })?,
                    None => 4,
                };
                vec![DockerNodeConfig {
                    name: "local".into(),
                    endpoint: "unix:///var/run/docker.sock".into(),
                    slots,
                }]
            }
        };

        Ok(Self {
            nats_url: env_opt("NATS_URL").unwrap_or_else(|| "nats://localhost:4222".into()),
            repos_root,
            repo_url_base,
            keys_dir: PathBuf::from(env_opt("KEYS_DIR").unwrap_or_else(|| "/data/keys".into())),
            nats_url_container: env_opt("NATS_URL_CONTAINER"),
            channel_binary: env_opt("CHANNEL_BINARY").map(PathBuf::from),
            agent_provider_default,
            agent_model_default: env_opt("AGENT_MODEL_DEFAULT"),
            docker_nodes,
            hook_bin: env_opt("HOOK_BIN").map(PathBuf::from),
        })
    }

    /// Read the age identity written by `chuggernaut init` (§12.1). Missing
    /// file → None: secrets are injected as stored (dev raw mode, §8.2).
    pub async fn age_identity(&self) -> Result<Option<String>> {
        let path = self.keys_dir.join("age_private.key");
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    /// Read the artifacts age identity written by `chuggernaut init` (§12.1).
    /// Distinct from `age_private.key`: the API also holds this one so it can
    /// decrypt transcripts for display, while the secrets key stays
    /// dispatcher-only (§10.2). Missing file → None: capture is disabled.
    pub async fn artifacts_identity(&self) -> Result<Option<String>> {
        let path = self.keys_dir.join("age_artifacts.key");
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    /// Read the platform NATS account seed written by `chuggernaut init`
    /// (§12.1). Missing file → None: containers connect unauthenticated
    /// (dev mode without the operator-mode server).
    pub async fn nats_account_seed(&self) -> Result<Option<String>> {
        let path = self.keys_dir.join("nats_account.seed");
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => Ok(Some(s.trim().to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    /// Read `dispatcher.creds` for the dispatcher's own NATS connection.
    pub async fn dispatcher_creds(&self) -> Result<Option<String>> {
        let path = self.keys_dir.join("dispatcher.creds");
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::Config(format!("reading {}: {e}", path.display()))),
        }
    }

    pub async fn core_config(&self) -> Result<CoreConfig> {
        let ssh_ca = self.keys_dir.join("ssh_ca");
        Ok(CoreConfig {
            ssh_ca: ssh_ca.is_file().then_some(ssh_ca),
            repo_url_base: self.repo_url_base.clone(),
            nats_url: self.nats_url_container.clone().unwrap_or_else(|| self.nats_url.clone()),
            channel_binary: self.channel_binary.clone(),
            age_identity: self.age_identity().await?,
            artifacts_identity: self.artifacts_identity().await?,
            agent_provider_default: Some(self.agent_provider_default.clone()),
            agent_model_default: self.agent_model_default.clone(),
            nats_account_seed: self.nats_account_seed().await?,
            hook_bin: self.hook_bin.clone(),
        })
    }
}

fn parse_docker_nodes(spec: &str) -> Result<Vec<DockerNodeConfig>> {
    spec.split(',')
        .map(|entry| {
            let parts: Vec<&str> = entry.trim().split('|').collect();
            let [name, endpoint, slots] = parts.as_slice() else {
                return Err(CoreError::Config(format!(
                    "DOCKER_NODES entry {entry:?}: expected name|endpoint|slots"
                )));
            };
            Ok(DockerNodeConfig {
                name: name.to_string(),
                endpoint: endpoint.to_string(),
                slots: slots.parse().map_err(|_| {
                    CoreError::Config(format!("DOCKER_NODES entry {entry:?}: bad slot count"))
                })?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_nodes_parse() {
        let nodes =
            parse_docker_nodes("a|unix:///var/run/docker.sock|4, b|tcp://10.0.0.2:2375|8").unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "a");
        assert_eq!(nodes[1].endpoint, "tcp://10.0.0.2:2375");
        assert_eq!(nodes[1].slots, 8);
        assert!(parse_docker_nodes("bad-entry").is_err());
        assert!(parse_docker_nodes("a|tcp://x|lots").is_err());
    }
}

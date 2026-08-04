//! Dispatcher configuration (spec §12.4). AGENT_PROVIDER_DEFAULT is required —
//! the dispatcher refuses to start without it.
//!
//! - **Accepts:** the process environment at startup.
//! - **Emits:** a validated `Config` value.
//! - **Guarantees:** fails fast — refuses to start on missing/invalid config.
//! - **Spec:** §12.4.

use crate::core::{CoreConfig, CoreError, Result};
use container::PlacementPolicy;
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
    /// `TRIAGE_IMAGE` — platform-level image for operator-dispatched triage
    /// agents (spec §1.2). A platform default rather than the failing job's own
    /// type image, so triage works uniformly on any job type (agent/command/
    /// human). Unset → the triage action is unavailable (422).
    pub triage_image: Option<String>,
    /// `DOCKER_NODES` — comma-separated `name|endpoint|slots` entries.
    /// Unset → single local-socket node with `DOCKER_SLOTS` slots (default 4).
    pub docker_nodes: Vec<DockerNodeConfig>,
    /// `PLACEMENT_POLICY` (`busyness` | `headroom`, §3.1). Platform-level: it
    /// applies to the whole fleet, not per job type. Unset → `busyness`.
    pub placement_policy: PlacementPolicy,
    /// `HOOK_BIN` — chuggernaut binary path baked into new repos' pre-receive
    /// hooks (§5.2), as seen from the SSH host (e.g.
    /// `/usr/local/bin/chuggernaut` inside the sshd container). Unset → this
    /// process's own path.
    pub hook_bin: Option<PathBuf>,
    /// `SELF_REPO` — the platform's own source repo as `owner/project`, hosted
    /// as a chuggernaut project. When set, the config snapshot resolves its
    /// `main` tip and how many commits the deployed dispatcher SHA is behind it,
    /// so operators get live "is prod in sync" deploy-drift visibility. Unset →
    /// the drift fields stay `None`.
    pub self_repo: Option<(String, String)>,
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl DispatcherConfig {
    pub fn from_env() -> Result<Self> {
        let repos_root =
            PathBuf::from(env_opt("REPOS_ROOT").unwrap_or_else(|| "/data/repos".into()));
        let repo_url_base =
            env_opt("REPO_URL_BASE").unwrap_or_else(|| format!("file://{}", repos_root.display()));

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

        let placement_policy = match env_opt("PLACEMENT_POLICY") {
            Some(v) => PlacementPolicy::parse(&v).map_err(CoreError::Config)?,
            None => PlacementPolicy::default(),
        };

        let docker_nodes = match std::env::var("DOCKER_NODES") {
            Ok(spec) if spec.trim().is_empty() => Vec::new(),
            Ok(spec) => parse_docker_nodes(&spec)?,
            Err(_) => {
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
            triage_image: env_opt("TRIAGE_IMAGE"),
            docker_nodes,
            placement_policy,
            hook_bin: env_opt("HOOK_BIN").map(PathBuf::from),
            self_repo: env_opt("SELF_REPO").and_then(|s| {
                s.split_once('/')
                    .map(|(owner, project)| (owner.to_string(), project.to_string()))
            }),
        })
    }

    /// One `keys_dir` file, verbatim. Missing file → None: every key below is
    /// optional by design (dev modes run without them), so a *missing* file is
    /// configuration and only an unreadable one is an error.
    async fn key_file_read(&self, name: &str) -> Result<Option<String>> {
        let path = self.keys_dir.join(name);
        match tokio::fs::read_to_string(&path).await {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::Config(format!(
                "reading {}: {e}",
                path.display()
            ))),
        }
    }

    /// [`Self::key_file_read`], trimmed — for the single-line keys and seeds,
    /// where a trailing newline from an editor would corrupt the value.
    async fn key_file_read_trimmed(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .key_file_read(name)
            .await?
            .map(|s| s.trim().to_string()))
    }

    /// Read the age identity written by `chuggernaut init` (§12.1). Missing
    /// file → None: secrets are injected as stored (dev raw mode, §8.2).
    pub async fn age_identity(&self) -> Result<Option<String>> {
        self.key_file_read_trimmed("age_private.key").await
    }

    /// Read the artifacts age identity written by `chuggernaut init` (§12.1).
    /// Distinct from `age_private.key`: the API also holds this one so it can
    /// decrypt transcripts for display, while the secrets key stays
    /// dispatcher-only (§10.2). Missing file → None: capture is disabled.
    pub async fn artifacts_identity(&self) -> Result<Option<String>> {
        self.key_file_read_trimmed("age_artifacts.key").await
    }

    /// Read the platform NATS account seed written by `chuggernaut init`
    /// (§12.1). Missing file → None: containers connect unauthenticated
    /// (dev mode without the operator-mode server).
    pub async fn nats_account_seed(&self) -> Result<Option<String>> {
        self.key_file_read_trimmed("nats_account.seed").await
    }

    /// Read `dispatcher.creds` for the dispatcher's own NATS connection. NOT
    /// trimmed: a creds file is a multi-line PEM-ish document that async-nats
    /// parses as written.
    pub async fn dispatcher_creds(&self) -> Result<Option<String>> {
        self.key_file_read("dispatcher.creds").await
    }

    /// Read the OIDC issuer keypair `chuggernaut init` wrote (§12.1, design
    /// #313 A2), with the identifier every minted token's `iss` carries. Either
    /// half missing → None: a platform that predates the key mints nothing, and
    /// a job type declaring `workload_identities:` fails its launch loudly.
    async fn oidc_issuer(&self) -> Result<Option<crate::core::OidcIssuer>> {
        let (Some(private_pem), Some(public_pem)) = (
            self.key_file_read("oidc_private.pem").await?,
            self.key_file_read("oidc_public.pem").await?,
        ) else {
            return Ok(None);
        };
        let issuer = auth::oidc::issuer_from_env()
            .map_err(|e| CoreError::Config(format!("oidc issuer: {e}")))?;
        Ok(Some(crate::core::OidcIssuer {
            private_pem,
            public_pem,
            issuer,
        }))
    }

    pub async fn core_config(&self) -> Result<CoreConfig> {
        let ssh_ca = self.keys_dir.join("ssh_ca");
        Ok(CoreConfig {
            ssh_ca: ssh_ca.is_file().then_some(ssh_ca),
            oidc_issuer: self.oidc_issuer().await?,
            repo_url_base: self.repo_url_base.clone(),
            nats_url: self
                .nats_url_container
                .clone()
                .unwrap_or_else(|| self.nats_url.clone()),
            channel_binary: self.channel_binary.clone(),
            age_identity: self.age_identity().await?,
            artifacts_identity: self.artifacts_identity().await?,
            agent_provider_default: Some(self.agent_provider_default.clone()),
            agent_model_default: self.agent_model_default.clone(),
            triage_image: self.triage_image.clone(),
            nats_account_seed: self.nats_account_seed().await?,
            hook_bin: self.hook_bin.clone(),
            launch_queue_max_wait: None,
            worker_heartbeat_timeout: None,
        })
    }
}

/// The deployed dispatcher's own build SHA (`CHUG_GIT_SHA`, baked at build time
/// — the SHA component that `version_string()` embeds). `None` for local/dev
/// builds without it baked in; the drift fields then stay unpopulated. Reads
/// *this* crate's build environment, which is why it stays here rather than
/// travelling with the CD snapshot into `chuggernaut-platform-ops`.
pub fn deployed_sha() -> Option<String> {
    option_env!("CHUG_GIT_SHA").map(|s| s.to_string())
}

impl DispatcherConfig {
    /// The static parts of the config snapshot, mapped once at startup from
    /// this config plus the boot fleet probe. The dynamic parts (per-node
    /// health/version, self-repo tip, commits-behind) are recomputed each scan
    /// tick by [`chuggernaut_platform_ops::cd::refresh`] — the context owns the
    /// freshness half, the config owns this mapping of itself onto the wire
    /// type.
    pub fn base_snapshot(
        &self,
        fleet: &[container::NodeStatus],
        deployed_sha: Option<String>,
        secrets_encryption: bool,
    ) -> types::DispatcherConfigSnapshot {
        types::DispatcherConfigSnapshot {
            nodes: self
                .docker_nodes
                .iter()
                .map(|n| {
                    let status = fleet.iter().find(|s| s.name == n.name);
                    types::WorkerNode {
                        name: n.name.clone(),
                        endpoint: n.endpoint.clone(),
                        slots: status.and_then(|s| s.slots).unwrap_or(n.slots),
                        available: status.map(|s| s.available).unwrap_or(true),
                        version: status.and_then(|s| s.version.clone()),
                        refresh_outcome: status.and_then(|s| s.refresh_outcome.clone()),
                        capacity_source: status.and_then(|s| s.capacity.map(|c| c.source())),
                        capacity_observed_at: status
                            .and_then(|s| s.capacity.and_then(|c| c.observed_at)),
                    }
                })
                .collect(),
            agent_provider_default: self.agent_provider_default.clone(),
            agent_model_default: self.agent_model_default.clone(),
            triage_image: self.triage_image.clone(),
            repos_root: self.repos_root.display().to_string(),
            repo_url_base: self.repo_url_base.clone(),
            nats_url: self.nats_url.clone(),
            nats_url_container: self.nats_url_container.clone(),
            channel_binary: self
                .channel_binary
                .as_ref()
                .map(|p| p.display().to_string()),
            hook_bin: self.hook_bin.as_ref().map(|p| p.display().to_string()),
            secrets_encryption,
            dispatcher_sha: deployed_sha,
            main_tip_sha: None,
            commits_behind: None,
            placement_policy: self.placement_policy.as_str().to_string(),
            schema_epoch: types::CONFIG_SCHEMA_EPOCH,
        }
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
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(CoreError::Config(format!(
                    "DOCKER_NODES entry {entry:?}: node name must be [A-Za-z0-9_-]+"
                )));
            }
            if *endpoint != "worker"
                && !endpoint.starts_with("unix://")
                && !endpoint.starts_with("tcp://")
                && !endpoint.starts_with("http://")
            {
                return Err(CoreError::Config(format!(
                    "DOCKER_NODES entry {entry:?}: endpoint must be unix://…, tcp://…, or `worker`"
                )));
            }
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]
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

    #[test]
    fn docker_nodes_worker_form() {
        let nodes =
            parse_docker_nodes("local|unix:///var/run/docker.sock|0, nuc|worker|4").unwrap();
        assert_eq!(nodes[1].endpoint, "worker");
        assert_eq!(nodes[1].slots, 4);
        assert!(parse_docker_nodes("nuc.0|worker|4").is_err());
        assert!(parse_docker_nodes("|worker|4").is_err());
        assert!(parse_docker_nodes("a|ssh://host|4").is_err());
    }

    fn snapshot_config() -> DispatcherConfig {
        DispatcherConfig {
            nats_url: "nats://localhost:4222".into(),
            repos_root: PathBuf::from("/data/repos"),
            repo_url_base: "file:///data/repos".into(),
            keys_dir: PathBuf::from("/data/keys"),
            nats_url_container: None,
            channel_binary: None,
            agent_provider_default: "claude".into(),
            agent_model_default: None,
            triage_image: None,
            docker_nodes: vec![DockerNodeConfig {
                name: "nuc".into(),
                endpoint: "worker".into(),
                slots: 4,
            }],
            hook_bin: None,
            self_repo: None,
            placement_policy: PlacementPolicy::Busyness,
        }
    }

    /// The base snapshot maps config + boot fleet probe onto the wire type,
    /// carrying the deployed SHA and leaving the scan-filled drift fields empty.
    /// The drift fields stay empty on an install with `SELF_REPO` unset: the
    /// gate is `ConfigSnapshot::self_repo` in `platform_ops::cd::refresh`, which
    /// skips the tip lookup entirely, so the UI reads "unavailable".
    #[test]
    fn base_snapshot_maps_fleet_and_sha() {
        let fleet = vec![container::NodeStatus {
            name: "nuc".into(),
            available: false,
            version: Some("0.1.0+abc".into()),
            refresh_outcome: None,
            slots: None,
            capacity: None,
        }];
        let snap = snapshot_config().base_snapshot(&fleet, Some("abc".into()), true);
        assert_eq!(snap.nodes.len(), 1);
        assert!(!snap.nodes[0].available);
        assert_eq!(snap.nodes[0].version.as_deref(), Some("0.1.0+abc"));
        assert_eq!(snap.dispatcher_sha.as_deref(), Some("abc"));
        assert_eq!(snap.main_tip_sha, None);
        assert_eq!(snap.commits_behind, None);
    }

    /// The boot snapshot's slot count and its provenance always describe the
    /// same number (design #293 §7): a worker node whose startup probe pulled
    /// capacity publishes the *observed* count as `node`, while a docker-endpoint
    /// node — which reports no live slots — keeps its `DOCKER_NODES` seed and no
    /// provenance at all. Publishing a seed labelled `node` would make the §8
    /// never-observed warning read against a number it doesn't describe.
    #[test]
    fn base_snapshot_publishes_observed_capacity_with_its_provenance() {
        let observed_at = chrono::Utc::now();
        let fleet = vec![container::NodeStatus {
            name: "nuc".into(),
            available: true,
            version: None,
            refresh_outcome: None,
            slots: Some(2),
            capacity: Some(types::worker::ObservedCapacity {
                mark: (1_000, 0),
                slots_max: Some(8),
                observed_at: Some(observed_at),
            }),
        }];
        let snap = snapshot_config().base_snapshot(&fleet, None, true);
        assert_eq!(snap.nodes[0].slots, 2, "the seed outlived its observation");
        assert_eq!(
            snap.nodes[0].capacity_source,
            Some(types::worker::CapacitySource::Node)
        );
        assert_eq!(snap.nodes[0].capacity_observed_at, Some(observed_at));

        let unobserved = vec![container::NodeStatus {
            name: "nuc".into(),
            available: true,
            version: None,
            refresh_outcome: None,
            slots: Some(4),
            capacity: Some(types::worker::ObservedCapacity::default()),
        }];
        let snap = snapshot_config().base_snapshot(&unobserved, None, true);
        assert_eq!(snap.nodes[0].slots, 4);
        assert_eq!(
            snap.nodes[0].capacity_source,
            Some(types::worker::CapacitySource::Seed)
        );
        assert_eq!(snap.nodes[0].capacity_observed_at, None);

        let docker = vec![container::NodeStatus {
            name: "nuc".into(),
            available: true,
            version: None,
            refresh_outcome: None,
            slots: None,
            capacity: None,
        }];
        let snap = snapshot_config().base_snapshot(&docker, None, true);
        assert_eq!(snap.nodes[0].slots, 4);
        assert_eq!(snap.nodes[0].capacity_source, None);
    }

    /// The republish decision `platform_ops::cd::refresh` makes compares
    /// serialized bytes: an identical snapshot is skipped (the common
    /// every-30s no-op), a changed node version fires.
    #[test]
    fn republish_only_on_change() {
        let fleet = vec![container::NodeStatus {
            name: "nuc".into(),
            available: true,
            version: Some("0.1.0+abc".into()),
            refresh_outcome: None,
            slots: None,
            capacity: None,
        }];
        let base = snapshot_config().base_snapshot(&fleet, Some("abc".into()), true);
        let published = serde_json::to_vec(&base).unwrap();

        let same = serde_json::to_vec(&base).unwrap();
        assert_eq!(published, same);

        let mut changed = base.clone();
        changed.nodes[0].version = Some("0.2.0+def".into());
        assert_ne!(published, serde_json::to_vec(&changed).unwrap());

        let mut behind = base.clone();
        behind.main_tip_sha = Some("def456".into());
        behind.commits_behind = Some(2);
        assert_ne!(published, serde_json::to_vec(&behind).unwrap());
    }

    #[test]
    fn placement_policy_parses_defaults_and_rejects_unknown() {
        assert_eq!(PlacementPolicy::default(), PlacementPolicy::Busyness);
        assert_eq!(
            PlacementPolicy::parse("busyness").unwrap(),
            PlacementPolicy::Busyness
        );
        assert_eq!(
            PlacementPolicy::parse("headroom").unwrap(),
            PlacementPolicy::Headroom
        );
        let err = PlacementPolicy::parse("random").unwrap_err();
        assert!(
            err.contains("busyness") && err.contains("headroom"),
            "{err}"
        );
    }
}

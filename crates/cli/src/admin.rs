//! `chuggernaut admin ...` — user and project management (spec §12.2–12.3).
//! Ingest tokens and secret key rotation land with their consumers.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::path::PathBuf;
use store::{NatsStore, keys};
use types::User;

#[derive(clap::Args, Debug)]
pub struct AdminArgs {
    #[arg(long, global = true, default_value = "nats://localhost:4222")]
    pub nats_url: String,
    /// Where `chuggernaut init` wrote the keypairs (§12.1). Used for
    /// `dispatcher.creds` (operator-mode NATS) and `age_public.key`
    /// (secret encryption); a plain dev server needs neither.
    #[arg(long, global = true, default_value = "/data/keys")]
    pub keys_dir: PathBuf,
    #[command(subcommand)]
    pub cmd: AdminCmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum AdminCmd {
    #[command(subcommand)]
    User(UserCmd),
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(subcommand)]
    Secret(SecretCmd),
    #[command(subcommand)]
    Var(VarCmd),
    /// Mint NATS credentials for a `chuggernaut worker` node daemon
    /// (spec §3.1): allowed only its own `req.worker.{node}.>` subjects.
    /// Local-only — reads the account seed, no NATS connection.
    WorkerCreds {
        /// Node name — must match its DOCKER_NODES entry (`{node}|worker|N`)
        /// and be subject-safe ([A-Za-z0-9_-]+).
        #[arg(long)]
        node: String,
        /// Output path; defaults to `worker-{node}.creds` in the keys dir.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Mint a worker node's read-only git credential (spec §3.1 self-refresh):
    /// generate a keypair, sign a long-lived READ-ONLY cert scoped to the
    /// platform repo, and write the two files to install under the node's
    /// `~/chuggernaut-worker/keys/` as `worker_git` + `worker_git-cert.pub`.
    /// Local-only — reads the `ssh_ca` key, no NATS connection.
    WorkerGitKey {
        /// Node name (its DOCKER_NODES entry, `{node}|worker|N`).
        #[arg(long)]
        node: String,
        /// Platform repo the node may pull, `{owner}/{project}` — the repo the
        /// node fetches its build context from over the ssh front.
        #[arg(long)]
        project: String,
        /// Directory to write `worker_git` + `worker_git-cert.pub` into;
        /// defaults to the keys dir.
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Cert validity in days (renew by re-running). Long-lived by default —
        /// a node credential is operator-installed, not per-job ephemeral.
        #[arg(long, default_value = "3650")]
        days: i64,
    },
    /// Ask a worker node to rebuild its images at a SHA and swap its daemon
    /// (spec §3.1 self-refresh). The dispatcher host cannot ssh a tagged worker,
    /// so the deploy inverts control and requests refresh over the worker RPC.
    /// Never fails the deploy: an unreachable/failed node is a WARNING with its
    /// version drift surfaced, not an error.
    WorkerRefresh {
        /// Node name (its `{node}|worker|N` DOCKER_NODES entry).
        #[arg(long)]
        node: String,
        /// Git SHA to build the node images at.
        #[arg(long)]
        sha: String,
        /// Image tag to build/run.
        #[arg(long, default_value = "prod")]
        tag: String,
        /// Seconds to wait for the node to come back on the new version. 0 →
        /// request and report the accept without confirming the swap (builds
        /// can take minutes; the new version still flows to the fleet snapshot).
        #[arg(long, default_value = "0")]
        wait_secs: u64,
    },
}

#[derive(clap::Args, Debug)]
pub struct ScopedName {
    /// `{owner}/{project}`.
    #[arg(long)]
    pub project: String,
    #[arg(long)]
    pub name: String,
}

impl ScopedName {
    fn split(&self) -> Result<(&str, &str)> {
        let Some((owner, project)) = self.project.split_once('/') else {
            bail!("--project must be owner/project, got {:?}", self.project);
        };
        Ok((owner, project))
    }
}

/// §8.2 secrets — encrypted with the platform age public key; values are
/// write-only from here (only the dispatcher decrypts).
#[derive(clap::Subcommand, Debug)]
pub enum SecretCmd {
    Set {
        #[command(flatten)]
        scoped: ScopedName,
        /// Omit to read the value from stdin (avoids shell history).
        #[arg(long)]
        value: Option<String>,
    },
    List {
        #[arg(long)]
        project: String,
    },
    Delete {
        #[command(flatten)]
        scoped: ScopedName,
    },
    /// Copy a secret's stored (encrypted) value between scopes without
    /// decrypting — e.g. promoting a project secret to the platform agent
    /// scope: `secret copy --from acme/demo --to global/agents --name TOKEN`.
    Copy {
        /// Source `{owner}/{project}` scope.
        #[arg(long)]
        from: String,
        /// Destination `{owner}/{project}` scope (`global/agents` = injected
        /// into every agent container).
        #[arg(long)]
        to: String,
        #[arg(long)]
        name: String,
    },
}

/// §8.1 vars — plaintext KV, injected as env by name.
#[derive(clap::Subcommand, Debug)]
pub enum VarCmd {
    Set {
        #[command(flatten)]
        scoped: ScopedName,
        #[arg(long)]
        value: String,
    },
    List {
        #[arg(long)]
        project: String,
    },
    Delete {
        #[command(flatten)]
        scoped: ScopedName,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum UserCmd {
    Create {
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: String,
        /// Grant platform_admin.
        #[arg(long)]
        admin: bool,
    },
    List,
    Delete {
        #[arg(long)]
        email: String,
    },
    /// Mint a bearer token for an existing user (§7.1): the same JWT a login
    /// issues, signed with `jwt_private.pem`, printed to stdout. For machine
    /// callers (`Authorization: Bearer <token>`) — e.g. a Claude session
    /// driving the API. The token carries the user's roles at mint time.
    Token {
        #[arg(long)]
        email: String,
        /// Lifetime, e.g. `720h` (30 days). Shared duration syntax (§1.1).
        #[arg(long, default_value = "720h")]
        ttl: String,
    },
    /// Grant, list, or remove a user's project roles (§7.5). Writes the
    /// `project_roles` map on the user record — the same `users.*` bucket
    /// `user create` seeds.
    #[command(subcommand)]
    Role(RoleCmd),
}

#[derive(clap::Subcommand, Debug)]
pub enum RoleCmd {
    /// Grant/update a user's role on a project.
    Set {
        #[arg(long)]
        email: String,
        /// `{owner}/{project}`.
        #[arg(long)]
        project: String,
        /// `owner` | `member` | `viewer` (`owner` is the top project role, §7.5).
        #[arg(long)]
        role: String,
    },
    /// List a user's project roles.
    List {
        #[arg(long)]
        email: String,
    },
    /// Remove a user's role on a project.
    Remove {
        #[arg(long)]
        email: String,
        /// `{owner}/{project}`.
        #[arg(long)]
        project: String,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum ProjectCmd {
    Create {
        #[arg(long)]
        owner: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "main")]
        default_branch: String,
        /// Path for bare git repos (must match the dispatcher's REPOS_ROOT).
        #[arg(long, default_value = "/data/repos")]
        repos_root: PathBuf,
        /// Path baked into the pre-receive hook (§5.2). Defaults to this
        /// binary — override when the SSH host sees the artifact elsewhere
        /// (e.g. `/usr/local/bin/chuggernaut` inside the sshd container).
        #[arg(long)]
        hook_bin: Option<PathBuf>,
    },
    /// Link an existing external repo (GitHub) as a new project: the
    /// dispatcher fetches the origin, creates the `integration` branch, and
    /// seeds the chuggernaut config. Requires a running dispatcher (it holds
    /// the deploy key) and the `CHUG_ORIGIN_DEPLOY_KEY` / `CHUG_ORIGIN_PAT`
    /// project secrets — set them first with `admin secret set`.
    Link {
        #[arg(long)]
        owner: String,
        #[arg(long)]
        name: String,
        /// Origin git URL, e.g. `ssh://git@github.com/acme/api.git`.
        #[arg(long)]
        origin_url: String,
        /// Origin default branch; autodetected from the origin when omitted.
        #[arg(long)]
        main_branch: Option<String>,
    },
    List {
        #[arg(long)]
        owner: Option<String>,
    },
}

pub async fn run(args: AdminArgs) -> Result<()> {
    // Local-only commands first — no NATS connection.
    if let AdminCmd::WorkerCreds { node, out } = &args.cmd {
        return mint_worker_creds(&args.keys_dir, node, out.as_deref()).await;
    }
    if let AdminCmd::WorkerGitKey {
        node,
        project,
        out_dir,
        days,
    } = &args.cmd
    {
        return mint_worker_git_key(&args.keys_dir, node, project, out_dir.as_deref(), *days).await;
    }
    // Operator-mode NATS requires the dispatcher credentials from init
    // (§12.1); without them (open dev server) connect plain.
    let store = match tokio::fs::read_to_string(args.keys_dir.join("dispatcher.creds")).await {
        Ok(creds) => NatsStore::connect_with_creds(&args.nats_url, &creds).await?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            NatsStore::connect(&args.nats_url).await?
        }
        Err(e) => bail!("reading dispatcher.creds: {e}"),
    };
    match args.cmd {
        AdminCmd::User(cmd) => run_user(&store, &args.keys_dir, cmd).await,
        AdminCmd::Project(cmd) => run_project(&store, cmd).await,
        AdminCmd::Secret(cmd) => run_secret(&store, &args.keys_dir, cmd).await,
        AdminCmd::Var(cmd) => run_var(&store, cmd).await,
        AdminCmd::WorkerRefresh {
            node,
            sha,
            tag,
            wait_secs,
        } => run_worker_refresh(&store, &node, &sha, &tag, wait_secs).await,
        AdminCmd::WorkerCreds { .. } | AdminCmd::WorkerGitKey { .. } => {
            unreachable!("handled before connect")
        }
    }
}

/// Request a worker self-refresh and report the outcome (spec §3.1). Soft by
/// design: it prints `refresh OK` / a `WARNING:` line and always returns `Ok`,
/// so a lagging or unreachable node never fails the deploy — the drift is
/// surfaced (here and via the fleet snapshot's per-node version), not fatal.
async fn run_worker_refresh(
    store: &NatsStore,
    node: &str,
    sha: &str,
    tag: &str,
    wait_secs: u64,
) -> Result<()> {
    use store::worker::WorkerRpc;
    use types::worker::RefreshRequest;

    let rpc = WorkerRpc::new(store.clone(), node);
    let req = RefreshRequest {
        sha: sha.to_string(),
        tag: tag.to_string(),
    };
    let ok = match rpc.refresh(&req).await {
        Ok(ok) => ok,
        Err(e) => {
            // Unreachable or refused: warn, don't fail the deploy.
            println!("WARNING: worker refresh node={node} not accepted (drift remains): {e}");
            return Ok(());
        }
    };
    if let Some(reason) = &ok.skipped {
        // The node has no git credential (spec §3.1 / #114): it could not even
        // attempt the refresh. Surface it LOUDLY so a deploy never looks like a
        // success that silently refreshed nothing. Non-fatal, like every other
        // refresh outcome — the drift also shows in the fleet snapshot.
        println!("node {node}: refresh SKIPPED — {reason}");
        return Ok(());
    }
    if !ok.accepted {
        // A refresh is already in flight (converging to some SHA) — not a
        // failure and not drift; nothing new to start or wait on.
        println!(
            "refresh already in progress: node={node} from={}",
            ok.from_version
        );
        return Ok(());
    }
    println!(
        "refresh requested: node={node} from={} -> sha={sha} tag={tag}",
        ok.from_version
    );

    if wait_secs == 0 {
        return Ok(());
    }
    // Confirm against the SAME reported field the fleet snapshot shows (ticket
    // #187): a swapped-in daemon carries the target SHA in its version, and the
    // surviving daemon of a FAILED refresh reports a `Failed` outcome — so a
    // broken refresh is surfaced immediately with its stage/error instead of
    // waiting out the whole timeout.
    use types::worker::{RefreshConfirmation, RefreshOutcome};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    loop {
        if let Ok(ping) = rpc.ping().await {
            match RefreshOutcome::confirm(sha, &ping.version, ping.refresh_outcome.as_ref()) {
                RefreshConfirmation::Confirmed => {
                    println!("refresh OK: node={node} version={}", ping.version);
                    return Ok(());
                }
                RefreshConfirmation::Failed { stage, error_tail } => {
                    println!(
                        "WARNING: worker refresh node={node} FAILED at {stage}: {error_tail} \
                         (prod stays on the old images; check the fleet snapshot)"
                    );
                    return Ok(());
                }
                RefreshConfirmation::Pending => {}
            }
        }
        if std::time::Instant::now() >= deadline {
            println!(
                "WARNING: worker refresh node={node} not confirmed within {wait_secs}s \
                 (build may still be running; check the fleet snapshot for its version)"
            );
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Mint a worker daemon's `.creds` from the platform account seed. Non-expiring
/// like `dispatcher.creds` — rotation is re-mint + restart.
async fn mint_worker_creds(
    keys_dir: &std::path::Path,
    node: &str,
    out: Option<&std::path::Path>,
) -> Result<()> {
    if node.is_empty()
        || !node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("--node {node:?} must be [A-Za-z0-9_-]+ (rides in NATS subjects)");
    }
    let seed_path = keys_dir.join("nats_account.seed");
    let seed = tokio::fs::read_to_string(&seed_path)
        .await
        .with_context(|| format!("reading {}", seed_path.display()))?;
    let signer = auth::nats::NatsUserSigner::from_account_seed(seed.trim())?;
    let creds = signer.mint_creds(
        &format!("worker.{node}"),
        &auth::nats::worker_permissions(node),
        None,
    )?;
    let path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| keys_dir.join(format!("worker-{node}.creds")));
    crate::keygen::write_key(&path, &creds, true).await?;
    println!("wrote {} (user worker.{node})", path.display());
    Ok(())
}

/// Mint a worker node's read-only git credential (spec §3.1 self-refresh
/// enrollment). Generates a keypair, signs a long-lived READ-ONLY cert scoped
/// to the platform repo (the same repo-scoped pull an eval container gets), and
/// writes `worker_git` + `worker_git-cert.pub` for the operator to install
/// under the node's `~/chuggernaut-worker/keys/`. Local-only: signs with the
/// `ssh_ca` key, no NATS.
async fn mint_worker_git_key(
    keys_dir: &std::path::Path,
    node: &str,
    project: &str,
    out_dir: Option<&std::path::Path>,
    days: i64,
) -> Result<()> {
    if node.is_empty()
        || !node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("--node {node:?} must be [A-Za-z0-9_-]+ (rides in NATS subjects)");
    }
    let Some((owner, repo)) = project.split_once('/') else {
        bail!("--project must be owner/project, got {project:?}");
    };
    if days <= 0 {
        bail!("--days must be positive, got {days}");
    }
    let ca_key = keys_dir.join("ssh_ca");
    if !ca_key.is_file() {
        bail!(
            "SSH CA key {} not found — run `chuggernaut init` first",
            ca_key.display()
        );
    }
    let cred = auth::ssh::SshCa::new(&ca_key)
        .issue_node_credential(owner, repo, node, chrono::Duration::days(days))
        .await?;

    let dir = out_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| keys_dir.into());
    let key_path = dir.join("worker_git");
    let cert_path = dir.join("worker_git-cert.pub");
    crate::keygen::write_key(&key_path, &cred.private_key, true).await?;
    crate::keygen::write_key(&cert_path, &cred.certificate, false).await?;

    println!(
        "wrote {} (read-only, {project}, {days}d)",
        key_path.display()
    );
    println!("wrote {}", cert_path.display());
    println!(
        "install both under the node's ~/chuggernaut-worker/keys/, then set on the \
         worker daemon: WORKER_REFRESH_GIT_URL=ssh://git@<ssh-front>:2222/{project}.git"
    );
    Ok(())
}

async fn run_secret(store: &NatsStore, keys_dir: &std::path::Path, cmd: SecretCmd) -> Result<()> {
    use store::secrets::{AgeSecretStore, SecretStore};
    let bucket = store.raw_bucket(store::buckets::SECRETS).await?;

    // Copy moves the stored ciphertext verbatim — no keys involved.
    if let SecretCmd::Copy { from, to, name } = &cmd {
        let split = |s: &str| -> Result<(String, String)> {
            let Some((o, p)) = s.split_once('/') else {
                bail!("scope must be owner/project, got {s:?}");
            };
            Ok((o.to_string(), p.to_string()))
        };
        let (fo, fp) = split(from)?;
        let (to_o, to_p) = split(to)?;
        let Some(value) = bucket
            .get_json::<String>(&format!("{fo}.{fp}.{name}"))
            .await?
        else {
            bail!("secret {from}.{name} not found");
        };
        bucket
            .put_json(&format!("{to_o}.{to_p}.{name}"), &value)
            .await?;
        println!("copied secret {from}.{name} -> {to}.{name}");
        return Ok(());
    }

    let public_key = tokio::fs::read_to_string(keys_dir.join("age_public.key"))
        .await
        .with_context(|| {
            format!(
                "reading {}/age_public.key (run init first)",
                keys_dir.display()
            )
        })?;
    let secrets = AgeSecretStore::for_api(bucket, public_key.trim())?;
    match cmd {
        SecretCmd::Set { scoped, value } => {
            let (owner, project) = scoped.split()?;
            let value = match value {
                Some(v) => v,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf)?;
                    buf.trim_end_matches('\n').to_string()
                }
            };
            secrets.set(owner, project, &scoped.name, &value).await?;
            println!("set secret {owner}/{project}.{}", scoped.name);
        }
        SecretCmd::List { project } => {
            let Some((owner, project)) = project.split_once('/') else {
                bail!("--project must be owner/project, got {project:?}");
            };
            for name in secrets.list(owner, project).await? {
                println!("{name}");
            }
        }
        SecretCmd::Delete { scoped } => {
            let (owner, project) = scoped.split()?;
            secrets.delete(owner, project, &scoped.name).await?;
            println!("deleted secret {owner}/{project}.{}", scoped.name);
        }
        SecretCmd::Copy { .. } => unreachable!("handled above"),
    }
    Ok(())
}

async fn run_var(store: &NatsStore, cmd: VarCmd) -> Result<()> {
    let vars = store.raw_bucket(store::buckets::VARS).await?;
    match cmd {
        VarCmd::Set { scoped, value } => {
            let (owner, project) = scoped.split()?;
            store::keys::validate_name(&scoped.name)?;
            vars.put_json(&format!("{owner}.{project}.{}", scoped.name), &value)
                .await?;
            println!("set var {owner}/{project}.{}", scoped.name);
        }
        VarCmd::List { project } => {
            let Some((owner, project)) = project.split_once('/') else {
                bail!("--project must be owner/project, got {project:?}");
            };
            let prefix = format!("{owner}.{project}.");
            for key in vars.keys_with_prefix(&prefix).await? {
                if let Some(name) = key.strip_prefix(&prefix) {
                    println!("{name}");
                }
            }
        }
        VarCmd::Delete { scoped } => {
            let (owner, project) = scoped.split()?;
            vars.delete(&format!("{owner}.{project}.{}", scoped.name))
                .await?;
            println!("deleted var {owner}/{project}.{}", scoped.name);
        }
    }
    Ok(())
}

async fn run_user(store: &NatsStore, keys_dir: &std::path::Path, cmd: UserCmd) -> Result<()> {
    let users = store.raw_bucket(store::buckets::USERS).await?;
    match cmd {
        UserCmd::Token { email, ttl } => {
            let Some(user) = users.get_json::<User>(&keys::user_key(&email)).await? else {
                bail!("user {email} not found");
            };
            let ttl = types::parse_duration(&ttl)
                .map_err(|e| anyhow::anyhow!("--ttl: {e}"))
                .and_then(|d| chrono::Duration::from_std(d).context("ttl out of range"))?;
            let pem = tokio::fs::read(keys_dir.join("jwt_private.pem"))
                .await
                .with_context(|| {
                    format!(
                        "reading {}/jwt_private.pem (run init first)",
                        keys_dir.display()
                    )
                })?;
            let signer = auth::jwt::JwtSigner::from_pem(&pem)?;
            let identity = types::Identity {
                sub: user.email.clone(),
                kind: types::IdentityKind::User,
                project_roles: user.project_roles.clone(),
                platform_admin: user.platform_admin,
            };
            // Token only — stdout is pipeable into a credentials file.
            println!("{}", signer.issue(&identity, ttl)?);
        }
        UserCmd::Create {
            email,
            password,
            admin,
        } => {
            if !create_user(store, &email, &password, admin).await? {
                bail!("user {email} already exists");
            }
            println!(
                "created {email}{}",
                if admin { " (platform admin)" } else { "" }
            );
        }
        UserCmd::List => {
            for key in users.keys_with_prefix("").await? {
                let Some(user) = users.get_json::<User>(&key).await? else {
                    continue;
                };
                let admin = if user.platform_admin {
                    "  [platform admin]"
                } else {
                    ""
                };
                println!("{}{admin}", user.email);
            }
        }
        UserCmd::Delete { email } => {
            users.delete(&keys::user_key(&email)).await?;
            println!("deleted {email}");
        }
        UserCmd::Role(cmd) => run_role(store, cmd).await?,
    }
    Ok(())
}

/// §7.5 project-role management on the user record. Mutates the `project_roles`
/// map in the `users.*` bucket — the same write path `user create` uses; the
/// admin CLI is the operator-side writer. `--project` is `{owner}/{project}`.
async fn run_role(store: &NatsStore, cmd: RoleCmd) -> Result<()> {
    let users = store.raw_bucket(store::buckets::USERS).await?;
    // Validate a `{owner}/{project}` slug into the map-key form it is stored as.
    let slug = |project: &str| -> Result<String> {
        let Some((owner, name)) = project.split_once('/') else {
            bail!("--project must be owner/project, got {project:?}");
        };
        keys::validate_subject_component(owner)?;
        keys::validate_subject_component(name)?;
        Ok(format!("{owner}/{name}"))
    };
    match cmd {
        RoleCmd::Set {
            email,
            project,
            role,
        } => {
            let slug = slug(&project)?;
            let Some(role) = types::ProjectRole::parse(&role) else {
                bail!("--role {role:?} must be owner|member|viewer");
            };
            let key = keys::user_key(&email);
            let Some(mut user) = users.get_json::<User>(&key).await? else {
                bail!("user {email} not found");
            };
            user.project_roles.insert(slug.clone(), role);
            users.put_json(&key, &user).await?;
            println!("granted {email} {role:?} on {slug}");
        }
        RoleCmd::List { email } => {
            let Some(user) = users.get_json::<User>(&keys::user_key(&email)).await? else {
                bail!("user {email} not found");
            };
            let mut roles: Vec<_> = user.project_roles.iter().collect();
            roles.sort_by_key(|(p, _)| p.as_str());
            for (project, role) in roles {
                println!("{project}\t{role:?}");
            }
        }
        RoleCmd::Remove { email, project } => {
            let slug = slug(&project)?;
            let key = keys::user_key(&email);
            let Some(mut user) = users.get_json::<User>(&key).await? else {
                bail!("user {email} not found");
            };
            if user.project_roles.remove(&slug).is_none() {
                bail!("user {email} has no role on {slug}");
            }
            users.put_json(&key, &user).await?;
            println!("removed {email}'s role on {slug}");
        }
    }
    Ok(())
}

/// Create a user record if absent; returns false if it already exists.
/// Safe as check-then-put: the admin CLI is the only writer of `users.*`.
pub async fn create_user(
    store: &NatsStore,
    email: &str,
    password: &str,
    admin: bool,
) -> Result<bool> {
    let users = store.raw_bucket(store::buckets::USERS).await?;
    let key = keys::user_key(email);
    if users.get_json::<User>(&key).await?.is_some() {
        return Ok(false);
    }
    let user = User {
        // Stable opaque id; the email's KV encoding is already unique.
        id: key.clone(),
        email: email.to_string(),
        password_hash: auth::hash_password(password)?,
        project_roles: Default::default(),
        platform_admin: admin,
        created_at: Utc::now(),
    };
    users.put_json(&key, &user).await?;
    Ok(true)
}

async fn run_project(store: &NatsStore, cmd: ProjectCmd) -> Result<()> {
    let counters = store.raw_bucket(store::buckets::COUNTERS).await?;
    match cmd {
        ProjectCmd::Create {
            owner,
            name,
            default_branch,
            repos_root,
            hook_bin,
        } => {
            // Owner/project become NATS key segments and subject tokens.
            keys::validate_subject_component(&owner)?;
            keys::validate_subject_component(&name)?;
            if owner == keys::RESERVED_OWNER {
                bail!("owner {owner:?} is reserved");
            }
            let key = format!("{owner}.{name}");
            if counters.get_json::<u64>(&key).await?.is_some() {
                bail!("project {owner}/{name} already exists");
            }
            // Repo before counter: a failed repo init leaves nothing behind,
            // so the command can simply be re-run (§12.2).
            vcs::RepoManager::new(&repos_root)
                .create_project(&owner, &name, &default_branch)
                .await?;
            // §5.2 per-ref push authorization for SSH traffic; local access
            // (no CHUGGERNAUT_PRINCIPAL env) passes through.
            let bin = match hook_bin {
                Some(path) => path,
                None => std::env::current_exe()?,
            };
            crate::sshfront::install_pre_receive_hook(&repos_root, &owner, &name, &bin).await?;
            counters.put_json(&key, &0u64).await?;
            println!(
                "created {owner}/{name} (default branch {default_branch}) at {}",
                repos_root
                    .join(&owner)
                    .join(format!("{name}.git"))
                    .display()
            );
        }
        ProjectCmd::Link {
            owner,
            name,
            origin_url,
            main_branch,
        } => {
            keys::validate_subject_component(&owner)?;
            keys::validate_subject_component(&name)?;
            // Preflight the origin credentials so the failure mode is a clear
            // pointer at `admin secret set` instead of a dispatcher error.
            let needs_ssh = origin_url.starts_with("ssh://") || origin_url.contains('@');
            let is_github = origin_url.contains("github.com");
            let secrets = store.raw_bucket(store::buckets::SECRETS).await?;
            for (needed, secret) in [
                (needs_ssh, "CHUG_ORIGIN_DEPLOY_KEY"),
                (is_github, "CHUG_ORIGIN_PAT"),
            ] {
                if needed
                    && secrets
                        .get_json::<String>(&format!("{owner}.{name}.{secret}"))
                        .await?
                        .is_none()
                {
                    bail!(
                        "secret {secret} is not set for {owner}/{name} — \
                         run: chuggernaut admin secret set --scoped {owner}/{name}.{secret}"
                    );
                }
            }
            let payload = serde_json::to_vec(&serde_json::json!({
                "owner": owner, "name": name, "origin_url": origin_url,
                "main_branch": main_branch,
            }))?;
            let reply = store
                .request_with_retry(
                    &store::subjects::projects_link(),
                    &payload,
                    3,
                    std::time::Duration::from_millis(300),
                )
                .await
                .context("dispatcher unavailable (link requires a running dispatcher)")?;
            let value: serde_json::Value = serde_json::from_slice(&reply.payload)?;
            if let Some(err) = value.get("error") {
                bail!("link failed: {err}");
            }
            let main = value["origin"]["main_branch"].as_str().unwrap_or("?");
            println!("linked {owner}/{name} -> {origin_url} (origin main: {main})");
        }
        ProjectCmd::List { owner } => {
            let prefix = owner.map(|o| format!("{o}.")).unwrap_or_default();
            for key in counters.keys_with_prefix(&prefix).await? {
                if let Some((o, p)) = key.split_once('.') {
                    println!("{o}/{p}");
                }
            }
        }
    }
    Ok(())
}

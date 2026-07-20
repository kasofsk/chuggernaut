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
    }
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

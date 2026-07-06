//! `chuggernaut admin ...` — user and project management (spec §12.2–12.3).
//! Ingest tokens and secret key rotation land with their consumers.

use anyhow::{Result, bail};
use chrono::Utc;
use std::path::PathBuf;
use store::{NatsStore, keys};
use types::User;

#[derive(clap::Args, Debug)]
pub struct AdminArgs {
    #[arg(long, global = true, default_value = "nats://localhost:4222")]
    pub nats_url: String,
    #[command(subcommand)]
    pub cmd: AdminCmd,
}

#[derive(clap::Subcommand, Debug)]
pub enum AdminCmd {
    #[command(subcommand)]
    User(UserCmd),
    #[command(subcommand)]
    Project(ProjectCmd),
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
    },
    List {
        #[arg(long)]
        owner: Option<String>,
    },
}

pub async fn run(args: AdminArgs) -> Result<()> {
    let store = NatsStore::connect(&args.nats_url).await?;
    match args.cmd {
        AdminCmd::User(cmd) => run_user(&store, cmd).await,
        AdminCmd::Project(cmd) => run_project(&store, cmd).await,
    }
}

async fn run_user(store: &NatsStore, cmd: UserCmd) -> Result<()> {
    let users = store.raw_bucket(store::buckets::USERS).await?;
    match cmd {
        UserCmd::Create { email, password, admin } => {
            if !create_user(store, &email, &password, admin).await? {
                bail!("user {email} already exists");
            }
            println!("created {email}{}", if admin { " (platform admin)" } else { "" });
        }
        UserCmd::List => {
            for key in users.keys_with_prefix("").await? {
                let Some(user) = users.get_json::<User>(&key).await? else { continue };
                let admin = if user.platform_admin { "  [platform admin]" } else { "" };
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
pub async fn create_user(store: &NatsStore, email: &str, password: &str, admin: bool) -> Result<bool> {
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
        ProjectCmd::Create { owner, name, default_branch, repos_root } => {
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
            let bin = std::env::current_exe()?;
            crate::sshfront::install_pre_receive_hook(&repos_root, &owner, &name, &bin).await?;
            counters.put_json(&key, &0u64).await?;
            println!(
                "created {owner}/{name} (default branch {default_branch}) at {}",
                repos_root.join(&owner).join(format!("{name}.git")).display()
            );
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

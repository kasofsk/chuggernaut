//! `chuggernaut init` — one-time idempotent platform bootstrap (spec §12.1).

use crate::keygen;
use anyhow::{Context, Result};
use std::path::PathBuf;
use store::NatsStore;

#[derive(clap::Args, Debug)]
pub struct InitArgs {
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,
    /// Path for bare git repos.
    #[arg(long, default_value = "/data/repos")]
    pub repos_root: PathBuf,
    /// Path to write/read keypairs.
    #[arg(long, default_value = "/data/keys")]
    pub keys_dir: PathBuf,
    /// Create the initial platform admin user.
    #[arg(long, requires = "admin_password")]
    pub admin_email: Option<String>,
    #[arg(long)]
    pub admin_password: Option<String>,
}

pub async fn run(args: InitArgs) -> Result<()> {
    // 1. Keypairs (skip existing).
    let report = keygen::ensure_all(&args.keys_dir).await?;
    for name in &report.generated {
        println!("generated {}", args.keys_dir.join(name).display());
    }
    if !report.skipped.is_empty() {
        println!("kept existing: {}", report.skipped.join(", "));
    }

    tokio::fs::create_dir_all(&args.repos_root)
        .await
        .with_context(|| format!("creating {}", args.repos_root.display()))?;

    // 2. NATS buckets + streams, VAPID public key at platform.vapid.public.
    // Connect with the dispatcher credentials keygen just ensured — a plain
    // (no-auth) dev server ignores them, an operator-mode server requires them.
    let creds = tokio::fs::read_to_string(args.keys_dir.join("dispatcher.creds")).await?;
    let store = NatsStore::connect_with_creds(&args.nats_url, &creds)
        .await
        .with_context(|| format!("connecting to {}", args.nats_url))?;
    store.ensure_topology().await?;
    println!("NATS buckets and streams ready at {}", args.nats_url);

    let vapid_public = tokio::fs::read_to_string(args.keys_dir.join("vapid_public.pem")).await?;
    store
        .raw_bucket(store::buckets::PLATFORM)
        .await?
        .put_json("vapid.public", &vapid_public)
        .await?;

    // 3. Optional initial admin user (skip if it already exists).
    if let Some(email) = &args.admin_email {
        let password = args.admin_password.as_deref().expect("clap requires");
        match crate::admin::create_user(&store, email, password, true).await? {
            true => println!("created platform admin {email}"),
            false => println!("user {email} already exists — left untouched"),
        }
    }

    println!("init complete");
    println!("private keys in {} — mount them into the dispatcher/API per §12.1", args.keys_dir.display());
    println!(
        "to enforce per-job credentials (§7.4), start nats-server with {}",
        args.keys_dir.join("nats-resolver.conf").display()
    );
    Ok(())
}

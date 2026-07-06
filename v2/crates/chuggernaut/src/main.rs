//! The chuggernaut binary: one artifact, five roles (crates.md).
//!
//! Dispatcher and API run as separate processes with different mounted keys —
//! sharing a binary just means one artifact to version and deploy.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "chuggernaut",
    version,
    about = "AI-native software delivery platform"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the dispatcher (sole writer of all job/task state).
    /// Configured via env: NATS_URL, REPOS_ROOT, REPO_URL_BASE, KEYS_DIR,
    /// CHANNEL_BINARY, AGENT_PROVIDER_DEFAULT (required), AGENT_MODEL_DEFAULT,
    /// DOCKER_NODES | DOCKER_SLOTS (spec §12.4).
    Dispatcher,
    /// Run the HTTP↔NATS API bridge (serves the PWA).
    Api,
    /// Run the webhook delivery service.
    Webhooks,
    /// One-time platform bootstrap: keypairs, NATS buckets/streams, admin user.
    Init(cli::InitArgs),
    /// Admin operations: users, projects, ingest tokens, key rotation, seeding.
    Admin(cli::AdminArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Dispatcher => {
            let config = dispatcher::config::DispatcherConfig::from_env()?;
            let _handle = dispatcher::run::run(config).await?;
            tokio::signal::ctrl_c().await?;
            eprintln!("shutting down");
            Ok(())
        }
        Command::Api => anyhow::bail!("not yet implemented: api"),
        Command::Webhooks => anyhow::bail!("not yet implemented: webhooks"),
        Command::Init(args) => cli::init::run(args).await,
        Command::Admin(args) => cli::admin::run(args).await,
    }
}

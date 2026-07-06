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
    /// Configured via env: NATS_URL, KEYS_DIR, BIND_ADDR, UI_DIST,
    /// SESSION_TTL (spec §6, §7.1).
    Api,
    /// Run the webhook delivery service.
    Webhooks,
    /// One-time platform bootstrap: keypairs, NATS buckets/streams, admin user.
    Init(cli::InitArgs),
    /// Admin operations: users, projects, ingest tokens, key rotation, seeding.
    Admin(cli::AdminArgs),
    /// SSH forced command (§5.2): gate and exec the git service. Embedded in
    /// certificates at signing time — not for interactive use.
    SshShell(cli::SshShellArgs),
    /// Pre-receive hook body (§5.2): per-ref push authorization. Installed
    /// into every bare repo by `admin project create`.
    SshAuthz,
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
        Command::Api => {
            let config = api::run::ApiConfig::from_env()?;
            api::run::run(config).await
        }
        Command::Webhooks => anyhow::bail!("not yet implemented: webhooks"),
        Command::Init(args) => cli::init::run(args).await,
        Command::Admin(args) => cli::admin::run(args).await,
        Command::SshShell(args) => cli::sshfront::run_shell(args).await,
        Command::SshAuthz => cli::sshfront::run_authz().await,
    }
}

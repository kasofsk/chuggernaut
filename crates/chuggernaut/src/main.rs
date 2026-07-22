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
    /// Run a worker-node daemon: executes container ops for this node against
    /// the local Docker socket, dialing OUT to NATS (spec §3.1). Configured
    /// via env: WORKER_NODE (required), NATS_URL (required), NATS_CREDS,
    /// WORKER_DOCKER_ENDPOINT, WORKER_CHANNEL_BINARY.
    Worker,
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
    /// Mint a user SSH certificate from the platform (§7.3): submit a public
    /// key to `POST /auth/ssh-cert` and write the signed cert, so you can
    /// clone/fetch platform repos over the SSH front. Re-run when it expires.
    SshCert(cli::SshCertArgs),
    /// SSH forced command (§5.2): gate and exec the git service. Embedded in
    /// certificates at signing time — not for interactive use.
    SshShell(cli::SshShellArgs),
    /// Pre-receive hook body (§5.2): per-ref push authorization. Installed
    /// into every bare repo by `admin project create`.
    SshAuthz,
    /// Emit the JSON Schema for repo-authored YAML (jobs/{type}.yaml,
    /// jobs/_defaults.yaml) — commit it and point yaml-language-server at it
    /// for in-editor validation.
    Schema(cli::SchemaArgs),
    /// Statically validate job type YAML files (parse + §1.1 field rules,
    /// with a sibling _defaults.yaml merged). Repo checks happen at release.
    Validate(cli::ValidateArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Dispatcher => {
            let config = dispatcher::config::DispatcherConfig::from_env()?;
            let dispatcher = dispatcher::run::run(config).await?;
            // SIGTERM (launchd `kickstart -k`) / SIGINT → graceful drain (§3.6):
            // quiesce the actor and flush records to KV before exiting clean.
            dispatcher::run::wait_for_signal().await;
            eprintln!("draining dispatcher for graceful shutdown");
            dispatcher.shutdown().await;
            eprintln!("dispatcher shut down");
            Ok(())
        }
        Command::Worker => {
            let config = worker::WorkerConfig::from_env()?;
            worker::run(config).await?;
            eprintln!("worker shut down");
            Ok(())
        }
        Command::Api => {
            let config = api::run::ApiConfig::from_env()?;
            api::run::run(config).await
        }
        Command::Webhooks => anyhow::bail!("not yet implemented: webhooks"),
        Command::Init(args) => cli::init::run(args).await,
        Command::Admin(args) => cli::admin::run(args).await,
        Command::SshCert(args) => cli::sshcert::run(args).await,
        Command::SshShell(args) => cli::sshfront::run_shell(args).await,
        Command::SshAuthz => cli::sshfront::run_authz().await,
        Command::Schema(args) => cli::schema::run(args),
        Command::Validate(args) => cli::validate::run(args),
    }
}

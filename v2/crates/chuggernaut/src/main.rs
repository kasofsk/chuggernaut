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
    Dispatcher,
    /// Run the HTTP↔NATS API bridge (serves the PWA).
    Api,
    /// Run the webhook delivery service.
    Webhooks,
    /// One-time platform bootstrap: keypairs, NATS buckets/streams, admin user.
    Init,
    /// Admin operations: users, projects, ingest tokens, key rotation, seeding.
    Admin,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Dispatcher => anyhow::bail!("not yet implemented: dispatcher"),
        Command::Api => anyhow::bail!("not yet implemented: api"),
        Command::Webhooks => anyhow::bail!("not yet implemented: webhooks"),
        Command::Init => anyhow::bail!("not yet implemented: init"),
        Command::Admin => anyhow::bail!("not yet implemented: admin"),
    }
}

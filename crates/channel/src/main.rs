use std::sync::Arc;

use clap::Parser;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use chuggernaut_channel::{ChannelState, mcp};

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "chuggernaut-channel",
    about = "MCP channel server bridging NATS ↔ Claude Code"
)]
struct Args {
    /// Job key (e.g. acme.payments.57)
    #[arg(long, env = "CHUGGERNAUT_JOB_KEY")]
    job_key: String,

    /// NATS server URL
    #[arg(
        long,
        env = "CHUGGERNAUT_NATS_URL",
        default_value = "nats://localhost:4222"
    )]
    nats_url: String,

    /// Push incoming messages to Claude as MCP notifications (default: true).
    /// When false, Claude must poll with channel_check instead.
    #[arg(long, env = "CHUGGERNAUT_PUSH_NOTIFICATIONS", default_value = "true")]
    push_notifications: bool,
}

// ---------------------------------------------------------------------------
// Stdout writer (serializes all writes to stdout)
// ---------------------------------------------------------------------------

async fn stdout_writer(mut rx: mpsc::Receiver<String>) {
    let mut stdout = io::stdout();
    while let Some(line) = rx.recv().await {
        if let Err(e) = stdout.write_all(line.as_bytes()).await {
            error!("stdout write error: {e}");
            break;
        }
        if let Err(e) = stdout.write_all(b"\n").await {
            error!("stdout write error: {e}");
            break;
        }
        if let Err(e) = stdout.flush().await {
            error!("stdout flush error: {e}");
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    info!(
        job_key = args.job_key,
        nats_url = args.nats_url,
        channel_mode = args.push_notifications,
        "starting chuggernaut-channel MCP server"
    );

    let nats = async_nats::connect(&args.nats_url).await?;
    let js = async_nats::jetstream::new(nats.clone());

    js.create_key_value(async_nats::jetstream::kv::Config {
        bucket: chuggernaut_types::buckets::CHANNELS.to_string(),
        history: 1,
        storage: async_nats::jetstream::stream::StorageType::File,
        ..Default::default()
    })
    .await?;

    let state = Arc::new(Mutex::new(ChannelState::new()));
    let (out_tx, out_rx) = mpsc::channel::<String>(256);

    tokio::spawn(stdout_writer(out_rx));

    tokio::spawn(chuggernaut_channel::nats_inbox_listener(
        nats.clone(),
        args.job_key.clone(),
        Arc::clone(&state),
        out_tx.clone(),
        args.push_notifications,
    ));

    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let req: mcp::JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                warn!("invalid JSON-RPC message: {e}");
                let resp = mcp::JsonRpcResponse::error(None, -32700, "parse error".into());
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap()).await;
                continue;
            }
        };

        debug!(method = req.method, "handling MCP request");

        if let Some(response) = chuggernaut_channel::handle_message(
            req,
            &state,
            &nats,
            &js,
            &args.job_key,
            args.push_notifications,
        )
        .await
        {
            let _ = out_tx.send(response).await;
        }
    }

    info!("stdin closed, shutting down");
    Ok(())
}

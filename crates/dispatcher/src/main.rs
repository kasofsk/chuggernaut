use std::net::SocketAddr;

use chuggernaut_dispatcher::{config, handlers, http, monitor, nats_init, recovery, state};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,chuggernaut_dispatcher=debug".parse().unwrap()),
        )
        .init();

    let config = config::Config::from_env();
    info!(?config, "starting chuggernaut-dispatcher");

    // Fail fast if static files are missing
    http::check_static_dir();

    // Connect to NATS
    let nats = async_nats::connect(&config.nats_url).await?;
    let js = async_nats::jetstream::new(nats.clone());
    info!(url = config.nats_url, "connected to NATS");

    // Initialize KV buckets and streams
    let kv = nats_init::initialize(&js, config.lease_secs).await?;

    // Build shared state
    let state = state::DispatcherState::new(config.clone(), nats, js, kv);

    // Phase 1: Recover state from KV (single-threaded, blocking).
    // This runs all repair logic (stale claims, merged PRs, orphaned jobs)
    // before any handlers or monitors start.
    recovery::recover(&state).await?;

    // Phase 2: Start NATS subscription handlers + assignment task.
    // Handlers subscribe to NATS subjects and the assignment task starts
    // consuming the dispatch channel. No dispatches happen yet because
    // the monitor hasn't sent any requests.
    handlers::start_handlers(state.clone()).await?;

    // Phase 3: Start monitor background task.
    // First scan fires after the handlers are ready, so dispatch requests
    // from scan_pending_reviews reach an active assignment task.
    monitor::start(state.clone());

    // Start HTTP server
    let listen_addr: SocketAddr = config.http_listen.parse()?;
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    info!(%listen_addr, "HTTP server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
    info!("shutdown signal received");
}

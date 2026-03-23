use std::net::SocketAddr;

use forge2_dispatcher::{config, handlers, http, monitor, nats_init, recovery, state};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,forge2_dispatcher=debug".parse().unwrap()),
        )
        .init();

    let config = config::Config::from_env();
    info!(?config, "starting forge2-dispatcher");

    // Connect to NATS
    let nats = async_nats::connect(&config.nats_url).await?;
    let js = async_nats::jetstream::new(nats.clone());
    info!(url = config.nats_url, "connected to NATS");

    // Initialize KV buckets and streams
    let kv = nats_init::initialize(&js, config.lease_secs, config.blacklist_ttl_secs).await?;

    // Build shared state
    let state = state::DispatcherState::new(config.clone(), nats, js, kv);

    // Recover state from KV
    recovery::recover(&state).await?;

    // Start monitor background task
    monitor::start(state.clone());

    // Start NATS subscription handlers
    handlers::start_handlers(state.clone()).await?;

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

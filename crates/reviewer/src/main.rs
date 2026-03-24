use tracing::info;

use chuggernaut_reviewer::{config, nats_setup, state, trigger};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,chuggernaut_reviewer=debug".parse().unwrap()),
        )
        .init();

    let config = config::Config::from_env();
    info!(?config, "starting chuggernaut-reviewer");

    // Connect to NATS
    let nats = async_nats::connect(&config.nats_url).await?;
    let js = async_nats::jetstream::new(nats.clone());
    info!(url = config.nats_url, "connected to NATS");

    // Initialize reviewer-owned KV buckets
    let kv = nats_setup::initialize(&js, config.merge_lock_ttl_secs).await?;

    // Build shared state
    let state = state::ReviewerState::new(config, nats, js, kv);

    // Reconcile: pick up InReview and Escalated jobs from before restart
    trigger::reconcile(&state).await?;

    // Start the main trigger loop (blocks forever)
    trigger::start(state).await?;

    Ok(())
}

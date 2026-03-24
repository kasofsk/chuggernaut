use std::sync::Arc;

use async_nats::jetstream;
use async_nats::jetstream::kv;
use dashmap::DashSet;

use chuggernaut_forgejo_api::ForgejoClient;
use chuggernaut_nats::NatsClient;

use crate::config::Config;

/// KV stores owned by the reviewer process.
pub struct KvStores {
    pub merge_queue: kv::Store,
    pub rework_counts: kv::Store,
}

/// Shared reviewer state, wrapped in Arc for use across tasks.
pub struct ReviewerState {
    pub config: Config,
    pub nats: NatsClient,
    pub js: jetstream::Context,
    pub kv: KvStores,
    pub forgejo: ForgejoClient,
    pub http: reqwest::Client,

    /// Dedup set: job keys currently being processed.
    /// Prevents duplicate review processing when the same transition
    /// is delivered more than once.
    pub in_flight: DashSet<String>,
}

impl ReviewerState {
    pub fn new(
        config: Config,
        nats: async_nats::Client,
        js: jetstream::Context,
        kv: KvStores,
    ) -> Arc<Self> {
        let forgejo = ForgejoClient::new(&config.forgejo_url, &config.forgejo_token);
        Arc::new(Self {
            config,
            nats: NatsClient::new(nats),
            js,
            kv,
            forgejo,
            http: reqwest::Client::new(),
            in_flight: DashSet::new(),
        })
    }
}

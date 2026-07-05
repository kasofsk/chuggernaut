//! NATS KV/stream access — the only crate that talks to NATS (spec §1.4–1.5).

pub mod buckets;
pub mod keys;
pub mod secrets;
pub mod vars;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("nats: {0}")]
    Nats(String),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid key segment: {0}")]
    InvalidKey(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Connected NATS handle wrapping a client + JetStream context.
///
/// TODO: typed accessors per bucket (JobStore, TaskStore, UserStore, ChannelStore,
/// PushStore, CounterStore, RdepsStore, knowledge), stream publish/consume helpers,
/// request-reply with bounded retry (spec §4.2 reliability).
pub struct NatsStore {
    // client: async_nats::Client,
    // js: async_nats::jetstream::Context,
}

impl NatsStore {
    pub async fn connect(_url: &str) -> Result<Self> {
        todo!("connect to NATS, build JetStream context")
    }
}

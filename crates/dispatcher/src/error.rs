use thiserror::Error;

#[derive(Debug, Error)]
pub enum DispatcherError {
    #[error("NATS error: {0}")]
    Nats(String),

    #[error("KV error: {0}")]
    Kv(String),

    #[error("CAS conflict after {retries} retries for key {key}")]
    CasExhausted { key: String, retries: u32 },

    #[error("Invalid state transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: chuggernaut_types::JobState,
        to: chuggernaut_types::JobState,
    },

    #[error("Job not found: {0}")]
    JobNotFound(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Cycle detected: adding edge {from} -> {to} would create a cycle")]
    CycleDetected { from: String, to: String },

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl From<async_nats::jetstream::kv::CreateError> for DispatcherError {
    fn from(e: async_nats::jetstream::kv::CreateError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::kv::PutError> for DispatcherError {
    fn from(e: async_nats::jetstream::kv::PutError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::kv::UpdateError> for DispatcherError {
    fn from(e: async_nats::jetstream::kv::UpdateError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::kv::EntryError> for DispatcherError {
    fn from(e: async_nats::jetstream::kv::EntryError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::context::CreateKeyValueError> for DispatcherError {
    fn from(e: async_nats::jetstream::context::CreateKeyValueError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::context::CreateStreamError> for DispatcherError {
    fn from(e: async_nats::jetstream::context::CreateStreamError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::jetstream::context::PublishError> for DispatcherError {
    fn from(e: async_nats::jetstream::context::PublishError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::PublishError> for DispatcherError {
    fn from(e: async_nats::PublishError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::jetstream::kv::WatchError> for DispatcherError {
    fn from(e: async_nats::jetstream::kv::WatchError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::kv::StatusError> for DispatcherError {
    fn from(e: async_nats::jetstream::kv::StatusError) -> Self {
        Self::Kv(e.to_string())
    }
}

pub type DispatcherResult<T> = Result<T, DispatcherError>;

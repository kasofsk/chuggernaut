use thiserror::Error;

#[derive(Debug, Error)]
pub enum ReviewerError {
    #[error("NATS error: {0}")]
    Nats(String),

    #[error("KV error: {0}")]
    Kv(String),

    #[error("Forgejo API error: {0}")]
    Forgejo(#[from] chuggernaut_forgejo_api::ForgejoError),

    #[error("Dispatcher API error: {0}")]
    Dispatcher(String),

    #[error("Job not found: {0}")]
    JobNotFound(String),

    #[error("Job has no PR URL: {0}")]
    NoPrUrl(String),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

impl From<async_nats::PublishError> for ReviewerError {
    fn from(e: async_nats::PublishError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::jetstream::context::CreateKeyValueError> for ReviewerError {
    fn from(e: async_nats::jetstream::context::CreateKeyValueError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::context::CreateStreamError> for ReviewerError {
    fn from(e: async_nats::jetstream::context::CreateStreamError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::jetstream::kv::PutError> for ReviewerError {
    fn from(e: async_nats::jetstream::kv::PutError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::kv::EntryError> for ReviewerError {
    fn from(e: async_nats::jetstream::kv::EntryError) -> Self {
        Self::Kv(e.to_string())
    }
}

impl From<async_nats::jetstream::consumer::StreamError> for ReviewerError {
    fn from(e: async_nats::jetstream::consumer::StreamError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::jetstream::consumer::pull::MessagesError> for ReviewerError {
    fn from(e: async_nats::jetstream::consumer::pull::MessagesError) -> Self {
        Self::Nats(e.to_string())
    }
}

impl From<async_nats::jetstream::context::PublishError> for ReviewerError {
    fn from(e: async_nats::jetstream::context::PublishError) -> Self {
        Self::Nats(e.to_string())
    }
}

pub type ReviewerResult<T> = Result<T, ReviewerError>;

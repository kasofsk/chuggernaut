//! NATS KV/stream access — the only crate that talks to NATS (spec §1.4–1.5).

pub mod buckets;
pub mod keys;
pub mod secrets;
pub mod stores;
pub mod subjects;
pub mod vars;

pub use stores::{Bucket, CounterStore, JobStore, RdepsStore, StepStore, TaskStore, split_project};

use async_nats::jetstream;
use std::time::Duration;
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

fn nats_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Nats(e.to_string())
}

const DAY: Duration = Duration::from_secs(86_400);

/// Connected NATS handle wrapping a client + JetStream context.
#[derive(Clone)]
pub struct NatsStore {
    client: async_nats::Client,
    js: jetstream::Context,
}

impl NatsStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = async_nats::connect(url).await.map_err(nats_err)?;
        let js = jetstream::new(client.clone());
        Ok(Self { client, js })
    }

    /// Connect with a scoped JWT/token (per-job container credentials, spec §7.4).
    pub async fn connect_with_token(url: &str, token: &str) -> Result<Self> {
        let client = async_nats::ConnectOptions::new()
            .token(token.to_string())
            .connect(url)
            .await
            .map_err(nats_err)?;
        let js = jetstream::new(client.clone());
        Ok(Self { client, js })
    }

    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    pub fn jetstream(&self) -> &jetstream::Context {
        &self.js
    }

    /// Create all fixed KV buckets and streams (spec §1.5). Idempotent; called
    /// by `chuggernaut init` and the test harness. Replicas: 1 (dev) — the
    /// production replica count is a deployment concern layered on later.
    pub async fn ensure_topology(&self) -> Result<()> {
        for &bucket in buckets::ALL_BUCKETS {
            let max_age = match bucket {
                buckets::CHANNELS => 7 * DAY,
                _ => Duration::ZERO, // no TTL
            };
            self.js
                .create_key_value(jetstream::kv::Config {
                    bucket: bucket.to_string(),
                    max_age,
                    storage: jetstream::stream::StorageType::File,
                    ..Default::default()
                })
                .await
                .map_err(nats_err)?;
        }

        let streams: &[(&str, &str, Duration)] = &[
            (buckets::STREAM_JOB_EVENTS, "job.events.>", 90 * DAY),
            (buckets::STREAM_CHANNEL_INBOX, "channel.inbox.>", 7 * DAY),
            (buckets::STREAM_INGEST, "ingest.>", 30 * DAY),
        ];
        for &(name, subject, max_age) in streams {
            self.js
                .get_or_create_stream(jetstream::stream::Config {
                    name: name.to_string(),
                    subjects: vec![subject.to_string()],
                    max_age,
                    storage: jetstream::stream::StorageType::File,
                    deny_delete: true,
                    ..Default::default()
                })
                .await
                .map_err(nats_err)?;
        }
        Ok(())
    }

    async fn bucket(&self, name: &str) -> Result<Bucket> {
        let kv = self.js.get_key_value(name).await.map_err(nats_err)?;
        Ok(Bucket::new(kv))
    }

    pub async fn jobs(&self) -> Result<JobStore> {
        Ok(JobStore(self.bucket(buckets::JOBS).await?))
    }

    pub async fn tasks(&self) -> Result<TaskStore> {
        Ok(TaskStore(self.bucket(buckets::TASKS).await?))
    }

    pub async fn steps(&self) -> Result<StepStore> {
        Ok(StepStore(self.bucket(buckets::STEPS).await?))
    }

    pub async fn counters(&self) -> Result<CounterStore> {
        Ok(CounterStore(self.bucket(buckets::COUNTERS).await?))
    }

    pub async fn rdeps(&self) -> Result<RdepsStore> {
        Ok(RdepsStore(self.bucket(buckets::RDEPS).await?))
    }

    /// Raw bucket access for stores not yet given a typed wrapper.
    pub async fn raw_bucket(&self, name: &str) -> Result<Bucket> {
        self.bucket(name).await
    }

    /// Request-reply with bounded retry (spec §4.2 reliability): retries until
    /// an ack is received or attempts are exhausted, with linear backoff.
    pub async fn request_with_retry(
        &self,
        subject: &str,
        payload: &[u8],
        attempts: u32,
        backoff: Duration,
    ) -> Result<async_nats::Message> {
        let mut last_err = None;
        for attempt in 1..=attempts {
            match self
                .client
                .request(subject.to_string(), payload.to_vec().into())
                .await
            {
                Ok(msg) => return Ok(msg),
                Err(e) => {
                    last_err = Some(nats_err(e));
                    if attempt < attempts {
                        tokio::time::sleep(backoff * attempt).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| StoreError::Nats("no attempts made".into())))
    }
}

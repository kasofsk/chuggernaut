//! NATS KV/stream access — the only crate that talks to NATS (spec §1.4–1.5).

pub mod artifacts;
pub mod buckets;
pub mod keys;
pub mod secrets;
pub mod stores;
pub mod subjects;
pub mod vars;
pub mod worker;

pub use artifacts::{ArtifactCrypto, ArtifactKind, ArtifactStore};
pub use stores::{
    Bucket, CounterStore, JobStore, ProjectStore, RdepsStore, StepStore, TaskStore, split_project,
};

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

    /// Connect with `.creds`-format credentials (per-job scoped user JWT +
    /// nkey seed, spec §7.4).
    pub async fn connect_with_creds(url: &str, creds: &str) -> Result<Self> {
        let client = async_nats::ConnectOptions::with_credentials(creds)
            .map_err(nats_err)?
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

        // Blobs live in an object store, not KV: transcripts routinely exceed
        // the 1MB max_payload. 90d matches `job-events`, the trail they pair
        // with. Deletion stays allowed — unlike the streams — because artifacts
        // are observability data an operator may need to purge.
        self.js
            .create_object_store(jetstream::object_store::Config {
                bucket: buckets::OBJECT_ARTIFACTS.to_string(),
                max_age: 90 * DAY,
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await
            .map_err(nats_err)?;
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

    pub async fn projects(&self) -> Result<ProjectStore> {
        Ok(ProjectStore(self.bucket(buckets::PROJECTS).await?))
    }

    /// Per-task blobs (transcripts, logs). `crypto` decides whether this handle
    /// can read: the dispatcher passes an identity, an encrypt-only caller does
    /// not. See [`artifacts`] for why this key is not the secrets key.
    pub async fn artifacts(&self, crypto: ArtifactCrypto) -> Result<ArtifactStore> {
        let obj = self
            .js
            .get_object_store(buckets::OBJECT_ARTIFACTS)
            .await
            .map_err(nats_err)?;
        Ok(ArtifactStore::new(obj, crypto))
    }

    /// Raw bucket access for stores not yet given a typed wrapper.
    pub async fn raw_bucket(&self, name: &str) -> Result<Bucket> {
        self.bucket(name).await
    }

    /// Read up to `max` message payloads from a stream via an ephemeral pull
    /// consumer — event-trail assertions in tests, webhook replay later.
    pub async fn read_stream(&self, stream: &str, max: usize) -> Result<Vec<Vec<u8>>> {
        use futures::StreamExt;
        let stream = self.js.get_stream(stream).await.map_err(nats_err)?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config::default())
            .await
            .map_err(nats_err)?;
        let mut batch = consumer
            .fetch()
            .max_messages(max)
            .messages()
            .await
            .map_err(nats_err)?;
        let mut out = Vec::new();
        while let Some(msg) = batch.next().await {
            out.push(msg.map_err(nats_err)?.payload.to_vec());
        }
        Ok(out)
    }

    /// Read messages on one subject after a given stream sequence — the
    /// channel_check contract (spec §4.2 polling mode). Returns
    /// `(stream_seq, payload)` pairs so callers can track their cursor.
    pub async fn read_subject_after(
        &self,
        stream: &str,
        subject: &str,
        after_seq: u64,
        max: usize,
    ) -> Result<Vec<(u64, Vec<u8>)>> {
        use futures::StreamExt;
        let stream = self.js.get_stream(stream).await.map_err(nats_err)?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                filter_subject: subject.to_string(),
                deliver_policy: if after_seq == 0 {
                    jetstream::consumer::DeliverPolicy::All
                } else {
                    jetstream::consumer::DeliverPolicy::ByStartSequence {
                        start_sequence: after_seq + 1,
                    }
                },
                ..Default::default()
            })
            .await
            .map_err(nats_err)?;
        let mut batch = consumer
            .fetch()
            .max_messages(max)
            .messages()
            .await
            .map_err(nats_err)?;
        let mut out = Vec::new();
        while let Some(msg) = batch.next().await {
            let msg = msg.map_err(nats_err)?;
            let seq = msg.info().map_err(nats_err)?.stream_sequence;
            out.push((seq, msg.payload.to_vec()));
        }
        Ok(out)
    }

    /// Live-subscribe to a JetStream stream with a subject filter — the SSE
    /// bridge (spec §6.4). An ephemeral push-less pull consumer is created at
    /// `after_seq + 1` (or the start of the stream when 0) and messages are
    /// delivered continuously; each item carries its stream sequence so
    /// clients can resume via `Last-Event-ID`.
    pub async fn subscribe_stream(
        &self,
        stream: &str,
        subject: &str,
        after_seq: u64,
    ) -> Result<StreamSubscription> {
        use futures::StreamExt;
        let stream = self.js.get_stream(stream).await.map_err(nats_err)?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                filter_subject: subject.to_string(),
                deliver_policy: if after_seq == 0 {
                    jetstream::consumer::DeliverPolicy::All
                } else {
                    jetstream::consumer::DeliverPolicy::ByStartSequence {
                        start_sequence: after_seq + 1,
                    }
                },
                ..Default::default()
            })
            .await
            .map_err(nats_err)?;
        let messages = consumer.messages().await.map_err(nats_err)?;
        Ok(StreamSubscription {
            inner: messages.boxed(),
        })
    }

    /// Subscribe to a request subject (wildcards allowed). Keeps async-nats
    /// confined to this crate: consumers get subject/payload and a replier.
    pub async fn subscribe_requests(&self, subject: &str) -> Result<RequestSubscription> {
        let sub = self
            .client
            .subscribe(subject.to_string())
            .await
            .map_err(nats_err)?;
        Ok(RequestSubscription {
            sub,
            client: self.client.clone(),
        })
    }

    /// Single request-reply bounded by a deadline — worker-node ops, where the
    /// caller (not NATS) owns timeout policy. A no-responder error surfaces
    /// immediately as `Nats`, not after the timeout.
    pub async fn request_timeout(
        &self,
        subject: &str,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<async_nats::Message> {
        match tokio::time::timeout(
            timeout,
            self.client
                .request(subject.to_string(), payload.to_vec().into()),
        )
        .await
        {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(e)) => Err(nats_err(e)),
            Err(_) => Err(StoreError::Nats(format!(
                "request to {subject} timed out after {timeout:?}"
            ))),
        }
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

/// A live JetStream subscription (see [`NatsStore::subscribe_stream`]).
pub struct StreamSubscription {
    inner: futures::stream::BoxStream<
        'static,
        std::result::Result<
            jetstream::Message,
            async_nats::error::Error<jetstream::consumer::pull::MessagesErrorKind>,
        >,
    >,
}

impl StreamSubscription {
    /// Next `(stream_seq, subject, payload)`; `None` when the underlying
    /// consumer ends. Transport errors end the stream (callers reconnect).
    pub async fn next(&mut self) -> Option<(u64, String, Vec<u8>)> {
        use futures::StreamExt;
        loop {
            let msg = self.inner.next().await?.ok()?;
            let _ = msg.ack().await;
            let Ok(info) = msg.info() else { continue };
            return Some((
                info.stream_sequence,
                msg.subject.to_string(),
                msg.payload.to_vec(),
            ));
        }
    }
}

/// A stream of inbound request-reply messages (see
/// [`NatsStore::subscribe_requests`]).
pub struct RequestSubscription {
    sub: async_nats::Subscriber,
    client: async_nats::Client,
}

impl RequestSubscription {
    pub async fn next(&mut self) -> Option<InboundRequest> {
        use futures::StreamExt;
        let msg = self.sub.next().await?;
        Some(InboundRequest {
            subject: msg.subject.to_string(),
            payload: msg.payload.to_vec(),
            reply_to: msg.reply.map(|r| r.to_string()),
            client: self.client.clone(),
        })
    }
}

pub struct InboundRequest {
    pub subject: String,
    pub payload: Vec<u8>,
    reply_to: Option<String>,
    client: async_nats::Client,
}

impl InboundRequest {
    pub async fn respond(&self, body: impl Into<Vec<u8>>) {
        if let Some(reply_to) = &self.reply_to {
            let _ = self
                .client
                .publish(reply_to.clone(), body.into().into())
                .await;
        }
    }
}

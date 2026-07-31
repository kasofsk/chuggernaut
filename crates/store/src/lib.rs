//! NATS KV/stream access — the only crate that talks to NATS (spec §1.4–1.5).

pub mod artifacts;
pub mod buckets;
pub mod keys;
pub mod secrets;
pub mod stores;
pub mod subjects;
pub mod vars;
pub mod worker;

pub use artifacts::{
    ArtifactCrypto, ArtifactKind, ArtifactStore, Attachment, DEFAULT_ATTACHMENT_CONTENT_TYPE,
};
pub use stores::{
    Bucket, CounterStore, JobStore, KvWatch, ProjectStore, RdepsStore, StepStore, TaskStore,
    split_project,
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

/// Prepend a namespace prefix to a NATS bucket/stream/object/subject name
/// (#206). Free function so the byte-identical-for-empty-prefix invariant is
/// unit-testable without a live connection: with the empty (production) prefix
/// the name is returned unchanged, so prod's wire format never migrates.
fn namespaced(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}{name}")
    }
}

const DAY: Duration = Duration::from_secs(86_400);

/// Connected NATS handle wrapping a client + JetStream context.
///
/// ## Namespacing (#206)
///
/// Every bucket, stream, object store, and subject name this handle touches is
/// transparently prefixed with [`NatsStore::prefix`]. Production connects with
/// the **empty** prefix, so the wire format is byte-identical to the un-prefixed
/// names — prod data never migrates. Tests connect with a unique per-test prefix
/// (via [`NatsStore::connect_namespaced`]) so many tests share one NATS server
/// without stepping on each other's KV, streams, or request/reply subjects.
///
/// The prefix is applied at exactly one place per NATS primitive — the methods
/// below — so no caller (and no other crate) ever needs to know it exists. This
/// keeps `store` the only crate that talks to NATS *and* the only crate that
/// knows names can be namespaced.
#[derive(Clone)]
pub struct NatsStore {
    client: async_nats::Client,
    js: jetstream::Context,
    /// Prepended to every bucket/stream/object/subject name. Empty in prod.
    prefix: String,
}

impl NatsStore {
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_namespaced(url, "").await
    }

    /// Connect with a per-handle namespace `prefix` prepended to every
    /// bucket/stream/object/subject name (#206). An empty prefix is identical to
    /// [`NatsStore::connect`] — the prod wire format. A non-empty prefix must be
    /// a single NATS-safe token fragment (`[A-Za-z0-9_-]`, no `.`) so it stays
    /// legal in both KV bucket names and subject tokens; test callers use
    /// [`crate::unique_prefix`]-style values such as `"t9f3a2c1-"`.
    pub async fn connect_namespaced(url: &str, prefix: &str) -> Result<Self> {
        let client = if prefix.is_empty() {
            async_nats::connect(url).await.map_err(nats_err)?
        } else {
            let mut attempt = 0;
            loop {
                attempt += 1;
                match async_nats::connect(url).await {
                    Ok(c) => break c,
                    Err(e) if attempt < 10 => {
                        tokio::time::sleep(Duration::from_millis(100 * attempt)).await;
                        let _ = e;
                    }
                    Err(e) => return Err(nats_err(e)),
                }
            }
        };
        let mut js = jetstream::new(client.clone());
        if !prefix.is_empty() {
            js.set_timeout(Duration::from_secs(30));
        }
        Ok(Self {
            client,
            js,
            prefix: prefix.to_string(),
        })
    }

    /// Connect with `.creds`-format credentials (per-job scoped user JWT +
    /// nkey seed, spec §7.4). Always the empty prefix (production).
    pub async fn connect_with_creds(url: &str, creds: &str) -> Result<Self> {
        let client = async_nats::ConnectOptions::with_credentials(creds)
            .map_err(nats_err)?
            .connect(url)
            .await
            .map_err(nats_err)?;
        let js = jetstream::new(client.clone());
        Ok(Self {
            client,
            js,
            prefix: String::new(),
        })
    }

    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    pub fn jetstream(&self) -> &jetstream::Context {
        &self.js
    }

    /// This handle's namespace prefix (empty in production).
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Prepend the namespace prefix to a bucket/stream/object/subject name.
    /// With the empty (prod) prefix this returns the input unchanged, so the
    /// wire format is byte-identical to today.
    #[inline]
    fn ns(&self, name: &str) -> String {
        namespaced(&self.prefix, name)
    }

    /// Create all fixed KV buckets and streams (spec §1.5). Idempotent; called
    /// by `chuggernaut init` and the test harness. Replicas: 1 (dev) — the
    /// production replica count is a deployment concern layered on later.
    ///
    /// A namespaced (test) handle retries the whole sequence on a JetStream
    /// timeout: under a `cargo test --workspace` fan-out, hundreds of namespaces
    /// call this at once against one shared server, and the JetStream meta-layer
    /// (which serializes stream/consumer creation) backs up. Retrying — the
    /// creates are idempotent — disperses that thundering herd instead of failing
    /// a test's setup. Production (empty prefix) runs once, unchanged.
    pub async fn ensure_topology(&self) -> Result<()> {
        if self.prefix.is_empty() {
            return self.ensure_topology_inner().await;
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.ensure_topology_inner().await {
                Ok(()) => return Ok(()),
                Err(e) if attempt < 8 => {
                    tokio::time::sleep(Duration::from_millis(150 * attempt)).await;
                    let _ = e;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn ensure_topology_inner(&self) -> Result<()> {
        let storage = if self.prefix.is_empty() {
            jetstream::stream::StorageType::File
        } else {
            jetstream::stream::StorageType::Memory
        };
        for &bucket in buckets::ALL_BUCKETS {
            let max_age = match bucket {
                buckets::CHANNELS => 7 * DAY,
                _ => Duration::ZERO,
            };
            self.js
                .create_key_value(jetstream::kv::Config {
                    bucket: self.ns(bucket),
                    max_age,
                    storage,
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
                    name: self.ns(name),
                    subjects: vec![self.ns(subject)],
                    max_age,
                    storage,
                    deny_delete: true,
                    ..Default::default()
                })
                .await
                .map_err(nats_err)?;
        }

        self.js
            .create_object_store(jetstream::object_store::Config {
                bucket: self.ns(buckets::OBJECT_ARTIFACTS),
                max_age: 90 * DAY,
                storage,
                ..Default::default()
            })
            .await
            .map_err(nats_err)?;
        Ok(())
    }

    async fn bucket(&self, name: &str) -> Result<Bucket> {
        let kv = self
            .js
            .get_key_value(self.ns(name))
            .await
            .map_err(nats_err)?;
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
            .get_object_store(self.ns(buckets::OBJECT_ARTIFACTS))
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
        let stream = self
            .js
            .get_stream(self.ns(stream))
            .await
            .map_err(nats_err)?;
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
        let stream = self
            .js
            .get_stream(self.ns(stream))
            .await
            .map_err(nats_err)?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                filter_subject: self.ns(subject),
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
        start: StreamStart,
    ) -> Result<StreamSubscription> {
        use futures::StreamExt;
        let stream = self
            .js
            .get_stream(self.ns(stream))
            .await
            .map_err(nats_err)?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                filter_subject: self.ns(subject),
                deliver_policy: match start {
                    StreamStart::New => jetstream::consumer::DeliverPolicy::New,
                    StreamStart::All => jetstream::consumer::DeliverPolicy::All,
                    StreamStart::After(seq) => {
                        jetstream::consumer::DeliverPolicy::ByStartSequence {
                            start_sequence: seq + 1,
                        }
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
            .subscribe(self.ns(subject))
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
        let subject = self.ns(subject);
        match tokio::time::timeout(
            timeout,
            self.client
                .request(subject.clone(), payload.to_vec().into()),
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

    /// Fire-and-forget publish on a plain core-NATS subject (no JetStream, no
    /// reply) — the worker announce heartbeat (spec §3.1 dynamic registration).
    /// A dropped message is covered by the next heartbeat, so this does not flush.
    pub async fn publish(&self, subject: &str, payload: &[u8]) -> Result<()> {
        self.client
            .publish(self.ns(subject), payload.to_vec().into())
            .await
            .map_err(nats_err)
    }

    /// Publish to a JetStream stream and await the server ack (the `job-events`
    /// trail, spec §6.3). Unlike [`NatsStore::publish`] this is durable: the
    /// double-await confirms the message reached the stream. The subject is
    /// namespaced like everything else, so the per-test stream captures it.
    pub async fn publish_event(&self, subject: &str, payload: &[u8]) -> Result<()> {
        self.js
            .publish(self.ns(subject), payload.to_vec().into())
            .await
            .map_err(nats_err)?
            .await
            .map_err(nats_err)?;
        Ok(())
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
        let subject = self.ns(subject);
        let mut last_err = None;
        for attempt in 1..=attempts {
            match self
                .client
                .request(subject.clone(), payload.to_vec().into())
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

/// Where a [`NatsStore::subscribe_stream`] consumer starts reading.
///
/// The distinction is load-bearing for SSE (spec §6.4): a *resuming* client
/// sends `Last-Event-ID` and must get everything after it or it silently loses
/// events, while a *fresh* subscriber may not want the stream's whole retained
/// history — on the dogfood project a cold project-scoped connect replayed
/// ~3900 events (900 KB) before delivering anything live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStart {
    /// Only events published from now on.
    New,
    /// Everything the stream still retains.
    All,
    /// Everything after this stream sequence — a client resuming from its
    /// `Last-Event-ID`.
    After(u64),
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

#[cfg(test)]
mod namespace_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::namespaced;

    /// #206: the empty (production) prefix must be byte-identical to the raw
    /// name — prod data never migrates. A non-empty prefix is a plain prepend,
    /// valid in both KV bucket names and subject tokens.
    #[test]
    fn empty_prefix_is_byte_identical() {
        for name in [
            "jobs",
            "job-events",
            "req.work.submit.acme.api.1",
            "artifacts",
        ] {
            assert_eq!(namespaced("", name), name);
        }
    }

    #[test]
    fn non_empty_prefix_prepends() {
        assert_eq!(namespaced("t9f3a2c1-", "jobs"), "t9f3a2c1-jobs");
        assert_eq!(
            namespaced("t9f3a2c1-", "req.work.submit.acme.api.1"),
            "t9f3a2c1-req.work.submit.acme.api.1"
        );
    }
}

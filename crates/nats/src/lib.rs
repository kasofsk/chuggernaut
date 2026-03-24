use async_nats::jetstream;
use bytes::Bytes;
use serde::Serialize;

use chuggernaut_types::{RequestSubject, Subject, SubjectFn};

/// A wrapper around the NATS client that provides typed publish/subscribe
/// and optional subject prefixing for test namespace isolation.
///
/// Used by dispatcher (with prefix in tests), worker, reviewer, and CLI.
#[derive(Clone)]
pub struct NatsClient {
    client: async_nats::Client,
    js: Option<jetstream::Context>,
    prefix: Option<String>,
}

impl NatsClient {
    /// Create a client without JetStream or prefix (worker, reviewer, CLI).
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            js: None,
            prefix: None,
        }
    }

    /// Create a client with JetStream context (dispatcher).
    pub fn with_jetstream(client: async_nats::Client, js: jetstream::Context) -> Self {
        Self {
            client,
            js: Some(js),
            prefix: None,
        }
    }

    /// Create a prefixed client with JetStream (dispatcher tests).
    pub fn with_prefix(
        client: async_nats::Client,
        js: jetstream::Context,
        prefix: String,
    ) -> Self {
        Self {
            client,
            js: Some(js),
            prefix: Some(prefix),
        }
    }

    /// The raw prefix (or None). Exposed for nats_init to name buckets/streams.
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    /// Access the underlying async_nats client.
    pub fn raw(&self) -> &async_nats::Client {
        &self.client
    }

    /// Access the JetStream context. Panics if not configured.
    pub fn jetstream(&self) -> &jetstream::Context {
        self.js.as_ref().expect("JetStream not configured on this NatsClient")
    }

    /// Apply the prefix to a subject string.
    fn prefixed(&self, subject: &str) -> String {
        match &self.prefix {
            Some(p) => format!("{p}_{subject}"),
            None => subject.to_string(),
        }
    }

    /// Strip the prefix from an incoming subject to recover the base subject.
    pub fn strip_prefix<'a>(&self, subject: &'a str) -> &'a str {
        match &self.prefix {
            Some(p) => {
                let pat = format!("{p}_");
                subject.strip_prefix(&pat).unwrap_or(subject)
            }
            None => subject,
        }
    }

    // -----------------------------------------------------------------------
    // Typed API — compile-time type safety for NATS subjects
    // -----------------------------------------------------------------------

    /// Publish a typed message to a static subject.
    pub async fn publish_msg<T: Serialize>(
        &self,
        subject: &Subject<T>,
        payload: &T,
    ) -> Result<(), async_nats::PublishError> {
        let s = self.prefixed(subject.name);
        let bytes = serde_json::to_vec(payload).expect("serialize");
        self.client.publish(s, Bytes::from(bytes)).await
    }

    /// Subscribe to a static subject.
    pub async fn subscribe_subject<T>(
        &self,
        subject: &Subject<T>,
    ) -> Result<async_nats::Subscriber, async_nats::SubscribeError> {
        let s = self.prefixed(subject.name);
        self.client.subscribe(s).await
    }

    /// Subscribe to a request-reply subject (for the responder side).
    pub async fn subscribe_request<Req, Resp>(
        &self,
        subject: &RequestSubject<Req, Resp>,
    ) -> Result<async_nats::Subscriber, async_nats::SubscribeError> {
        let s = self.prefixed(subject.name);
        self.client.subscribe(s).await
    }

    /// Send a typed request-reply message. Returns the raw reply message
    /// (caller deserializes the response).
    pub async fn request_msg<Req: Serialize, Resp>(
        &self,
        subject: &RequestSubject<Req, Resp>,
        payload: &Req,
    ) -> Result<async_nats::Message, async_nats::RequestError> {
        let s = self.prefixed(subject.name);
        let bytes = serde_json::to_vec(payload).expect("serialize");
        self.client.request(s, Bytes::from(bytes)).await
    }

    /// Publish a typed message to a dynamic (parametric) subject.
    pub async fn publish_to<T: Serialize>(
        &self,
        subject: &SubjectFn<T>,
        param: &str,
        payload: &T,
    ) -> Result<(), async_nats::PublishError> {
        let s = self.prefixed(&subject.format(param));
        let bytes = serde_json::to_vec(payload).expect("serialize");
        self.client.publish(s, Bytes::from(bytes)).await
    }

    /// Subscribe to a dynamic subject with a specific parameter value.
    pub async fn subscribe_dynamic<T>(
        &self,
        subject: &SubjectFn<T>,
        param: &str,
    ) -> Result<async_nats::Subscriber, async_nats::SubscribeError> {
        let s = self.prefixed(&subject.format(param));
        self.client.subscribe(s).await
    }

    // -----------------------------------------------------------------------
    // Raw API — for wildcard subscribes, reply subjects, JetStream publish
    // -----------------------------------------------------------------------

    /// Publish raw bytes to a string subject (prefixed).
    pub async fn publish(
        &self,
        subject: &str,
        payload: Bytes,
    ) -> Result<(), async_nats::PublishError> {
        let s = self.prefixed(subject);
        self.client.publish(s, payload).await
    }

    /// Subscribe to a raw string subject (prefixed). Use for wildcards like "chuggernaut.monitor.>".
    pub async fn subscribe(
        &self,
        subject: &str,
    ) -> Result<async_nats::Subscriber, async_nats::SubscribeError> {
        let s = self.prefixed(subject);
        self.client.subscribe(s).await
    }

    /// Send a raw request (prefixed).
    pub async fn request(
        &self,
        subject: &str,
        payload: Bytes,
    ) -> Result<async_nats::Message, async_nats::RequestError> {
        let s = self.prefixed(subject);
        self.client.request(s, payload).await
    }

    /// Publish to a JetStream subject (for streams like TRANSITIONS).
    pub async fn js_publish(
        &self,
        subject: &str,
        payload: Bytes,
    ) -> Result<async_nats::jetstream::context::PublishAckFuture, async_nats::jetstream::context::PublishError>
    {
        let s = self.prefixed(subject);
        self.jetstream().publish(s, payload).await
    }

    /// Publish to a raw (non-prefixed) subject. Used for NATS reply subjects
    /// which are generated by NATS itself and must not be prefixed.
    pub async fn publish_raw(
        &self,
        subject: String,
        payload: Bytes,
    ) -> Result<(), async_nats::PublishError> {
        self.client.publish(subject, payload).await
    }
}

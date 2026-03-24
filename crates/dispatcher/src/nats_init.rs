use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::kv;
use async_nats::jetstream::stream::{RetentionPolicy, StorageType};
use tracing::info;

use crate::error::DispatcherResult;

/// All KV store handles used by the dispatcher.
pub struct KvStores {
    pub jobs: kv::Store,
    pub claims: kv::Store,
    pub deps: kv::Store,
    pub counters: kv::Store,
    pub activities: kv::Store,
    pub journal: kv::Store,
    pub channels: kv::Store,
}

/// Create or verify all KV buckets and JetStream streams.
/// If `prefix` is Some, all bucket and stream names are suffixed with `_{prefix}`
/// and stream subjects are prefixed with `{prefix}_`. Used for test isolation.
pub async fn initialize(
    js: &jetstream::Context,
    _lease_secs: u64,
) -> DispatcherResult<KvStores> {
    initialize_with_prefix(js, _lease_secs, None).await
}

pub async fn initialize_with_prefix(
    js: &jetstream::Context,
    _lease_secs: u64,
    prefix: Option<&str>,
) -> DispatcherResult<KvStores> {
    let name = |base: &str| -> String {
        match prefix {
            Some(p) => format!("{base}_{p}"),
            None => base.to_string(),
        }
    };

    info!(prefix, "initializing NATS KV buckets and streams");

    let jobs = js
        .create_key_value(kv::Config {
            bucket: name(chuggernaut_types::buckets::JOBS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let claims = js
        .create_key_value(kv::Config {
            bucket: name(chuggernaut_types::buckets::CLAIMS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let deps = js
        .create_key_value(kv::Config {
            bucket: name(chuggernaut_types::buckets::DEPS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let counters = js
        .create_key_value(kv::Config {
            bucket: name(chuggernaut_types::buckets::COUNTERS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let activities = js
        .create_key_value(kv::Config {
            bucket: name(chuggernaut_types::buckets::ACTIVITIES),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let journal = js
        .create_key_value(kv::Config {
            bucket: name(chuggernaut_types::buckets::JOURNAL),
            history: 1,
            storage: StorageType::File,
            max_age: Duration::from_secs(7 * 24 * 3600),
            ..Default::default()
        })
        .await?;

    let channels = js
        .create_key_value(kv::Config {
            bucket: name(chuggernaut_types::buckets::CHANNELS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    // JetStream streams — prefix both name and subjects
    let (trans_name, trans_subjects) = match prefix {
        Some(p) => (
            format!("{}_{p}", chuggernaut_types::streams::TRANSITIONS),
            vec![format!("{p}_chuggernaut.transitions.>")],
        ),
        None => (
            chuggernaut_types::streams::TRANSITIONS.to_string(),
            vec!["chuggernaut.transitions.>".to_string()],
        ),
    };
    js.create_stream(jetstream::stream::Config {
        name: trans_name,
        subjects: trans_subjects,
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(7 * 24 * 3600),
        storage: StorageType::File,
        ..Default::default()
    })
    .await?;

    let (mon_name, mon_subjects) = match prefix {
        Some(p) => (
            format!("{}_{p}", chuggernaut_types::streams::MONITOR),
            vec![format!("{p}_chuggernaut.monitor.>")],
        ),
        None => (
            chuggernaut_types::streams::MONITOR.to_string(),
            vec!["chuggernaut.monitor.>".to_string()],
        ),
    };
    js.create_stream(jetstream::stream::Config {
        name: mon_name,
        subjects: mon_subjects,
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(24 * 3600),
        storage: StorageType::File,
        ..Default::default()
    })
    .await?;

    info!("NATS initialization complete");

    Ok(KvStores {
        jobs,
        claims,
        deps,
        counters,
        activities,
        journal,
        channels,
    })
}

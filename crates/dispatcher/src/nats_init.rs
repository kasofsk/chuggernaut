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
    pub workers: kv::Store,
    pub counters: kv::Store,
    pub sessions: kv::Store,
    pub activities: kv::Store,
    pub pending_reworks: kv::Store,
    pub abandon_blacklist: kv::Store,
    pub journal: kv::Store,
}

/// Create or verify all KV buckets and JetStream streams.
/// If `prefix` is Some, all bucket and stream names are suffixed with `_{prefix}`
/// and stream subjects are prefixed with `{prefix}_`. Used for test isolation.
pub async fn initialize(
    js: &jetstream::Context,
    lease_secs: u64,
    blacklist_ttl_secs: u64,
) -> DispatcherResult<KvStores> {
    initialize_with_prefix(js, lease_secs, blacklist_ttl_secs, None).await
}

pub async fn initialize_with_prefix(
    js: &jetstream::Context,
    lease_secs: u64,
    blacklist_ttl_secs: u64,
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
            bucket: name(forge2_types::buckets::JOBS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let claims = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::CLAIMS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let deps = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::DEPS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let workers = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::WORKERS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let counters = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::COUNTERS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let sessions = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::SESSIONS),
            history: 1,
            storage: StorageType::File,
            max_age: Duration::from_secs(lease_secs),
            ..Default::default()
        })
        .await?;

    let activities = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::ACTIVITIES),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let pending_reworks = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::PENDING_REWORKS),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    let abandon_blacklist = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::ABANDON_BLACKLIST),
            history: 1,
            storage: StorageType::File,
            max_age: Duration::from_secs(blacklist_ttl_secs),
            ..Default::default()
        })
        .await?;

    let journal = js
        .create_key_value(kv::Config {
            bucket: name(forge2_types::buckets::JOURNAL),
            history: 1,
            storage: StorageType::File,
            max_age: Duration::from_secs(7 * 24 * 3600),
            ..Default::default()
        })
        .await?;

    // JetStream streams — prefix both name and subjects
    let (trans_name, trans_subjects) = match prefix {
        Some(p) => (
            format!("{}_{p}", forge2_types::streams::TRANSITIONS),
            vec![format!("{p}_forge2.transitions.>")],
        ),
        None => (
            forge2_types::streams::TRANSITIONS.to_string(),
            vec!["forge2.transitions.>".to_string()],
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

    let (we_name, we_subjects) = match prefix {
        Some(p) => (
            format!("{}_{p}", forge2_types::streams::WORKER_EVENTS),
            vec![
                format!("{p}_{}", forge2_types::subjects::WORKER_REGISTER),
                format!("{p}_{}", forge2_types::subjects::WORKER_IDLE),
                format!("{p}_{}", forge2_types::subjects::WORKER_OUTCOME),
                format!("{p}_{}", forge2_types::subjects::WORKER_UNREGISTER),
            ],
        ),
        None => (
            forge2_types::streams::WORKER_EVENTS.to_string(),
            vec![
                forge2_types::subjects::WORKER_REGISTER.to_string(),
                forge2_types::subjects::WORKER_IDLE.to_string(),
                forge2_types::subjects::WORKER_OUTCOME.to_string(),
                forge2_types::subjects::WORKER_UNREGISTER.to_string(),
            ],
        ),
    };
    js.create_stream(jetstream::stream::Config {
        name: we_name,
        subjects: we_subjects,
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(24 * 3600),
        storage: StorageType::File,
        ..Default::default()
    })
    .await?;

    let (mon_name, mon_subjects) = match prefix {
        Some(p) => (
            format!("{}_{p}", forge2_types::streams::MONITOR),
            vec![format!("{p}_forge2.monitor.>")],
        ),
        None => (
            forge2_types::streams::MONITOR.to_string(),
            vec!["forge2.monitor.>".to_string()],
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
        workers,
        counters,
        sessions,
        activities,
        pending_reworks,
        abandon_blacklist,
        journal,
    })
}

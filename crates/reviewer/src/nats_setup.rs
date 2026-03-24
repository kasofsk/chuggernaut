use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::kv;
use async_nats::jetstream::stream::StorageType;
use tracing::info;

use crate::error::ReviewerResult;
use crate::state::KvStores;

/// Initialize reviewer-owned KV buckets. The CHUGGERNAUT-TRANSITIONS stream
/// and other buckets are owned by the dispatcher — the reviewer only
/// reads from the stream and publishes to core subjects.
pub async fn initialize(
    js: &jetstream::Context,
    merge_lock_ttl_secs: u64,
) -> ReviewerResult<KvStores> {
    info!("initializing reviewer KV buckets");

    let merge_queue = js
        .create_key_value(kv::Config {
            bucket: chuggernaut_types::buckets::MERGE_QUEUE.to_string(),
            history: 1,
            storage: StorageType::File,
            max_age: Duration::from_secs(merge_lock_ttl_secs),
            ..Default::default()
        })
        .await?;

    let rework_counts = js
        .create_key_value(kv::Config {
            bucket: chuggernaut_types::buckets::REWORK_COUNTS.to_string(),
            history: 1,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;

    info!("reviewer KV initialization complete");

    Ok(KvStores {
        merge_queue,
        rework_counts,
    })
}

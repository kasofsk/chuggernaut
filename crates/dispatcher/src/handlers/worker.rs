//! The worker announce/heartbeat subject (spec §3.1 dynamic registration).
//!
//! - **Accepts:** `req.worker.announce` — a node's `WorkerAnnounce`
//!   (node, slots, version).
//! - **Emits:** `CoreHandle::announce_worker`, which merges the node into the
//!   live fleet.
//! - **Guarantees:** a plain (non-JetStream) subscription — heartbeats are
//!   transient, and losing the stream is what deregisters a node. A malformed
//!   announce is logged and dropped, never fatal.
//! - **Spec:** §3.1.

use crate::core::CoreHandle;
use store::NatsStore;

/// Subscribe the worker announce/heartbeat subject and forward each announce
/// into the core actor, which merges it into the live fleet. Harmless on a
/// single-node Docker deployment: the backend's `register_worker` no-ops there.
pub async fn spawn_worker_announce_handler(
    store: &NatsStore,
    handle: CoreHandle,
) -> store::Result<()> {
    let mut sub = store
        .subscribe_requests(&store::subjects::worker_announce())
        .await?;
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            match serde_json::from_slice::<types::worker::WorkerAnnounce>(&req.payload) {
                Ok(a) => {
                    if let Err(e) = handle.announce_worker(a).await {
                        tracing::warn!("worker announce forward failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("malformed worker announce: {e}"),
            }
        }
    });
    Ok(())
}

//! The two live-state probes the api forwards: liveness and the §3.5 launch
//! queue snapshot. Both round-trip the core actor on purpose — a wedged state
//! loop (not just a dead process) must read as unhealthy, and a queue depth
//! copied from anywhere else would be stale.
//!
//! - **Accepts:** `req.health`, `req.queue.list.{owner}.{project}`.
//! - **Emits:** `CoreHandle::{ping, queue_snapshot}` calls;
//!   `{"dispatcher":"ok","version"}` / the `QueueSnapshot` JSON, or a 503.
//! - **Guarantees:** never answers from cached state; a crash-looping
//!   dispatcher has no responder at all, so the api's bounded probe fails into
//!   a 503 rather than being fooled by the SPA fallback answering 200 (the
//!   2026-07-22 masquerade).
//! - **Spec:** §3.5, §6.1.

use super::reply::{bad_request, service_unavailable};
use crate::core::CoreHandle;
use store::NatsStore;

/// `req.health` — the §6.x liveness probe, answered through the core actor.
pub(super) async fn spawn_health_handler(
    store: &NatsStore,
    handle: CoreHandle,
) -> store::Result<()> {
    let mut health_sub = store.subscribe_requests(&store::subjects::health()).await?;
    tokio::spawn(async move {
        while let Some(req) = health_sub.next().await {
            let body = match handle.ping().await {
                Ok(()) => serde_json::json!({
                    "dispatcher": "ok",
                    "version": env!("CARGO_PKG_VERSION"),
                })
                .to_string()
                .into_bytes(),
                Err(e) => service_unavailable(&e.to_string()),
            };
            req.respond(body).await;
        }
    });
    Ok(())
}

/// `req.queue.list.{owner}.{project}` — the §3.5 capacity launch-queue
/// snapshot. The api forwards this for the "queued" badge; a wedged core simply
/// fails the bounded request into a graceful UI omission.
pub(super) async fn spawn_queue_handler(
    store: &NatsStore,
    handle: CoreHandle,
) -> store::Result<()> {
    let mut queue_sub = store.subscribe_requests("req.queue.list.>").await?;
    tokio::spawn(async move {
        while let Some(req) = queue_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3), parts.get(4)) else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = match handle.queue_snapshot(owner, project).await {
                Ok(snap) => serde_json::to_vec(&snap).unwrap_or_default(),
                Err(e) => service_unavailable(&e.to_string()),
            };
            req.respond(body).await;
        }
    });
    Ok(())
}

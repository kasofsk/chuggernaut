//! The fleet-scoped subjects (spec §3.1 operator capacity control). Fleet state
//! belongs to the platform rather than to any project, so nothing owner/project
//! rides in the subject and the node name travels in the payload — one
//! subscription serves the whole fleet, and an unknown name comes back a 404
//! instead of a request nobody answers.
//!
//! - **Accepts:** `req.fleet.capacity.set` — `{ node, slots, by }`.
//! - **Emits:** `CoreHandle::set_node_capacity`; the
//!   [`types::NodeCapacityAck`] 202 body, or the §6.5 error envelope (400
//!   malformed, 404 unknown node, 409 docker-endpoint node).
//! - **Guarantees:** answers without waiting on the node — the actor records
//!   intent and starts the push, so "recorded and converging" is the honest
//!   status. Writes nothing itself; the dispatcher stays the single writer of
//!   `fleet.capacity`.
//! - **Spec:** §3.1, §6.1.

use super::reply::{bad_request, error_reply, ok_reply};
use crate::core::CoreHandle;
use serde::Deserialize;
use store::NatsStore;

/// `req.fleet.capacity.set` body. `by` is the authenticated platform admin's
/// identity, read from the JWT by the api and never supplied by a browser.
#[derive(Debug, Deserialize)]
struct SetCapacityRequest {
    node: String,
    slots: u32,
    #[serde(default)]
    by: Option<String>,
}

/// Subscribe `req.fleet.capacity.set` and forward each command into the actor.
pub(super) async fn spawn_fleet_capacity_handler(
    store: &NatsStore,
    handle: CoreHandle,
) -> store::Result<()> {
    let mut sub = store
        .subscribe_requests(&store::subjects::fleet_capacity_set())
        .await?;
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            let body = match serde_json::from_slice::<SetCapacityRequest>(&req.payload) {
                Ok(body) => body,
                Err(e) => {
                    req.respond(bad_request(&format!("malformed capacity request: {e}")))
                        .await;
                    continue;
                }
            };
            // An unattributed change would leave the record's audit stamp lying;
            // `unknown` says what actually happened instead.
            let by = body.by.unwrap_or_else(|| "unknown".to_string());
            let reply = match handle.set_node_capacity(&body.node, body.slots, &by).await {
                Ok(ack) => ok_reply(&ack),
                Err(e) => error_reply(&e),
            };
            req.respond(reply).await;
        }
    });
    Ok(())
}

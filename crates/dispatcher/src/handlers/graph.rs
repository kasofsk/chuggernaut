//! The project DAG read (spec §6.1): every job record in the project, which is
//! what the graph view lays out. A pure store read — the in-memory DAG the core
//! keeps is a working copy, and KV stays the truth.
//!
//! - **Accepts:** `req.graph.get.{owner}.{project}`.
//! - **Emits:** `Job[]` JSON, or a §6.5 error envelope.
//! - **Guarantees:** read-only; never touches the core actor.
//! - **Spec:** §6.1, §1.4.

use super::reply::{bad_request, error_reply, ok_reply};
use store::NatsStore;

/// Subscribe `req.graph.get.>` — the whole job list, unsummarized.
pub(super) async fn spawn_graph_handler(store: &NatsStore) -> store::Result<()> {
    let mut graph_sub = store.subscribe_requests("req.graph.get.>").await?;
    let graph_store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = graph_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = match graph_store.jobs().await {
                Ok(jobs) => match jobs.list(owner, project).await {
                    Ok(list) => ok_reply(&list),
                    Err(e) => error_reply(&e.into()),
                },
                Err(e) => error_reply(&e.into()),
            };
            req.respond(body).await;
        }
    });
    Ok(())
}

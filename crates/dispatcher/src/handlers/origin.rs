//! The origin release surface for linked projects (§5.3): PR-based shipping,
//! its status, and the pull of origin's default branch back into the platform
//! copy. All three round-trip the core actor — origin credentials live with the
//! dispatcher and never enter a container.
//!
//! - **Accepts:** `req.origin.{release,status,sync}.{owner}.{project}`.
//! - **Emits:** `CoreHandle::{origin_release, origin_status, origin_sync}`
//!   calls; the resulting record/status JSON or a §6.5 error envelope.
//! - **Guarantees:** read-only with respect to job state — the origin surface
//!   ships what is already merged, it never drives a transition.
//! - **Spec:** §5.3.

use super::reply::{error_reply, ok_reply};
use crate::core::CoreHandle;
use store::NatsStore;

/// Subscribe the three origin verbs; one subscription and task per verb.
pub(super) async fn spawn_origin_handlers(
    store: &NatsStore,
    handle: CoreHandle,
) -> store::Result<()> {
    for kind in ["release", "status", "sync"] {
        let mut sub = store
            .subscribe_requests(&format!("req.origin.{kind}.>"))
            .await?;
        let handle = handle.clone();
        tokio::spawn(async move {
            while let Some(req) = sub.next().await {
                let parts: Vec<&str> = req.subject.split('.').collect();
                let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
                else {
                    req.respond(br#"{"error":"malformed subject"}"#.to_vec())
                        .await;
                    continue;
                };
                let body = match kind {
                    "release" => match handle.origin_release(owner, project).await {
                        Ok(record) => ok_reply(&record),
                        Err(e) => error_reply(&e),
                    },
                    "status" => match handle.origin_status(owner, project).await {
                        Ok(status) => ok_reply(&status),
                        Err(e) => error_reply(&e),
                    },
                    _ => match handle.origin_sync(owner, project).await {
                        Ok(status) => ok_reply(&status),
                        Err(e) => error_reply(&e),
                    },
                };
                req.respond(body).await;
            }
        });
    }
    Ok(())
}

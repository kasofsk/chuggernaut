//! NATS req.* subject handlers (spec §6.1). Each subscription translates a
//! request into a `CoreHandle` call and replies — the same idempotent,
//! bounded-retry contract the channel MCP server counts on (§4.2). This slice
//! wires the container-facing subjects (work/eval submit); the API-facing
//! families land with the api crate.

use crate::core::{CoreHandle, EvalSubmission, WorkSubmission};
use store::NatsStore;

/// Subscribe the container-facing subjects. Returns after subscriptions are
/// established; handler tasks run for the life of the NATS connection.
pub async fn spawn_container_handlers(store: &NatsStore, handle: CoreHandle) -> store::Result<()> {
    let mut work_sub = store.subscribe_requests("req.work.submit.>").await?;
    let work_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(req) = work_sub.next().await {
            // req.work.submit.{owner}.{project}.{seq}
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project), Some(seq)) = (
                parts.get(3).copied(),
                parts.get(4).copied(),
                parts.get(5).and_then(|s| s.parse::<u64>().ok()),
            ) else {
                req.respond(br#"{"error":"malformed subject"}"#.to_vec()).await;
                continue;
            };
            let submission: WorkSubmission =
                serde_json::from_slice(&req.payload).unwrap_or_default();
            let body = match work_handle.submit_result(owner, project, seq, submission).await {
                Ok(()) => r#"{"ok":true}"#.to_string(),
                Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
            };
            req.respond(body.into_bytes()).await;
        }
    });

    let mut eval_sub = store.subscribe_requests("req.eval.submit.>").await?;
    tokio::spawn(async move {
        while let Some(req) = eval_sub.next().await {
            // req.eval.submit.{owner}.{project}.{seq}.{task_id}
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project), Some(seq), Some(task_id)) = (
                parts.get(3).copied(),
                parts.get(4).copied(),
                parts.get(5).and_then(|s| s.parse::<u64>().ok()),
                parts.get(6).and_then(|s| s.parse::<u64>().ok()),
            ) else {
                req.respond(br#"{"error":"malformed subject"}"#.to_vec()).await;
                continue;
            };
            // §4.2: payload must include pass — malformed submissions are
            // rejected, not defaulted.
            let body = match serde_json::from_slice::<EvalSubmission>(&req.payload) {
                Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                Ok(submission) => {
                    match handle.submit_eval(owner, project, seq, task_id, submission).await {
                        Ok(()) => r#"{"ok":true}"#.to_string(),
                        Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                    }
                }
            };
            req.respond(body.into_bytes()).await;
        }
    });
    Ok(())
}

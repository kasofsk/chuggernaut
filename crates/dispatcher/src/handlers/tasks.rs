//! The `req.tasks.*` family (spec §6.1): the human inbox, a job's task log,
//! operator resolutions, and the live container output tail (§4.2).
//!
//! `output` is why this family is served off the core actor: it reads a
//! container's stdout through the backend, and a slow or wedged node must never
//! stall state transitions. Each output request is spawned on its own task so
//! one hung tail cannot block the list/resolve legs either. A single
//! subscription owns the whole family — a second overlapping subscription would
//! double-reply under core NATS.
//!
//! - **Accepts:** `req.tasks.list.pending.{owner}.{project}`,
//!   `req.tasks.list.{owner}.{project}.{job_seq}`,
//!   `req.tasks.resolve.{owner}.{project}.{job_seq}.{task_id}`,
//!   `req.tasks.output.{owner}.{project}.{seq}.{task_id}`.
//! - **Emits:** store reads, `ContainerBackend::logs_tail`, and
//!   `CoreHandle::resolve_task` (the one leg that mutates).
//! - **Guarantees:** reads never touch the core actor; a wedged node answers
//!   502, never a hang. A Pending task on a terminal job is filtered out of the
//!   inbox — resolving it could not advance anything.
//! - **Spec:** §6.1, §4.2, §1.2.

use super::reply::{NOT_FOUND, bad_gateway, bad_request, error_reply, ok_reply};
use crate::core::CoreHandle;
use container::ContainerBackend;
use std::sync::Arc;
use store::NatsStore;
use types::{TaskResolution, TaskState};

/// Wire body for `req.tasks.resolve`: the §6.2 `TaskResolution` plus the
/// operator identity the api layer read from the JWT cookie.
#[derive(serde::Deserialize)]
struct ResolveBody {
    resolution: TaskResolution,
    operator: String,
}

/// Subscribe `req.tasks.>` — one subscription for the whole family.
pub async fn spawn_tasks_handler(
    store: &NatsStore,
    handle: CoreHandle,
    backend: Arc<dyn ContainerBackend>,
) -> store::Result<()> {
    let mut tasks_sub = store.subscribe_requests("req.tasks.>").await?;
    let tasks_store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = tasks_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            // req.tasks.output.{owner}.{project}.{seq}.{task_id} — live output.
            // Spawned with cloned handles so a wedged node's slow tail neither
            // blocks other output reads nor the list/resolve legs of this loop.
            if parts.get(2) == Some(&"output") {
                let coords = (
                    parts.get(3).copied().map(String::from),
                    parts.get(4).copied().map(String::from),
                    parts.get(5).and_then(|s| s.parse::<u64>().ok()),
                    parts.get(6).and_then(|s| s.parse::<u64>().ok()),
                );
                let since = serde_json::from_slice::<serde_json::Value>(&req.payload)
                    .ok()
                    .and_then(|v| v.get("since").and_then(|s| s.as_u64()))
                    .unwrap_or(0);
                let store = tasks_store.clone();
                let backend = backend.clone();
                tokio::spawn(async move {
                    let body = match coords {
                        (Some(owner), Some(project), Some(seq), Some(task_id)) => {
                            task_output(&store, &backend, &owner, &project, seq, task_id, since)
                                .await
                        }
                        _ => bad_request("malformed subject"),
                    };
                    req.respond(body).await;
                });
                continue;
            }
            let body = tasks_dispatch(&tasks_store, &handle, &parts, &req.payload).await;
            req.respond(body).await;
        }
    });
    Ok(())
}

/// The store/actor legs of the family: the human inbox, a job's task log, and
/// an operator resolution. `output` is handled by the caller, off this loop.
async fn tasks_dispatch(
    store: &NatsStore,
    handle: &CoreHandle,
    parts: &[&str],
    payload: &[u8],
) -> Vec<u8> {
    match parts.get(2).copied() {
        // req.tasks.list.pending.{owner}.{project}
        Some("list") if parts.get(3) == Some(&"pending") => {
            match (parts.get(4).copied(), parts.get(5).copied()) {
                (Some(owner), Some(project)) => list_pending(store, owner, project).await,
                _ => bad_request("malformed subject"),
            }
        }
        // req.tasks.list.{owner}.{project}.{job_seq}
        Some("list") => match (
            parts.get(3).copied(),
            parts.get(4).copied(),
            parts.get(5).and_then(|s| s.parse::<u64>().ok()),
        ) {
            (Some(owner), Some(project), Some(job_seq)) => match store.tasks().await {
                Ok(tasks) => match tasks.list_for_job(owner, project, job_seq).await {
                    Ok(list) => ok_reply(&list),
                    Err(e) => error_reply(&e.into()),
                },
                Err(e) => error_reply(&e.into()),
            },
            _ => bad_request("malformed subject"),
        },
        // req.tasks.resolve.{owner}.{project}.{job_seq}.{task_id}
        Some("resolve") => match (
            parts.get(3).copied(),
            parts.get(4).copied(),
            parts.get(5).and_then(|s| s.parse::<u64>().ok()),
            parts.get(6).and_then(|s| s.parse::<u64>().ok()),
        ) {
            (Some(owner), Some(project), Some(job_seq), Some(task_id)) => {
                match serde_json::from_slice::<ResolveBody>(payload) {
                    Err(e) => bad_request(&e.to_string()),
                    Ok(b) => match handle
                        .resolve_task(owner, project, job_seq, task_id, b.resolution, &b.operator)
                        .await
                    {
                        Ok(()) => br#"{"ok":true}"#.to_vec(),
                        Err(e) => error_reply(&e),
                    },
                }
            }
            _ => bad_request("malformed subject"),
        },
        _ => bad_request("malformed subject"),
    }
}

/// Serve one `req.tasks.output` request (spec §4.2): the running container's
/// captured stdout/stderr from byte cursor `since`, or a `running:false` signal
/// telling the api to fall back to the harvested `stdout.log` artifact once the
/// container is gone. A read-only container tail — no core-actor involvement.
///
/// - Running task with a live container → `{ offset, data, running:true }`.
/// - Finished task (Done/Failed), or the container removed out from under a
///   still-Running record (the harvest/dispose race) → `{ running:false }`, the
///   api's cue to serve the artifact at the same byte offsets.
/// - Running task with no container yet (agent pre-launch, human/claimed) → 404.
/// - Unknown task → 404. A wedged/unreachable node → 502, never a hang.
async fn task_output(
    store: &NatsStore,
    backend: &Arc<dyn ContainerBackend>,
    owner: &str,
    project: &str,
    seq: u64,
    task_id: u64,
    since: u64,
) -> Vec<u8> {
    let task = match store.tasks().await {
        Ok(tasks) => match tasks.get(owner, project, seq, task_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return NOT_FOUND.to_vec(),
            Err(e) => return error_reply(&e.into()),
        },
        Err(e) => return error_reply(&e.into()),
    };
    match (&task.container_id, task.state) {
        (Some(id), TaskState::Running) => match backend.logs_tail(id, since).await {
            Ok(tail) => ok_reply(&serde_json::json!({
                "offset": tail.offset,
                "data": String::from_utf8_lossy(&tail.data),
                "running": true,
            })),
            // The container vanished under a still-Running record (harvest then
            // dispose, before the exit is recorded): fall back to the artifact.
            Err(container::BackendError::NotFound(_)) => finished_reply(),
            // A wedged/unreachable node: an error envelope, not a stall. This
            // request fails; others are untouched (it runs off the actor).
            Err(e) => bad_gateway(&format!("container output unavailable: {e}")),
        },
        // Finished → the api serves the harvested stdout.log at the same offsets.
        _ if task.state != TaskState::Running => finished_reply(),
        // Running, but no container yet (agent pre-launch, human/claimed attempt).
        _ => NOT_FOUND.to_vec(),
    }
}

/// The `running:false` cue: the task has no live container, so the api serves
/// the harvested `stdout.log` artifact (same byte-offset semantics) instead.
fn finished_reply() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "running": false }))
        .unwrap_or_else(|_| br#"{"running":false}"#.to_vec())
}

async fn list_pending(store: &NatsStore, owner: &str, project: &str) -> Vec<u8> {
    let all = match store.tasks().await {
        Ok(tasks) => match tasks.list_for_project(owner, project).await {
            Ok(all) => all,
            Err(e) => return error_reply(&e.into()),
        },
        Err(e) => return error_reply(&e.into()),
    };
    // A Pending task whose owning job is terminal (revoked/done) is a zombie:
    // resolving it is pointless because the job no longer exists to advance.
    // Revoke now closes its own tasks (§2.1), but records predating that fix
    // must still disappear from the inbox without a migration, so join against
    // the job records already in KV and drop any terminal-job task here.
    let terminal: std::collections::HashSet<u64> = match store.jobs().await {
        Ok(jobs) => match jobs.list(owner, project).await {
            Ok(jobs) => jobs
                .into_iter()
                .filter(|j| j.state.is_terminal())
                .map(|j| j.id)
                .collect(),
            Err(e) => return error_reply(&e.into()),
        },
        Err(e) => return error_reply(&e.into()),
    };
    let pending: Vec<_> = all
        .into_iter()
        .filter(|t| {
            // Human-kind waits AND claimed attempts of any kind (§1.2 claims)
            // — the latter are in-progress-by-human, not passive waits;
            // performed_by distinguishes them.
            t.state == types::TaskState::Pending
                && (matches!(t.kind, types::TaskKind::Human { .. })
                    || t.performed_by == Some(types::Performer::Human))
                && !terminal.contains(&t.job_seq)
        })
        .collect();
    ok_reply(&pending)
}

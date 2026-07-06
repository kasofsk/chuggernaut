//! NATS req.* subject handlers (spec §6.1). Each subscription translates a
//! request into a `CoreHandle` call and replies — the same idempotent,
//! bounded-retry contract the channel MCP server counts on (§4.2).
//! `spawn_container_handlers` wires the container-facing subjects (work/eval
//! submit); `spawn_api_handlers` wires the families the api crate bridges
//! (jobs, graph, tasks, vcs.diff).
//!
//! API-facing reply envelope: success is the resource JSON verbatim; failure
//! is `{"error": {"status": u16, "message": string, "errors": [...]?}}` so
//! the HTTP bridge can map straight to §6.5 responses.

use crate::core::{CoreError, CoreHandle, CreateJobRequest, EvalSubmission, WorkSubmission};
use std::collections::HashMap;
use std::sync::Arc;
use store::NatsStore;
use types::TaskResolution;
use vcs::RepoManager;

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

/// Map a core error to the §6.5 envelope with an HTTP status hint.
fn error_reply(e: &CoreError) -> Vec<u8> {
    let status = match e {
        CoreError::NotFound(_) => 404,
        CoreError::Validation(_) => 422,
        CoreError::Transition(_) | CoreError::InvalidResolution(_) => 409,
        _ => 500,
    };
    let mut body = serde_json::json!({
        "error": { "status": status, "message": e.to_string() }
    });
    if let CoreError::Validation(errs) = e {
        body["error"]["errors"] = serde_json::json!(errs);
    }
    serde_json::to_vec(&body).unwrap_or_else(|_| br#"{"error":{"status":500}}"#.to_vec())
}

fn ok_reply<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| br#"{"error":{"status":500}}"#.to_vec())
}

fn bad_request(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "error": { "status": 400, "message": message }
    }))
    .unwrap()
}

const NOT_FOUND: &[u8] = br#"{"error":{"status":404,"message":"not found"}}"#;

/// Wire body for `req.jobs.create` (spec §6.2 POST .../jobs).
#[derive(serde::Deserialize)]
struct CreateJobBody {
    r#type: String,
    #[serde(default)]
    inputs: HashMap<String, u64>,
    #[serde(default)]
    knowledge_tags: Vec<String>,
}

/// Wire body for `req.tasks.resolve`: the §6.2 `TaskResolution` plus the
/// operator identity the api layer read from the JWT cookie.
#[derive(serde::Deserialize)]
struct ResolveBody {
    resolution: TaskResolution,
    operator: String,
}

/// Subscribe the API-facing subject families (spec §6.1): jobs, graph.get,
/// tasks (pending/list/resolve), vcs.diff. Reads go straight to the store or
/// repos; mutations go through the core actor.
pub async fn spawn_api_handlers(
    store: &NatsStore,
    handle: CoreHandle,
    repos: Arc<RepoManager>,
) -> store::Result<()> {
    // ── req.jobs.{create,get,list,release,revoke} ───────────────────────
    let mut jobs_sub = store.subscribe_requests("req.jobs.>").await?;
    let jobs_store = store.clone();
    let jobs_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(req) = jobs_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            // req.jobs.{verb}.{owner}.{project}[.{seq}]
            let (Some(verb), Some(owner), Some(project)) =
                (parts.get(2).copied(), parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let seq = parts.get(5).and_then(|s| s.parse::<u64>().ok());
            let body = match (verb, seq) {
                ("create", None) => match serde_json::from_slice::<CreateJobBody>(&req.payload) {
                    Err(e) => bad_request(&e.to_string()),
                    Ok(b) => {
                        let create = CreateJobRequest {
                            owner: owner.to_string(),
                            project: project.to_string(),
                            r#type: b.r#type,
                            inputs: b.inputs,
                            knowledge_tags: b.knowledge_tags,
                            factory: None,
                        };
                        match jobs_handle.create_job(create).await {
                            Ok(job) => ok_reply(&job),
                            Err(e) => error_reply(&e),
                        }
                    }
                },
                ("get", Some(seq)) => match jobs_store.jobs().await {
                    Ok(jobs) => match jobs.get(owner, project, seq).await {
                        Ok(Some(job)) => ok_reply(&job),
                        Ok(None) => NOT_FOUND.to_vec(),
                        Err(e) => error_reply(&e.into()),
                    },
                    Err(e) => error_reply(&e.into()),
                },
                ("list", None) => match jobs_store.jobs().await {
                    Ok(jobs) => match jobs.list(owner, project).await {
                        Ok(list) => ok_reply(&list),
                        Err(e) => error_reply(&e.into()),
                    },
                    Err(e) => error_reply(&e.into()),
                },
                ("release", Some(seq)) => {
                    match jobs_handle.release_job(owner, project, seq).await {
                        Err(e) => error_reply(&e),
                        Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                    }
                }
                ("revoke", Some(seq)) => match jobs_handle.revoke_job(owner, project, seq).await {
                    Err(e) => error_reply(&e),
                    Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                },
                _ => bad_request("malformed subject"),
            };
            req.respond(body).await;
        }
    });

    // ── req.graph.get.{owner}.{project} — Job[] (spec §6.1) ─────────────
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

    // ── req.tasks.{list.pending,list,resolve} ───────────────────────────
    let mut tasks_sub = store.subscribe_requests("req.tasks.>").await?;
    let tasks_store = store.clone();
    let tasks_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(req) = tasks_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let body = match parts.get(2).copied() {
                // req.tasks.list.pending.{owner}.{project}
                Some("list") if parts.get(3) == Some(&"pending") => {
                    match (parts.get(4).copied(), parts.get(5).copied()) {
                        (Some(owner), Some(project)) => {
                            list_pending(&tasks_store, owner, project).await
                        }
                        _ => bad_request("malformed subject"),
                    }
                }
                // req.tasks.list.{owner}.{project}.{job_seq}
                Some("list") => {
                    match (
                        parts.get(3).copied(),
                        parts.get(4).copied(),
                        parts.get(5).and_then(|s| s.parse::<u64>().ok()),
                    ) {
                        (Some(owner), Some(project), Some(job_seq)) => {
                            match tasks_store.tasks().await {
                                Ok(tasks) => {
                                    match tasks.list_for_job(owner, project, job_seq).await {
                                        Ok(list) => ok_reply(&list),
                                        Err(e) => error_reply(&e.into()),
                                    }
                                }
                                Err(e) => error_reply(&e.into()),
                            }
                        }
                        _ => bad_request("malformed subject"),
                    }
                }
                // req.tasks.resolve.{owner}.{project}.{job_seq}.{task_id}
                Some("resolve") => {
                    match (
                        parts.get(3).copied(),
                        parts.get(4).copied(),
                        parts.get(5).and_then(|s| s.parse::<u64>().ok()),
                        parts.get(6).and_then(|s| s.parse::<u64>().ok()),
                    ) {
                        (Some(owner), Some(project), Some(job_seq), Some(task_id)) => {
                            match serde_json::from_slice::<ResolveBody>(&req.payload) {
                                Err(e) => bad_request(&e.to_string()),
                                Ok(b) => match tasks_handle
                                    .resolve_task(
                                        owner, project, job_seq, task_id, b.resolution,
                                        &b.operator,
                                    )
                                    .await
                                {
                                    Ok(()) => br#"{"ok":true}"#.to_vec(),
                                    Err(e) => error_reply(&e),
                                },
                            }
                        }
                        _ => bad_request("malformed subject"),
                    }
                }
                _ => bad_request("malformed subject"),
            };
            req.respond(body).await;
        }
    });

    // ── req.vcs.diff.{owner}.{project}.{seq} ────────────────────────────
    let mut diff_sub = store.subscribe_requests("req.vcs.diff.>").await?;
    let diff_store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = diff_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project), Some(seq)) = (
                parts.get(3).copied(),
                parts.get(4).copied(),
                parts.get(5).and_then(|s| s.parse::<u64>().ok()),
            ) else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = match diff_store.jobs().await {
                Ok(jobs) => match jobs.get(owner, project, seq).await {
                    Ok(Some(job)) => match repos.diff_for_job(&job).await {
                        Ok(diff) => ok_reply(&diff),
                        Err(e) => error_reply(&e.into()),
                    },
                    Ok(None) => NOT_FOUND.to_vec(),
                    Err(e) => error_reply(&e.into()),
                },
                Err(e) => error_reply(&e.into()),
            };
            req.respond(body).await;
        }
    });

    Ok(())
}

async fn fetch_job(store: &NatsStore, owner: &str, project: &str, seq: u64) -> Vec<u8> {
    match store.jobs().await {
        Ok(jobs) => match jobs.get(owner, project, seq).await {
            Ok(Some(job)) => ok_reply(&job),
            Ok(None) => NOT_FOUND.to_vec(),
            Err(e) => error_reply(&e.into()),
        },
        Err(e) => error_reply(&e.into()),
    }
}

async fn list_pending(store: &NatsStore, owner: &str, project: &str) -> Vec<u8> {
    match store.tasks().await {
        Ok(tasks) => match tasks.list_for_project(owner, project).await {
            Ok(all) => {
                let pending: Vec<_> = all
                    .into_iter()
                    .filter(|t| {
                        matches!(t.kind, types::TaskKind::Human { .. })
                            && t.state == types::TaskState::Pending
                    })
                    .collect();
                ok_reply(&pending)
            }
            Err(e) => error_reply(&e.into()),
        },
        Err(e) => error_reply(&e.into()),
    }
}

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

use crate::core::{
    ChannelPost, CoreError, CoreHandle, CreateJobRequest, EvalSubmission, WorkSubmission,
};
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
    let eval_handle = handle.clone();
    tokio::spawn(async move {
        let handle = eval_handle;
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

    // req.channel.{update,reply}.{owner}.{project}.{seq} — the container used
    // to write `channels` KV directly; routing through the core restores the
    // single-writer rule and turns each post into durable event history.
    for kind in ["update", "reply"] {
        let mut sub = store.subscribe_requests(&format!("req.channel.{kind}.>")).await?;
        let handle = handle.clone();
        tokio::spawn(async move {
            while let Some(req) = sub.next().await {
                let parts: Vec<&str> = req.subject.split('.').collect();
                let (Some(owner), Some(project), Some(seq)) = (
                    parts.get(3).copied(),
                    parts.get(4).copied(),
                    parts.get(5).and_then(|s| s.parse::<u64>().ok()),
                ) else {
                    req.respond(br#"{"error":"malformed subject"}"#.to_vec()).await;
                    continue;
                };
                let post = match kind {
                    "update" => serde_json::from_slice(&req.payload).map(ChannelPost::Update),
                    _ => serde_json::from_slice(&req.payload).map(ChannelPost::Reply),
                };
                let body = match post {
                    Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                    Ok(post) => match handle.channel_post(owner, project, seq, post).await {
                        Ok(()) => r#"{"ok":true}"#.to_string(),
                        Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                    },
                };
                req.respond(body.into_bytes()).await;
            }
        });
    }
    Ok(())
}

/// Map a core error to the §6.5 envelope with an HTTP status hint.
fn error_reply(e: &CoreError) -> Vec<u8> {
    let status = match e {
        CoreError::NotFound(_) => 404,
        CoreError::Validation(_) => 422,
        CoreError::Transition(_) | CoreError::InvalidResolution(_) | CoreError::Conflict(_) => 409,
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
    title: String,
    #[serde(default)]
    description: String,
    /// Upstream job ids this job depends on (must be Done before it starts).
    #[serde(default)]
    deps: Vec<u64>,
    #[serde(default)]
    knowledge_tags: Vec<String>,
    /// Additive per-job evaluators (design-lifecycle.md); validated at release.
    #[serde(default)]
    eval: Vec<types::Evaluator>,
    /// Optional per-job work-task timeout override (duration string, §1.1);
    /// layers over the type's `resources.task_timeout` for Work tasks only.
    /// Parseability validated at release. Absent → the type default applies.
    #[serde(default)]
    timeout: Option<String>,
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
    // Binary path baked into new repos' pre-receive hooks (§5.2) — the path
    // the binary has on the SSH host (`HOOK_BIN`); None → this process's own.
    hook_bin: Option<std::path::PathBuf>,
) -> store::Result<()> {
    // ── req.projects.create — bare repo + hook + starter template ───────
    let mut projects_sub = store.subscribe_requests(&store::subjects::projects_create()).await?;
    let projects_store = store.clone();
    let projects_repos = repos.clone();
    tokio::spawn(async move {
        while let Some(req) = projects_sub.next().await {
            #[derive(serde::Deserialize)]
            struct Body {
                owner: String,
                name: String,
            }
            let body = match serde_json::from_slice::<Body>(&req.payload) {
                Err(e) => bad_request(&e.to_string()),
                Ok(b) => {
                    create_project(&projects_store, &projects_repos, hook_bin.as_deref(), &b.owner, &b.name)
                        .await
                }
            };
            req.respond(body).await;
        }
    });

    // ── req.projects.link — linked-origin project creation (origin fetch +
    // integration HEAD + config seed). Runs through the core actor: it needs
    // the dispatcher's age identity for the deploy key.
    let mut link_sub = store.subscribe_requests(&store::subjects::projects_link()).await?;
    let link_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(req) = link_sub.next().await {
            #[derive(serde::Deserialize)]
            struct Body {
                owner: String,
                name: String,
                origin_url: String,
                #[serde(default)]
                main_branch: Option<String>,
            }
            let body = match serde_json::from_slice::<Body>(&req.payload) {
                Err(e) => bad_request(&e.to_string()),
                Ok(b) => {
                    if let Err(e) = store::keys::validate_subject_component(&b.owner)
                        .and_then(|()| store::keys::validate_subject_component(&b.name))
                    {
                        bad_request(&e.to_string())
                    } else if b.owner == store::keys::RESERVED_OWNER {
                        bad_request(&format!("owner {:?} is reserved", b.owner))
                    } else {
                        match link_handle
                            .link_project(&b.owner, &b.name, &b.origin_url, b.main_branch)
                            .await
                        {
                            Ok(record) => ok_reply(&record),
                            Err(e) => error_reply(&e),
                        }
                    }
                }
            };
            req.respond(body).await;
        }
    });

    // ── req.origin.{release,status,sync}.{owner}.{project} — the origin
    // release surface (PR-based shipping for linked projects).
    for kind in ["release", "status", "sync"] {
        let mut sub = store.subscribe_requests(&format!("req.origin.{kind}.>")).await?;
        let handle = handle.clone();
        tokio::spawn(async move {
            while let Some(req) = sub.next().await {
                let parts: Vec<&str> = req.subject.split('.').collect();
                let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
                else {
                    req.respond(br#"{"error":"malformed subject"}"#.to_vec()).await;
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
    spawn_read_handlers(store, handle, repos).await
}

/// §12.2 project creation, dispatcher-side (the API path; `admin project
/// create` remains the CLI path). Repo before counter — a failed init leaves
/// nothing behind, so the request can simply be retried.
async fn create_project(
    store: &NatsStore,
    repos: &RepoManager,
    hook_bin: Option<&std::path::Path>,
    owner: &str,
    name: &str,
) -> Vec<u8> {
    if let Err(e) = store::keys::validate_subject_component(owner)
        .and_then(|()| store::keys::validate_subject_component(name))
    {
        return bad_request(&e.to_string());
    }
    if owner == store::keys::RESERVED_OWNER {
        return bad_request(&format!("owner {owner:?} is reserved"));
    }
    let counters = match store.raw_bucket(store::buckets::COUNTERS).await {
        Ok(c) => c,
        Err(e) => return error_reply(&e.into()),
    };
    let key = format!("{owner}.{name}");
    match counters.get_json::<u64>(&key).await {
        Ok(Some(_)) => {
            return serde_json::to_vec(&serde_json::json!({
                "error": { "status": 409, "message": format!("project {owner}/{name} already exists") }
            }))
            .unwrap();
        }
        Ok(None) => {}
        Err(e) => return error_reply(&e.into()),
    }
    if let Err(e) = repos.create_project(owner, name, "main").await {
        return error_reply(&CoreError::Vcs(e));
    }
    let bin = hook_bin
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| "/usr/local/bin/chuggernaut".into());
    if let Err(e) = repos
        .install_pre_receive_hook(owner, name, &auth::ssh::pre_receive_hook_body(&bin))
        .await
    {
        return error_reply(&CoreError::Vcs(e));
    }
    if let Err(e) = repos
        .seed_files(owner, name, crate::seed::CODE_TEMPLATE,
            "chuggernaut: seed the Code starter template", false)
        .await
    {
        return error_reply(&CoreError::Vcs(e));
    }
    if let Err(e) = counters.put_json(&key, &0u64).await {
        return error_reply(&e.into());
    }
    ok_reply(&serde_json::json!({ "project": format!("{owner}/{name}") }))
}

/// The read/mutation subject families behind the project-creation handler.
async fn spawn_read_handlers(
    store: &NatsStore,
    handle: CoreHandle,
    repos: Arc<RepoManager>,
) -> store::Result<()> {
    // ── req.jobs.{create,get,list,release,revoke,criteria} ──────────────
    let mut jobs_sub = store.subscribe_requests("req.jobs.>").await?;
    let jobs_store = store.clone();
    let jobs_handle = handle.clone();
    let jobs_repos = repos.clone();
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
                            title: b.title,
                            description: b.description,
                            deps: b.deps,
                            knowledge_tags: b.knowledge_tags,
                            eval: b.eval,
                            timeout: b.timeout,
                            factory: None,
                        };
                        match jobs_handle.create_job(create).await {
                            Ok(job) => ok_reply(&job),
                            Err(e) => error_reply(&e),
                        }
                    }
                },
                ("get", Some(seq)) => fetch_job(&jobs_store, owner, project, seq).await,
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
                // Operator-dispatched advisory triage (§1.2): launches a triage
                // agent over the job state; never changes job state. Reply is
                // the job as-is (unchanged), like revoke's re-fetch.
                ("triage", Some(seq)) => match jobs_handle.triage_job(owner, project, seq).await {
                    Err(e) => error_reply(&e),
                    Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                },
                ("criteria", Some(seq)) => {
                    job_criteria(&jobs_store, &jobs_repos, owner, project, seq).await
                }
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

    // ── req.jobtypes.list.{owner}.{project} — String[] (spec §1.1) ──────
    let mut jobtypes_sub = store.subscribe_requests("req.jobtypes.list.>").await?;
    let jobtypes_repos = repos.clone();
    tokio::spawn(async move {
        while let Some(req) = jobtypes_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = match list_job_types(&jobtypes_repos, owner, project).await {
                Ok(types) => ok_reply(&types),
                Err(e) => error_reply(&e.into()),
            };
            req.respond(body).await;
        }
    });

    // ── req.jobtypes.get.{owner}.{project} — one type in full, for the
    // library UI. Payload: { name }. Returns raw YAML plus the parsed type
    // (defaults merged — the platform's view of what runs), or parse errors.
    let mut jobtype_get_sub = store.subscribe_requests("req.jobtypes.get.>").await?;
    let jobtype_get_repos = repos.clone();
    tokio::spawn(async move {
        while let Some(req) = jobtype_get_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let name = serde_json::from_slice::<serde_json::Value>(&req.payload)
                .ok()
                .and_then(|v| v["name"].as_str().map(String::from));
            let body = match name {
                None => bad_request("payload must carry { name }"),
                Some(name) => get_job_type(&jobtype_get_repos, owner, project, &name).await,
            };
            req.respond(body).await;
        }
    });

    // ── req.vcs.file.{owner}.{project} — one repo file at default HEAD.
    // Payload: { path }. For prompt viewers: the file exactly as an agent
    // would receive it (modulo the appended job brief / rework context).
    let mut file_sub = store.subscribe_requests("req.vcs.file.>").await?;
    let file_repos = repos.clone();
    tokio::spawn(async move {
        while let Some(req) = file_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let path = serde_json::from_slice::<serde_json::Value>(&req.payload)
                .ok()
                .and_then(|v| v["path"].as_str().map(String::from));
            let body = match path {
                None => bad_request("payload must carry { path }"),
                Some(path) => read_repo_file(&file_repos, owner, project, &path).await,
            };
            req.respond(body).await;
        }
    });

    // ── req.vcs.tree.{owner}.{project} — full recursive tree at default
    // HEAD, for the repo browser.
    let mut tree_sub = store.subscribe_requests("req.vcs.tree.>").await?;
    let tree_repos = repos.clone();
    tokio::spawn(async move {
        while let Some(req) = tree_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            req.respond(read_repo_tree(&tree_repos, owner, project).await).await;
        }
    });

    // ── req.tags.list.{owner}.{project} — String[] (tags/*.md stems) ────
    let mut tags_sub = store.subscribe_requests("req.tags.list.>").await?;
    let tags_repos = repos.clone();
    tokio::spawn(async move {
        while let Some(req) = tags_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = match list_tags(&tags_repos, owner, project).await {
                Ok(tags) => ok_reply(&tags),
                Err(e) => error_reply(&e.into()),
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

/// Resolved evaluation criteria for one job: the type's evaluators (with
/// project defaults merged, §1.1) plus the job's additive ones
/// (design-lifecycle.md), each annotated with its source. Resolved at the
/// job's pinned `base_ref`, or default-branch HEAD before Ready — the same
/// ref execution will use. Type-load failures degrade to the job's own
/// evaluators plus the error list rather than a hard error, so the UI can
/// still render something for a job whose type YAML is currently broken.
async fn job_criteria(
    store: &NatsStore,
    repos: &RepoManager,
    owner: &str,
    project: &str,
    seq: u64,
) -> Vec<u8> {
    let job = match store.jobs().await {
        Ok(jobs) => match jobs.get(owner, project, seq).await {
            Ok(Some(job)) => job,
            Ok(None) => return NOT_FOUND.to_vec(),
            Err(e) => return error_reply(&e.into()),
        },
        Err(e) => return error_reply(&e.into()),
    };
    let reference = match &job.base_ref {
        Some(r) => r.clone(),
        None => {
            match repos.default_branch(owner, project).await {
                Ok(branch) => match repos.resolve_ref(owner, project, &branch).await {
                    Ok(head) => head,
                    Err(e) => return error_reply(&CoreError::Vcs(e)),
                },
                Err(e) => return error_reply(&CoreError::Vcs(e)),
            }
        }
    };

    let annotate = |evals: &[types::Evaluator], source: &str| -> Vec<serde_json::Value> {
        evals
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .map(|mut v| {
                v["source"] = serde_json::json!(source);
                v
            })
            .collect()
    };
    let mut evaluators = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut wrap_up = None;
    match crate::release::load_job_type(repos, owner, project, &reference, &job.r#type, Some(seq))
        .await
    {
        Ok(jt) => {
            wrap_up = Some(format!("{:?}", jt.wrap_up.r#type).to_lowercase());
            evaluators.extend(annotate(&jt.eval, "type"));
            if let Err(errs) = crate::release::with_job_evaluators(jt, &job) {
                errors.extend(errs.into_iter().map(|e| format!("{}: {}", e.field, e.message)));
            }
        }
        Err(errs) => {
            errors.extend(errs.into_iter().map(|e| format!("{}: {}", e.field, e.message)));
        }
    }
    evaluators.extend(annotate(&job.eval, "job"));
    ok_reply(&serde_json::json!({
        "ref": reference,
        "wrap_up": wrap_up,
        "evaluators": evaluators,
        "errors": errors,
    }))
}

async fn fetch_job(store: &NatsStore, owner: &str, project: &str, seq: u64) -> Vec<u8> {
    let jobs = match store.jobs().await {
        Ok(j) => j,
        Err(e) => return error_reply(&e.into()),
    };
    let job = match jobs.get(owner, project, seq).await {
        Ok(Some(job)) => job,
        Ok(None) => return NOT_FOUND.to_vec(),
        Err(e) => return error_reply(&e.into()),
    };
    // Task-log read is best-effort: the job still serializes if it fails.
    let tasks = match store.tasks().await {
        Ok(t) => t.list_for_job(owner, project, seq).await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    job_reply_with_awaiting(&job, &tasks)
}

/// Serialize a job with the derived `awaiting_human` field (fix #3): the first
/// Pending Human task in the log, if any, with its kind inferred from the job
/// state. Makes "is a human being asked to do something?" answerable from the
/// job payload — a Pending human task can sit in Work (human work), Evaluation
/// (human evaluator), or an escalation state (Escalated/Stalled). Derived on
/// read, never stored, like the retry/rework counts (§1.1).
fn job_reply_with_awaiting(job: &types::Job, tasks: &[types::Task]) -> Vec<u8> {
    use types::JobState;
    let awaiting = tasks
        .iter()
        .find(|t| {
            t.state == types::TaskState::Pending && matches!(t.kind, types::TaskKind::Human { .. })
        })
        .map(|t| {
            let kind = match job.state {
                JobState::Work => "work",
                JobState::Evaluation => "eval",
                _ => "escalation", // Escalated | Stalled
            };
            serde_json::json!({ "task_id": t.id, "kind": kind })
        });
    let mut value = serde_json::to_value(job).unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(map) = &mut value {
        map.insert("awaiting_human".into(), awaiting.unwrap_or(serde_json::Value::Null));
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| br#"{"error":{"status":500}}"#.to_vec())
}

/// One job type in full for the library UI: raw YAML as authored, plus the
/// parsed type with `jobs/_defaults.yaml` merged (the platform's view of what
/// actually runs). Parse/field-rule failures come back as `errors` alongside
/// the raw YAML — a broken type should still be inspectable in the library.
async fn get_job_type(repos: &RepoManager, owner: &str, project: &str, name: &str) -> Vec<u8> {
    // Same shape rules as list_job_types: top-level stems only.
    if name.is_empty() || name.contains('/') || name.starts_with('_') {
        return bad_request("invalid job type name");
    }
    let branch = match repos.default_branch(owner, project).await {
        Ok(b) => b,
        Err(e) => return error_reply(&CoreError::Vcs(e)),
    };
    let head = match repos.resolve_ref(owner, project, &branch).await {
        Ok(h) => h,
        Err(e) => return error_reply(&CoreError::Vcs(e)),
    };
    let yaml = match repos.read_file_at(owner, project, &head, &format!("jobs/{name}.yaml")).await
    {
        Ok(Some(y)) => y,
        Ok(None) => return NOT_FOUND.to_vec(),
        Err(e) => return error_reply(&CoreError::Vcs(e)),
    };
    let (job_type, errors) =
        match crate::release::load_job_type(repos, owner, project, &head, name, None).await {
            Ok(jt) => (serde_json::to_value(&jt).ok(), Vec::new()),
            Err(errs) => (
                None,
                errs.into_iter().map(|e| format!("{}: {}", e.field, e.message)).collect(),
            ),
        };
    ok_reply(&serde_json::json!({
        "name": name,
        "ref": head,
        "yaml": yaml,
        "job_type": job_type,
        "errors": errors,
    }))
}

/// Enumerate top-level `jobs/{type}.yaml` at default-branch HEAD, minus the
/// `_defaults.yaml` overlay and other `_`-prefixed helpers, with the display
/// metadata the create-form type picker shows. A file that fails to parse
/// still lists (stem only) — a broken type stays visible, not invisible.
async fn list_job_types(
    repos: &RepoManager,
    owner: &str,
    project: &str,
) -> vcs::Result<Vec<serde_json::Value>> {
    let branch = repos.default_branch(owner, project).await?;
    let head = repos.resolve_ref(owner, project, &branch).await?;
    let mut stems: Vec<String> = repos
        .tree(owner, project, &branch)
        .await?
        .into_iter()
        .filter(|e| e.r#type == "blob")
        .filter_map(|e| {
            let name = e.path.strip_prefix("jobs/")?.strip_suffix(".yaml")?;
            // top-level jobs/*.yaml only; skip _defaults.yaml and helpers
            (!name.is_empty() && !name.contains('/') && !name.starts_with('_'))
                .then(|| name.to_string())
        })
        .collect();
    stems.sort();

    let mut out = Vec::with_capacity(stems.len());
    for stem in stems {
        let parsed = repos
            .read_file_at(owner, project, &head, &format!("jobs/{stem}.yaml"))
            .await?
            .and_then(|yaml| types::JobType::parse(&yaml).ok());
        out.push(serde_json::json!({
            "name": stem,
            "display_name": parsed
                .as_ref()
                .and_then(|jt| jt.display_name.clone())
                .unwrap_or_else(|| stem.clone()),
            "description": parsed.and_then(|jt| jt.description).unwrap_or_default(),
        }));
    }
    Ok(out)
}

/// Full recursive tree at default-branch HEAD, for the repo browser.
async fn read_repo_tree(repos: &RepoManager, owner: &str, project: &str) -> Vec<u8> {
    let branch = match repos.default_branch(owner, project).await {
        Ok(b) => b,
        Err(e) => return error_reply(&CoreError::Vcs(e)),
    };
    let head = match repos.resolve_ref(owner, project, &branch).await {
        Ok(h) => h,
        Err(e) => return error_reply(&CoreError::Vcs(e)),
    };
    match repos.tree(owner, project, &head).await {
        Ok(entries) => ok_reply(&serde_json::json!({
            "branch": branch,
            "ref": head,
            "entries": entries,
        })),
        Err(e) => error_reply(&CoreError::Vcs(e)),
    }
}

/// One repo file at default-branch HEAD, for the UI's prompt viewer.
async fn read_repo_file(repos: &RepoManager, owner: &str, project: &str, path: &str) -> Vec<u8> {
    let head = match repos.default_branch(owner, project).await {
        Ok(branch) => match repos.resolve_ref(owner, project, &branch).await {
            Ok(h) => h,
            Err(e) => return error_reply(&CoreError::Vcs(e)),
        },
        Err(e) => return error_reply(&CoreError::Vcs(e)),
    };
    match repos.read_file_at(owner, project, &head, path).await {
        Ok(Some(content)) => ok_reply(&serde_json::json!({
            "path": path,
            "ref": head,
            "content": content,
        })),
        Ok(None) => NOT_FOUND.to_vec(),
        Err(e) => error_reply(&CoreError::Vcs(e)),
    }
}

/// Enumerate available knowledge tags: top-level `tags/{tag}.md` stems at
/// default-branch HEAD. Tags are repo-versioned — a tag's meaning lives in
/// its markdown file, next to the code it describes.
async fn list_tags(repos: &RepoManager, owner: &str, project: &str) -> vcs::Result<Vec<String>> {
    let branch = repos.default_branch(owner, project).await?;
    let mut tags: Vec<String> = repos
        .tree(owner, project, &branch)
        .await?
        .into_iter()
        .filter(|e| e.r#type == "blob")
        .filter_map(|e| {
            let name = e.path.strip_prefix("tags/")?.strip_suffix(".md")?;
            (!name.is_empty() && !name.contains('/')).then(|| name.to_string())
        })
        .collect();
    tags.sort();
    Ok(tags)
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

#[cfg(test)]
mod tests {
    use super::job_reply_with_awaiting;
    use chrono::Utc;
    use types::{Job, JobState, Task, TaskKind, TaskPhase, TaskState};

    fn job(state: JobState) -> Job {
        Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "t".into(),
            title: String::new(),
            description: String::new(),
            deps: vec![],
            state,
            branch: "job/1".into(),
            base_ref: None,
            knowledge_tags: vec![],
            eval: vec![],
            timeout: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: None,
        }
    }

    fn human_task(id: u64, phase: TaskPhase, state: TaskState) -> Task {
        Task {
            id,
            job_seq: 1,
            project: "acme/api".into(),
            phase,
            cycle: 1,
            kind: TaskKind::Human { prompt: "do it".into() },
            state,
            attempt: 1,
            evaluator: None,
            container_id: None,
            session_id: None,
            result: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    fn awaiting(job: &Job, tasks: &[Task]) -> serde_json::Value {
        let bytes = job_reply_with_awaiting(job, tasks);
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["awaiting_human"].clone()
    }

    #[test]
    fn awaiting_human_kind_follows_state() {
        // Post-work escalation: kind escalation, carrying the task id.
        let v = awaiting(&job(JobState::Escalated), &[human_task(3, TaskPhase::Work, TaskState::Pending)]);
        assert_eq!(v["task_id"], 3);
        assert_eq!(v["kind"], "escalation");
        // Pre-work escalation (Stalled) is escalation too.
        let v = awaiting(&job(JobState::Stalled), &[human_task(3, TaskPhase::Work, TaskState::Pending)]);
        assert_eq!(v["kind"], "escalation");
        // Human work task in the Work phase.
        let v = awaiting(&job(JobState::Work), &[human_task(1, TaskPhase::Work, TaskState::Pending)]);
        assert_eq!(v["kind"], "work");
        // Human evaluator task in the Evaluation phase.
        let v = awaiting(&job(JobState::Evaluation), &[human_task(2, TaskPhase::Evaluation, TaskState::Pending)]);
        assert_eq!(v["kind"], "eval");
    }

    #[test]
    fn awaiting_human_null_without_pending_human_task() {
        // A resolved (Done) human task does not count.
        let v = awaiting(&job(JobState::Evaluation), &[human_task(1, TaskPhase::Evaluation, TaskState::Done)]);
        assert!(v.is_null());
        // No tasks at all.
        let v = awaiting(&job(JobState::Work), &[]);
        assert!(v.is_null());
    }
}

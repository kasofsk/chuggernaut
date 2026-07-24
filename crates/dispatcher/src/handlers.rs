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
//!
//! - **Accepts:** `req.*` NATS requests — container-facing (work/eval submit)
//!   and api-facing (jobs, graph, tasks, vcs.diff, …).
//! - **Emits:** `CoreHandle` calls and reply envelopes (resource JSON on
//!   success; `{"error": {...}}` on failure).
//! - **Guarantees:** the idempotent, bounded-retry contract (§4.2); mutates no
//!   state outside the core call.
//! - **Spec:** §6.1, §6.5.

use crate::core::{
    ChannelPost, CoreError, CoreHandle, CreateJobRequest, EvalSubmission, UpdateJobRequest,
    WorkSubmission,
};
use container::ContainerBackend;
use std::sync::Arc;
use store::NatsStore;
use types::{TaskResolution, TaskState};
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
                req.respond(br#"{"error":"malformed subject"}"#.to_vec())
                    .await;
                continue;
            };
            let submission: WorkSubmission =
                serde_json::from_slice(&req.payload).unwrap_or_default();
            // Reject an oversized cover at ingest, before the record is stored
            // (job #143) — the text summary still lands only via a resubmission.
            let body = if let Some(err) = agent_cover_rejection(&submission.cover_html) {
                format!(r#"{{"error":{}}}"#, serde_json::json!(err))
            } else {
                match work_handle
                    .submit_result(owner, project, seq, submission)
                    .await
                {
                    Ok(()) => r#"{"ok":true}"#.to_string(),
                    Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                }
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
                req.respond(br#"{"error":"malformed subject"}"#.to_vec())
                    .await;
                continue;
            };
            // §4.2: payload must include pass — malformed submissions are
            // rejected, not defaulted.
            let body = match serde_json::from_slice::<EvalSubmission>(&req.payload) {
                Err(e) => format!(r#"{{"error":{}}}"#, serde_json::json!(e.to_string())),
                Ok(submission) if agent_cover_rejection(&submission.cover_html).is_some() => {
                    let err = agent_cover_rejection(&submission.cover_html).unwrap();
                    format!(r#"{{"error":{}}}"#, serde_json::json!(err))
                }
                Ok(submission) => {
                    match handle
                        .submit_eval(owner, project, seq, task_id, submission)
                        .await
                    {
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
        let mut sub = store
            .subscribe_requests(&format!("req.channel.{kind}.>"))
            .await?;
        let handle = handle.clone();
        tokio::spawn(async move {
            while let Some(req) = sub.next().await {
                let parts: Vec<&str> = req.subject.split('.').collect();
                let (Some(owner), Some(project), Some(seq)) = (
                    parts.get(3).copied(),
                    parts.get(4).copied(),
                    parts.get(5).and_then(|s| s.parse::<u64>().ok()),
                ) else {
                    req.respond(br#"{"error":"malformed subject"}"#.to_vec())
                        .await;
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

/// Subscribe the worker announce/heartbeat subject (spec §3.1 dynamic
/// registration) and forward each announce into the core actor, which merges it
/// into the live fleet. A plain (non-JetStream) subscription — heartbeats are
/// transient, and losing the stream is what deregisters a node. Harmless on a
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
                    if let Err(e) = handle.announce_worker(a.node, a.slots, a.version).await {
                        tracing::warn!("worker announce forward failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("malformed worker announce: {e}"),
            }
        }
    });
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

/// 422 for a well-formed body that fails a semantic bound (e.g. an oversized
/// `cover_html`) — distinct from the 400 a malformed/unparseable body gets.
fn unprocessable(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "error": { "status": 422, "message": message }
    }))
    .unwrap()
}

/// Byte cap on a job's optional `cover_html` (§1.1). The one job field with a
/// size bound today: it can carry a whole rendered page, so a runaway value
/// would bloat every job-list reply. ~256 KiB is generous for a self-contained
/// styled page yet keeps records sane. Over → 422.
const COVER_HTML_MAX_BYTES: usize = 256 * 1024;

/// True when a supplied `cover_html` exceeds [`COVER_HTML_MAX_BYTES`]. None
/// (no cover) is always fine.
fn cover_too_large(cover_html: &Option<String>) -> bool {
    cover_html
        .as_ref()
        .is_some_and(|h| h.len() > COVER_HTML_MAX_BYTES)
}

/// Byte cap on an agent-authored `cover_html` attached to a work/eval
/// submission (job #143). Tighter than a job brief's cover: an agent cover is a
/// compact "what I did" visual, not a whole authored page, and it rides on
/// every task record. Over → the submission is rejected (never truncated) with
/// an actionable error the agent can react to.
const AGENT_COVER_HTML_MAX_BYTES: usize = 64 * 1024;

/// `Some(error)` when a submitted `cover_html` is over
/// [`AGENT_COVER_HTML_MAX_BYTES`]; `None` when absent or within bound. The text
/// summary is canonical, so an oversized cover is rejected rather than dropped
/// silently — the agent learns to omit or shrink it.
fn agent_cover_rejection(cover_html: &Option<String>) -> Option<String> {
    cover_html
        .as_ref()
        .filter(|h| h.len() > AGENT_COVER_HTML_MAX_BYTES)
        .map(|_| {
            format!(
                "cover_html exceeds the {AGENT_COVER_HTML_MAX_BYTES}-byte limit \
                 (cover pages are presentational — omit it or shrink it; the text \
                 summary is what matters)"
            )
        })
}

fn service_unavailable(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "error": { "status": 503, "message": message }
    }))
    .unwrap()
}

fn bad_gateway(message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "error": { "status": 502, "message": message }
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
    /// Optional rich cover page for the UI (§1.1, §4.3). Presentational only —
    /// never enters an agent prompt. Size-capped ([`COVER_HTML_MAX_BYTES`], 422).
    #[serde(default)]
    cover_html: Option<String>,
    /// Upstream job ids this job depends on (must be Done before it starts).
    #[serde(default)]
    deps: Vec<u64>,
    /// Member job seqs to absorb into a batch (spec §2.1 batches). Non-empty →
    /// this request creates a batch instead of an ordinary job.
    #[serde(default)]
    members: Vec<u64>,
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
    /// Optional per-job Work agent model override (§12.4); wins over the job
    /// type, project, and platform defaults. Absent → the resolution chain applies.
    #[serde(default)]
    model: Option<String>,
    /// Land the job in Draft instead of Frozen (§2.1) so it can be edited
    /// before release. Absent/false preserves today's create-lands-Frozen path.
    #[serde(default)]
    draft: bool,
}

/// Wire body for `req.jobs.update` (spec §6.2 PATCH .../jobs/{seq}): the same
/// shape as create, minus the `draft` flag (an update never changes the state).
#[derive(serde::Deserialize)]
struct UpdateJobBody {
    r#type: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    cover_html: Option<String>,
    #[serde(default)]
    deps: Vec<u64>,
    #[serde(default)]
    knowledge_tags: Vec<String>,
    #[serde(default)]
    eval: Vec<types::Evaluator>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// Wire body for `req.jobs.members` (spec §2.1 draft batches): the seqs to
/// add to / remove from a Draft batch's member list. Both default empty so a
/// caller can add-only or remove-only.
#[derive(serde::Deserialize)]
struct MembersBody {
    #[serde(default)]
    add: Vec<u64>,
    #[serde(default)]
    remove: Vec<u64>,
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
    // SSH CA private key path (§7.3) for user-cert minting; None (no ssh_ca, or
    // `file://` dev repos) → `req.ssh.sign-user-cert` replies 503.
    ssh_ca: Option<std::path::PathBuf>,
    // Container backend for the read-only live-output tail (`req.tasks.output`).
    // Served off the core actor, so a slow node never wedges state transitions.
    backend: Arc<dyn ContainerBackend>,
) -> store::Result<()> {
    // ── req.health — §6.x liveness probe. Round-trips the core actor so a
    // wedged state loop (not just a dead process) reads as unhealthy, and
    // replies {"dispatcher":"ok","version"}. A crash-looping dispatcher has no
    // responder, so the api's bounded probe fails into a 503 rather than being
    // fooled by the SPA fallback answering 200 (the 2026-07-22 masquerade).
    let mut health_sub = store.subscribe_requests(&store::subjects::health()).await?;
    let health_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(req) = health_sub.next().await {
            let body = match health_handle.ping().await {
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

    // ── req.queue.list.{owner}.{project} — §3.5 capacity launch-queue snapshot.
    // Round-trips the core actor so the FIFO order and depth reflect live state,
    // not a stale copy. The api forwards this for the "queued" badge; a wedged
    // core simply fails the bounded request into a graceful UI omission.
    let mut queue_sub = store.subscribe_requests("req.queue.list.>").await?;
    let queue_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(req) = queue_sub.next().await {
            // req.queue.list.{owner}.{project}
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3), parts.get(4)) else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = match queue_handle.queue_snapshot(owner, project).await {
                Ok(snap) => serde_json::to_vec(&snap).unwrap_or_default(),
                Err(e) => service_unavailable(&e.to_string()),
            };
            req.respond(body).await;
        }
    });

    // ── req.projects.create — bare repo + hook + starter template ───────
    let mut projects_sub = store
        .subscribe_requests(&store::subjects::projects_create())
        .await?;
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
                    create_project(
                        &projects_store,
                        &projects_repos,
                        hook_bin.as_deref(),
                        &b.owner,
                        &b.name,
                    )
                    .await
                }
            };
            req.respond(body).await;
        }
    });

    // ── req.projects.link — linked-origin project creation (origin fetch +
    // integration HEAD + config seed). Runs through the core actor: it needs
    // the dispatcher's age identity for the deploy key.
    let mut link_sub = store
        .subscribe_requests(&store::subjects::projects_link())
        .await?;
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
    // ── req.ssh.sign-user-cert — §7.3 user SSH cert minting. The API forwards
    // the authenticated caller's email + submitted public key; we load the
    // user's roles from their record (never a client-supplied map) and sign a
    // 24h cert with the CA key.
    let mut ssh_sub = store
        .subscribe_requests(&store::subjects::ssh_sign_user_cert())
        .await?;
    let ssh_store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = ssh_sub.next().await {
            #[derive(serde::Deserialize)]
            struct Body {
                public_key: String,
                email: String,
            }
            let body = match serde_json::from_slice::<Body>(&req.payload) {
                Err(e) => bad_request(&e.to_string()),
                Ok(b) => {
                    sign_user_cert(&ssh_store, ssh_ca.as_deref(), &b.email, &b.public_key).await
                }
            };
            req.respond(body).await;
        }
    });

    // ── req.members.{set,remove,list}.{owner}.{project} — §7.5 project-role
    // management. The dispatcher is the single writer of `users.*`, so the API
    // (platform-admin-gated) forwards role mutations here rather than writing KV
    // itself. Payload: `{ email, role? }`.
    let mut members_sub = store.subscribe_requests("req.members.>").await?;
    let members_store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = members_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(verb), Some(owner), Some(project)) = (
                parts.get(2).copied(),
                parts.get(3).copied(),
                parts.get(4).copied(),
            ) else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            #[derive(serde::Deserialize)]
            struct Body {
                #[serde(default)]
                email: String,
                #[serde(default)]
                role: Option<String>,
            }
            let body = match serde_json::from_slice::<Body>(&req.payload) {
                Err(e) => bad_request(&e.to_string()),
                Ok(b) => {
                    manage_members(
                        &members_store,
                        verb,
                        owner,
                        project,
                        &b.email,
                        b.role.as_deref(),
                    )
                    .await
                }
            };
            req.respond(body).await;
        }
    });

    spawn_read_handlers(store, handle, repos, backend).await
}

/// §7.5 project-role management, dispatcher-side (single writer of `users.*`).
/// `set` grants/updates a role, `remove` clears one, `list` returns the users
/// holding any role on the project. Role names accept `owner` as an alias for
/// `admin` (see `ProjectRole::parse`). 404 when the target user is missing.
async fn manage_members(
    store: &NatsStore,
    verb: &str,
    owner: &str,
    project: &str,
    email: &str,
    role: Option<&str>,
) -> Vec<u8> {
    if let Err(e) = store::keys::validate_subject_component(owner)
        .and_then(|()| store::keys::validate_subject_component(project))
    {
        return bad_request(&e.to_string());
    }
    let slug = format!("{owner}/{project}");
    let users = match store.raw_bucket(store::buckets::USERS).await {
        Ok(b) => b,
        Err(e) => return error_reply(&e.into()),
    };

    // list: no target email — enumerate members holding a role on this project.
    if verb == "list" {
        let keys = match users.keys_with_prefix("").await {
            Ok(k) => k,
            Err(e) => return error_reply(&e.into()),
        };
        let mut members = Vec::new();
        for key in keys {
            match users.get_json::<types::User>(&key).await {
                Ok(Some(user)) => {
                    if let Some(role) = user.project_roles.get(&slug) {
                        members.push(serde_json::json!({ "email": user.email, "role": role }));
                    }
                }
                Ok(None) => {}
                Err(e) => return error_reply(&e.into()),
            }
        }
        members.sort_by(|a, b| a["email"].as_str().cmp(&b["email"].as_str()));
        return ok_reply(&serde_json::json!({ "members": members }));
    }

    if email.is_empty() {
        return bad_request("payload must carry { email }");
    }
    let key = store::keys::user_key(email);
    let mut user = match users.get_json::<types::User>(&key).await {
        Ok(Some(u)) => u,
        Ok(None) => return NOT_FOUND.to_vec(),
        Err(e) => return error_reply(&e.into()),
    };
    match verb {
        "set" => {
            let Some(role) = role else {
                return bad_request("payload must carry { role }");
            };
            let Some(role) = types::ProjectRole::parse(role) else {
                return bad_request(&format!(
                    "invalid role {role:?} (expected owner|member|viewer)"
                ));
            };
            user.project_roles.insert(slug.clone(), role);
            if let Err(e) = users.put_json(&key, &user).await {
                return error_reply(&e.into());
            }
            ok_reply(&serde_json::json!({ "email": user.email, "project": slug, "role": role }))
        }
        "remove" => {
            user.project_roles.remove(&slug);
            if let Err(e) = users.put_json(&key, &user).await {
                return error_reply(&e.into());
            }
            ok_reply(&serde_json::json!({ "email": user.email, "project": slug }))
        }
        _ => bad_request("malformed subject"),
    }
}

/// §7.3 user SSH cert minting. Loads the caller's roles from their user record
/// — the roles map baked into the cert is the user's current grants, never a
/// client-supplied one — and signs a 24h cert with the CA key. 503 when the CA
/// key is not mounted; 404 when the user record is missing.
async fn sign_user_cert(
    store: &NatsStore,
    ssh_ca: Option<&std::path::Path>,
    email: &str,
    public_key: &str,
) -> Vec<u8> {
    let Some(ca_key) = ssh_ca else {
        return service_unavailable("ssh certificate authority not configured");
    };
    let users = match store.raw_bucket(store::buckets::USERS).await {
        Ok(b) => b,
        Err(e) => return error_reply(&e.into()),
    };
    let user: Option<types::User> = match users.get_json(&store::keys::user_key(email)).await {
        Ok(u) => u,
        Err(e) => return error_reply(&e.into()),
    };
    let Some(user) = user else {
        return NOT_FOUND.to_vec();
    };
    let ca = auth::ssh::SshCa::new(ca_key);
    match ca
        .sign_user_cert(
            public_key,
            &user.email,
            &user.project_roles,
            user.platform_admin,
            chrono::Duration::hours(24),
        )
        .await
    {
        Ok(certificate) => ok_reply(&serde_json::json!({ "certificate": certificate })),
        // A bad key that slipped past the API's structural check, or a CA
        // failure — 500; the API validated parseability, so this is unexpected.
        Err(e) => error_reply(&CoreError::Config(e.to_string())),
    }
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
        .seed_files(
            owner,
            name,
            crate::seed::CODE_TEMPLATE,
            "chuggernaut: seed the Code starter template",
            false,
        )
        .await
    {
        return error_reply(&CoreError::Vcs(e));
    }
    if let Err(e) = counters.put_json(&key, &0u64).await {
        return error_reply(&e.into());
    }
    ok_reply(&serde_json::json!({ "project": format!("{owner}/{name}") }))
}

/// `req.tasks.{list.pending,list,resolve,output}` (spec §6.1). `output` is the
/// live-container tail (§4.2); it needs the container backend, and it must run
/// **off the core actor** so a slow/wedged node never stalls state transitions
/// — so it is served here (store + backend reads only), spawned per request so
/// one hung tail can't block the list/resolve legs either. A single
/// subscription owns the whole `req.tasks.>` family; a second overlapping
/// subscription would double-reply under core NATS.
pub async fn spawn_tasks_handler(
    store: &NatsStore,
    handle: CoreHandle,
    backend: Arc<dyn ContainerBackend>,
) -> store::Result<()> {
    let mut tasks_sub = store.subscribe_requests("req.tasks.>").await?;
    let tasks_store = store.clone();
    let tasks_handle = handle;
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
                                        owner,
                                        project,
                                        job_seq,
                                        task_id,
                                        b.resolution,
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
    Ok(())
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

/// The read/mutation subject families behind the project-creation handler.
async fn spawn_read_handlers(
    store: &NatsStore,
    handle: CoreHandle,
    repos: Arc<RepoManager>,
    backend: Arc<dyn ContainerBackend>,
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
            let (Some(verb), Some(owner), Some(project)) = (
                parts.get(2).copied(),
                parts.get(3).copied(),
                parts.get(4).copied(),
            ) else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let seq = parts.get(5).and_then(|s| s.parse::<u64>().ok());
            let body = match (verb, seq) {
                ("create", None) => match serde_json::from_slice::<CreateJobBody>(&req.payload) {
                    Err(e) => bad_request(&e.to_string()),
                    Ok(b) if cover_too_large(&b.cover_html) => unprocessable(&format!(
                        "cover_html exceeds the {COVER_HTML_MAX_BYTES}-byte limit"
                    )),
                    Ok(b) => {
                        let create = CreateJobRequest {
                            owner: owner.to_string(),
                            project: project.to_string(),
                            r#type: b.r#type,
                            title: b.title,
                            description: b.description,
                            cover_html: b.cover_html,
                            deps: b.deps,
                            members: b.members,
                            knowledge_tags: b.knowledge_tags,
                            eval: b.eval,
                            timeout: b.timeout,
                            model: b.model,
                            factory: None,
                            draft: b.draft,
                        };
                        match jobs_handle.create_job(create).await {
                            Ok(job) => ok_reply(&job),
                            Err(e) => error_reply(&e),
                        }
                    }
                },
                // §2.1 draft edit: full-field replace of a Draft job (409 in
                // any other state). Same body shape as create.
                ("update", Some(seq)) => {
                    match serde_json::from_slice::<UpdateJobBody>(&req.payload) {
                        Err(e) => bad_request(&e.to_string()),
                        Ok(b) if cover_too_large(&b.cover_html) => unprocessable(&format!(
                            "cover_html exceeds the {COVER_HTML_MAX_BYTES}-byte limit"
                        )),
                        Ok(b) => {
                            let update = UpdateJobRequest {
                                owner: owner.to_string(),
                                project: project.to_string(),
                                seq,
                                r#type: b.r#type,
                                title: b.title,
                                description: b.description,
                                cover_html: b.cover_html,
                                deps: b.deps,
                                knowledge_tags: b.knowledge_tags,
                                eval: b.eval,
                                timeout: b.timeout,
                                model: b.model,
                            };
                            match jobs_handle.update_job(update).await {
                                Err(e) => error_reply(&e),
                                Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                            }
                        }
                    }
                }
                // §2.1 Frozen → Draft: reopen a never-released job for editing.
                ("draft", Some(seq)) => match jobs_handle.draft_job(owner, project, seq).await {
                    Err(e) => error_reply(&e),
                    Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                },
                // #166 Draft → Frozen: finalize the edited definition (validate
                // like release, park re-batchable). 409 in any non-Draft state.
                ("finalize", Some(seq)) => {
                    match jobs_handle.finalize_job(owner, project, seq).await {
                        Err(e) => error_reply(&e),
                        Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                    }
                }
                ("get", Some(seq)) => fetch_job(&jobs_store, owner, project, seq).await,
                ("list", None) => match jobs_store.jobs().await {
                    Ok(jobs) => match jobs.list(owner, project).await {
                        Ok(list) => {
                            let channels =
                                latest_channel_updates(&jobs_store, owner, project).await;
                            let summaries: Vec<types::JobSummary<'_>> = list
                                .iter()
                                .map(|job| {
                                    types::JobSummary::from(job).with_channel(channels.get(&job.id))
                                })
                                .collect();
                            ok_reply(&summaries)
                        }
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
                // §2.1 draft batches: add/remove a Draft batch's members while
                // composing it (409 in any other state). Reply is the updated job.
                ("members", Some(seq)) => match serde_json::from_slice::<MembersBody>(&req.payload)
                {
                    Err(e) => bad_request(&e.to_string()),
                    Ok(b) => match jobs_handle
                        .edit_members(owner, project, seq, b.add, b.remove)
                        .await
                    {
                        Err(e) => error_reply(&e),
                        Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                    },
                },
                // §1.2 claims: park the next work attempt for a human /
                // clear a pending claim. Reply is the updated job.
                ("claim", Some(seq)) => match jobs_handle.claim_job(owner, project, seq).await {
                    Err(e) => error_reply(&e),
                    Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                },
                ("unclaim", Some(seq)) => {
                    match jobs_handle.unclaim_job(owner, project, seq).await {
                        Err(e) => error_reply(&e),
                        Ok(_) => fetch_job(&jobs_store, owner, project, seq).await,
                    }
                }
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

    spawn_tasks_handler(store, handle.clone(), backend).await?;

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
            req.respond(read_repo_tree(&tree_repos, owner, project).await)
                .await;
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
    let diff_repos = repos.clone();
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
                    Ok(Some(job)) => match diff_repos.diff_for_job(&job).await {
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
        None => match repos.default_branch(owner, project).await {
            Ok(branch) => match repos.resolve_ref(owner, project, &branch).await {
                Ok(head) => head,
                Err(e) => return error_reply(&CoreError::Vcs(e)),
            },
            Err(e) => return error_reply(&CoreError::Vcs(e)),
        },
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
                errors.extend(
                    errs.into_iter()
                        .map(|e| format!("{}: {}", e.field, e.message)),
                );
            }
        }
        Err(errs) => {
            errors.extend(
                errs.into_iter()
                    .map(|e| format!("{}: {}", e.field, e.message)),
            );
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

/// Each job's latest channel post, keyed by job seq (spec §4.2).
///
/// The `channels` bucket already holds exactly this — one entry per job,
/// overwritten in place — so the whole project costs one prefix scan. Attaching
/// it to the jobs list is what lets the operator UI show a live job's progress
/// line without opening an SSE replay of the project's entire event history.
///
/// Best-effort by design: the progress line is a nicety, and an unreadable
/// channels bucket must never fail the jobs list. A miss just drops the line.
async fn latest_channel_updates(
    store: &NatsStore,
    owner: &str,
    project: &str,
) -> std::collections::HashMap<u64, types::ChannelUpdate> {
    let Ok(bucket) = store.raw_bucket(store::buckets::CHANNELS).await else {
        return std::collections::HashMap::new();
    };
    // Mirrors `keys::channel_key`, whose per-job key is `{owner}.{project}.jobs.{seq}`.
    let prefix = format!("{owner}.{project}.jobs.");
    let entries: Vec<(String, types::ChannelEntry)> = match bucket.list_prefix_keyed(&prefix).await
    {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(%owner, %project, error = %e, "channel scan failed; jobs list drops progress lines");
            return std::collections::HashMap::new();
        }
    };
    entries
        .into_iter()
        .filter_map(|(key, entry)| {
            let seq: u64 = key.strip_prefix(&prefix)?.parse().ok()?;
            Some((seq, entry.update?))
        })
        .collect()
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
        Ok(t) => t
            .list_for_job(owner, project, seq)
            .await
            .unwrap_or_default(),
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
    // A terminal job (Done/Revoked) asks nothing of a human, even if a stale
    // Pending task record lingers in its log (a pre-fix zombie). Guard here so
    // the derived field agrees with list_pending's terminal-job filter.
    let awaiting = (!job.state.is_terminal())
        .then(|| {
            tasks.iter().find(|t| {
                t.state == types::TaskState::Pending
                    && (matches!(t.kind, types::TaskKind::Human { .. })
                        || t.performed_by == Some(types::Performer::Human))
            })
        })
        .flatten()
        .map(|t| {
            let kind = match job.state {
                JobState::Work => "work",
                JobState::Evaluation => "eval",
                _ => "escalation", // Escalated | Stalled
            };
            // `claimed` marks a parked claimed attempt: a human is actively
            // working it (§1.2 claims), vs. passively awaited human input.
            serde_json::json!({
                "task_id": t.id,
                "kind": kind,
                "claimed": t.performed_by == Some(types::Performer::Human),
            })
        });
    let mut value = serde_json::to_value(job).unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(map) = &mut value {
        map.insert(
            "awaiting_human".into(),
            awaiting.unwrap_or(serde_json::Value::Null),
        );
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
    let yaml = match repos
        .read_file_at(owner, project, &head, &format!("jobs/{name}.yaml"))
        .await
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
                errs.into_iter()
                    .map(|e| format!("{}: {}", e.field, e.message))
                    .collect(),
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

#[cfg(test)]
mod tests {
    use super::{
        AGENT_COVER_HTML_MAX_BYTES, COVER_HTML_MAX_BYTES, CreateJobBody, UpdateJobBody,
        agent_cover_rejection, cover_too_large, job_reply_with_awaiting, unprocessable,
    };
    use chrono::Utc;
    use types::{Job, JobState, Task, TaskKind, TaskPhase, TaskState};

    fn status(body: &[u8]) -> i64 {
        serde_json::from_slice::<serde_json::Value>(body).unwrap()["error"]["status"]
            .as_i64()
            .unwrap()
    }

    /// Agent-authored cover cap (job #143): absent is always fine (full back-
    /// compat), at the cap is fine, one byte over is rejected with an actionable
    /// error the agent can react to — never truncated.
    #[test]
    fn agent_cover_over_limit_rejected_with_actionable_error() {
        assert!(agent_cover_rejection(&None).is_none());
        assert!(agent_cover_rejection(&Some("x".repeat(AGENT_COVER_HTML_MAX_BYTES))).is_none());
        let err = agent_cover_rejection(&Some("x".repeat(AGENT_COVER_HTML_MAX_BYTES + 1)))
            .expect("over-limit cover rejected");
        // The message names the field and the fact it's optional/presentational.
        assert!(err.contains("cover_html"), "{err}");
        assert!(err.contains("summary"), "{err}");
        // The agent cap is tighter than a job brief's cover cap.
        const { assert!(AGENT_COVER_HTML_MAX_BYTES < COVER_HTML_MAX_BYTES) };
    }

    /// The §1.1 cover cap: a body at/under the limit is accepted, one over it is
    /// rejected with a 422 (not a 400 — the body parsed fine). Exercised on both
    /// create and the Draft PATCH bodies, since both carry `cover_html`.
    #[test]
    fn cover_html_size_cap_rejects_over_limit_with_422() {
        // Boundary: exactly at the cap is fine, one byte over is not.
        assert!(!cover_too_large(&None));
        assert!(!cover_too_large(&Some("x".repeat(COVER_HTML_MAX_BYTES))));
        assert!(cover_too_large(&Some("x".repeat(COVER_HTML_MAX_BYTES + 1))));
        // The rejection is a 422.
        assert_eq!(status(&unprocessable("too big")), 422);

        // An oversized cover still deserializes (well-formed body) — the cap is a
        // semantic check on top, not a parse error.
        let over = format!(
            r#"{{"type":"code","cover_html":{}}}"#,
            serde_json::to_string(&"x".repeat(COVER_HTML_MAX_BYTES + 1)).unwrap()
        );
        let create: CreateJobBody = serde_json::from_str(&over).unwrap();
        assert!(cover_too_large(&create.cover_html));
        let update: UpdateJobBody = serde_json::from_str(&over).unwrap();
        assert!(cover_too_large(&update.cover_html));

        // A modest cover is accepted and round-trips through the wire body.
        let ok = r#"{"type":"code","cover_html":"<h1>hi</h1>"}"#;
        let create: CreateJobBody = serde_json::from_str(ok).unwrap();
        assert_eq!(create.cover_html.as_deref(), Some("<h1>hi</h1>"));
        assert!(!cover_too_large(&create.cover_html));

        // Absent cover parses to None (back-compat with pre-cover clients).
        let bare: CreateJobBody = serde_json::from_str(r#"{"type":"code"}"#).unwrap();
        assert!(bare.cover_html.is_none());
    }

    fn job(state: JobState) -> Job {
        Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "t".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state,
            branch: "job/1".into(),
            base_ref: None,
            knowledge_tags: vec![],
            eval: vec![],
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
        }
    }

    fn human_task(id: u64, phase: TaskPhase, state: TaskState) -> Task {
        Task {
            id,
            job_seq: 1,
            project: "acme/api".into(),
            phase,
            cycle: 1,
            kind: TaskKind::Human {
                prompt: "do it".into(),
            },
            state,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            session_id: None,
            pending_reason: None,
            queued_at: None,
            reviewed_tip: None,
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
        let v = awaiting(
            &job(JobState::Escalated),
            &[human_task(3, TaskPhase::Work, TaskState::Pending)],
        );
        assert_eq!(v["task_id"], 3);
        assert_eq!(v["kind"], "escalation");
        // Pre-work escalation (Stalled) is escalation too.
        let v = awaiting(
            &job(JobState::Stalled),
            &[human_task(3, TaskPhase::Work, TaskState::Pending)],
        );
        assert_eq!(v["kind"], "escalation");
        // Human work task in the Work phase.
        let v = awaiting(
            &job(JobState::Work),
            &[human_task(1, TaskPhase::Work, TaskState::Pending)],
        );
        assert_eq!(v["kind"], "work");
        // Human evaluator task in the Evaluation phase.
        let v = awaiting(
            &job(JobState::Evaluation),
            &[human_task(2, TaskPhase::Evaluation, TaskState::Pending)],
        );
        assert_eq!(v["kind"], "eval");
    }

    #[test]
    fn awaiting_human_null_without_pending_human_task() {
        // A resolved (Done) human task does not count.
        let v = awaiting(
            &job(JobState::Evaluation),
            &[human_task(1, TaskPhase::Evaluation, TaskState::Done)],
        );
        assert!(v.is_null());
        // No tasks at all.
        let v = awaiting(&job(JobState::Work), &[]);
        assert!(v.is_null());
    }

    #[test]
    fn awaiting_human_null_on_terminal_job() {
        // A terminal job asks nothing of a human even if a stale Pending human
        // task lingers in its log (a pre-fix zombie): the derived field is null.
        for state in [JobState::Revoked, JobState::Done] {
            let v = awaiting(
                &job(state),
                &[human_task(3, TaskPhase::Work, TaskState::Pending)],
            );
            assert!(v.is_null(), "{state:?} should not await a human");
        }
    }
}

//! Project creation, both flavours: a bare platform-owned repo (§12.2) and a
//! linked-origin project (§5.3). The link flow runs through the core actor
//! because it needs the dispatcher's age identity for the deploy key; the plain
//! flow only touches the repo port and the counters bucket, so it is served
//! here.
//!
//! - **Accepts:** `req.projects.create` (`{ owner, name }`),
//!   `req.projects.link` (`{ owner, name, origin_url, main_branch? }`).
//! - **Emits:** `RepoManager::{create_project, install_pre_receive_hook,
//!   seed_files}` plus the project's seq counter; `CoreHandle::link_project`.
//! - **Guarantees:** repo before counter — a failed init leaves nothing behind,
//!   so the request can simply be retried. The reserved owner and any
//!   subject-unsafe component are rejected before anything is written.
//! - **Spec:** §12.2, §5.3.

use super::reply::{bad_request, conflict, error_reply, ok_reply};
use crate::core::{CoreError, CoreHandle};
use std::sync::Arc;
use store::NatsStore;
use vcs::RepoManager;

/// `req.projects.create` — bare repo + pre-receive hook + starter template.
pub(super) async fn spawn_projects_create_handler(
    store: &NatsStore,
    repos: Arc<RepoManager>,
    // Binary path baked into new repos' pre-receive hooks (§5.2) — the path
    // the binary has on the SSH host (`HOOK_BIN`); None → this process's own.
    hook_bin: Option<std::path::PathBuf>,
) -> store::Result<()> {
    let mut projects_sub = store
        .subscribe_requests(&store::subjects::projects_create())
        .await?;
    let projects_store = store.clone();
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
                        &repos,
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
    Ok(())
}

/// `req.projects.link` — linked-origin project creation (origin fetch +
/// integration HEAD + config seed).
pub(super) async fn spawn_projects_link_handler(
    store: &NatsStore,
    handle: CoreHandle,
) -> store::Result<()> {
    let mut link_sub = store
        .subscribe_requests(&store::subjects::projects_link())
        .await?;
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
                        match handle
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
    Ok(())
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
        Ok(Some(_)) => return conflict(&format!("project {owner}/{name} already exists")),
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
            crate::platform_ops::seed::CODE_TEMPLATE,
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

//! The repo-backed reads: the file/tree browser, a job's diff, and the
//! knowledge-tag list. Everything here reads the project repo through the `vcs`
//! port at default-branch HEAD (the diff at the job's own refs), because the
//! things it serves — prompts, tags, config — are repo-versioned and travel
//! with the project, not with the platform.
//!
//! - **Accepts:** `req.vcs.file.{owner}.{project}` (`{ path }`),
//!   `req.vcs.tree.{owner}.{project}`,
//!   `req.vcs.diff.{owner}.{project}.{seq}`,
//!   `req.tags.list.{owner}.{project}`.
//! - **Emits:** `RepoManager::{default_branch, resolve_ref, read_file_at, tree,
//!   diff_for_job}` reads; file/tree/diff/tag JSON or a §6.5 error envelope.
//!   Tags list as `{ name, path }` — the file read is verbatim, so the caller
//!   fetching a tag back needs the path the listing resolved to.
//! - **Guarantees:** read-only, and pinned to one resolved ref per reply — the
//!   same file an agent would receive, not a mix of two HEADs.
//! - **Spec:** §6.1, §5.2.

use super::reply::{NOT_FOUND, bad_request, error_reply, ok_reply};
use crate::core::CoreError;
use std::sync::Arc;
use store::NatsStore;
use vcs::RepoManager;

/// Subscribe the repo-read subjects: `req.vcs.{file,tree,diff}` and
/// `req.tags.list`.
pub(super) async fn spawn_repo_handlers(
    store: &NatsStore,
    repos: Arc<RepoManager>,
) -> store::Result<()> {
    spawn_file_handler(store, repos.clone()).await?;
    spawn_tree_handler(store, repos.clone()).await?;
    spawn_tags_handler(store, repos.clone()).await?;
    spawn_diff_handler(store, repos).await
}

/// `req.vcs.file.{owner}.{project}` — one repo file at default HEAD. Payload:
/// `{ path }`. For prompt viewers: the file exactly as an agent would receive
/// it (modulo the appended job brief / rework context).
async fn spawn_file_handler(store: &NatsStore, repos: Arc<RepoManager>) -> store::Result<()> {
    let mut file_sub = store.subscribe_requests("req.vcs.file.>").await?;
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
                Some(path) => read_repo_file(&repos, owner, project, &path).await,
            };
            req.respond(body).await;
        }
    });
    Ok(())
}

/// `req.vcs.tree.{owner}.{project}` — full recursive tree at default HEAD, for
/// the repo browser.
async fn spawn_tree_handler(store: &NatsStore, repos: Arc<RepoManager>) -> store::Result<()> {
    let mut tree_sub = store.subscribe_requests("req.vcs.tree.>").await?;
    tokio::spawn(async move {
        while let Some(req) = tree_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            req.respond(read_repo_tree(&repos, owner, project).await)
                .await;
        }
    });
    Ok(())
}

/// `req.tags.list.{owner}.{project}` — `{ name, path }[]` for the project's
/// `.chug/tags/*.md`.
async fn spawn_tags_handler(store: &NatsStore, repos: Arc<RepoManager>) -> store::Result<()> {
    let mut tags_sub = store.subscribe_requests("req.tags.list.>").await?;
    tokio::spawn(async move {
        while let Some(req) = tags_sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = match list_tags(&repos, owner, project).await {
                Ok(tags) => ok_reply(&tags),
                Err(e) => error_reply(&e.into()),
            };
            req.respond(body).await;
        }
    });
    Ok(())
}

/// `req.vcs.diff.{owner}.{project}.{seq}` — the job branch's diff against its
/// base, read at the job's own refs.
async fn spawn_diff_handler(store: &NatsStore, repos: Arc<RepoManager>) -> store::Result<()> {
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

/// Enumerate available knowledge tags: top-level `.chug/tags/{tag}.md` at
/// default-branch HEAD, each as `{ name, path }`. Tags are repo-versioned — a
/// tag's meaning lives in its markdown file, next to the code it describes.
///
/// The path is the one the listing resolved to, because `read_repo_file` is the
/// plain file browser and reads verbatim: a caller fetching a tag's contents
/// back must use the path found here, not re-guess the layout.
async fn list_tags(
    repos: &RepoManager,
    owner: &str,
    project: &str,
) -> vcs::Result<Vec<serde_json::Value>> {
    let branch = repos.default_branch(owner, project).await?;
    let tree = repos.tree(owner, project, &branch).await?;
    Ok(crate::project_config::entries(&tree, "tags", ".md")
        .into_iter()
        .map(|entry| serde_json::json!({ "name": entry.stem, "path": entry.path }))
        .collect())
}

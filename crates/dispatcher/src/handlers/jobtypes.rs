//! The job-type library (spec §1.1): the create form's type picker and the
//! library UI's full view of one type. Job types are repo-versioned and
//! project-owned, so both verbs read `jobs/*.yaml` at default-branch HEAD
//! through the `vcs` port — never from KV.
//!
//! A broken type stays visible on purpose: a parse failure comes back as an
//! `errors` list alongside the raw YAML rather than a hard error, because a
//! type you cannot see is a type you cannot fix.
//!
//! - **Accepts:** `req.jobtypes.list.{owner}.{project}`,
//!   `req.jobtypes.get.{owner}.{project}` (`{ name }`).
//! - **Emits:** repo reads plus `release::load_job_type` (which merges
//!   `jobs/_defaults.yaml` — the platform's view of what actually runs).
//! - **Guarantees:** top-level stems only — `_`-prefixed helpers and nested
//!   paths are not job types and never list.
//! - **Spec:** §1.1, §6.1.

use super::reply::{NOT_FOUND, bad_request, error_reply, ok_reply};
use crate::core::CoreError;
use std::sync::Arc;
use store::NatsStore;
use vcs::RepoManager;

/// Subscribe `req.jobtypes.{list,get}.>`.
pub(super) async fn spawn_jobtypes_handlers(
    store: &NatsStore,
    repos: Arc<RepoManager>,
) -> store::Result<()> {
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
                Some(name) => get_job_type(&repos, owner, project, &name).await,
            };
            req.respond(body).await;
        }
    });
    Ok(())
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

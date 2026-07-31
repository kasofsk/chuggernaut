//! The two **derived** reads over `Job.groups` (design #321 slice B): the group
//! roll-up and the `docs/design/` registry.
//!
//! Neither serves anything stored. `req.groups.list` is one pass over the
//! project's job records — a group exists because a job says so, so the
//! enumeration *is* the derivation and an empty group is unrepresentable.
//! `req.designs.list` starts from the other end, listing the documents at
//! default-branch HEAD and joining each to the group its jobs carry, so a
//! design nobody has filed a job against is a row too. Both are read-only and
//! never touch the core actor: a slow repo read must not wedge state
//! transitions, and KV is the truth the actor's in-memory graph is a working
//! copy of (the `req.graph.get` posture).
//!
//! - **Accepts:** `req.groups.list.{owner}.{project}`,
//!   `req.designs.list.{owner}.{project}`.
//! - **Emits:** `types::GroupEntry[]` / `types::DesignEntry[]` JSON, or a §6.5
//!   error envelope.
//! - **Guarantees:** read-only and derived — no aggregate is read from or
//!   written to any bucket; one resolved HEAD per reply, so a listing and the
//!   statuses in it are never a mix of two trees; both repo reads are bounded by
//!   `types::DESIGNS_MAX` and log what they drop. The group read's document join
//!   is best-effort: an unreadable repo costs the status lines, never the
//!   roll-up.
//! - **Spec:** §1.1 (`groups`), §6.1, §6.2.

use super::reply::{bad_request, error_reply, ok_reply};
use crate::core::CoreError;
use std::collections::BTreeMap;
use std::sync::Arc;
use store::NatsStore;
use types::{DesignEntry, GroupEntry, GroupRollup};
use vcs::RepoManager;

/// Subscribe the derived group reads: `req.groups.list` and `req.designs.list`.
pub(super) async fn spawn_groups_handlers(
    store: &NatsStore,
    repos: Arc<RepoManager>,
) -> store::Result<()> {
    spawn_derived_read(store, repos.clone(), "req.groups.list.>", list_groups).await?;
    spawn_derived_read(store, repos, "req.designs.list.>", list_designs).await
}

/// Subscribe one `{subject}.{owner}.{project}` read and answer it with `reply`.
///
/// Both subjects have the same shape and the same ports, so they share the
/// subscription loop rather than each carrying a copy of the subject parse —
/// the two reads differ only in what they derive.
async fn spawn_derived_read<F, Fut>(
    store: &NatsStore,
    repos: Arc<RepoManager>,
    subject: &str,
    reply: F,
) -> store::Result<()>
where
    F: Fn(NatsStore, Arc<RepoManager>, String, String) -> Fut + Send + 'static,
    Fut: Future<Output = Vec<u8>> + Send,
{
    let mut sub = store.subscribe_requests(subject).await?;
    let store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            let parts: Vec<&str> = req.subject.split('.').collect();
            let (Some(owner), Some(project)) = (parts.get(3).copied(), parts.get(4).copied())
            else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let body = reply(
                store.clone(),
                repos.clone(),
                owner.to_string(),
                project.to_string(),
            )
            .await;
            req.respond(body).await;
        }
    });
    Ok(())
}

/// One design document as `req.designs.list` enumerates it: where it lives, the
/// group name its jobs carry, and what its opening lines say.
struct DesignDoc {
    path: String,
    slug: String,
    group_name: String,
    head: types::DesignDocHead,
}

/// What a `design/`-namespaced group name resolves to at default HEAD — the
/// other direction of the same convention, which is all `req.groups.list` needs.
struct GroupDoc {
    path: String,
    status: Option<String>,
}

/// `req.groups.list` — the group roll-ups, plus the design document each
/// `design/`-namespaced name resolves to at default HEAD.
///
/// The document join is deliberately best-effort (design #321 Decision 7, the
/// spec §4.4 posture for a knowledge tag with no file): a name whose document
/// is absent — or a repo that cannot be read at all — still lists, without a
/// `doc_path`/`doc_status`. Only the names an actual group carries are looked
/// up, so a project whose groups name no design touches the repo not at all.
async fn list_groups(
    store: NatsStore,
    repos: Arc<RepoManager>,
    owner: String,
    project: String,
) -> Vec<u8> {
    let jobs = match project_jobs(&store, &owner, &project).await {
        Ok(jobs) => jobs,
        Err(body) => return body,
    };
    let rollups = types::group_rollups(&jobs);
    let docs = match group_docs(&repos, &owner, &project, rollups.keys()).await {
        Ok(docs) => docs,
        Err(e) => {
            tracing::warn!(%owner, %project, error = %e, "design docs unreadable; groups list drops status lines");
            BTreeMap::new()
        }
    };
    let entries: Vec<GroupEntry> = rollups
        .into_values()
        .map(|group| {
            let doc = docs.get(group.name.as_str());
            GroupEntry {
                doc_path: doc.map(|d| d.path.clone()),
                doc_status: doc.and_then(|d| d.status.clone()),
                group,
            }
        })
        .collect();
    ok_reply(&entries)
}

/// `req.designs.list` — the design registry: every document under
/// `docs/design/` at default HEAD, each joined to its group's roll-up (empty
/// for a design nobody has ticketed).
///
/// Unlike the group read, an unreadable repo is a hard error here — the
/// enumeration *is* the repo, so a degraded reply would be an empty registry
/// indistinguishable from a project with no designs.
async fn list_designs(
    store: NatsStore,
    repos: Arc<RepoManager>,
    owner: String,
    project: String,
) -> Vec<u8> {
    let jobs = match project_jobs(&store, &owner, &project).await {
        Ok(jobs) => jobs,
        Err(body) => return body,
    };
    let mut rollups = types::group_rollups(&jobs);
    let docs = match design_docs(&repos, &owner, &project).await {
        Ok(docs) => docs,
        Err(e) => return error_reply(&CoreError::Vcs(e)),
    };
    let entries: Vec<DesignEntry> = docs
        .into_iter()
        .map(|doc| {
            let group = rollups
                .remove(&doc.group_name)
                .unwrap_or_else(|| GroupRollup::empty(doc.group_name));
            DesignEntry::new(doc.path, &doc.slug, doc.head, group)
        })
        .collect();
    ok_reply(&entries)
}

/// The project's job records, or the §6.5 error envelope to reply with. KV is
/// the source both reads derive from — the aggregate exists only for the length
/// of the reply.
async fn project_jobs(
    store: &NatsStore,
    owner: &str,
    project: &str,
) -> Result<Vec<types::Job>, Vec<u8>> {
    match store.jobs().await {
        Ok(jobs) => jobs
            .list(owner, project)
            .await
            .map_err(|e| error_reply(&e.into())),
        Err(e) => Err(error_reply(&e.into())),
    }
}

/// The document each `design/`-namespaced name among `names` resolves to at
/// default-branch HEAD, keyed by group name. A name in another namespace, and a
/// name whose document is not there, are simply absent.
///
/// The join runs **name → path**, through `types::design_doc_path` — the
/// convention stated in the same module that decides what a name may be, which
/// is what keeps the shape-legal `design/../../etc/passwd` from resolving to a
/// path here at all. Running it in this direction is also what lets an
/// ungrouped project — and any project whose groups name no design — pay
/// nothing: with nothing to look up there is no branch to resolve and no tree
/// to walk. `req.designs.list` needs the opposite direction, and enumerates
/// instead.
///
/// Bounded (STYLE.md Tier 2 #3): the distinct group count is bounded only by
/// `GROUPS_COUNT_MAX` × the project's jobs, so at most `types::DESIGNS_MAX`
/// documents are looked up and a drop is logged, never silent.
async fn group_docs<'a>(
    repos: &RepoManager,
    owner: &str,
    project: &str,
    names: impl Iterator<Item = &'a String>,
) -> vcs::Result<BTreeMap<String, GroupDoc>> {
    let mut wanted: Vec<(&str, String)> = names
        .filter_map(|name| Some((name.as_str(), types::design_doc_path(name)?)))
        .collect();
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    if wanted.len() > types::DESIGNS_MAX {
        tracing::warn!(
            %owner, %project, found = wanted.len(), max = types::DESIGNS_MAX,
            "design-namespaced groups over the lookup bound; status lines are truncated"
        );
        wanted.truncate(types::DESIGNS_MAX);
    }

    let branch = repos.default_branch(owner, project).await?;
    let head = repos.resolve_ref(owner, project, &branch).await?;
    let mut docs = BTreeMap::new();
    for (name, path) in wanted {
        let Some(text) = repos.read_file_at(owner, project, &head, &path).await? else {
            continue;
        };
        docs.insert(
            name.to_string(),
            GroupDoc {
                path,
                status: types::design_doc_head(&text).status,
            },
        );
    }
    Ok(docs)
}

/// Every `docs/design/*.md` at default-branch HEAD, in path order, with the head
/// of each document parsed — the registry `req.designs.list` is, and the one
/// direction a name→path lookup cannot serve: a design nobody has ticketed has
/// no group name to look it up by.
///
/// One resolved HEAD serves the listing and every read, so a document's status
/// always belongs to the tree the listing came from. Bounded twice (STYLE.md
/// Tier 2 #3): at most `types::DESIGNS_MAX` documents are read — a drop is
/// logged, never silent — and each read parses only the document's opening
/// lines rather than scanning a 60 KB body.
///
/// `project_config::entries` is deliberately not reused: it resolves the
/// `.chug/` config root and its repo-root fallback, and `docs/design/` has no
/// second location to resolve — a "design doc" at the repo root would be a
/// coincidence, not a layout.
async fn design_docs(
    repos: &RepoManager,
    owner: &str,
    project: &str,
) -> vcs::Result<Vec<DesignDoc>> {
    let branch = repos.default_branch(owner, project).await?;
    let head = repos.resolve_ref(owner, project, &branch).await?;
    let mut paths: Vec<String> = repos
        .tree(owner, project, &head)
        .await?
        .into_iter()
        .filter(|entry| entry.r#type == "blob" && types::design_slug(&entry.path).is_some())
        .map(|entry| entry.path)
        .collect();
    paths.sort();
    if paths.len() > types::DESIGNS_MAX {
        tracing::warn!(
            %owner, %project, found = paths.len(), max = types::DESIGNS_MAX,
            "design docs over the enumeration bound; listing is truncated"
        );
        paths.truncate(types::DESIGNS_MAX);
    }

    let mut docs = Vec::new();
    for path in paths {
        let Some(slug) = types::design_slug(&path).map(str::to_string) else {
            continue;
        };
        let group_name = types::design_group_name(&slug);
        let text = repos
            .read_file_at(owner, project, &head, &path)
            .await?
            .unwrap_or_default();
        docs.push(DesignDoc {
            path,
            slug,
            group_name,
            head: types::design_doc_head(&text),
        });
    }
    debug_assert!(
        docs.len() <= types::DESIGNS_MAX,
        "the enumeration bound holds after the head reads"
    );
    Ok(docs)
}

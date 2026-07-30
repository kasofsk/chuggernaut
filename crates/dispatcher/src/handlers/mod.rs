//! NATS `req.*` subject handlers (spec §6.1), one module per subject family.
//! Each subscription translates a request into a `CoreHandle` call or a store
//! read and replies — the same idempotent, bounded-retry contract the channel
//! MCP server counts on (§4.2).
//!
//! This module is the wiring only: it names the families and hands each one its
//! ports. The three entry points mirror who is calling — containers
//! (`spawn_container_handlers`), worker nodes (`spawn_worker_announce_handler`),
//! and the api crate's HTTP bridge (`spawn_api_handlers`).
//!
//! | family | subjects | module |
//! | --- | --- | --- |
//! | container | `req.{work,eval}.submit`, `req.channel.*` | [`container`] |
//! | worker | `req.worker.announce` | [`worker`] |
//! | fleet | `req.fleet.capacity.set` | [`fleet`] |
//! | status | `req.health`, `req.queue.list` | [`status`] |
//! | projects | `req.projects.{create,link}` | [`projects`] |
//! | origin | `req.origin.{release,status,sync}` | [`origin`] |
//! | access | `req.ssh.sign-user-cert`, `req.members.*` | [`access`] |
//! | jobs | `req.jobs.*` | [`jobs`], [`jobs_reply`] |
//! | graph | `req.graph.get` | [`graph`] |
//! | groups | `req.groups.list`, `req.designs.list` | [`groups`] |
//! | tasks | `req.tasks.*` | [`tasks`] |
//! | job types | `req.jobtypes.{list,get}` | [`jobtypes`] |
//! | repo | `req.vcs.{file,tree,diff}`, `req.tags.list` | [`repo`] |
//!
//! API-facing reply envelope ([`reply`]): success is the resource JSON
//! verbatim; failure is `{"error": {"status": u16, "message": string,
//! "errors": [...]?}}` so the HTTP bridge can map straight to §6.5 responses.
//!
//! - **Accepts:** `req.*` NATS requests — container-facing (work/eval submit)
//!   and api-facing (jobs, graph, tasks, vcs.diff, …).
//! - **Emits:** `CoreHandle` calls and reply envelopes (resource JSON on
//!   success; `{"error": {...}}` on failure).
//! - **Guarantees:** the idempotent, bounded-retry contract (§4.2); mutates no
//!   state outside the core call. One subscription owns each family, so no
//!   subject is ever answered twice.
//! - **Spec:** §6.1, §6.5.

mod access;
mod container;
mod fleet;
mod graph;
mod groups;
mod jobs;
mod jobs_reply;
mod jobtypes;
mod origin;
mod projects;
mod reply;
mod repo;
mod status;
mod tasks;
mod worker;

pub use container::spawn_container_handlers;
pub use tasks::spawn_tasks_handler;
pub use worker::spawn_worker_announce_handler;

use crate::core::CoreHandle;
use std::sync::Arc;
use store::NatsStore;
use vcs::RepoManager;

/// Subscribe the API-facing subject families (spec §6.1): the status probes,
/// project creation, origin, access, jobs, graph, tasks, job types and the repo
/// reads. Reads go straight to the store or repos; mutations go through the
/// core actor. Returns once every subscription is established; the handler
/// tasks run for the life of the NATS connection.
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
    // Path-qualified: `container` names this directory's module here.
    backend: Arc<dyn ::container::ContainerBackend>,
) -> store::Result<()> {
    status::spawn_health_handler(store, handle.clone()).await?;
    fleet::spawn_fleet_capacity_handler(store, handle.clone()).await?;
    status::spawn_queue_handler(store, handle.clone()).await?;
    projects::spawn_projects_create_handler(store, repos.clone(), hook_bin).await?;
    projects::spawn_projects_link_handler(store, handle.clone()).await?;
    origin::spawn_origin_handlers(store, handle.clone()).await?;
    access::spawn_ssh_handler(store, ssh_ca).await?;
    access::spawn_members_handler(store).await?;
    jobs::spawn_jobs_handler(store, handle.clone(), repos.clone()).await?;
    graph::spawn_graph_handler(store).await?;
    groups::spawn_groups_handlers(store, repos.clone()).await?;
    tasks::spawn_tasks_handler(store, handle, backend).await?;
    jobtypes::spawn_jobtypes_handlers(store, repos.clone()).await?;
    repo::spawn_repo_handlers(store, repos).await
}

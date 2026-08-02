//! NATS request-reply subject construction (spec §6.1) and event subjects
//! (spec §6.3). Only the subjects that cross crate boundaries live here —
//! containers publish `req.work.submit` / `req.eval.submit` / `req.step.report`
//! via the injected binaries, the dispatcher subscribes, and the API layer
//! bridges the rest of the §6.1 surface as it gets implemented.

pub fn work_submit(owner: &str, project: &str, seq: u64) -> String {
    format!("req.work.submit.{owner}.{project}.{seq}")
}

pub fn eval_submit(owner: &str, project: &str, seq: u64, task_id: u64) -> String {
    format!("req.eval.submit.{owner}.{project}.{seq}.{task_id}")
}

/// Harness-only step reporting (spec §4.5).
pub fn step_report(owner: &str, project: &str, seq: u64, task_id: u64) -> String {
    format!("req.step.report.{owner}.{project}.{seq}.{task_id}")
}

pub fn steps_list(owner: &str, project: &str, job_seq: u64, task_id: u64) -> String {
    format!("req.steps.list.{owner}.{project}.{job_seq}.{task_id}")
}

/// Job event stream subject (spec §6.3): `job.events.{owner}.{project}.{seq}.{event_type}`.
pub fn job_event(owner: &str, project: &str, seq: u64, event_type: &str) -> String {
    format!("job.events.{owner}.{project}.{seq}.{event_type}")
}

pub fn channel_inbox(owner: &str, project: &str, seq: u64) -> String {
    format!("channel.inbox.{owner}.{project}.{seq}")
}

/// Agent status updates and replies (spec §4.2), routed through the dispatcher
/// rather than written to KV by the container.
///
/// The container used to write the `channels` bucket itself, which made it a
/// second writer and left the dispatcher blind — so updates could only ever be
/// last-write-wins, with no history. Going through the dispatcher keeps the
/// single-writer rule and lets each update also become a `job-events` entry.
pub fn channel_update(owner: &str, project: &str, seq: u64) -> String {
    format!("req.channel.update.{owner}.{project}.{seq}")
}

pub fn channel_reply(owner: &str, project: &str, seq: u64) -> String {
    format!("req.channel.reply.{owner}.{project}.{seq}")
}

/// §6.x liveness probe: a project-agnostic request that only a live dispatcher
/// answers, and that round-trips the core actor (so a *wedged* state loop reads
/// as unhealthy, not merely a dead process). The api's `GET /api/v1/health`
/// bridges it; a crash-looping dispatcher has no responder.
pub fn health() -> String {
    "req.health".into()
}

/// Read-only capacity launch-queue snapshot scoped to one project (spec §3.5).
/// Served off the dispatcher's core actor so the reported FIFO order and depth
/// match the live in-memory queue; the api forwards it for the queue badge.
pub fn queue_list(owner: &str, project: &str) -> String {
    format!("req.queue.list.{owner}.{project}")
}

pub fn jobs_create(owner: &str, project: &str) -> String {
    format!("req.jobs.create.{owner}.{project}")
}

pub fn jobs_get(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.get.{owner}.{project}.{seq}")
}

/// Available job types at default-branch HEAD (`jobs/*.yaml`), for the create UI.
pub fn job_types_list(owner: &str, project: &str) -> String {
    format!("req.jobtypes.list.{owner}.{project}")
}

/// One job type in full (raw YAML + parsed, defaults merged) at default-branch
/// HEAD, for the library UI. The type name rides in the payload — file stems
/// are not valid subject tokens.
pub fn job_types_get(owner: &str, project: &str) -> String {
    format!("req.jobtypes.get.{owner}.{project}")
}

/// Available knowledge tags at default-branch HEAD (`.chug/tags/*.md`) as
/// `{ name, path }[]`, for the create-job tag picker. Tags are repo-versioned
/// like job types.
pub fn tags_list(owner: &str, project: &str) -> String {
    format!("req.tags.list.{owner}.{project}")
}

pub fn jobs_list(owner: &str, project: &str) -> String {
    format!("req.jobs.list.{owner}.{project}")
}

/// Resolved evaluation criteria for a job: the type's evaluators (incl.
/// project defaults) plus the job's additive ones, at the job's pinned ref.
pub fn jobs_criteria(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.criteria.{owner}.{project}.{seq}")
}

pub fn jobs_release(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.release.{owner}.{project}.{seq}")
}

pub fn jobs_revoke(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.revoke.{owner}.{project}.{seq}")
}

/// Full-field replace of a Draft job's definition (spec §2.1). 409 in any
/// non-Draft state.
pub fn jobs_update(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.update.{owner}.{project}.{seq}")
}

/// Move a Frozen (never-released) job back to Draft for editing (spec §2.1).
pub fn jobs_draft(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.draft.{owner}.{project}.{seq}")
}

/// Finalize an edited Draft back to Frozen (#166): validate the definition
/// like release, but park it (re-batchable) instead of scheduling. Draft-only.
pub fn jobs_finalize(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.finalize.{owner}.{project}.{seq}")
}

/// Add/remove the members of a Draft batch while composing it (spec §2.1
/// draft batches). Draft-only.
pub fn jobs_members(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.members.{owner}.{project}.{seq}")
}

/// Add/remove a job's group labels (spec §6.2, design #321). Accepted in every
/// state, terminal included — `groups` is an annotation, inert to execution.
pub fn jobs_groups(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.groups.{owner}.{project}.{seq}")
}

/// Set/clear the job's operator sign-off gate (spec §1.1 require-approval).
/// Pre-Work states only — past Work entry the criteria are already resolved.
pub fn jobs_approval(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.approval.{owner}.{project}.{seq}")
}

/// Claim the job's next work attempt for a human (spec §1.2 claims).
pub fn jobs_claim(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.claim.{owner}.{project}.{seq}")
}

/// Clear a pending claim that has not materialized into a parked task yet.
pub fn jobs_unclaim(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.unclaim.{owner}.{project}.{seq}")
}

/// Operator-dispatched advisory triage (spec §1.2).
pub fn jobs_triage(owner: &str, project: &str, seq: u64) -> String {
    format!("req.jobs.triage.{owner}.{project}.{seq}")
}

/// Create a project (§12.2 via the API): bare repo, hook, starter template,
/// counter. Owner/name ride in the payload — they are being validated, so
/// they cannot ride in the subject.
pub fn projects_create() -> String {
    "req.projects.create".into()
}

/// Link an existing external repo as a new project (linked-origin mode):
/// bare repo + fetch from origin + `integration` branch + hook + config seed.
/// Owner/name ride in the payload, same as `projects_create`.
pub fn projects_link() -> String {
    "req.projects.link".into()
}

/// Open an origin release: push `integration` to the origin as
/// `chug/release-{n}` and open a PR into the origin's default branch.
pub fn origin_release(owner: &str, project: &str) -> String {
    format!("req.origin.release.{owner}.{project}")
}

/// Origin link + current release state (+ opportunistic PR check).
pub fn origin_status(owner: &str, project: &str) -> String {
    format!("req.origin.status.{owner}.{project}")
}

/// Fetch the origin and reconcile: merged PR → reset `integration` onto the
/// new origin main and clear the merge-queue hold.
pub fn origin_sync(owner: &str, project: &str) -> String {
    format!("req.origin.sync.{owner}.{project}")
}

/// Read one repo file at default-branch HEAD (payload: { path }) — prompt
/// viewers and the like. Repo paths cannot ride in subjects.
pub fn vcs_file(owner: &str, project: &str) -> String {
    format!("req.vcs.file.{owner}.{project}")
}

/// Full recursive tree at default-branch HEAD — the repo browser.
pub fn vcs_tree(owner: &str, project: &str) -> String {
    format!("req.vcs.tree.{owner}.{project}")
}

pub fn graph_get(owner: &str, project: &str) -> String {
    format!("req.graph.get.{owner}.{project}")
}

/// Every group the project's jobs name, with its members and per-state counts
/// (design #321 slice B). Derived at read time from the job records — a group
/// exists because a job says so, so there is no aggregate to read and an empty
/// group does not exist.
pub fn groups_list(owner: &str, project: &str) -> String {
    format!("req.groups.list.{owner}.{project}")
}

/// The design registry: `docs/design/*.md` at default HEAD, each joined to its
/// group's roll-up. The complement of [`groups_list`] — repo-derived rather
/// than member-derived, so a design with no jobs is a row.
pub fn designs_list(owner: &str, project: &str) -> String {
    format!("req.designs.list.{owner}.{project}")
}

pub fn tasks_list_pending(owner: &str, project: &str) -> String {
    format!("req.tasks.list.pending.{owner}.{project}")
}

pub fn tasks_list(owner: &str, project: &str, job_seq: u64) -> String {
    format!("req.tasks.list.{owner}.{project}.{job_seq}")
}

pub fn tasks_resolve(owner: &str, project: &str, job_seq: u64, task_id: u64) -> String {
    format!("req.tasks.resolve.{owner}.{project}.{job_seq}.{task_id}")
}

/// Live/cursor-paged stdout of a running task's container (spec §4.2). Payload
/// `{ since }`; reply `{ offset, data, running }` while the container runs.
/// Served off the dispatcher's core actor (a read-only container tail), and
/// the api falls back to the harvested `stdout.log` artifact after exit.
pub fn tasks_output(owner: &str, project: &str, job_seq: u64, task_id: u64) -> String {
    format!("req.tasks.output.{owner}.{project}.{job_seq}.{task_id}")
}

pub fn vcs_diff(owner: &str, project: &str, seq: u64) -> String {
    format!("req.vcs.diff.{owner}.{project}.{seq}")
}

/// §7.5 project-role management: the dispatcher (single writer of `users.*`)
/// mutates a user record's `project_roles`. `verb` is `set` | `remove` | `list`;
/// the target email rides in the payload (`{ email, role? }`) — emails are not
/// valid subject tokens. Platform-admin-gated at the API before it forwards here.
pub fn members(verb: &str, owner: &str, project: &str) -> String {
    format!("req.members.{verb}.{owner}.{project}")
}

/// §7.3: mint a 24h user SSH certificate. Payload: `{ public_key, email }` —
/// the email is the authenticated caller's, read from the JWT by the API; a
/// client-supplied identity is never forwarded. No owner/project token: the
/// signed cert spans whatever roles the user holds at signing time.
pub fn ssh_sign_user_cert() -> String {
    "req.ssh.sign-user-cert".into()
}

/// Set a worker node's **desired** slot count (design #293 §3). Payload
/// `{ node, slots, by }`; the reply is the 202 body
/// ([`types::NodeCapacityAck`]) or the `{"error": {...}}` envelope. The node name
/// rides in the payload rather than the subject so one subscription serves the
/// whole fleet and an unknown name comes back as a 404 rather than silence.
pub fn fleet_capacity_set() -> String {
    "req.fleet.capacity.set".into()
}

pub fn worker_op(node: &str, op: &str) -> String {
    format!("req.worker.{node}.{op}")
}

/// The daemon's wildcard subscription for its node.
pub fn worker_all(node: &str) -> String {
    format!("req.worker.{node}.>")
}

/// Worker announce/heartbeat (spec §3.1 dynamic registration): the daemon
/// publishes its [`types::worker::WorkerAnnounce`] here periodically and the
/// dispatcher subscribes, merging the node into the live fleet with no restart.
/// A plain (non-JetStream) subject — heartbeats are transient, so there is no
/// need to durably retain them; the current fleet is whichever nodes are still
/// announcing.
pub fn worker_announce() -> String {
    "event.worker.announce".into()
}

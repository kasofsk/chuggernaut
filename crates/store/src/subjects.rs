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

// ── API-facing request subjects (spec §6.1) ─────────────────────────────────
// Published by the api crate, handled by the dispatcher.

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

/// Available knowledge tags at default-branch HEAD (`tags/*.md` stems), for
/// the create-job tag picker. Tags are repo-versioned like job types.
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

/// One turn of the New Job "job wizard" chat: the conversation rides in the
/// payload, the dispatcher grounds it in repo/job context and calls the LLM.
pub fn wizard_chat(owner: &str, project: &str) -> String {
    format!("req.wizard.chat.{owner}.{project}")
}

/// §7.3: mint a 24h user SSH certificate. Payload: `{ public_key, email }` —
/// the email is the authenticated caller's, read from the JWT by the API; a
/// client-supplied identity is never forwarded. No owner/project token: the
/// signed cert spans whatever roles the user holds at signing time.
pub fn ssh_sign_user_cert() -> String {
    "req.ssh.sign-user-cert".into()
}

// ── Worker-node protocol (spec §3.1) ────────────────────────────────────────
// Published by the dispatcher's fleet backend, served by the `chuggernaut
// worker` daemon on the node. Node names are validated subject-safe at
// DOCKER_NODES parse time.

pub fn worker_op(node: &str, op: &str) -> String {
    format!("req.worker.{node}.{op}")
}

/// The daemon's wildcard subscription for its node.
pub fn worker_all(node: &str) -> String {
    format!("req.worker.{node}.>")
}

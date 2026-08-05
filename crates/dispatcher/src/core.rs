//! Single-writer core (spec §3.1): one tokio task owns all job/task state, the
//! in-memory graphs, and the work queue. Everything else — NATS handlers,
//! container monitors, scan timers — talks to it via the [`Msg`] channel and
//! never mutates state directly. Container monitoring is concurrent; state
//! transitions are sequential.
//!
//! - **Accepts:** `Msg` values over the mpsc channel — handler calls, container
//!   exits, and scan ticks.
//! - **Emits:** every job/task state write, graph/queue mutation, and container
//!   launch; drives the other slices as `impl Core` blocks.
//! - **Guarantees:** exactly one writer of platform state; transitions
//!   processed one at a time; no shared mutable state, so no lock to misuse.
//! - **Spec:** §3.1.

use crate::decide::{authoring, ready};
use crate::forge_ingest::origin::OriginStatusResponse;
use crate::graph::JobGraph;
use crate::queue::{QueuedJob, QueuedLaunch, ReadyQueue};
use crate::release::{self, KvNames, ValidationError};
use crate::state::{InvalidTransition, assert_transition};
use crate::{escalation, exec, inputs, queue};
use agent::AgentProvider;
use chrono::{DateTime, Utc};
use container::ContainerBackend;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use store::{
    CounterStore, JobStore, NatsStore, ProjectStore, RdepsStore, TaskStore, split_project, subjects,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use types::{BatchComposition, Job, JobState, TaskResolution, TokenUsage};
use vcs::RepoManager;

pub use types::CreateSpec;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Vcs(#[from] vcs::VcsError),
    #[error(transparent)]
    Transition(#[from] InvalidTransition),
    #[error("job not found: {0}")]
    NotFound(String),
    #[error("validation failed: {0:?}")]
    Validation(Vec<ValidationError>),
    #[error("invalid resolution: {0}")]
    InvalidResolution(String),
    /// Request is well-formed but clashes with current state (HTTP 409):
    /// project already exists, release already open, gate in flight, …
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Backend(#[from] container::BackendError),
    #[error("core loop stopped")]
    Stopped,
}

impl CoreError {
    /// Map an io error into `Config` with context — for the odd filesystem
    /// touch outside the store/vcs layers (origin deploy-key tempfiles).
    pub(crate) fn from_io(context: &'static str) -> impl FnOnce(std::io::Error) -> CoreError {
        move |e| CoreError::Config(format!("{context}: {e}"))
    }
}

impl From<Vec<ValidationError>> for CoreError {
    fn from(errs: Vec<ValidationError>) -> Self {
        CoreError::Validation(errs)
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// Full-field replacement of a Draft job's definition (spec §2.1). The same
/// shape as [`CreateSpec`] minus the immutable identity: only a job in
/// Draft accepts it. Validation is identical to create — deferred to release.
pub struct UpdateJobRequest {
    pub owner: String,
    pub project: String,
    pub seq: u64,
    pub r#type: String,
    pub title: String,
    pub description: String,
    /// Optional rich cover page (spec §1.1, §4.3); never enters a prompt.
    pub cover_html: Option<String>,
    pub deps: Vec<u64>,
    pub knowledge_tags: Vec<String>,
    pub eval: Vec<types::Evaluator>,
    /// Whether the job needs an operator sign-off ([`types::Job::require_approval`]).
    /// Editable after Draft too, through [`Core::set_require_approval`].
    pub require_approval: bool,
    pub timeout: Option<String>,
    pub model: Option<String>,
    /// The supplied inputs, replaced wholesale like every other field. A Draft
    /// edit is the *same* writer as creation (spec §2.1) — once the job leaves
    /// Draft, [`Job::inputs`] is written only by the Ready-transition default
    /// fill, and never again after that.
    pub inputs: BTreeMap<String, String>,
    /// The job's groups, replaced wholesale like every other field here
    /// (design #321). This is the *definition* write path; the add/remove verb
    /// ([`Core::edit_groups`]) is the one that stays available after release.
    pub groups: Vec<String>,
}

/// `req.work.submit.*` payload (spec §4.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkSubmission {
    pub summary: Option<String>,
    pub structured: Option<serde_json::Value>,
    pub token_usage: Option<TokenUsage>,
    /// Optional agent-authored HTML cover page (job #143). Presentational only:
    /// size-capped and rejected (never truncated) at ingest, stored on the task
    /// record, and ignored by the merge gate/squash body. Text `summary` stays
    /// canonical and required.
    #[serde(default)]
    pub cover_html: Option<String>,
}

/// `req.eval.submit.*` payload (spec §4.2). `pass` is the authoritative verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSubmission {
    pub pass: bool,
    /// "Not satisfiable by rework" (docs/reference/design-lifecycle.md): implies fail; a
    /// required evaluator's abort escalates instead of consuming rework budget.
    #[serde(default)]
    pub abort: bool,
    pub structured: Option<serde_json::Value>,
    pub token_usage: Option<TokenUsage>,
    /// Optional agent-authored HTML cover page for the verdict summary (job
    /// #143), same handling as [`WorkSubmission::cover_html`].
    #[serde(default)]
    pub cover_html: Option<String>,
}

/// What a container monitor observed when its task exited. These three always
/// travel together from the monitor to the exit handlers.
#[derive(Debug, Clone, Default)]
pub struct TaskExit {
    pub exit_code: i32,
    /// `/workspace/eval-result.json` for command eval containers.
    pub eval_json: Option<serde_json::Value>,
    /// Usage measured from the agent CLI's JSON result, collected by the
    /// monitor before it reported the exit. `None` for command containers, for
    /// unparseable stdout, and on the restart re-attach path (where the monitor
    /// that would have parsed it no longer exists).
    pub usage: Option<TokenUsage>,
    /// The agent CLI's final result text, harvested from stdout. Only carried
    /// by triage runs (spec §1.2), which have no channel MCP and so report
    /// their assessment through the CLI's JSON result rather than `submit_result`.
    pub assessment: Option<String>,
    /// Set when the container never launched — the backend rejected it (bad
    /// image, invalid resource limit, node pressure). Reported through the same
    /// [`Msg::TaskExited`] fan-in as a real exit so launch failure flows into
    /// the task-failure/escalation machinery instead of leaving the task
    /// `Running` forever. Carries the single-wrapped, human-readable reason
    /// (e.g. `container launch failed: invalid memory limit "5g"`).
    pub launch_error: Option<String>,
    /// A tail of the container's captured stdout+stderr, harvested by the
    /// monitor alongside the exit. Only carried by command eval / merge-gate
    /// containers ([`Core::spawn_eval_monitor`]); it is what lets a failing gate
    /// stage's compiler output reach the gate-fix brief (job #154). `None`
    /// elsewhere (agent runs report through `submit_result`, not stdout).
    pub log_tail: Option<String>,
    /// Set only by restart reconciliation (§3.6, `settle_running`) when a task's
    /// container is GONE at restart — docker pruned it, the node rebooted,
    /// colima restarted. This is an infrastructure loss, not a real nonzero
    /// exit: the attempt is relaunched without spending a `work_retries`/
    /// `eval_retries` budget (capped, then escalates `infra_loss`). Never set on
    /// the in-container exit paths — a real exit keeps burning budget.
    pub infra_loss: bool,
    /// A structured report harvested from a command work task's stdout (ticket
    /// #187): the `@chug:leg`/`@chug:report` lines a deploy emits, parsed into a
    /// [`types::DeployReport`] and serialized. Only set by the command
    /// work/wrap-up log monitor; `None` for agent runs (which report through
    /// `submit_result`) and for command work that emitted no leg lines. Consumed
    /// by [`Core::on_work_exited`] into the task's structured result.
    pub structured: Option<serde_json::Value>,
}

impl TaskExit {
    /// An exit with nothing harvested — command containers, scans, reconcile.
    pub fn code(exit_code: i32) -> Self {
        Self {
            exit_code,
            ..Default::default()
        }
    }

    /// A container that was GONE when restart reconciliation looked for it
    /// (§3.6): an infrastructure loss, distinct from a real nonzero exit. Routed
    /// through the exit fan-in so [`Core::on_task_exited`] relaunches the attempt
    /// without spending retry budget (capped, then `infra_loss` escalation).
    pub fn infra_loss() -> Self {
        Self {
            exit_code: -1,
            infra_loss: true,
            ..Default::default()
        }
    }

    /// A container that never launched: reported through the exit fan-in so the
    /// launch failure lands in the task-failure path (retry/infra/escalation)
    /// rather than wedging the task at `Running`. Exit code -1 mirrors the
    /// agent path, which already surfaces a failed run that way.
    pub fn launch_failed(reason: String) -> Self {
        Self {
            exit_code: -1,
            launch_error: Some(reason),
            ..Default::default()
        }
    }
}

/// One agent-authored channel post. `ChannelEntry` keeps only the latest of
/// each, so the durable history is the event stream, not the KV entry.
#[derive(Debug, Clone)]
pub enum ChannelPost {
    Update(types::ChannelUpdate),
    Reply(types::AgentReply),
}

type Reply<T> = oneshot::Sender<Result<T>>;

pub enum Msg {
    CreateJob(CreateSpec, Reply<Job>),
    ReleaseJob {
        owner: String,
        project: String,
        seq: u64,
        reply: Reply<JobState>,
    },
    RevokeJob {
        owner: String,
        project: String,
        seq: u64,
        reply: Reply<Vec<u64>>,
    },
    /// `req.jobs.update.*` (spec §2.1): full-field replace of a Draft job's
    /// definition. 409 in any non-Draft state.
    UpdateJob(UpdateJobRequest, Reply<Job>),
    /// `req.jobs.draft.*` (spec §2.1): move a Frozen (never-released) job back
    /// to Draft for editing. Only Frozen → Draft.
    DraftJob {
        owner: String,
        project: String,
        seq: u64,
        reply: Reply<()>,
    },
    /// `req.jobs.finalize.*` (#166): finalize an edited Draft back to Frozen —
    /// validates the definition like release but parks it (re-batchable)
    /// instead of scheduling. Only Draft → Frozen.
    FinalizeJob {
        owner: String,
        project: String,
        seq: u64,
        reply: Reply<()>,
    },
    /// `req.jobs.members.*` (spec §2.1 batches, draft batches): add/remove the
    /// members of a **Draft** batch while composing it. Draft-only (409 in any
    /// other state); adds are re-validated per-candidate. Members are not
    /// absorbed here — a draft holds a non-binding list, absorbed only at
    /// finalize/release.
    EditMembers {
        owner: String,
        project: String,
        seq: u64,
        add: Vec<u64>,
        remove: Vec<u64>,
        reply: Reply<Job>,
    },
    /// `req.jobs.groups.*` (spec §6.2, design #321): add/remove a job's group
    /// labels. Accepted in **every** state, terminal included — `groups` is an
    /// annotation, inert to execution.
    EditGroups {
        owner: String,
        project: String,
        seq: u64,
        add: Vec<String>,
        remove: Vec<String>,
        reply: Reply<Job>,
    },
    /// `req.jobs.approval.*` (spec §1.1 require-approval): set/clear the job's
    /// operator sign-off gate. 422 once the job has entered Work, where the
    /// resolved criteria are already pinned and the edit could not take effect.
    SetRequireApproval {
        owner: String,
        project: String,
        seq: u64,
        require: bool,
        reply: Reply<Job>,
    },
    /// `req.jobs.claim.*` (spec §1.2 claims): a human claims the job's next
    /// work attempt. 409 while an attempt is in flight.
    ClaimJob {
        owner: String,
        project: String,
        seq: u64,
        reply: Reply<()>,
    },
    /// `req.jobs.unclaim.*`: clear a pending claim that has not materialized
    /// into a parked task yet.
    UnclaimJob {
        owner: String,
        project: String,
        seq: u64,
        reply: Reply<()>,
    },
    /// `req.jobs.triage.*` (spec §1.2): dispatch an advisory triage agent over
    /// an Escalated/Stalled job. Never changes job state.
    TriageJob {
        owner: String,
        project: String,
        seq: u64,
        reply: Reply<()>,
    },
    SubmitResult {
        owner: String,
        project: String,
        seq: u64,
        submission: WorkSubmission,
        reply: Reply<()>,
    },
    SubmitEval {
        owner: String,
        project: String,
        seq: u64,
        task_id: u64,
        submission: EvalSubmission,
        reply: Reply<()>,
    },
    /// Operator resolves a Human task via the inbox (spec §1.2).
    ResolveTask {
        owner: String,
        project: String,
        seq: u64,
        task_id: u64,
        resolution: TaskResolution,
        operator: String,
        reply: Reply<()>,
    },
    /// `req.channel.update` / `req.channel.reply` (spec §4.2). Routed through
    /// the core so the dispatcher stays the sole writer of `channels` KV and
    /// every update also lands in the event stream as history.
    ChannelPost {
        owner: String,
        project: String,
        seq: u64,
        post: ChannelPost,
        reply: Reply<()>,
    },
    /// `req.projects.link`: create a linked-origin project (origin fetch +
    /// `integration` HEAD + config seed). Runs in the core actor because it
    /// needs the age identity (deploy key) and writes platform project state.
    LinkProject {
        owner: String,
        name: String,
        origin_url: String,
        main_branch: Option<String>,
        reply: Reply<types::ProjectRecord>,
    },
    /// `req.origin.release`: push `integration` as `chug/release-{n}` and open
    /// a PR on the origin; holds the project's merge queue while it is open.
    OriginRelease {
        owner: String,
        project: String,
        reply: Reply<types::ProjectRecord>,
    },
    /// `req.origin.status`: link + release state with an opportunistic PR
    /// check (a merged/closed PR is reconciled inline).
    OriginStatus {
        owner: String,
        project: String,
        reply: Reply<OriginStatusResponse>,
    },
    /// `req.origin.sync`: fetch the origin and reconcile — merged PR → reset
    /// `integration` onto the new origin main and release the hold.
    OriginSync {
        owner: String,
        project: String,
        reply: Reply<OriginStatusResponse>,
    },
    /// `req.health` (spec §6.x): a no-op round-trip through the core actor. A
    /// reply proves the single-threaded state loop is draining messages, not
    /// merely that the process is up — the strongest cheap liveness signal.
    Ping {
        reply: Reply<()>,
    },
    /// `req.queue.list.{owner}.{project}` (spec §3.5): a read-only snapshot of
    /// the capacity launch queue, scoped to one project. Served off the actor so
    /// the reported FIFO order and depth match the live in-memory queue.
    QueueSnapshot {
        owner: String,
        project: String,
        reply: Reply<types::QueueSnapshot>,
    },
    /// §3.5 scans; fired by the internal ticker, or with a reply from
    /// [`CoreHandle::trigger_scan`] (tests).
    Scan {
        reply: Option<Reply<()>>,
    },
    /// Graceful shutdown (spec §3.6 drain): flip the core into draining mode,
    /// process the in-flight mailbox so container ids/exits land in KV, audit
    /// every Running task to KV, then stop the loop. Posted by the SIGTERM
    /// handler (or a test via [`CoreHandle::drain`]). The reply fires once the
    /// drain is complete; the loop returns immediately after.
    Drain {
        reply: Reply<()>,
    },
    /// Posted by container monitor tasks — never by anything outside the crate.
    TaskExited {
        owner: String,
        project: String,
        seq: u64,
        task_id: u64,
        exit: TaskExit,
    },
    /// Posted by the launch forwarder the instant an agent container launches,
    /// so the id lands on the Running task record — providers only surface it in
    /// `AgentOutput` after exit. Never posted by anything outside the crate.
    TaskContainerStarted {
        owner: String,
        project: String,
        seq: u64,
        task_id: u64,
        container_id: String,
    },
    /// Posted by an agent launch task (an agent evaluator) when the fleet is at
    /// capacity: the provider erases [`container::BackendError::NoCapacity`], so
    /// the spawned task signals it back here for the actor to queue the launch
    /// through [`Core::defer_launch`] instead of burning `eval_retries` (#140).
    /// Never posted by anything outside the crate.
    LaunchDeferred {
        owner: String,
        project: String,
        seq: u64,
        task_id: u64,
        reason: String,
    },
    /// A worker daemon's announce heartbeat (spec §3.1 dynamic registration),
    /// forwarded from the `event.worker.announce` subscriber. Merges the node
    /// into the live fleet *inside the actor* — the fleet's single writer — so
    /// scheduling and the NoCapacity launch queue see the capacity immediately.
    /// One-way: a heartbeat carries no reply.
    ///
    /// Carries the whole announce, not its slot count alone, because the
    /// capacity it reports is only applied when its
    /// `(capacity_epoch, capacity_generation)` pair clears the node's watermark
    /// (spec §3.1 slot source) — a decision the pair has to travel for.
    WorkerAnnounce {
        announce: types::worker::WorkerAnnounce,
    },
    /// `req.fleet.capacity.set` (spec §3.1 operator capacity control): record the
    /// operator's **desired** slot count for a node and command the node to adopt
    /// it. Answered with the 202 body — the actor persists intent and starts the
    /// push, and never blocks on the node's RPC.
    SetNodeCapacity {
        node: String,
        slots: u32,
        /// The platform admin who asked, for the record's audit stamp (§9).
        by: String,
        reply: Reply<types::NodeCapacityAck>,
    },
    /// One `set_slots` push's reply, posted by the spawned RPC — never by anything
    /// outside the crate. This is how the ledger stays a thing only the single
    /// writer writes while the RPC itself runs off the actor thread.
    CapacityPushed {
        node: String,
        /// The value the push carried, so a reply that lost a race with a newer
        /// operator ask is recognised and dropped.
        slots: u32,
        outcome: crate::capacity::PushOutcome,
    },
}

impl Msg {
    /// The variant's name, for attributing an invariant violation to the message
    /// that introduced it (refactor-plan B1a). Spelled out rather than derived so
    /// a new `Msg` has to be named here too — the compiler's exhaustiveness check
    /// is what keeps the attribution honest.
    fn label(&self) -> &'static str {
        match self {
            Msg::CreateJob(..) => "CreateJob",
            Msg::ReleaseJob { .. } => "ReleaseJob",
            Msg::RevokeJob { .. } => "RevokeJob",
            Msg::UpdateJob { .. } => "UpdateJob",
            Msg::DraftJob { .. } => "DraftJob",
            Msg::FinalizeJob { .. } => "FinalizeJob",
            Msg::EditMembers { .. } => "EditMembers",
            Msg::EditGroups { .. } => "EditGroups",
            Msg::SetRequireApproval { .. } => "SetRequireApproval",
            Msg::ClaimJob { .. } => "ClaimJob",
            Msg::UnclaimJob { .. } => "UnclaimJob",
            Msg::TriageJob { .. } => "TriageJob",
            Msg::SubmitResult { .. } => "SubmitResult",
            Msg::SubmitEval { .. } => "SubmitEval",
            Msg::ResolveTask { .. } => "ResolveTask",
            Msg::ChannelPost { .. } => "ChannelPost",
            Msg::LinkProject { .. } => "LinkProject",
            Msg::OriginRelease { .. } => "OriginRelease",
            Msg::OriginStatus { .. } => "OriginStatus",
            Msg::OriginSync { .. } => "OriginSync",
            Msg::Ping { .. } => "Ping",
            Msg::QueueSnapshot { .. } => "QueueSnapshot",
            Msg::Scan { .. } => "Scan",
            Msg::Drain { .. } => "Drain",
            Msg::TaskExited { .. } => "TaskExited",
            Msg::TaskContainerStarted { .. } => "TaskContainerStarted",
            Msg::LaunchDeferred { .. } => "LaunchDeferred",
            Msg::WorkerAnnounce { .. } => "WorkerAnnounce",
            Msg::SetNodeCapacity { .. } => "SetNodeCapacity",
            Msg::CapacityPushed { .. } => "CapacityPushed",
        }
    }
}

/// Cloneable façade over the core channel; the only way other components
/// reach the dispatcher's state.
#[derive(Clone)]
pub struct CoreHandle {
    tx: mpsc::Sender<Msg>,
}

impl CoreHandle {
    async fn call<T>(&self, build: impl FnOnce(Reply<T>) -> Msg) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(build(tx))
            .await
            .map_err(|_| CoreError::Stopped)?;
        rx.await.map_err(|_| CoreError::Stopped)?
    }

    /// Round-trip the core actor for the §6.x health probe. `Ok(())` means the
    /// state loop accepted and answered a message — i.e. it is not wedged.
    pub async fn ping(&self) -> Result<()> {
        self.call(|reply| Msg::Ping { reply }).await
    }

    /// A read-only view of the capacity launch queue scoped to one project
    /// (§3.5), served off the actor so the reported order matches live state.
    pub async fn queue_snapshot(&self, owner: &str, project: &str) -> Result<types::QueueSnapshot> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::QueueSnapshot {
            owner,
            project,
            reply,
        })
        .await
    }

    pub async fn create_job(&self, req: CreateSpec) -> Result<Job> {
        self.call(|reply| Msg::CreateJob(req, reply)).await
    }

    /// Forward a worker announce heartbeat into the actor (spec §3.1 dynamic
    /// registration). One-way — the announce subscriber does not wait for a
    /// reply; the actor merges the node into the live fleet on its own turn.
    pub async fn announce_worker(&self, announce: types::worker::WorkerAnnounce) -> Result<()> {
        self.tx
            .send(Msg::WorkerAnnounce { announce })
            .await
            .map_err(|_| CoreError::Stopped)
    }

    /// Set a worker node's desired slot count (spec §3.1 operator capacity
    /// control). Returns the 202 body: intent is recorded and converging, because
    /// the actor does not wait on the node's RPC.
    pub async fn set_node_capacity(
        &self,
        node: &str,
        slots: u32,
        by: &str,
    ) -> Result<types::NodeCapacityAck> {
        let (node, by) = (node.to_string(), by.to_string());
        self.call(|reply| Msg::SetNodeCapacity {
            node,
            slots,
            by,
            reply,
        })
        .await
    }

    pub async fn release_job(&self, owner: &str, project: &str, seq: u64) -> Result<JobState> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::ReleaseJob {
            owner,
            project,
            seq,
            reply,
        })
        .await
    }

    pub async fn revoke_job(&self, owner: &str, project: &str, seq: u64) -> Result<Vec<u64>> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::RevokeJob {
            owner,
            project,
            seq,
            reply,
        })
        .await
    }

    pub async fn update_job(&self, req: UpdateJobRequest) -> Result<Job> {
        self.call(|reply| Msg::UpdateJob(req, reply)).await
    }

    pub async fn draft_job(&self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::DraftJob {
            owner,
            project,
            seq,
            reply,
        })
        .await
    }

    pub async fn finalize_job(&self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::FinalizeJob {
            owner,
            project,
            seq,
            reply,
        })
        .await
    }

    /// Add/remove the members of a Draft batch while composing it (spec §2.1
    /// batches, draft batches). Draft-only; adds re-validated per-candidate.
    pub async fn edit_members(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        add: Vec<u64>,
        remove: Vec<u64>,
    ) -> Result<Job> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::EditMembers {
            owner,
            project,
            seq,
            add,
            remove,
            reply,
        })
        .await
    }

    /// Add/remove a job's group labels (design #321 Decision 5). Accepted in
    /// every state, including terminal ones; the resulting list is re-checked
    /// against the §1.1 bounds (422).
    pub async fn edit_groups(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        add: Vec<String>,
        remove: Vec<String>,
    ) -> Result<Job> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::EditGroups {
            owner,
            project,
            seq,
            add,
            remove,
            reply,
        })
        .await
    }

    /// Set or clear the job's operator sign-off gate (spec §1.1). Accepted only
    /// while the job has not yet entered Work — past that the criteria are
    /// already resolved, so the edit would be a silent no-op (422).
    pub async fn set_require_approval(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        require: bool,
    ) -> Result<Job> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::SetRequireApproval {
            owner,
            project,
            seq,
            require,
            reply,
        })
        .await
    }

    pub async fn claim_job(&self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::ClaimJob {
            owner,
            project,
            seq,
            reply,
        })
        .await
    }

    pub async fn unclaim_job(&self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::UnclaimJob {
            owner,
            project,
            seq,
            reply,
        })
        .await
    }

    pub async fn triage_job(&self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::TriageJob {
            owner,
            project,
            seq,
            reply,
        })
        .await
    }

    pub async fn submit_result(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        submission: WorkSubmission,
    ) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::SubmitResult {
            owner,
            project,
            seq,
            submission,
            reply,
        })
        .await
    }

    pub async fn submit_eval(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        submission: EvalSubmission,
    ) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::SubmitEval {
            owner,
            project,
            seq,
            task_id,
            submission,
            reply,
        })
        .await
    }

    pub async fn channel_post(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        post: ChannelPost,
    ) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::ChannelPost {
            owner,
            project,
            seq,
            post,
            reply,
        })
        .await
    }

    /// Run the §3.5 scans now and wait for them to finish. Production relies
    /// on the internal ticker; this is for tests and admin tooling.
    pub async fn trigger_scan(&self) -> Result<()> {
        self.call(|reply| Msg::Scan { reply: Some(reply) }).await
    }

    /// Graceful shutdown (spec §3.6 drain): drain the actor and flush any
    /// memory-only state to KV so records are true at exit, then stop the loop.
    /// Returns once the drain finishes; the core stops processing messages after.
    /// Driven by the SIGTERM handler in production, or directly in tests.
    pub async fn drain(&self) -> Result<()> {
        self.call(|reply| Msg::Drain { reply }).await
    }

    pub async fn link_project(
        &self,
        owner: &str,
        name: &str,
        origin_url: &str,
        main_branch: Option<String>,
    ) -> Result<types::ProjectRecord> {
        let (owner, name, origin_url) =
            (owner.to_string(), name.to_string(), origin_url.to_string());
        self.call(|reply| Msg::LinkProject {
            owner,
            name,
            origin_url,
            main_branch,
            reply,
        })
        .await
    }

    pub async fn origin_release(&self, owner: &str, project: &str) -> Result<types::ProjectRecord> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::OriginRelease {
            owner,
            project,
            reply,
        })
        .await
    }

    pub async fn origin_status(&self, owner: &str, project: &str) -> Result<OriginStatusResponse> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::OriginStatus {
            owner,
            project,
            reply,
        })
        .await
    }

    pub async fn origin_sync(&self, owner: &str, project: &str) -> Result<OriginStatusResponse> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::OriginSync {
            owner,
            project,
            reply,
        })
        .await
    }

    pub async fn resolve_task(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        resolution: TaskResolution,
        operator: &str,
    ) -> Result<()> {
        let (owner, project, operator) =
            (owner.to_string(), project.to_string(), operator.to_string());
        self.call(|reply| Msg::ResolveTask {
            owner,
            project,
            seq,
            task_id,
            resolution,
            operator,
            reply,
        })
        .await
    }
}

/// The issuer half of the workload-token mint (spec §12.1, design #313 A2):
/// the keypair `chuggernaut init` wrote and the identifier every token's `iss`
/// carries.
#[derive(Debug, Clone)]
pub struct OidcIssuer {
    pub private_pem: String,
    pub public_pem: String,
    pub issuer: String,
}

#[derive(Default)]
pub struct CoreConfig {
    /// Base for `REPO_URL` env injection: `{repo_url_base}/{owner}/{project}.git`.
    /// Must be reachable from every fleet node (spec §3.1). Tests use a local
    /// path base.
    pub repo_url_base: String,
    /// Container-facing NATS URL (`NATS_URL` env injection) — may differ from
    /// the dispatcher's own connection URL (`NATS_URL_CONTAINER`, §12.4).
    pub nats_url: String,
    /// Path to the chuggernaut-channel binary, injected into every agent
    /// container at /usr/local/bin/chuggernaut-channel (spec §4.2). None →
    /// agents run without the channel MCP (tests, degraded dev mode).
    pub channel_binary: Option<std::path::PathBuf>,
    /// age identity (`AGE-SECRET-KEY-1...`) for decrypting secrets at launch
    /// (spec §8.2). None → secret env values are injected as stored (dev).
    pub age_identity: Option<String>,
    /// Separate age identity (`age_artifacts.key`) for transcripts and logs.
    /// Deliberately not `age_identity`: the API needs to decrypt artifacts to
    /// display them, while the secrets key stays dispatcher-only (§10.2).
    /// None → artifacts are not captured.
    pub artifacts_identity: Option<String>,
    /// §12.4 platform provider default. None (tests) falls back to `claude`;
    /// the production path always sets it — `DispatcherConfig` requires it.
    pub agent_provider_default: Option<String>,
    /// §12.4 platform model default — the bottom of the resolution chain. Any
    /// more specific layer overrides it: per-job `Job::model`, job-type/evaluator
    /// `model:`, and the project default (`jobs/_defaults.yaml`).
    pub agent_model_default: Option<String>,
    /// `TRIAGE_IMAGE` (§1.2): platform image for operator-dispatched triage
    /// agents. None → the triage action is unavailable.
    pub triage_image: Option<String>,
    /// Platform NATS account seed (`nats_account.seed`, §12.1) for minting
    /// per-container scoped credentials (§7.4). None → containers connect
    /// unauthenticated (tests, open dev NATS).
    pub nats_account_seed: Option<String>,
    /// SSH CA private key path (`ssh_ca`, §12.1) for per-job certificates
    /// (§7.4). Certs are injected only when `repo_url_base` is `ssh://` —
    /// `file://` dev repos need none.
    pub ssh_ca: Option<std::path::PathBuf>,
    /// The OIDC issuer keypair and identifier (`oidc_private.pem` /
    /// `oidc_public.pem`, §12.1, design #313 A2) for minting workload tokens
    /// (§8.3). None → a job type declaring `workload_identities:` fails its
    /// launch rather than running without the credential it declared.
    pub oidc_issuer: Option<OidcIssuer>,
    /// Binary path baked into new repos' pre-receive hooks (§5.2) — the path
    /// the binary has on the SSH host. None → this process's own executable.
    /// Used by the core for `req.projects.link`; `spawn_api_handlers` takes
    /// the same value for `req.projects.create`.
    pub hook_bin: Option<std::path::PathBuf>,
    /// Maximum time a launch may wait in the capacity queue before it escalates
    /// as a backstop (spec §3.5). None → [`crate::launch_queue::MAX_QUEUE_WAIT`]
    /// (30m). Tests shrink it to exercise the timeout without waiting.
    pub launch_queue_max_wait: Option<std::time::Duration>,
    /// How long a dynamically-announced worker may go without an announce
    /// heartbeat before it is marked unschedulable (spec §3.1 dynamic
    /// registration). None → [`crate::scan::WORKER_HEARTBEAT_TIMEOUT`]. Tests
    /// shrink it to exercise heartbeat loss without waiting.
    pub worker_heartbeat_timeout: Option<std::time::Duration>,
}

pub struct Core {
    pub(crate) store: NatsStore,
    pub(crate) jobs: JobStore,
    pub(crate) tasks: TaskStore,
    pub(crate) counters: CounterStore,
    pub(crate) rdeps: RdepsStore,
    pub(crate) repos: RepoManager,
    pub(crate) backend: Arc<dyn ContainerBackend>,
    pub(crate) provider: Arc<dyn AgentProvider>,
    pub(crate) config: CoreConfig,
    pub(crate) graphs: HashMap<String, JobGraph>,
    pub queue: ReadyQueue,
    /// Container launches deferred because the fleet had no free slot (spec
    /// §3.5): FIFO, drained when a container exit frees a slot and by the
    /// periodic scan as a backstop. In-memory like `queue`; restart
    /// reconciliation re-queues the Pending tasks it finds.
    pub(crate) launch_queue: std::collections::VecDeque<QueuedLaunch>,
    /// Execution state for jobs in Work/Evaluation (this process's working
    /// memory; restart rebuild is the reconcile slice).
    pub(crate) active: HashMap<(String, String, u64), exec::ExecState>,
    /// chuggernaut-channel binary bytes, loaded once at startup.
    pub(crate) channel_binary: Option<Vec<u8>>,
    /// Decrypting secret store; None runs raw-injection dev mode.
    pub(crate) secrets: Option<store::secrets::AgeSecretStore>,
    /// Signs one workload token per (container, declared identity) at launch
    /// (§8.3, design #313 A3). None on a platform with no issuer keypair, where
    /// a declared identity fails its launch instead.
    pub(crate) workload_signer: Option<auth::workload::WorkloadTokenSigner>,
    /// Transcript/log blob store; None disables capture (see `harvest`).
    pub(crate) artifacts: Option<Arc<store::ArtifactStore>>,
    /// Per-project landing pipeline (spec §3.3 Merge Gate: the FIFO queue +
    /// the depth-1 gate slot), keyed by project slug. The VALUE is owned by
    /// the merge-gate decider (refactor-plan C2) — the shim swaps it
    /// wholesale per decision; entries are dropped when idle.
    pub(crate) merge_gates: HashMap<String, chuggernaut_domain::decide::merge_gate::MergeGateState>,
    /// Platform project records (linked-origin state).
    pub(crate) projects: ProjectStore,
    /// PR surface for origin releases; a fake in integration tests.
    pub(crate) pr_api: Arc<dyn crate::forge_ingest::github::PullRequestApi>,
    /// Project slugs whose merge queue is held by an Open origin release
    /// (nothing lands on integration until the release PR resolves). Derived
    /// from `projects.*` KV; rebuilt at startup.
    pub(crate) release_holds: HashSet<String>,
    /// Set by [`spawn`]; monitors post `TaskExited` through it.
    pub(crate) self_tx: Option<mpsc::Sender<Msg>>,
    /// Graceful-shutdown drain mode (spec §3.6): flipped inside the single-writer
    /// loop by [`Msg::Drain`]. While set, the core initiates NO new work — no
    /// container launches, no gate starts, no wrap-up launches (the launch paths
    /// early-return) — but keeps PROCESSING in-flight messages so container exits
    /// and container-start ids still record. This lets a SIGTERM drain the actor
    /// and flush memory-only state to KV so records are true at exit.
    pub(crate) draining: bool,
    /// Config-snapshot republish state (CD deploy-drift visibility, see
    /// [`crate::platform_ops::cd`]). `None` when snapshot publishing isn't
    /// wired (most tests); the scan tick republishes it when the serialized
    /// bytes change.
    pub(crate) snapshot: Option<crate::platform_ops::cd::ConfigSnapshot>,
    /// The fleet roster (node names + slot caps + boot health) for live
    /// occupancy publishing (see [`crate::platform_ops::fleet`]). Mirrors the
    /// config snapshot's `nodes`; empty in tests that don't wire a fleet. Live
    /// health/version comes from the backend, slot caps from here.
    pub(crate) fleet_roster: Vec<types::WorkerNode>,
    /// Last-published `fleet.status` bytes, for change detection — a launch/exit
    /// that leaves occupancy unchanged republishes nothing.
    pub(crate) last_fleet_status: Option<Vec<u8>>,
    /// Dynamically-announced workers → last heartbeat time (spec §3.1 dynamic
    /// registration). The scan tick marks a node whose heartbeat lapsed past the
    /// timeout unschedulable; a fresh announce refreshes the entry and re-admits
    /// it. Only in-actor state — the backend holds the live schedulability flag.
    pub(crate) announced_workers: HashMap<String, DateTime<Utc>>,
    /// Names of the boot-time (`DOCKER_NODES`) fleet seed. These never get
    /// heartbeat-gated: static nodes rely on the ping-based health path, so a
    /// seed that also re-announces (e.g. to change its slot count) is not removed
    /// from scheduling just because it stops announcing.
    pub(crate) seed_node_names: HashSet<String>,
    /// When this dispatcher started, for the design #293 §8 grace period: a
    /// worker is only accused of never reporting capacity once it has had a few
    /// minutes past OUR start to do so.
    pub(crate) started_at: DateTime<Utc>,
    /// Node → when the §8 never-observed warning last fired for it, so the
    /// warning stays at a bounded cadence. Pruned each scan to nodes still in
    /// that state, so it can never outgrow the roster.
    pub(crate) capacity_warned_at: HashMap<String, DateTime<Utc>>,
    /// The operator's desired capacity per node (design #293 §2), mirroring the
    /// persisted `fleet.capacity` record. **Never a placement input** — see
    /// [`crate::capacity`], which owns the record and asserts that invariant on
    /// every launch.
    pub(crate) capacity_intent: crate::capacity::CapacityIntent,
    /// Node → what the reconciler has pushed there and what came back (design
    /// #293 §4). Pruned each scan to nodes that still have intent, so it can never
    /// outgrow the roster.
    pub(crate) capacity_pushes: HashMap<String, crate::capacity::PushRecord>,
    /// Node → when the reconciler last warned about its intent not converging
    /// (design #293 §4). Bounded exactly like [`Self::capacity_warned_at`], and
    /// pruned each scan to nodes that still have intent.
    pub(crate) capacity_intent_warned_at: HashMap<String, DateTime<Utc>>,
    /// Golden-trace recorder (refactor-plan B3, [`crate::trace`]). `None` in
    /// production — a test attaches a [`crate::trace::TraceSink`] via
    /// [`Core::attach_trace`] to capture every transition and effect. Inert
    /// otherwise: a single `Option` check per `set_state`/`publish`.
    pub(crate) trace: Option<crate::trace::TraceSink>,
    /// Every project's loaded schedules by slug (spec §1.1 schedules,
    /// [`crate::schedules`]). Held in memory so the 30-second tick does no git
    /// I/O; refreshed at startup, after a squash-merge lands, and on the
    /// periodic backstop.
    pub(crate) schedules: HashMap<String, crate::schedules::ScheduleTable>,
    /// Scan ticks since startup, for that periodic backstop — the one bound
    /// that decides how stale a schedule table may get.
    pub(crate) schedule_ticks: u64,
    /// Invariant-violation log (refactor-plan B1a, [`crate::invariants`]). `None`
    /// in production — a test attaches an
    /// [`InvariantSink`](crate::invariants::InvariantSink) via
    /// [`Core::attach_invariant_sink`] and the state loop then checks every
    /// invariant after each message it handles. Inert otherwise: a single
    /// `Option` check per message.
    pub(crate) invariant_sink: Option<crate::invariants::InvariantSink>,
}

const SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Start the single-writer loop; returns the handle everything else uses.
/// Restart reconciliation (§3.6) runs inside the actor task before the first
/// message is processed.
pub fn spawn(mut core: Core) -> CoreHandle {
    let (tx, rx) = mpsc::channel(256);
    core.self_tx = Some(tx.clone());

    let ticker_tx = tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SCAN_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            if ticker_tx.send(Msg::Scan { reply: None }).await.is_err() {
                break;
            }
        }
    });

    tokio::spawn(async move {
        if let Err(e) = core.reconcile().await {
            tracing::error!("restart reconciliation: {e}");
        }
        if let Err(e) = core.drain_queue().await {
            tracing::error!("post-reconcile drain: {e}");
        }
        if let Err(e) = core.drain_launch_queue().await {
            tracing::error!("post-reconcile launch drain: {e}");
        }
        core.refresh_schedules().await;
        core.refresh_fleet_status().await;
        core.run(rx).await
    });
    CoreHandle { tx }
}

impl Core {
    /// Connect stores and rebuild in-memory state from `jobs.*` KV (spec §3.6
    /// steps 1 and 5): graphs, the rdeps index (written back — it is a derived
    /// cache), and the Ready queue.
    #[allow(
        clippy::too_many_lines,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
    pub async fn new(
        store: NatsStore,
        repos: RepoManager,
        backend: Arc<dyn ContainerBackend>,
        provider: Arc<dyn AgentProvider>,
        config: CoreConfig,
    ) -> Result<Self> {
        let jobs = store.jobs().await?;
        let tasks = store.tasks().await?;
        let counters = store.counters().await?;
        let rdeps = store.rdeps().await?;
        let projects = store.projects().await?;

        let channel_binary = match &config.channel_binary {
            Some(path) => Some(
                tokio::fs::read(path)
                    .await
                    .map_err(|e| CoreError::NotFound(format!("channel binary {path:?}: {e}")))?,
            ),
            None => None,
        };
        let secrets = match &config.age_identity {
            Some(identity) => Some(store::secrets::AgeSecretStore::for_dispatcher(
                store.raw_bucket(store::buckets::SECRETS).await?,
                identity,
            )?),
            None => None,
        };
        let artifacts = crate::platform_ops::harvest::artifact_store(
            &store,
            config.artifacts_identity.as_deref(),
        )
        .await
        .map_err(|e| CoreError::Config(e.to_string()))?;
        let workload_signer = match &config.oidc_issuer {
            Some(keys) => Some(
                auth::workload::WorkloadTokenSigner::new(
                    keys.private_pem.as_bytes(),
                    &keys.public_pem,
                    keys.issuer.clone(),
                )
                .map_err(|e| CoreError::Config(format!("oidc issuer keypair: {e}")))?,
            ),
            None => None,
        };

        let mut core = Self {
            store,
            jobs,
            tasks,
            counters,
            rdeps,
            repos,
            backend,
            provider,
            config,
            channel_binary,
            secrets,
            workload_signer,
            artifacts,
            graphs: HashMap::new(),
            queue: ReadyQueue::default(),
            launch_queue: std::collections::VecDeque::new(),
            active: HashMap::new(),
            merge_gates: HashMap::new(),
            projects,
            pr_api: Arc::new(crate::forge_ingest::github::GithubClient::new()),
            release_holds: HashSet::new(),
            self_tx: None,
            draining: false,
            snapshot: None,
            fleet_roster: Vec::new(),
            last_fleet_status: None,
            announced_workers: HashMap::new(),
            seed_node_names: HashSet::new(),
            started_at: Utc::now(),
            capacity_warned_at: HashMap::new(),
            capacity_intent: crate::capacity::CapacityIntent::default(),
            capacity_pushes: HashMap::new(),
            capacity_intent_warned_at: HashMap::new(),
            schedules: HashMap::new(),
            schedule_ticks: 0,
            trace: None,
            invariant_sink: None,
        };

        core.load_capacity_intent().await;

        for (key, record) in core.projects.list_all().await? {
            if matches!(&record.release, Some(r) if r.status == types::ReleaseStatus::Open)
                && let Some((owner, project)) = key.split_once('.')
            {
                core.release_holds.insert(format!("{owner}/{project}"));
            }
        }

        let all: Vec<Job> = core.jobs.list_all().await?;
        for job in all {
            let (owner, project) = split_slug(&job.project)?;
            for &upstream in &job.deps {
                core.rdeps
                    .append(&owner, &project, upstream, job.id)
                    .await?;
            }
            if job.state == JobState::Ready {
                core.queue.enqueue(QueuedJob {
                    owner: owner.clone(),
                    project: project.clone(),
                    seq: job.id,
                });
            }
            core.graphs
                .entry(job.project.clone())
                .or_default()
                .insert(job);
        }
        Ok(core)
    }

    /// Swap the PR client — integration tests inject a scripted fake.
    pub fn with_pr_api(
        mut self,
        pr_api: Arc<dyn crate::forge_ingest::github::PullRequestApi>,
    ) -> Self {
        self.pr_api = pr_api;
        self
    }

    /// Attach the config-snapshot republish state so the scan tick keeps the
    /// `platform` bucket fresh (CD deploy-drift, see
    /// [`crate::platform_ops::cd`]). Wired by [`crate::run`] after the
    /// boot-time publish.
    pub(crate) fn with_config_snapshot(
        mut self,
        snapshot: crate::platform_ops::cd::ConfigSnapshot,
    ) -> Self {
        self.snapshot = Some(snapshot);
        self
    }

    /// Attach the fleet roster (node names + slot caps) so live occupancy
    /// publishing reports every node — idle ones included — with its capacity
    /// (see [`crate::platform_ops::fleet`]). Wired by [`crate::run`] from the
    /// config snapshot's nodes; tests set it to assert per-node slots.
    pub fn with_fleet_roster(mut self, roster: Vec<types::WorkerNode>) -> Self {
        self.seed_node_names = roster.iter().map(|n| n.name.clone()).collect();
        self.fleet_roster = roster;
        self
    }

    /// Merge a worker announce into the live fleet (spec §3.1 dynamic
    /// registration). Runs on the single-writer actor, so it is the fleet's sole
    /// writer. Records the heartbeat for the timeout scan, hands the node to the
    /// backend (which applies the precedence and the `(epoch, generation)`
    /// ordering), and reflects the membership in the roster the UI reads
    /// (`fleet.status` / the config snapshot). The `Core::run` loop then
    /// re-drains the launch queue and republishes fleet status, so
    /// newly-announced capacity is used at once.
    ///
    /// The roster deliberately does **not** take the announce's slot count for a
    /// node it already knows: the backend owns observed capacity across both
    /// transports and reports it back through `fleet_status`, so a stale
    /// announce that lost the ordering race cannot smuggle its number into the
    /// snapshot by the side door (design #293 §1/§7).
    pub(crate) fn on_worker_announce(&mut self, announce: types::worker::WorkerAnnounce) {
        let (node, version) = (announce.node.clone(), announce.version.clone());
        let capacity = types::CapacityObservation::from_announce(&announce);
        if !self.backend.supports_dynamic_workers() {
            tracing::debug!(node = %node, "ignoring worker announce — backend has no dynamic fleet");
            return;
        }
        self.announced_workers.insert(node.clone(), Utc::now());
        let joined = self
            .backend
            .register_worker(&node, capacity, Some(version.clone()));
        if joined {
            tracing::info!(
                node = %node,
                slots = capacity.slots,
                version = %version,
                "worker joined the live fleet"
            );
        }
        match self.fleet_roster.iter_mut().find(|n| n.name == node) {
            Some(n) if n.endpoint == worker::backend::WORKER_ENDPOINT => {
                n.available = true;
                n.version = Some(version);
            }
            Some(_) => {}
            None => self.fleet_roster.push(types::WorkerNode {
                name: node.clone(),
                endpoint: worker::backend::WORKER_ENDPOINT.to_string(),
                slots: capacity.slots,
                available: true,
                version: Some(version),
                refresh_outcome: None,
                capacity_source: None,
                capacity_observed_at: None,
            }),
        }
        if joined {
            debug_assert!(
                self.fleet_holds(&node),
                "an accepted announce must reach the roster before the capacity \
                 re-assert reads it (design #293 §4)"
            );
            self.reconcile_node_capacity(&node);
        }
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            if let Msg::Drain { reply } = msg {
                let result = self.drain(&mut rx).await;
                let _ = reply.send(result);
                return;
            }
            let occupancy_relevant = !matches!(msg, Msg::Ping { .. });
            let checked = self.invariant_sink.is_some().then(|| msg.label());
            self.handle_msg(msg).await;
            if let Err(e) = self.drain_queue().await {
                tracing::error!("drain_queue: {e}");
            }
            if let Err(e) = self.drain_launch_queue().await {
                tracing::error!("drain_launch_queue: {e}");
            }
            if occupancy_relevant {
                self.refresh_fleet_status().await;
            }
            if let (Some(label), Some(sink)) = (checked, &self.invariant_sink) {
                sink.check(label, &self.state());
            }
        }
    }

    /// Graceful-shutdown drain (spec §3.6): flip into draining mode, process the
    /// backlog already sitting in the mailbox — so a just-launched container's id
    /// lands on its task record and any arrived exit records terminally — then
    /// audit that every Running task carries its real `container_id` before the
    /// process exits. Initiates no new work: the launch paths early-return while
    /// draining, so recovered records are re-derived on restart rather than being
    /// half-launched now. Non-blocking on the mailbox (drains what is present, not
    /// what may yet arrive) and idempotent — even cut short by launchd it only
    /// ever makes records MORE accurate, never worse.
    pub(crate) async fn drain(&mut self, rx: &mut mpsc::Receiver<Msg>) -> Result<()> {
        self.draining = true;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Drain { reply } => {
                    let _ = reply.send(Ok(()));
                }
                other => self.handle_msg(other).await,
            }
        }
        self.flush_running_container_ids().await;
        Ok(())
    }

    /// The drain audit (spec §3.6): stamp any `Running` task still missing its
    /// `container_id` from the live fleet's identity labels, so restart
    /// re-attaches its monitor rather than failing it as container-gone. A launch
    /// records the id via [`Msg::TaskContainerStarted`], but that message can be
    /// in flight when SIGTERM lands; this closes the gap. Best-effort — a backend
    /// hiccup only warns, and a missing stamp is no worse than today.
    async fn flush_running_container_ids(&mut self) {
        let running = match self.backend.list_managed_running().await {
            Ok(cs) => cs,
            Err(e) => {
                tracing::warn!("drain: listing running containers failed: {e}");
                return;
            }
        };
        let mut by_identity: HashMap<(String, u64, u64), String> = HashMap::new();
        for rc in running {
            if let (Some(p), Some(j), Some(t)) = (&rc.project, rc.job, rc.task) {
                by_identity.insert((p.clone(), j, t), rc.id.clone());
            }
        }
        if by_identity.is_empty() {
            return;
        }
        let jobs: Vec<Job> = self
            .graphs
            .values()
            .flat_map(|g| g.jobs().cloned().collect::<Vec<_>>())
            .collect();
        for job in jobs {
            let Ok((owner, project)) = split_slug(&job.project) else {
                continue;
            };
            let tasks = match self.tasks.list_for_job(&owner, &project, job.id).await {
                Ok(tasks) => tasks,
                Err(e) => {
                    tracing::warn!("drain: listing tasks for job {} failed: {e}", job.id);
                    continue;
                }
            };
            for mut task in tasks {
                if task.state == types::TaskState::Running
                    && task.container_id.is_none()
                    && let Some(id) = by_identity.get(&(job.project.clone(), job.id, task.id))
                {
                    task.container_id = Some(id.clone());
                    if let Err(e) = self.task_put(&task).await {
                        tracing::warn!(
                            "drain: stamping container id for {}/{} failed: {e}",
                            job.id,
                            task.id
                        );
                    }
                }
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
    async fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Ping { reply } => {
                let _ = reply.send(Ok(()));
            }
            Msg::QueueSnapshot {
                owner,
                project,
                reply,
            } => {
                let _ = reply.send(Ok(self.queue_snapshot(&owner, &project)));
            }
            Msg::CreateJob(req, reply) => {
                let _ = reply.send(self.create_job(req).await);
            }
            Msg::ReleaseJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.release_job(&owner, &project, seq).await);
            }
            Msg::RevokeJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.revoke_job(&owner, &project, seq).await);
            }
            Msg::UpdateJob(req, reply) => {
                let _ = reply.send(self.update_job(req).await);
            }
            Msg::DraftJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.draft_job(&owner, &project, seq).await);
            }
            Msg::FinalizeJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.finalize_job(&owner, &project, seq).await);
            }
            Msg::TriageJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.triage_job(&owner, &project, seq).await);
            }
            Msg::EditMembers {
                owner,
                project,
                seq,
                add,
                remove,
                reply,
            } => {
                let _ = reply.send(self.edit_members(&owner, &project, seq, add, remove).await);
            }
            Msg::EditGroups {
                owner,
                project,
                seq,
                add,
                remove,
                reply,
            } => {
                let _ = reply.send(self.edit_groups(&owner, &project, seq, add, remove).await);
            }
            Msg::SetRequireApproval {
                owner,
                project,
                seq,
                require,
                reply,
            } => {
                let _ = reply.send(
                    self.set_require_approval(&owner, &project, seq, require)
                        .await,
                );
            }
            Msg::ClaimJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.claim_job(&owner, &project, seq).await);
            }
            Msg::UnclaimJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.unclaim_job(&owner, &project, seq).await);
            }
            Msg::SubmitResult {
                owner,
                project,
                seq,
                submission,
                reply,
            } => {
                let _ = reply.send(
                    self.handle_submit_result(&owner, &project, seq, submission)
                        .await,
                );
            }
            Msg::SubmitEval {
                owner,
                project,
                seq,
                task_id,
                submission,
                reply,
            } => {
                let _ = reply.send(
                    self.handle_submit_eval(&owner, &project, seq, task_id, submission)
                        .await,
                );
            }
            Msg::ResolveTask {
                owner,
                project,
                seq,
                task_id,
                resolution,
                operator,
                reply,
            } => {
                let _ = reply.send(
                    self.handle_resolve_task(&owner, &project, seq, task_id, resolution, &operator)
                        .await,
                );
            }
            Msg::LinkProject {
                owner,
                name,
                origin_url,
                main_branch,
                reply,
            } => {
                let _ = reply.send(
                    self.link_project(&owner, &name, &origin_url, main_branch.as_deref())
                        .await,
                );
            }
            Msg::OriginRelease {
                owner,
                project,
                reply,
            } => {
                let _ = reply.send(self.origin_release(&owner, &project).await);
            }
            Msg::OriginStatus {
                owner,
                project,
                reply,
            } => {
                let _ = reply.send(self.origin_status(&owner, &project).await);
            }
            Msg::OriginSync {
                owner,
                project,
                reply,
            } => {
                let _ = reply.send(self.origin_sync(&owner, &project).await);
            }
            Msg::Drain { reply } => {
                let _ = reply.send(Ok(()));
            }
            Msg::Scan { reply } => {
                let result = self.run_scans().await;
                match reply {
                    Some(reply) => {
                        let _ = reply.send(result);
                    }
                    None => {
                        if let Err(e) = result {
                            tracing::error!("scan: {e}");
                        }
                    }
                }
            }
            Msg::ChannelPost {
                owner,
                project,
                seq,
                post,
                reply,
            } => {
                let _ = reply.send(self.on_channel_post(&owner, &project, seq, post).await);
            }
            Msg::TaskExited {
                owner,
                project,
                seq,
                task_id,
                exit,
            } => {
                if let Err(e) = self
                    .on_task_exited(&owner, &project, seq, task_id, exit)
                    .await
                {
                    tracing::error!("task exit handling for {owner}/{project}#{seq}: {e}");
                }
            }
            Msg::TaskContainerStarted {
                owner,
                project,
                seq,
                task_id,
                container_id,
            } => {
                if let Err(e) = self
                    .on_container_started(&owner, &project, seq, task_id, container_id)
                    .await
                {
                    tracing::error!("container-start handling for {owner}/{project}#{seq}: {e}");
                }
            }
            Msg::LaunchDeferred {
                owner,
                project,
                seq,
                task_id,
                reason,
            } => {
                if let Err(e) = self
                    .on_launch_deferred(&owner, &project, seq, task_id, reason)
                    .await
                {
                    tracing::error!("deferring launch for {owner}/{project}#{seq}: {e}");
                }
            }
            Msg::WorkerAnnounce { announce } => {
                self.on_worker_announce(announce);
            }
            Msg::SetNodeCapacity {
                node,
                slots,
                by,
                reply,
            } => {
                let result = self.set_node_capacity(node, slots, by).await;
                let _ = reply.send(result);
            }
            Msg::CapacityPushed {
                node,
                slots,
                outcome,
            } => {
                self.on_capacity_pushed(&node, slots, &outcome);
            }
        }
    }

    /// Stamp a just-launched container's id onto its task record (§3.2). Runs on
    /// the single-writer loop so it never races the exit handler; a no-op if the
    /// task vanished or already carries an id (a retry reused the slot).
    pub(crate) async fn on_container_started(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        container_id: String,
    ) -> Result<()> {
        let Some(mut task) = self.tasks.get(owner, project, seq, task_id).await? else {
            return Ok(());
        };
        if task.container_id.is_some() {
            return Ok(());
        }
        task.container_id = Some(container_id);
        self.task_put(&task).await?;
        self.publish_task_launched(owner, project, seq, &task).await
    }

    /// Announce that a task's container was placed (§6.3), carrying the
    /// identities it minted in place of the tokens themselves (§10.3, #313 A6).
    /// Every launch path calls this from the site that confirmed the placement,
    /// exactly once, so no event claims a delivery that never happened.
    pub(crate) async fn publish_task_launched(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task: &types::Task,
    ) -> Result<()> {
        self.publish(
            owner,
            project,
            seq,
            "task-launched",
            crate::workload::task_launched_payload(task),
        )
        .await
    }

    /// Build a [`agent::LaunchReporter`] wired back into the core: it spawns a
    /// tiny forwarder that turns the provider's launch signal into a
    /// [`Msg::TaskContainerStarted`], so the container id reaches the task
    /// record through the single-writer loop rather than a direct mutation.
    #[allow(
        clippy::expect_used,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
    pub(crate) fn launch_reporter(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
    ) -> agent::LaunchReporter {
        let (tx_id, mut rx_id) = mpsc::unbounded_channel();
        let tx = self.self_tx.clone().expect("spawned core");
        let (owner, project) = (owner.to_string(), project.to_string());
        tokio::spawn(async move {
            if let Some(container_id) = rx_id.recv().await {
                let _ = tx
                    .send(Msg::TaskContainerStarted {
                        owner,
                        project,
                        seq,
                        task_id,
                        container_id,
                    })
                    .await;
            }
        });
        agent::LaunchReporter::new(tx_id)
    }

    /// §3.1 step 5: launch every queued Ready job. Slot caps live in the
    /// backend (fleet) — the core does not throttle.
    pub(crate) async fn drain_queue(&mut self) -> Result<()> {
        if self.draining {
            return Ok(());
        }
        while let Some(q) = self.queue.dequeue() {
            self.start_job(q).await?;
        }
        Ok(())
    }

    /// Handle `req.jobs.create.*` (spec §3.1 step 1). Jobs land Frozen by
    /// default; with `draft: true` they land in [`JobState::Draft`] for
    /// editing (§2.1). Either way, wiring is validated at release, not creation.
    pub async fn create_job(&mut self, req: CreateSpec) -> Result<Job> {
        if !req.members.is_empty() {
            return self.create_batch(req).await;
        }
        let seq = self.counters.next(&req.owner, &req.project).await?;
        let job = Job {
            id: seq,
            project: format!("{}/{}", req.owner, req.project),
            r#type: req.r#type,
            title: req.title,
            description: req.description,
            cover_html: req.cover_html,
            deps: req.deps,
            members: vec![],
            batch_id: None,
            state: if req.draft {
                JobState::Draft
            } else {
                JobState::Frozen
            },
            branch: format!("job/{seq}"),
            base_ref: None,
            knowledge_tags: req.knowledge_tags,
            eval: req.eval,
            require_approval: req.require_approval,
            timeout: req.timeout,
            model: req.model,
            inputs: req.inputs,
            groups: req.groups,
            claim_next: false,
            escalation: None,
            factory: req.factory,
            schedule: req.schedule,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
            task_time_ms: None,
        };
        self.jobs.put(&job).await?;
        for &upstream in &job.deps {
            let _ = self
                .rdeps
                .append(&req.owner, &req.project, upstream, seq)
                .await;
        }
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        let mut extra = serde_json::json!({});
        inputs::stamp_event_inputs(&mut extra, &job.inputs);
        stamp_event_schedule(&mut extra, job.schedule.as_deref());
        self.publish(&req.owner, &req.project, seq, "job-created", extra)
            .await?;
        Ok(job)
    }

    /// Create a **batch** (spec §2.1 batches): a job that absorbs N Frozen
    /// members of the same type, unions their external deps and additive
    /// evaluators, and lands one branch whose single merge completes them all.
    ///
    /// A plain `POST jobs {members}` (default `draft:false`) is **atomic**: the
    /// members are validated (≥2, each existing, Frozen, same-type, not already
    /// batched, not itself a batch), their external-dep and evaluator unions and
    /// auto-description computed, and each member absorbed Frozen→Batched — all
    /// at create. `draft:true` instead stages a **Draft batch**: the member list
    /// is validated per-candidate but **not absorbed** (members stay Frozen and
    /// claimable/batchable elsewhere); membership is edited via
    /// [`Core::edit_members`] and absorption is deferred to finalize/release,
    /// which recompute the unions against *current* state ([`Core::absorb_plan`]).
    async fn create_batch(&mut self, req: CreateSpec) -> Result<Job> {
        let (owner, project) = (req.owner.clone(), req.project.clone());
        let member_seqs = req.members.clone();

        let min = if req.draft { 1 } else { 2 };
        let comp = self.plan_batch(&owner, &project, &req.r#type, &member_seqs, min)?;
        let (deps, eval, require_approval, description) = Self::create_batch_committed(&req, comp);

        let seq = self.counters.next(&owner, &project).await?;
        let batch = Job {
            id: seq,
            project: format!("{owner}/{project}"),
            r#type: req.r#type,
            title: req.title,
            description,
            cover_html: req.cover_html,
            deps,
            members: member_seqs.clone(),
            batch_id: None,
            state: if req.draft {
                JobState::Draft
            } else {
                JobState::Frozen
            },
            branch: format!("job/{seq}"),
            base_ref: None,
            knowledge_tags: req.knowledge_tags,
            eval,
            require_approval,
            timeout: req.timeout,
            model: req.model,
            inputs: req.inputs,
            groups: req.groups,
            claim_next: false,
            escalation: None,
            factory: req.factory,
            schedule: req.schedule,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
            task_time_ms: None,
        };
        self.jobs.put(&batch).await?;
        for &upstream in &batch.deps {
            let _ = self.rdeps.append(&owner, &project, upstream, seq).await;
        }
        self.graphs
            .entry(batch.project.clone())
            .or_default()
            .insert(batch.clone());
        let mut extra = serde_json::json!({});
        inputs::stamp_event_inputs(&mut extra, &batch.inputs);
        stamp_event_schedule(&mut extra, batch.schedule.as_deref());
        self.publish(&owner, &project, seq, "job-created", extra)
            .await?;

        if !req.draft {
            self.absorb_batch(&owner, &project, seq, &member_seqs)
                .await?;
        }
        Ok(batch)
    }

    /// The deps / evaluators / approval gate / description a new batch commits
    /// (spec §2.1). A **Draft** batch commits none of the composition — its
    /// membership is not absorbed yet, so finalize/release recomputes the unions
    /// against current state; an atomic batch commits them all, with the approval
    /// gate ORed over the members.
    fn create_batch_committed(
        req: &CreateSpec,
        comp: types::BatchComposition,
    ) -> (Vec<u64>, Vec<types::Evaluator>, bool, String) {
        if req.draft {
            return (
                Vec::new(),
                Vec::new(),
                req.require_approval,
                req.description.clone(),
            );
        }
        let description = if req.description.is_empty() {
            Self::batch_auto_description(&req.r#type, &req.members)
        } else {
            req.description.clone()
        };
        (
            comp.deps,
            comp.eval,
            req.require_approval || comp.require_approval,
            description,
        )
    }

    /// The batch membership rules (spec §2.1), resolved against this project's
    /// graph: see [`authoring::validate_member`], which owns them. Shared by
    /// every path that admits a member — atomic create, draft-member edits, and
    /// the finalize/release re-validation.
    fn validate_member(
        &self,
        owner: &str,
        project: &str,
        ty: &str,
        m: u64,
        errs: &mut Vec<ValidationError>,
    ) -> Option<Job> {
        authoring::validate_member(self.graph(owner, project), ty, m, errs)
    }

    /// The batch's composition against this project's graph: see
    /// [`authoring::plan_batch`], which owns the rules and the unions. No
    /// mutation — the caller absorbs; a violated rule surfaces as
    /// [`CoreError::Validation`] (the 422), exactly as before the rules moved.
    fn plan_batch(
        &self,
        owner: &str,
        project: &str,
        ty: &str,
        member_seqs: &[u64],
        min_members: usize,
    ) -> Result<BatchComposition> {
        Ok(authoring::plan_batch(
            self.graph(owner, project),
            ty,
            member_seqs,
            min_members,
        )?)
    }

    /// The auto-index description a batch defaults to (spec §2.1):
    /// `Batch of N {type} jobs: #a #b …`.
    fn batch_auto_description(ty: &str, member_seqs: &[u64]) -> String {
        authoring::batch_auto_description(ty, member_seqs)
    }

    /// Absorb a batch's members: each Frozen→Batched with `batch_id` set,
    /// emitting `job-batched` (spec §2.1). Shared by atomic create and the
    /// finalize/release paths where a Draft batch commits its members.
    pub(crate) async fn absorb_batch(
        &mut self,
        owner: &str,
        project: &str,
        batch_seq: u64,
        member_seqs: &[u64],
    ) -> Result<()> {
        for &m in member_seqs {
            let mut member = self.must_get(owner, project, m)?.clone();
            member.batch_id = Some(batch_seq);
            self.set_state(&mut member, JobState::Batched).await?;
            self.publish(
                owner,
                project,
                m,
                "job-batched",
                serde_json::json!({ "batch_id": batch_seq }),
            )
            .await?;
        }
        Ok(())
    }

    /// Return a batch's absorbed members to Frozen: each Batched→Frozen with
    /// `batch_id` cleared, emitting `job-unbatched` (spec §2.1). A member that
    /// is not Batched (a Draft batch that never absorbed) is left untouched.
    /// Shared by revoke (batch dropped) and Frozen→Draft (batch reopened for
    /// editing).
    async fn release_batch_members(
        &mut self,
        owner: &str,
        project: &str,
        batch_seq: u64,
        members: &[u64],
    ) -> Result<()> {
        for &m in members {
            let mut member = self.must_get(owner, project, m)?.clone();
            if member.state == JobState::Batched {
                member.batch_id = None;
                self.set_state(&mut member, JobState::Frozen).await?;
                self.publish(
                    owner,
                    project,
                    m,
                    "job-unbatched",
                    serde_json::json!({ "batch_id": batch_seq }),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Re-validate a Draft batch's members against *current* state and compute
    /// the composition to commit when it leaves Draft (finalize or release,
    /// spec §2.1). Mutates `job` in place with the recomputed dep/eval unions
    /// and, if its description is still empty, the auto-index — exactly what an
    /// atomic create would have written. A stale member (released/claimed/
    /// batched meanwhile, or the list now below 2) yields a field error and
    /// `job` is left untouched, so the caller keeps it Draft with nothing
    /// absorbed. Returns the member list to absorb once the caller's own
    /// validation passes.
    #[allow(
        clippy::expect_used,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
    fn absorb_plan(&self, job: &mut Job) -> Result<Vec<u64>> {
        let (owner, project) = job
            .project
            .split_once('/')
            .expect("project slug is owner/project");
        let comp = self.plan_batch(owner, project, &job.r#type, &job.members, 2)?;
        job.deps = comp.deps;
        job.eval = comp.eval;
        job.require_approval |= comp.require_approval;
        if job.description.is_empty() {
            job.description = Self::batch_auto_description(&job.r#type, &job.members);
        }
        Ok(job.members.clone())
    }

    /// Handle `req.jobs.release.*` (spec §2.2 release-time pass + §2.1
    /// Frozen→Ready|Blocked). Returns the resulting state.
    pub async fn release_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<JobState> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if !matches!(job.state, JobState::Frozen | JobState::Draft) {
            return Err(InvalidTransition {
                from: job.state,
                to: JobState::Ready,
            }
            .into());
        }
        let from_draft = job.state == JobState::Draft;

        let batch_members = if from_draft && job.is_batch() {
            let members = self.absorb_plan(&mut job)?;
            self.graphs
                .entry(job.project.clone())
                .or_default()
                .insert(job.clone());
            members
        } else {
            Vec::new()
        };

        let (head, declared_inputs) = self.release_validation(owner, project, &job).await?;

        self.run_ready(
            owner,
            project,
            seq,
            ready::ReadyEvent::Released {
                head,
                from_draft,
                absorb: batch_members,
                declared_inputs,
            },
        )
        .await?;
        Ok(self.must_get(owner, project, seq)?.state)
    }

    /// The §2.2 release-time pass: resolve the default branch HEAD and run the
    /// wiring and static checks against the job exactly as it will be committed
    /// (a Draft batch's unions are already folded in). Returns the validated
    /// HEAD — the commit an admitted job pins as its `base_ref` — and that HEAD's
    /// declared `inputs:`, so an admission materializes defaults from the very
    /// tree it validated (design #311 Decision 3). Unlike the Blocked→Ready
    /// re-validation this one checks KV names too, since a release is where an
    /// operator learns a declared secret is missing.
    async fn release_validation(
        &mut self,
        owner: &str,
        project: &str,
        job: &Job,
    ) -> Result<(String, Vec<types::Input>)> {
        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;
        let seq = job.id;

        let job_type =
            release::load_job_type(&self.repos, owner, project, &head, &job.r#type, Some(seq))
                .await?;
        let job_type = release::with_job_evaluators(job_type, job)?;
        let graph = self.graphs.entry(job.project.clone()).or_default();
        let mut errs = release::wiring_errors(job, graph);
        let kv = self.kv_names(owner, project).await?;
        errs.extend(
            release::static_errors(
                &self.repos,
                owner,
                project,
                &head,
                job,
                &job_type,
                Some(&kv),
            )
            .await?,
        );
        if !errs.is_empty() {
            return Err(errs.into());
        }
        Ok((head, job_type.inputs))
    }

    /// Handle a job reaching Done (spec §3.1 step 2): unblock dependents whose
    /// dependencies are now all Done, re-validating static config at the
    /// freshly pinned `base_ref` (§2.2 Ready-transition pass).
    pub async fn on_job_done(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let slug = format!("{owner}/{project}");
        let dependents: Vec<u64> = self
            .graphs
            .get(&slug)
            .map(|g| g.dependents(seq).to_vec())
            .unwrap_or_default();

        for dep_seq in dependents {
            self.try_unblock(owner, project, dep_seq).await?;
        }
        Ok(())
    }

    /// Handle `req.jobs.revoke.*` (spec §2.1 Revoked row). Returns the seqs of
    /// cascaded dependents.
    pub async fn revoke_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<Vec<u64>> {
        let job = self.must_get(owner, project, seq)?.clone();
        assert_transition(job.state, JobState::Revoked)?;

        let slug = format!("{owner}/{project}");
        let cascaded = self
            .graphs
            .get(&slug)
            .map(|g| g.cascade_targets(seq))
            .unwrap_or_default();

        let slug = format!("{owner}/{project}");
        for &target in std::iter::once(&seq).chain(cascaded.iter()) {
            self.kill_running_containers(owner, project, target).await;
            self.close_pending_tasks(owner, project, target).await;
            self.spawn_outputs_gc(owner, project, target);
            self.active
                .remove(&(owner.to_string(), project.to_string(), target));
            let mut j = self.must_get(owner, project, target)?.clone();
            self.set_state(&mut j, JobState::Revoked).await?;
            let _ = self.repos.delete_branch(owner, project, &j.branch).await;
            let _ = self
                .repos
                .delete_branch(owner, project, &format!("merge-gate/{target}"))
                .await;
            self.queue.remove(&queue::QueuedJob {
                owner: owner.into(),
                project: project.into(),
                seq: target,
            });
            if let Some(state) = self.merge_gates.get_mut(&slug) {
                state.remove(target);
                if state.is_empty() {
                    self.merge_gates.remove(&slug);
                }
            }
        }
        let members = job.members.clone();
        self.release_batch_members(owner, project, seq, &members)
            .await?;
        self.pump_merges(owner, project).await?;
        self.publish(
            owner,
            project,
            seq,
            "job-revoked",
            serde_json::json!({ "cascaded": cascaded }),
        )
        .await?;
        Ok(cascaded)
    }

    /// Drop a revoked job's output archives (design #362 R2), off the actor
    /// thread and best-effort — the same never-fail-a-job discipline as
    /// `dispose`. Outputs only: transcripts, stdout and attachments survive,
    /// because a revoked job is still an audit record.
    fn spawn_outputs_gc(&self, owner: &str, project: &str, seq: u64) {
        let harvest = self.harvester();
        let (o, p) = (owner.to_string(), project.to_string());
        tokio::spawn(async move { harvest.delete_outputs(&o, &p, seq).await });
    }

    /// Handle `req.jobs.update.*` (spec §2.1): full-field replace of a Draft
    /// job's definition. Only a job in Draft is editable — any other state is a
    /// 409 (`Conflict`), so a released job is never mutated. Validation is
    /// identical to create (deferred to release), so the edit just rewrites the
    /// record and publishes `job-updated` naming the fields that changed.
    pub async fn update_job(&mut self, req: UpdateJobRequest) -> Result<Job> {
        let mut job = self.must_get(&req.owner, &req.project, req.seq)?.clone();
        if job.state != JobState::Draft {
            return Err(CoreError::Conflict(format!(
                "job {} is {:?}; only a Draft job can be edited",
                req.seq, job.state
            )));
        }
        let changed = Self::update_job_changed_fields(&job, &req);
        let UpdateJobRequest {
            owner,
            project,
            seq,
            r#type,
            title,
            description,
            cover_html,
            deps,
            knowledge_tags,
            eval,
            require_approval,
            timeout,
            model,
            inputs,
            groups,
        } = req;

        let old_deps = job.deps.clone();

        job.r#type = r#type;
        job.title = title;
        job.description = description;
        job.cover_html = cover_html;
        job.deps = deps;
        job.knowledge_tags = knowledge_tags;
        job.eval = eval;
        job.require_approval = require_approval;
        job.timeout = timeout;
        job.model = model;
        job.inputs = inputs;
        job.groups = groups;

        self.jobs.put(&job).await?;
        for &upstream in &job.deps {
            let _ = self.rdeps.append(&owner, &project, upstream, seq).await;
        }
        for &upstream in &old_deps {
            if !job.deps.contains(&upstream) {
                let _ = self.rdeps.remove(&owner, &project, upstream, seq).await;
            }
        }
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        self.publish(
            &owner,
            &project,
            seq,
            "job-updated",
            serde_json::json!({ "fields": changed }),
        )
        .await?;
        Ok(job)
    }

    /// Name every field of a Draft edit whose new value differs from the old, so
    /// the `job-updated` event carries changed field names rather than a full
    /// payload (§2.1 events). Every field [`Core::update_job`] replaces has a row
    /// here — a field with no row would be edited invisibly.
    fn update_job_changed_fields(job: &Job, req: &UpdateJobRequest) -> Vec<&'static str> {
        let mut changed: Vec<&'static str> = Vec::new();
        for (differs, field) in [
            (job.r#type != req.r#type, "type"),
            (job.title != req.title, "title"),
            (job.description != req.description, "description"),
            (job.cover_html != req.cover_html, "cover_html"),
            (job.deps != req.deps, "deps"),
            (job.knowledge_tags != req.knowledge_tags, "knowledge_tags"),
            (job.eval != req.eval, "eval"),
            (
                job.require_approval != req.require_approval,
                "require_approval",
            ),
            (job.timeout != req.timeout, "timeout"),
            (job.model != req.model, "model"),
            (job.inputs != req.inputs, "inputs"),
            (job.groups != req.groups, "groups"),
        ] {
            if differs {
                changed.push(field);
            }
        }
        changed
    }

    /// Handle `req.jobs.draft.*` (spec §2.1): move a Frozen (never-released)
    /// job back to Draft for editing. Only Frozen → Draft — `set_state`'s guard
    /// rejects any other origin as an invalid transition (409).
    pub async fn draft_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        self.set_state(&mut job, JobState::Draft).await?;
        if job.is_batch() {
            let members = job.members.clone();
            self.release_batch_members(owner, project, seq, &members)
                .await?;
        }
        self.publish(owner, project, seq, "job-drafted", serde_json::json!({}))
            .await
    }

    /// Handle `req.jobs.members.*` (spec §2.1 draft batches): add/remove the
    /// members of a **Draft** batch while composing it. Draft-only — any other
    /// state (or a non-batch job) is a 409 (`Conflict`), so a committed batch's
    /// membership is never mutated in place. Adds are re-validated per-candidate
    /// against current state (exists, Frozen, same type, unbatched, not a
    /// batch); a member is **not** absorbed here (a draft holds a non-binding
    /// list — absorption happens at finalize/release). Removes are trivial —
    /// nothing was absorbed. The result keeps at least one member so the batch
    /// retains its identity (`members` non-empty *is* the batch marker, §2.1);
    /// revoke the draft to discard it entirely. Emits `job-updated {members}`.
    pub async fn edit_members(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        add: Vec<u64>,
        remove: Vec<u64>,
    ) -> Result<Job> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if !job.is_batch() {
            return Err(CoreError::Conflict(format!(
                "job {seq} is not a batch; it has no members to edit"
            )));
        }
        if job.state != JobState::Draft {
            return Err(CoreError::Conflict(format!(
                "job {seq} is {:?}; only a Draft batch's members can be edited",
                job.state
            )));
        }

        let mut errs: Vec<ValidationError> = Vec::new();
        let mut present: HashSet<u64> = job.members.iter().copied().collect();
        for &a in &add {
            if !present.insert(a) {
                errs.push(ValidationError::new(
                    Some(a),
                    "members",
                    format!("member #{a} is already in the batch"),
                ));
                continue;
            }
            self.validate_member(owner, project, &job.r#type, a, &mut errs);
        }
        if !errs.is_empty() {
            return Err(errs.into());
        }

        let mut members = job.members.clone();
        members.retain(|m| !remove.contains(m));
        for &a in &add {
            if !members.contains(&a) {
                members.push(a);
            }
        }
        if members.is_empty() {
            return Err(CoreError::Conflict(format!(
                "these removals would empty batch {seq}; a batch keeps at least one member (revoke it to discard)"
            )));
        }

        job.members = members;
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        self.publish(
            owner,
            project,
            seq,
            "job-updated",
            serde_json::json!({ "fields": ["members"] }),
        )
        .await?;
        Ok(job)
    }

    /// Handle `req.jobs.groups.*` (spec §6.2, design #321 Decision 5): add and
    /// remove the operator's group labels on a job.
    ///
    /// **Accepted in every state, including `Done` and `Revoked`** — there is no
    /// state guard here and that is the design, not an omission. What "terminal
    /// jobs are immutable" protects is the *execution* record (what ran, against
    /// which `base_ref`, judged how); [`Job::groups`] is inert to all of it, so
    /// annotating a finished ticket with what it was part of changes nothing it
    /// did. Retroactive grouping is the requirement, not a nicety: the jobs of
    /// the group that motivated #321 are all Done.
    ///
    /// **Add/remove rather than a whole-list replace**, so it is idempotent and
    /// two operators grouping the same job from two tabs both succeed where a
    /// replace would lose one. Removes apply first, so a name in both lists ends
    /// up present. The resulting list is re-checked whole ([`types::check_groups`])
    /// — the bounds are on the *result*, not on the delta. Emits `job-updated`
    /// with `{"fields": ["groups"]}`; a request that changes nothing writes
    /// nothing and announces nothing.
    pub async fn edit_groups(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        add: Vec<String>,
        remove: Vec<String>,
    ) -> Result<Job> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        let state_before = job.state;

        let mut groups = job.groups.clone();
        groups.retain(|g| !remove.contains(g));
        for name in &add {
            if !groups.contains(name) {
                groups.push(name.clone());
            }
        }
        types::check_groups(&groups).map_err(|e| {
            CoreError::Validation(vec![ValidationError::new(
                Some(seq),
                "groups",
                e.to_string(),
            )])
        })?;
        if groups == job.groups {
            return Ok(job);
        }

        job.groups = groups;
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        debug_assert_eq!(job.state, state_before, "a group edit never moves a job");
        self.publish(
            owner,
            project,
            seq,
            "job-updated",
            serde_json::json!({ "fields": ["groups"] }),
        )
        .await?;
        Ok(job)
    }

    /// Handle `req.jobs.approval.*` (spec §1.1 require-approval): set or clear the
    /// job's operator sign-off gate, in the pre-Work states only — Draft, Frozen,
    /// Blocked, Ready, Stalled. Past Work entry the criteria are already resolved,
    /// so the edit is a 422 naming the state; a request that changes nothing
    /// writes nothing and announces nothing, like [`Core::edit_groups`].
    pub async fn set_require_approval(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        require: bool,
    ) -> Result<Job> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if job.require_approval == require {
            return Ok(job);
        }
        if !matches!(
            job.state,
            JobState::Draft
                | JobState::Frozen
                | JobState::Blocked
                | JobState::Ready
                | JobState::Stalled
        ) {
            return Err(CoreError::Validation(vec![ValidationError::new(
                Some(seq),
                "require_approval",
                format!(
                    "job {seq} is {:?}; the approval gate is only editable before the job enters \
                     Work, where its evaluation criteria are resolved",
                    job.state
                ),
            )]));
        }
        let state_before = job.state;
        job.require_approval = require;
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        debug_assert_eq!(
            job.state, state_before,
            "an approval edit never moves a job"
        );
        self.publish(
            owner,
            project,
            seq,
            "job-updated",
            serde_json::json!({ "fields": ["require_approval"] }),
        )
        .await?;
        Ok(job)
    }

    /// Handle `req.jobs.finalize.*` (#166): Draft → Frozen. Finalizes an edited
    /// Draft's definition — validating the field rules and evaluator collisions
    /// exactly as release does — but parks the job Frozen (re-batchable) instead
    /// of scheduling it. Draft-only: any other state is a 409 (`InvalidTransition`).
    /// Wiring and static config (deps, prompt files, KV) stay deferred to
    /// release, matching a freshly-created Frozen job (§2.1); validation failure
    /// returns field errors (422) and the job stays Draft.
    pub async fn finalize_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if job.state != JobState::Draft {
            return Err(InvalidTransition {
                from: job.state,
                to: JobState::Frozen,
            }
            .into());
        }

        let batch_members = if job.is_batch() {
            self.absorb_plan(&mut job)?
        } else {
            Vec::new()
        };

        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;
        let job_type =
            release::load_job_type(&self.repos, owner, project, &head, &job.r#type, Some(seq))
                .await?;
        release::with_job_evaluators(job_type, &job)?;

        self.set_state(&mut job, JobState::Frozen).await?;
        if !batch_members.is_empty() {
            let deps = job.deps.clone();
            for upstream in deps {
                let _ = self.rdeps.append(owner, project, upstream, seq).await;
            }
            self.absorb_batch(owner, project, seq, &batch_members)
                .await?;
        }
        self.publish(owner, project, seq, "job-finalized", serde_json::json!({}))
            .await
    }

    /// Handle `req.jobs.claim.*` (spec §1.2 claims): mark the job's next work
    /// attempt as human-performed. The claim rides on the job record until
    /// `launch_work_task` consumes it — the same serialized code path that
    /// would launch the container — so an attempt is either launched or
    /// parked, never both. 409 while an attempt is in flight: a Running work
    /// task, or (job in Work) an already-parked Pending one. Idempotent on an
    /// already-claimed job.
    pub async fn claim_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if job.state.is_terminal() {
            return Err(CoreError::Conflict(format!(
                "job {seq} is {:?}; nothing left to claim",
                job.state
            )));
        }
        if job.state == JobState::Draft {
            return Err(CoreError::Conflict(format!(
                "job {seq} is Draft; release it before claiming a work attempt"
            )));
        }
        if job.state == JobState::Batched {
            return Err(CoreError::Conflict(format!(
                "job {seq} is Batched; claim its batch (#{}), not the member",
                job.batch_id.map_or_else(|| "?".into(), |b| b.to_string())
            )));
        }
        let in_flight = self
            .tasks
            .list_for_job(owner, project, seq)
            .await?
            .iter()
            .any(|t| {
                t.phase == types::TaskPhase::Work
                    && t.evaluator.is_none()
                    && (t.state == types::TaskState::Running
                        || (t.state == types::TaskState::Pending && job.state == JobState::Work))
            });
        if in_flight {
            return Err(CoreError::Conflict(format!(
                "job {seq} has a work attempt in flight; resolve or revoke it first"
            )));
        }
        if job.claim_next {
            return Ok(());
        }
        job.claim_next = true;
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job);
        self.publish(owner, project, seq, "job-claimed", serde_json::json!({}))
            .await
    }

    /// Handle `req.jobs.unclaim.*`: clear a claim that has not materialized
    /// into a parked task. Once parked, the way out is resolving the task
    /// (Pass or Fail) — one attempt, one human decision.
    pub async fn unclaim_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        if !job.claim_next {
            return Err(CoreError::Conflict(format!(
                "job {seq} has no pending claim (a materialized claim is resolved via its task)"
            )));
        }
        job.claim_next = false;
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job);
        self.publish(owner, project, seq, "job-unclaimed", serde_json::json!({}))
            .await
    }

    pub(crate) async fn kill_running_containers(&self, owner: &str, project: &str, seq: u64) {
        let Ok(tasks) = self.tasks.list_for_job(owner, project, seq).await else {
            return;
        };
        for t in tasks {
            if t.state == types::TaskState::Running
                && let Some(cid) = &t.container_id
            {
                let _ = self.backend.kill(cid).await;
            }
        }
    }

    /// Force-close a revoked job's Pending human/escalation tasks (spec §1.2,
    /// §2.1 Revoked transition): a terminal job keeps no live inbox item, the
    /// same way its Running containers are killed. Each is marked `Done` with a
    /// synthetic `TaskResult::Human` (`operator: "system"`, `action: Revoke`)
    /// so the task log stays truthful and nothing left in the inbox resolves to
    /// a job that no longer exists.
    pub(crate) async fn close_pending_tasks(&mut self, owner: &str, project: &str, seq: u64) {
        let Ok(tasks) = self.tasks.list_for_job(owner, project, seq).await else {
            return;
        };
        for t in tasks {
            let human_pending = t.state == types::TaskState::Pending
                && (matches!(t.kind, types::TaskKind::Human { .. })
                    || t.performed_by == Some(types::Performer::Human));
            if !human_pending {
                continue;
            }
            let now = Utc::now();
            let mut closed = t;
            closed.result = Some(types::TaskResult::Human {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({ "closed_by": "revoke" })),
                action: Some(types::EscalationAction::Revoke),
                operator: "system".into(),
                resolved_at: now,
                summary: None,
            });
            closed.state = types::TaskState::Done;
            closed.completed_at = Some(now);
            let _ = self.task_put(&closed).await;
        }
    }

    /// Create a Human escalation task and move the job to Escalated. `reason` is
    /// the machine code (also the event reason); `detail` is the human-readable
    /// explanation shown in the intervention task and mirrored onto the job's
    /// [`types::Escalation`] record; `failing_task` names the task whose failure
    /// triggered this, when one exists.
    pub(crate) async fn escalate(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        reason: &str,
        detail: String,
        failing_task: Option<u64>,
    ) -> Result<()> {
        let cycle = self
            .active
            .get(&(owner.to_string(), project.to_string(), seq))
            .map(|e| e.cycle)
            .unwrap_or(1);
        self.run_escalation(
            owner,
            project,
            seq,
            escalation::EscalationKind::Escalate,
            reason,
            detail,
            failing_task,
            cycle,
        )
        .await
    }

    /// Create a Human escalation task and move the job to Stalled — the
    /// pre-work counterpart of [`escalate`] (§1.2 pre-Work escalations). Used
    /// when no work task exists: Ready-transition re-validation failure, or a
    /// job_deadline that elapsed while the job was still Ready. The operator
    /// resolves Retry (re-run the failed step) or Revoke only.
    pub(crate) async fn stall(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        reason: &str,
        detail: String,
        failing_task: Option<u64>,
    ) -> Result<()> {
        self.run_escalation(
            owner,
            project,
            seq,
            escalation::EscalationKind::Stall,
            reason,
            detail,
            failing_task,
            1,
        )
        .await
    }

    /// The C1 template shim (docs/reference/contracts.md §2): gather the reads into the view,
    /// call the pure decider, apply its transitions through the `set_state`
    /// funnel, run its effects through the interpreter. Every later phase
    /// decider's call site copies this four-step shape.
    #[allow(clippy::too_many_arguments)]
    async fn run_escalation(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        kind: escalation::EscalationKind,
        reason: &str,
        detail: String,
        failing_task: Option<u64>,
        cycle: u32,
    ) -> Result<()> {
        let job = self.must_get(owner, project, seq)?.clone();
        let next_task_id = self.next_task_id(owner, project, seq).await?;
        let view = escalation::EscalationView {
            job: &job,
            next_task_id,
            cycle,
            now: Utc::now(),
        };
        let (transitions, effects) = escalation::decide(
            &view,
            escalation::EscalationEvent {
                kind,
                reason: reason.to_string(),
                detail,
                failing_task,
            },
        );
        for mut t in transitions {
            self.set_state(&mut t.job, t.to).await?;
        }
        for effect in effects {
            Box::pin(self.interpret(effect)).await?;
        }
        Ok(())
    }

    pub fn graph(&self, owner: &str, project: &str) -> Option<&JobGraph> {
        self.graphs.get(&format!("{owner}/{project}"))
    }

    /// Read-only view of the in-memory scheduling state, for the invariant
    /// checker (docs/reference/contracts.md §3). Borrowing, so it is free to build; tests run
    /// [`check_invariants`](crate::invariants::check_invariants) on it after
    /// every message to catch state corruption at the point it is introduced.
    pub fn state(&self) -> crate::invariants::CoreState<'_> {
        crate::invariants::CoreState {
            graphs: &self.graphs,
            queue: &self.queue,
            active: &self.active,
            merge_gates: &self.merge_gates,
        }
    }

    pub(crate) fn must_get(&self, owner: &str, project: &str, seq: u64) -> Result<&Job> {
        self.graphs
            .get(&format!("{owner}/{project}"))
            .and_then(|g| g.get(seq))
            .ok_or_else(|| CoreError::NotFound(format!("{owner}/{project}#{seq}")))
    }

    /// The §4.3 job brief injected into work and eval prompts. For a batch this
    /// is the batch-aware block — a preamble plus every member's ticket — so a
    /// single work agent (and every evaluator) addresses all of them; for an
    /// ordinary job it is the plain per-job brief.
    pub(crate) fn work_brief(&self, owner: &str, project: &str, job: &Job) -> String {
        if !job.is_batch() {
            return crate::exec::job_brief_block(job);
        }
        let members: Vec<Job> = job
            .members
            .iter()
            .filter_map(|&m| self.must_get(owner, project, m).ok().cloned())
            .collect();
        crate::exec::batch_brief_block(job, &members)
    }

    /// Attach a golden-trace recorder (refactor-plan B3, [`crate::trace`]). A
    /// test-only hook: production never calls this, so the sink stays `None` and
    /// tracing is inert. Attach before [`spawn`] so the moved `Core` keeps a
    /// clone of the shared sink.
    pub fn attach_trace(&mut self, sink: crate::trace::TraceSink) {
        self.trace = Some(sink);
    }

    /// Attach an invariant log (refactor-plan B1a, [`crate::invariants`]), turning
    /// on the after-every-message check inside the state loop. A test-only hook,
    /// like [`attach_trace`](Self::attach_trace): production never calls it, so
    /// the sink stays `None` and the check never runs. Attach before [`spawn`] so
    /// the moved `Core` keeps a clone of the shared log.
    pub fn attach_invariant_sink(&mut self, sink: crate::invariants::InvariantSink) {
        self.invariant_sink = Some(sink);
    }

    /// The single state-write path: §2.1 guard, then KV, then memory.
    pub(crate) async fn set_state(&mut self, job: &mut Job, to: JobState) -> Result<()> {
        assert_transition(job.state, to)?;
        if let Some(trace) = &self.trace {
            trace.transition(job.id, job.state, to);
        }
        job.state = to;
        if to.is_terminal() {
            job.completed_at.get_or_insert_with(Utc::now);
        }
        self.jobs.put(job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        Ok(())
    }

    pub(crate) async fn next_task_id(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
    ) -> Result<u64> {
        Ok(self
            .tasks
            .list_for_job(owner, project, job_seq)
            .await?
            .len() as u64
            + 1)
    }

    pub(crate) async fn kv_names(&self, owner: &str, project: &str) -> Result<KvNames> {
        let prefix = format!("{owner}.{project}.");
        let name_set = |keys: Vec<String>| -> HashSet<String> {
            keys.iter()
                .filter_map(|k| k.strip_prefix(&prefix))
                .map(String::from)
                .collect()
        };
        let secrets = self
            .store
            .raw_bucket(store::buckets::SECRETS)
            .await?
            .keys_with_prefix(&prefix)
            .await?;
        let vars = self
            .store
            .raw_bucket(store::buckets::VARS)
            .await?
            .keys_with_prefix(&prefix)
            .await?;
        let cloud_identities = self
            .store
            .raw_bucket(store::buckets::CLOUD_IDENTITIES)
            .await?
            .keys_with_prefix(&prefix)
            .await?;
        Ok(KvNames {
            secrets: name_set(secrets),
            vars: name_set(vars),
            cloud_identities: name_set(cloud_identities),
        })
    }

    pub(crate) async fn publish(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        event_type: &str,
        extra: serde_json::Value,
    ) -> Result<()> {
        let mut payload = serde_json::json!({
            "job_seq": seq,
            "project": format!("{owner}/{project}"),
            "ts": Utc::now(),
            "event_type": event_type,
        });
        if let (Some(obj), Some(ext)) = (payload.as_object_mut(), extra.as_object()) {
            obj.extend(ext.clone());
        }
        let subject = subjects::job_event(owner, project, seq, event_type);
        self.store
            .publish_event(&subject, &serde_json::to_vec(&payload)?)
            .await?;
        if let Some(trace) = &self.trace {
            trace.effect(format!("PublishEvent {event_type}"));
        }
        Ok(())
    }

    /// Persist a freshly created task record and announce it (`task-created`).
    ///
    /// Every creation path — work, evaluator, merge gate, wrap-up, triage — is
    /// this same pair, and the pair is not separable: a stored task with no event
    /// is a task the operator UI never learns about, and an event with no record
    /// is a phantom. The event's identity fields are DERIVED from the record (so
    /// they cannot disagree with it) and the job it publishes under is the
    /// record's own `job_seq`; `extra` carries only what one phase adds — the
    /// retry attempt, an evaluator's name and stage, a human performer.
    pub(crate) async fn task_create(
        &mut self,
        owner: &str,
        project: &str,
        task: &types::Task,
        extra: serde_json::Value,
    ) -> Result<()> {
        debug_assert!(
            task.result.is_none(),
            "task {} is being created with a result already on it",
            task.id
        );
        debug_assert!(
            task.completed_at.is_none(),
            "created task already completed"
        );
        self.task_put(task).await?;
        let mut payload = serde_json::json!({
            "task_id": task.id,
            "phase": task.phase,
            "cycle": task.cycle,
        });
        if let (Some(obj), Some(ext)) = (payload.as_object_mut(), extra.as_object()) {
            obj.extend(ext.clone());
        }
        self.publish(owner, project, task.job_seq, "task-created", payload)
            .await
    }

    /// The single task-write path: persist the record, then bring the owning
    /// job's [`Job::task_time_ms`] back in step with it.
    ///
    /// A task is written back at a dozen call sites, ten of which stamp
    /// `completed_at`. Accumulating (`job.task_time_ms += span`) at each would
    /// be that many copies of one rule — and the first site that forgets drifts
    /// the total permanently. Recomputing here instead keeps the rule in one
    /// place, which is what makes it self-healing rather than cumulative.
    ///
    /// Public for the same reason the other core operations are: it is the
    /// contract a tier-2 test drives directly (`tests/task_time.rs`).
    pub async fn task_put(&mut self, task: &types::Task) -> Result<()> {
        self.tasks.put(task).await?;
        if task.completed_at.is_some() {
            self.task_put_time_refresh(&task.project, task.job_seq)
                .await?;
        }
        Ok(())
    }

    /// Recompute one job's task time from that job's own tasks and write it
    /// back. The read is bounded by the job's own attempt count — never by the
    /// project's job or task count, so it cannot become the growing full-bucket
    /// scan #290 was — and it is idempotent: a write lost to a crash between
    /// the task and the job self-heals at the next completion instead of
    /// leaving a permanently wrong sum.
    async fn task_put_time_refresh(&mut self, slug: &str, job_seq: u64) -> Result<()> {
        let (owner, project) = split_slug(slug)?;
        let tasks = self.tasks.list_for_job(&owner, &project, job_seq).await?;
        let task_time_ms = types::task_time_ms(&tasks);
        let job = match self.must_get(&owner, &project, job_seq) {
            Ok(job) => Some(job.clone()),
            Err(_) => self.jobs.get(&owner, &project, job_seq).await?,
        };
        let Some(mut job) = job else {
            return Ok(());
        };
        if job.task_time_ms == task_time_ms {
            return Ok(());
        }
        job.task_time_ms = task_time_ms;
        self.jobs.put(&job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job);
        Ok(())
    }
}

/// Stamp trigger provenance onto a `job-created` payload (spec §6.3): the
/// schedule that originated the job, omitted entirely for every other origin.
fn stamp_event_schedule(extra: &mut serde_json::Value, schedule: Option<&str>) {
    let Some(name) = schedule else {
        return;
    };
    let Some(object) = extra.as_object_mut() else {
        debug_assert!(false, "an event payload is a JSON object, got {extra}");
        return;
    };
    let previous = object.insert("schedule".to_string(), serde_json::json!(name));
    debug_assert!(
        previous.is_none(),
        "the event payload already carried a 'schedule' field"
    );
}

fn split_slug(slug: &str) -> Result<(String, String)> {
    let (o, p) = split_project(slug)?;
    Ok((o.to_string(), p.to_string()))
}

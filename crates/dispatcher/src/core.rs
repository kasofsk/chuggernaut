//! Single-writer core (spec §3.1): one tokio task owns all job/task state, the
//! in-memory graphs, and the work queue. Everything else — NATS handlers,
//! container monitors, scan timers — talks to it via the [`Msg`] channel and
//! never mutates state directly. Container monitoring is concurrent; state
//! transitions are sequential.

use crate::graph::JobGraph;
use crate::origin::OriginStatusResponse;
use crate::queue::{QueuedJob, QueuedLaunch, ReadyQueue};
use crate::release::{self, KvNames, ValidationError};
use crate::state::{InvalidTransition, assert_transition};
use crate::{escalation, exec, queue};
use agent::AgentProvider;
use chrono::Utc;
use container::ContainerBackend;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use store::{
    CounterStore, JobStore, NatsStore, ProjectStore, RdepsStore, TaskStore, split_project, subjects,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use types::{Job, JobState, TaskResolution, TokenUsage};
use vcs::RepoManager;

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

pub struct CreateJobRequest {
    pub owner: String,
    pub project: String,
    pub r#type: String,
    /// Ticket-style identity: what this run is for (optional, empty = none).
    pub title: String,
    pub description: String,
    pub deps: Vec<u64>,
    pub knowledge_tags: Vec<String>,
    /// Additive per-job evaluators; validated (field rules + name collisions
    /// against the type's list) at release, not creation.
    pub eval: Vec<types::Evaluator>,
    /// Optional per-job work-task timeout override (duration string, §1.1);
    /// parseability validated at release. None → the type default applies.
    pub timeout: Option<String>,
    /// Optional per-job Work agent model override (§12.4); wins over the job
    /// type, project, and platform defaults. None → the resolution chain applies.
    pub model: Option<String>,
    pub factory: Option<String>,
    /// Land the job in [`JobState::Draft`] instead of Frozen (spec §2.1): its
    /// definition can be edited (via [`Core::update_job`]) before release.
    /// Default false preserves today's behavior (created jobs land Frozen).
    pub draft: bool,
}

/// Full-field replacement of a Draft job's definition (spec §2.1). The same
/// shape as [`CreateJobRequest`] minus the immutable identity: only a job in
/// Draft accepts it. Validation is identical to create — deferred to release.
pub struct UpdateJobRequest {
    pub owner: String,
    pub project: String,
    pub seq: u64,
    pub r#type: String,
    pub title: String,
    pub description: String,
    pub deps: Vec<u64>,
    pub knowledge_tags: Vec<String>,
    pub eval: Vec<types::Evaluator>,
    pub timeout: Option<String>,
    pub model: Option<String>,
}

/// `req.work.submit.*` payload (spec §4.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkSubmission {
    pub summary: Option<String>,
    pub structured: Option<serde_json::Value>,
    pub token_usage: Option<TokenUsage>,
}

/// `req.eval.submit.*` payload (spec §4.2). `pass` is the authoritative verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSubmission {
    pub pass: bool,
    /// "Not satisfiable by rework" (design-lifecycle.md): implies fail; a
    /// required evaluator's abort escalates instead of consuming rework budget.
    #[serde(default)]
    pub abort: bool,
    pub structured: Option<serde_json::Value>,
    pub token_usage: Option<TokenUsage>,
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
    /// Set only by restart reconciliation (§3.6, `settle_running`) when a task's
    /// container is GONE at restart — docker pruned it, the node rebooted,
    /// colima restarted. This is an infrastructure loss, not a real nonzero
    /// exit: the attempt is relaunched without spending a `work_retries`/
    /// `eval_retries` budget (capped, then escalates `infra_loss`). Never set on
    /// the in-container exit paths — a real exit keeps burning budget.
    pub infra_loss: bool,
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
    CreateJob(CreateJobRequest, Reply<Job>),
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

    pub async fn create_job(&self, req: CreateJobRequest) -> Result<Job> {
        self.call(|reply| Msg::CreateJob(req, reply)).await
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
    /// Binary path baked into new repos' pre-receive hooks (§5.2) — the path
    /// the binary has on the SSH host. None → this process's own executable.
    /// Used by the core for `req.projects.link`; `spawn_api_handlers` takes
    /// the same value for `req.projects.create`.
    pub hook_bin: Option<std::path::PathBuf>,
    /// Maximum time a launch may wait in the capacity queue before it escalates
    /// as a backstop (spec §3.5). None → [`crate::launch_queue::MAX_QUEUE_WAIT`]
    /// (30m). Tests shrink it to exercise the timeout without waiting.
    pub launch_queue_max_wait: Option<std::time::Duration>,
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
    /// Transcript/log blob store; None disables capture (see `harvest`).
    pub(crate) artifacts: Option<Arc<store::ArtifactStore>>,
    /// Per-project merge queue (spec §3.3 Merge Gate: depth-1 serialization).
    /// All post-eval finalization flows through it, keyed by project slug.
    pub(crate) merge_queue: HashMap<String, std::collections::VecDeque<u64>>,
    /// Project slug → seq whose merge gate is currently running.
    pub(crate) gating: HashMap<String, u64>,
    /// Platform project records (linked-origin state).
    pub(crate) projects: ProjectStore,
    /// PR surface for origin releases; a fake in integration tests.
    pub(crate) pr_api: Arc<dyn crate::github::PullRequestApi>,
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
        interval.tick().await; // consume the immediate first tick
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
        // Re-attempt launches reconciliation re-queued under capacity pressure.
        if let Err(e) = core.drain_launch_queue().await {
            tracing::error!("post-reconcile launch drain: {e}");
        }
        core.run(rx).await
    });
    CoreHandle { tx }
}

impl Core {
    /// Connect stores and rebuild in-memory state from `jobs.*` KV (spec §3.6
    /// steps 1 and 5): graphs, the rdeps index (written back — it is a derived
    /// cache), and the Ready queue.
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
        let artifacts =
            crate::harvest::artifact_store(&store, config.artifacts_identity.as_deref()).await?;

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
            artifacts,
            graphs: HashMap::new(),
            queue: ReadyQueue::default(),
            launch_queue: std::collections::VecDeque::new(),
            active: HashMap::new(),
            merge_queue: HashMap::new(),
            gating: HashMap::new(),
            projects,
            pr_api: Arc::new(crate::github::GithubClient::new()),
            release_holds: HashSet::new(),
            self_tx: None,
            draining: false,
        };

        // Restore merge-queue holds for Open origin releases before reconcile
        // runs — recovered Evaluation jobs must re-enqueue without landing.
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
    pub fn with_pr_api(mut self, pr_api: Arc<dyn crate::github::PullRequestApi>) -> Self {
        self.pr_api = pr_api;
        self
    }

    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            // Graceful shutdown (spec §3.6 drain): quiesce and stop the loop.
            // Handled here, not in `handle_msg`, because it needs the receiver
            // to sweep the remaining mailbox.
            if let Msg::Drain { reply } = msg {
                let result = self.drain(&mut rx).await;
                let _ = reply.send(result);
                return;
            }
            self.handle_msg(msg).await;
            if let Err(e) = self.drain_queue().await {
                tracing::error!("drain_queue: {e}");
            }
            // A just-handled container exit may have freed a fleet slot; retry
            // any launches queued under capacity pressure (spec §3.5).
            if let Err(e) = self.drain_launch_queue().await {
                tracing::error!("drain_launch_queue: {e}");
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
        // Sweep the mailbox. `handle_msg` records exits and stamps container ids
        // as normal; the launch paths it may reach are no-ops while draining.
        while let Ok(msg) = rx.try_recv() {
            match msg {
                // A second drain: already draining — just ack it.
                Msg::Drain { reply } => {
                    let _ = reply.send(Ok(()));
                }
                other => self.handle_msg(other).await,
            }
        }
        // Audit + flush: a Running task whose container-start message was still
        // in flight would carry no id and reconcile as a synthetic -1. Recover
        // the id from the live fleet so restart re-attaches instead.
        self.flush_running_container_ids().await;
        Ok(())
    }

    /// The drain audit (spec §3.6): stamp any `Running` task still missing its
    /// `container_id` from the live fleet's identity labels, so restart
    /// re-attaches its monitor rather than failing it as container-gone. A launch
    /// records the id via [`Msg::TaskContainerStarted`], but that message can be
    /// in flight when SIGTERM lands; this closes the gap. Best-effort — a backend
    /// hiccup only warns, and a missing stamp is no worse than today.
    async fn flush_running_container_ids(&self) {
        let running = match self.backend.list_managed_running().await {
            Ok(cs) => cs,
            Err(e) => {
                tracing::warn!("drain: listing running containers failed: {e}");
                return;
            }
        };
        // Index the live containers by the (project, job, task) identity their
        // labels carry, so a task with no recorded id can be matched back.
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
                    if let Err(e) = self.tasks.put(&task).await {
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

    async fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Ping { reply } => {
                let _ = reply.send(Ok(()));
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
            Msg::TriageJob {
                owner,
                project,
                seq,
                reply,
            } => {
                let _ = reply.send(self.triage_job(&owner, &project, seq).await);
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
            // Drain is intercepted by `run` (and by `drain`'s own sweep) because
            // it needs the receiver; it never reaches here. Ack defensively.
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
        self.tasks.put(&task).await?;
        Ok(())
    }

    /// Build a [`agent::LaunchReporter`] wired back into the core: it spawns a
    /// tiny forwarder that turns the provider's launch signal into a
    /// [`Msg::TaskContainerStarted`], so the container id reaches the task
    /// record through the single-writer loop rather than a direct mutation.
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
        // Draining (spec §3.6): initiate no new work. Ready jobs stay enqueued in
        // KV and are re-enqueued on restart, so nothing is lost.
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
    pub async fn create_job(&mut self, req: CreateJobRequest) -> Result<Job> {
        let seq = self.counters.next(&req.owner, &req.project).await?;
        let job = Job {
            id: seq,
            project: format!("{}/{}", req.owner, req.project),
            r#type: req.r#type,
            title: req.title,
            description: req.description,
            deps: req.deps,
            state: if req.draft {
                JobState::Draft
            } else {
                JobState::Frozen
            },
            branch: format!("job/{seq}"),
            base_ref: None,
            knowledge_tags: req.knowledge_tags,
            eval: req.eval,
            timeout: req.timeout,
            model: req.model,
            claim_next: false,
            escalation: None,
            factory: req.factory,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
        };
        self.jobs.put(&job).await?;
        for &upstream in &job.deps {
            // Non-fatal by spec §2.3 — the index is rebuilt on startup.
            let _ = self
                .rdeps
                .append(&req.owner, &req.project, upstream, seq)
                .await;
        }
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        self.publish(
            &req.owner,
            &req.project,
            seq,
            "job-created",
            serde_json::json!({}),
        )
        .await?;
        Ok(job)
    }

    /// Handle `req.jobs.release.*` (spec §2.2 release-time pass + §2.1
    /// Frozen→Ready|Blocked). Returns the resulting state.
    pub async fn release_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<JobState> {
        let job = self.must_get(owner, project, seq)?.clone();
        // Frozen and Draft both release; a Draft is finalized (its edited
        // definition locked in) in the same step (§2.1). Any other state rejects.
        if !matches!(job.state, JobState::Frozen | JobState::Draft) {
            return Err(InvalidTransition {
                from: job.state,
                to: JobState::Ready,
            }
            .into());
        }
        let from_draft = job.state == JobState::Draft;

        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;

        let job_type =
            release::load_job_type(&self.repos, owner, project, &head, &job.r#type, Some(seq))
                .await?;
        let job_type = release::with_job_evaluators(job_type, &job)?;
        let graph = self.graphs.entry(job.project.clone()).or_default();
        let mut errs = release::wiring_errors(&job, graph);
        let kv = self.kv_names(owner, project).await?;
        errs.extend(
            release::static_errors(
                &self.repos,
                owner,
                project,
                &head,
                &job,
                &job_type,
                Some(&kv),
            )
            .await?,
        );
        if !errs.is_empty() {
            return Err(errs.into());
        }

        let graph = self.graphs.entry(job.project.clone()).or_default();
        let target = if graph.deps_done(seq) {
            JobState::Ready
        } else {
            JobState::Blocked
        };
        let mut updated = job;
        if target == JobState::Ready {
            updated.base_ref = Some(head);
            updated.ready_at.get_or_insert_with(Utc::now);
        }
        self.set_state(&mut updated, target).await?;
        if target == JobState::Ready {
            self.queue.enqueue(QueuedJob {
                owner: owner.into(),
                project: project.into(),
                seq,
            });
        }
        // Leaving Draft finalizes the edited definition: emit job-finalized so
        // the UI/SSE can distinguish it from a plain Frozen release (§2.1).
        if from_draft {
            self.publish(owner, project, seq, "job-finalized", serde_json::json!({}))
                .await?;
        }
        self.publish(
            owner,
            project,
            seq,
            "job-released",
            serde_json::json!({ "state": target }),
        )
        .await?;
        Ok(target)
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

    /// §2.1 Blocked→Ready with the §2.2 Ready-transition re-validation pass.
    /// No-op unless the job is Blocked with all dependencies Done. Also used
    /// by restart reconciliation (§3.6 step 3).
    pub(crate) async fn try_unblock(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let slug = format!("{owner}/{project}");
        let Some(dep) = self.graphs.get(&slug).and_then(|g| g.get(seq)) else {
            return Ok(());
        };
        let ready = dep.state == JobState::Blocked
            && self.graphs.get(&slug).is_some_and(|g| g.deps_done(seq));
        if !ready {
            return Ok(());
        }
        let mut dep = dep.clone();

        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;

        let revalidation = match release::load_job_type(
            &self.repos,
            owner,
            project,
            &head,
            &dep.r#type,
            Some(seq),
        )
        .await
        .and_then(|jt| release::with_job_evaluators(jt, &dep))
        {
            Ok(jt) => release::static_errors(&self.repos, owner, project, &head, &dep, &jt, None)
                .await
                .and_then(|errs| if errs.is_empty() { Ok(()) } else { Err(errs) }),
            Err(errs) => Err(errs),
        };

        match revalidation {
            Ok(()) => {
                dep.base_ref = Some(head);
                dep.ready_at.get_or_insert_with(Utc::now);
                self.set_state(&mut dep, JobState::Ready).await?;
                self.queue.enqueue(QueuedJob {
                    owner: owner.into(),
                    project: project.into(),
                    seq,
                });
                self.publish(owner, project, seq, "job-unblocked", serde_json::json!({}))
                    .await?;
            }
            Err(errs) => {
                let detail = errs
                    .iter()
                    .map(|e| format!("- {}: {}", e.field, e.message))
                    .collect::<Vec<_>>()
                    .join("\n");
                let prompt =
                    format!("Job {seq} failed Ready-transition re-validation at {head}:\n{detail}");
                self.stall(owner, project, seq, "revalidation_failed", prompt, None)
                    .await?;
            }
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
            let mut j = self.must_get(owner, project, target)?.clone();
            self.kill_running_containers(owner, project, target).await;
            self.close_pending_tasks(owner, project, target).await;
            self.active
                .remove(&(owner.to_string(), project.to_string(), target));
            self.set_state(&mut j, JobState::Revoked).await?;
            // Delete job/{seq} and any parked candidate; missing refs are fine.
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
            if let Some(q) = self.merge_queue.get_mut(&slug) {
                q.retain(|&s| s != target);
            }
            if self.gating.get(&slug) == Some(&target) {
                self.gating.remove(&slug);
            }
        }
        // A revoked gate occupant frees the queue for the next candidate.
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

    /// Handle `req.jobs.update.*` (spec §2.1): full-field replace of a Draft
    /// job's definition. Only a job in Draft is editable — any other state is a
    /// 409 (`Conflict`), so a released job is never mutated. Validation is
    /// identical to create (deferred to release), so the edit just rewrites the
    /// record and publishes `job-updated` naming the fields that changed.
    pub async fn update_job(&mut self, req: UpdateJobRequest) -> Result<Job> {
        let UpdateJobRequest {
            owner,
            project,
            seq,
            r#type,
            title,
            description,
            deps,
            knowledge_tags,
            eval,
            timeout,
            model,
        } = req;
        let mut job = self.must_get(&owner, &project, seq)?.clone();
        if job.state != JobState::Draft {
            return Err(CoreError::Conflict(format!(
                "job {seq} is {:?}; only a Draft job can be edited",
                job.state
            )));
        }

        // Name every field whose new value differs from the old, so the event
        // carries changed field names rather than a full payload (§2.1 events).
        let mut changed: Vec<&str> = Vec::new();
        if job.r#type != r#type {
            changed.push("type");
        }
        if job.title != title {
            changed.push("title");
        }
        if job.description != description {
            changed.push("description");
        }
        if job.deps != deps {
            changed.push("deps");
        }
        if job.knowledge_tags != knowledge_tags {
            changed.push("knowledge_tags");
        }
        if job.eval != eval {
            changed.push("eval");
        }
        if job.timeout != timeout {
            changed.push("timeout");
        }
        if job.model != model {
            changed.push("model");
        }

        // Upstreams this edit drops — used to prune both the KV rdeps index
        // (below) and, implicitly, the in-memory reverse edges when the graph
        // re-inserts the job (see `JobGraph::insert`). Without pruning, a later
        // revoke of a dropped upstream would cascade to this job by a stale edge.
        let old_deps = job.deps.clone();

        // Full-field replace; identity (id/branch/created_at) and lifecycle
        // fields (state/base_ref/ready_at/claim_next) are untouched — a Draft
        // holds no branch or base_ref, exactly like a Frozen job.
        job.r#type = r#type;
        job.title = title;
        job.description = description;
        job.deps = deps;
        job.knowledge_tags = knowledge_tags;
        job.eval = eval;
        job.timeout = timeout;
        job.model = model;

        self.jobs.put(&job).await?;
        for &upstream in &job.deps {
            // Mirror create: best-effort rdeps append (§2.3, rebuilt on startup).
            let _ = self.rdeps.append(&owner, &project, upstream, seq).await;
        }
        for &upstream in &old_deps {
            // Prune the reverse edge for any upstream this edit dropped, so the
            // KV index stays consistent (best-effort, §2.3 — rebuilt on startup).
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

    /// Handle `req.jobs.draft.*` (spec §2.1): move a Frozen (never-released)
    /// job back to Draft for editing. Only Frozen → Draft — `set_state`'s guard
    /// rejects any other origin as an invalid transition (409).
    pub async fn draft_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        self.set_state(&mut job, JobState::Draft).await?;
        self.publish(owner, project, seq, "job-drafted", serde_json::json!({}))
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
        // A Draft job has no work attempt to claim — it is invisible to
        // scheduling until released (§2.1). Claim it after release, not before.
        if job.state == JobState::Draft {
            return Err(CoreError::Conflict(format!(
                "job {seq} is Draft; release it before claiming a work attempt"
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
    pub(crate) async fn close_pending_tasks(&self, owner: &str, project: &str, seq: u64) {
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
            let _ = self.tasks.put(&closed).await;
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
        let mut job = self.must_get(owner, project, seq)?.clone();
        let task_id = self.next_task_id(owner, project, seq).await?;
        let cycle = self
            .active
            .get(&(owner.to_string(), project.to_string(), seq))
            .map(|e| e.cycle)
            .unwrap_or(1);
        let task = escalation::escalation_task(task_id, seq, &job.project, cycle, detail.clone());
        self.tasks.put(&task).await?;
        // Record WHY on the job itself (§1.2), so operators see the reason in the
        // header instead of digging through dispatcher logs (#69).
        job.escalation = Some(types::Escalation {
            reason: reason.to_string(),
            detail,
            failing_task,
            at: Utc::now(),
        });
        self.set_state(&mut job, JobState::Escalated).await?;
        self.publish(
            owner,
            project,
            seq,
            "job-escalated",
            serde_json::json!({ "reason": reason }),
        )
        .await?;
        Ok(())
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
        let mut job = self.must_get(owner, project, seq)?.clone();
        let task_id = self.next_task_id(owner, project, seq).await?;
        // Pre-work: cycle 1, no exec state.
        let task = escalation::escalation_task(task_id, seq, &job.project, 1, detail.clone());
        self.tasks.put(&task).await?;
        job.escalation = Some(types::Escalation {
            reason: reason.to_string(),
            detail,
            failing_task,
            at: Utc::now(),
        });
        self.set_state(&mut job, JobState::Stalled).await?;
        self.publish(
            owner,
            project,
            seq,
            "job-stalled",
            serde_json::json!({ "reason": reason }),
        )
        .await?;
        Ok(())
    }

    pub fn graph(&self, owner: &str, project: &str) -> Option<&JobGraph> {
        self.graphs.get(&format!("{owner}/{project}"))
    }

    pub(crate) fn must_get(&self, owner: &str, project: &str, seq: u64) -> Result<&Job> {
        self.graphs
            .get(&format!("{owner}/{project}"))
            .and_then(|g| g.get(seq))
            .ok_or_else(|| CoreError::NotFound(format!("{owner}/{project}#{seq}")))
    }

    /// The single state-write path: §2.1 guard, then KV, then memory.
    pub(crate) async fn set_state(&mut self, job: &mut Job, to: JobState) -> Result<()> {
        assert_transition(job.state, to)?;
        job.state = to;
        // Stamp the completion moment once, at the terminal transition (Done or
        // Revoked). This is the single funnel every job-state write flows
        // through, so it covers the finalize (Evaluation/WrapUp→Done) and revoke
        // paths uniformly. `get_or_insert_with` keeps it immutable — terminal
        // states are absorbing, but be defensive.
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
        // Sequential within job (§1.2); safe as read-then-write: single writer.
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
        Ok(KvNames {
            secrets: name_set(secrets),
            vars: name_set(vars),
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
            .jetstream()
            .publish(subject, serde_json::to_vec(&payload)?.into())
            .await
            .map_err(|e| store::StoreError::Nats(e.to_string()))?
            .await
            .map_err(|e| store::StoreError::Nats(e.to_string()))?;
        Ok(())
    }
}

fn split_slug(slug: &str) -> Result<(String, String)> {
    let (o, p) = split_project(slug)?;
    Ok((o.to_string(), p.to_string()))
}

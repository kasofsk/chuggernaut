//! Single-writer core (spec §3.1): one tokio task owns all job/task state, the
//! in-memory graphs, and the work queue. Everything else — NATS handlers,
//! container monitors, scan timers — talks to it via the [`Msg`] channel and
//! never mutates state directly. Container monitoring is concurrent; state
//! transitions are sequential.

use crate::graph::JobGraph;
use crate::origin::OriginStatusResponse;
use crate::queue::{QueuedJob, ReadyQueue};
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
}

impl TaskExit {
    /// An exit with nothing harvested — command containers, scans, reconcile.
    pub fn code(exit_code: i32) -> Self {
        Self {
            exit_code,
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
    /// §3.5 scans; fired by the internal ticker, or with a reply from
    /// [`CoreHandle::trigger_scan`] (tests).
    Scan {
        reply: Option<Reply<()>>,
    },
    /// Posted by container monitor tasks — never by anything outside the crate.
    TaskExited {
        owner: String,
        project: String,
        seq: u64,
        task_id: u64,
        exit: TaskExit,
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
            active: HashMap::new(),
            merge_queue: HashMap::new(),
            gating: HashMap::new(),
            projects,
            pr_api: Arc::new(crate::github::GithubClient::new()),
            release_holds: HashSet::new(),
            self_tx: None,
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
            self.handle_msg(msg).await;
            if let Err(e) = self.drain_queue().await {
                tracing::error!("drain_queue: {e}");
            }
        }
    }

    async fn handle_msg(&mut self, msg: Msg) {
        match msg {
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
        }
    }

    /// §3.1 step 5: launch every queued Ready job. Slot caps live in the
    /// backend (fleet) — the core does not throttle.
    pub(crate) async fn drain_queue(&mut self) -> Result<()> {
        while let Some(q) = self.queue.dequeue() {
            self.start_job(q).await?;
        }
        Ok(())
    }

    /// Handle `req.jobs.create.*` (spec §3.1 step 1). Jobs always land Frozen;
    /// wiring is validated at release, not creation.
    pub async fn create_job(&mut self, req: CreateJobRequest) -> Result<Job> {
        let seq = self.counters.next(&req.owner, &req.project).await?;
        let job = Job {
            id: seq,
            project: format!("{}/{}", req.owner, req.project),
            r#type: req.r#type,
            title: req.title,
            description: req.description,
            deps: req.deps,
            state: JobState::Frozen,
            branch: format!("job/{seq}"),
            base_ref: None,
            knowledge_tags: req.knowledge_tags,
            eval: req.eval,
            timeout: req.timeout,
            model: req.model,
            claim_next: false,
            factory: req.factory,
            created_at: Utc::now(),
            ready_at: None,
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
        if job.state != JobState::Frozen {
            return Err(InvalidTransition {
                from: job.state,
                to: JobState::Ready,
            }
            .into());
        }

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
                self.stall(owner, project, seq, "revalidation_failed", prompt)
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

    /// Create a Human escalation task and move the job to Escalated.
    pub(crate) async fn escalate(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        reason: &str,
        prompt: String,
    ) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        let task_id = self.next_task_id(owner, project, seq).await?;
        let cycle = self
            .active
            .get(&(owner.to_string(), project.to_string(), seq))
            .map(|e| e.cycle)
            .unwrap_or(1);
        let task = escalation::escalation_task(task_id, seq, &job.project, cycle, prompt);
        self.tasks.put(&task).await?;
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
        prompt: String,
    ) -> Result<()> {
        let mut job = self.must_get(owner, project, seq)?.clone();
        let task_id = self.next_task_id(owner, project, seq).await?;
        // Pre-work: cycle 1, no exec state.
        let task = escalation::escalation_task(task_id, seq, &job.project, 1, prompt);
        self.tasks.put(&task).await?;
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

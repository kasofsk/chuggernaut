//! Single-writer core (spec §3.1): one tokio task owns all job/task state, the
//! in-memory graphs, and the work queue. Everything else — NATS handlers,
//! container monitors, scan timers — talks to it via the [`Msg`] channel and
//! never mutates state directly. Container monitoring is concurrent; state
//! transitions are sequential.

use crate::graph::JobGraph;
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
use store::{CounterStore, JobStore, NatsStore, RdepsStore, TaskStore, split_project, subjects};
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
    #[error("configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Backend(#[from] container::BackendError),
    #[error("core loop stopped")]
    Stopped,
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
    pub inputs: HashMap<String, u64>,
    pub knowledge_tags: Vec<String>,
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
    pub structured: Option<serde_json::Value>,
    pub token_usage: Option<TokenUsage>,
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
    /// §3.5 scans; fired by the internal ticker, or with a reply from
    /// [`CoreHandle::trigger_scan`] (tests).
    Scan { reply: Option<Reply<()>> },
    /// Posted by container monitor tasks — never by anything outside the crate.
    TaskExited {
        owner: String,
        project: String,
        seq: u64,
        task_id: u64,
        exit_code: i32,
        /// `/workspace/eval-result.json` for command eval containers.
        eval_json: Option<serde_json::Value>,
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
        self.tx.send(build(tx)).await.map_err(|_| CoreError::Stopped)?;
        rx.await.map_err(|_| CoreError::Stopped)?
    }

    pub async fn create_job(&self, req: CreateJobRequest) -> Result<Job> {
        self.call(|reply| Msg::CreateJob(req, reply)).await
    }

    pub async fn release_job(&self, owner: &str, project: &str, seq: u64) -> Result<JobState> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::ReleaseJob { owner, project, seq, reply }).await
    }

    pub async fn revoke_job(&self, owner: &str, project: &str, seq: u64) -> Result<Vec<u64>> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::RevokeJob { owner, project, seq, reply }).await
    }

    pub async fn submit_result(
        &self,
        owner: &str,
        project: &str,
        seq: u64,
        submission: WorkSubmission,
    ) -> Result<()> {
        let (owner, project) = (owner.to_string(), project.to_string());
        self.call(|reply| Msg::SubmitResult { owner, project, seq, submission, reply })
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
        self.call(|reply| Msg::SubmitEval { owner, project, seq, task_id, submission, reply })
            .await
    }

    /// Run the §3.5 scans now and wait for them to finish. Production relies
    /// on the internal ticker; this is for tests and admin tooling.
    pub async fn trigger_scan(&self) -> Result<()> {
        self.call(|reply| Msg::Scan { reply: Some(reply) }).await
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
            owner, project, seq, task_id, resolution, operator, reply,
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
    pub nats_url: String,
    /// Path to the chuggernaut-channel binary, injected into every agent
    /// container at /usr/local/bin/chuggernaut-channel (spec §4.2). None →
    /// agents run without the channel MCP (tests, degraded dev mode).
    pub channel_binary: Option<std::path::PathBuf>,
    /// age identity (`AGE-SECRET-KEY-1...`) for decrypting secrets at launch
    /// (spec §8.2). None → secret env values are injected as stored (dev).
    pub age_identity: Option<String>,
    /// §12.4 platform provider default. None (tests) falls back to `claude`;
    /// the production path always sets it — `DispatcherConfig` requires it.
    pub agent_provider_default: Option<String>,
    /// §12.4 platform model default; job-type/evaluator `model:` overrides it.
    pub agent_model_default: Option<String>,
    /// Platform NATS account seed (`nats_account.seed`, §12.1) for minting
    /// per-container scoped credentials (§7.4). None → containers connect
    /// unauthenticated (tests, open dev NATS).
    pub nats_account_seed: Option<String>,
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
    /// Per-project merge queue (spec §3.3 Merge Gate: depth-1 serialization).
    /// All post-eval finalization flows through it, keyed by project slug.
    pub(crate) merge_queue: HashMap<String, std::collections::VecDeque<u64>>,
    /// Project slug → seq whose merge gate is currently running.
    pub(crate) gating: HashMap<String, u64>,
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

        let channel_binary = match &config.channel_binary {
            Some(path) => Some(tokio::fs::read(path).await.map_err(|e| {
                CoreError::NotFound(format!("channel binary {path:?}: {e}"))
            })?),
            None => None,
        };
        let secrets = match &config.age_identity {
            Some(identity) => Some(store::secrets::AgeSecretStore::for_dispatcher(
                store.raw_bucket(store::buckets::SECRETS).await?,
                identity,
            )?),
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
            graphs: HashMap::new(),
            queue: ReadyQueue::default(),
            active: HashMap::new(),
            merge_queue: HashMap::new(),
            gating: HashMap::new(),
            self_tx: None,
        };

        let all: Vec<Job> = core.jobs.list_all().await?;
        for job in all {
            let (owner, project) = split_slug(&job.project)?;
            for &upstream in job.inputs.values() {
                core.rdeps.append(&owner, &project, upstream, job.id).await?;
            }
            if job.state == JobState::Ready {
                core.queue.enqueue(QueuedJob {
                    owner: owner.clone(),
                    project: project.clone(),
                    seq: job.id,
                });
            }
            core.graphs.entry(job.project.clone()).or_default().insert(job);
        }
        Ok(core)
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
            Msg::ReleaseJob { owner, project, seq, reply } => {
                let _ = reply.send(self.release_job(&owner, &project, seq).await);
            }
            Msg::RevokeJob { owner, project, seq, reply } => {
                let _ = reply.send(self.revoke_job(&owner, &project, seq).await);
            }
            Msg::SubmitResult { owner, project, seq, submission, reply } => {
                let _ = reply.send(self.handle_submit_result(&owner, &project, seq, submission).await);
            }
            Msg::SubmitEval { owner, project, seq, task_id, submission, reply } => {
                let _ = reply
                    .send(self.handle_submit_eval(&owner, &project, seq, task_id, submission).await);
            }
            Msg::ResolveTask { owner, project, seq, task_id, resolution, operator, reply } => {
                let _ = reply.send(
                    self.handle_resolve_task(&owner, &project, seq, task_id, resolution, &operator)
                        .await,
                );
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
            Msg::TaskExited { owner, project, seq, task_id, exit_code, eval_json } => {
                if let Err(e) = self
                    .on_task_exited(&owner, &project, seq, task_id, exit_code, eval_json)
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
            inputs: req.inputs,
            state: JobState::Frozen,
            branch: format!("job/{seq}"),
            base_ref: None,
            knowledge_tags: req.knowledge_tags,
            factory: req.factory,
            created_at: Utc::now(),
            ready_at: None,
        };
        self.jobs.put(&job).await?;
        for &upstream in job.inputs.values() {
            // Non-fatal by spec §2.3 — the index is rebuilt on startup.
            let _ = self.rdeps.append(&req.owner, &req.project, upstream, seq).await;
        }
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        self.publish(&req.owner, &req.project, seq, "job-created", serde_json::json!({}))
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
        let head = self.repos.resolve_ref(owner, project, &default_branch).await?;

        let job_type =
            release::load_job_type(&self.repos, owner, project, &head, &job.r#type, Some(seq))
                .await?;
        let graph = self.graphs.entry(job.project.clone()).or_default();
        let mut errs = release::wiring_errors(&job, &job_type, graph);
        let kv = self.kv_names(owner, project).await?;
        errs.extend(
            release::static_errors(&self.repos, owner, project, &head, &job, &job_type, Some(&kv))
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
        let head = self.repos.resolve_ref(owner, project, &default_branch).await?;

        let revalidation = match release::load_job_type(
            &self.repos,
            owner,
            project,
            &head,
            &dep.r#type,
            Some(seq),
        )
        .await
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
                self.escalate(owner, project, seq, "revalidation_failed", prompt).await?;
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
            self.active.remove(&(owner.to_string(), project.to_string(), target));
            self.set_state(&mut j, JobState::Revoked).await?;
            // Delete job/{seq} and any parked candidate; missing refs are fine.
            let _ = self.repos.delete_branch(owner, project, &j.branch).await;
            let _ = self.repos.delete_branch(owner, project, &format!("merge-gate/{target}")).await;
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

    pub(crate) async fn kill_running_containers(&self, owner: &str, project: &str, seq: u64) {
        let Ok(tasks) = self.tasks.list_for_job(owner, project, seq).await else {
            return;
        };
        for t in tasks {
            if t.state == types::TaskState::Running
                && let Some(cid) = &t.container_id {
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

    pub(crate) async fn next_task_id(&self, owner: &str, project: &str, job_seq: u64) -> Result<u64> {
        // Sequential within job (§1.2); safe as read-then-write: single writer.
        Ok(self.tasks.list_for_job(owner, project, job_seq).await?.len() as u64 + 1)
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

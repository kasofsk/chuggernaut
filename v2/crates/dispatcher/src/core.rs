//! Single-writer core (spec §3.1): owns job records, the in-memory graphs, and
//! the work queue. This slice covers the pre-execution lifecycle — creation,
//! release validation, Blocked→Ready unblocking with re-validation, and revoke
//! cascades. Execution (Ready→Work onward) is the next slice.

use crate::graph::JobGraph;
use crate::queue::{QueuedJob, ReadyQueue};
use crate::release::{self, KvNames, ValidationError};
use crate::state::{InvalidTransition, assert_transition};
use crate::{escalation, queue};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use store::{CounterStore, JobStore, NatsStore, RdepsStore, TaskStore, split_project, subjects};
use thiserror::Error;
use types::{Job, JobState};
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

pub struct Core {
    store: NatsStore,
    jobs: JobStore,
    tasks: TaskStore,
    counters: CounterStore,
    rdeps: RdepsStore,
    repos: RepoManager,
    graphs: HashMap<String, JobGraph>,
    pub queue: ReadyQueue,
}

impl Core {
    /// Connect stores and rebuild in-memory state from `jobs.*` KV (spec §3.6
    /// steps 1 and 5): graphs, the rdeps index (written back — it is a derived
    /// cache), and the Ready queue.
    pub async fn new(store: NatsStore, repos: RepoManager) -> Result<Self> {
        let jobs = store.jobs().await?;
        let tasks = store.tasks().await?;
        let counters = store.counters().await?;
        let rdeps = store.rdeps().await?;

        let mut core = Self {
            store,
            jobs,
            tasks,
            counters,
            rdeps,
            repos,
            graphs: HashMap::new(),
            queue: ReadyQueue::default(),
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

    pub fn graph(&self, owner: &str, project: &str) -> Option<&JobGraph> {
        self.graphs.get(&format!("{owner}/{project}"))
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
            let Some(dep) = self.graphs.get(&slug).and_then(|g| g.get(dep_seq)) else {
                continue;
            };
            let ready = dep.state == JobState::Blocked
                && self.graphs.get(&slug).is_some_and(|g| g.deps_done(dep_seq));
            if !ready {
                continue;
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
                Some(dep_seq),
            )
            .await
            {
                Ok(jt) => {
                    release::static_errors(&self.repos, owner, project, &head, &dep, &jt, None)
                        .await
                        .map(|errs| (jt, errs))
                        .and_then(|(jt, errs)| if errs.is_empty() { Ok(jt) } else { Err(errs) })
                }
                Err(errs) => Err(errs),
            };

            match revalidation {
                Ok(_) => {
                    dep.base_ref = Some(head);
                    dep.ready_at.get_or_insert_with(Utc::now);
                    self.set_state(&mut dep, JobState::Ready).await?;
                    self.queue.enqueue(QueuedJob {
                        owner: owner.into(),
                        project: project.into(),
                        seq: dep_seq,
                    });
                    self.publish(owner, project, dep_seq, "job-unblocked", serde_json::json!({}))
                        .await?;
                }
                Err(errs) => {
                    let task_id = self.next_task_id(owner, project, dep_seq).await?;
                    let prompt = format!(
                        "Job {dep_seq} failed Ready-transition re-validation at {head}:\n{}",
                        errs.iter()
                            .map(|e| format!("- {}: {}", e.field, e.message))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    let task =
                        escalation::escalation_task(task_id, dep_seq, &dep.project, 1, prompt);
                    self.tasks.put(&task).await?;
                    self.set_state(&mut dep, JobState::Escalated).await?;
                    self.publish(
                        owner,
                        project,
                        dep_seq,
                        "job-escalated",
                        serde_json::json!({ "reason": "revalidation_failed" }),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Handle `req.jobs.revoke.*` (spec §2.1 Revoked row). Returns the seqs of
    /// cascaded dependents. Task killing lands with the execution slice.
    pub async fn revoke_job(&mut self, owner: &str, project: &str, seq: u64) -> Result<Vec<u64>> {
        let job = self.must_get(owner, project, seq)?.clone();
        assert_transition(job.state, JobState::Revoked)?;

        let slug = format!("{owner}/{project}");
        let cascaded = self
            .graphs
            .get(&slug)
            .map(|g| g.cascade_targets(seq))
            .unwrap_or_default();

        for &target in std::iter::once(&seq).chain(cascaded.iter()) {
            let mut j = self.must_get(owner, project, target)?.clone();
            self.set_state(&mut j, JobState::Revoked).await?;
            // Delete job/{seq} if it exists; a missing branch is fine.
            let _ = self.repos.delete_branch(owner, project, &j.branch).await;
            self.queue.remove(&queue::QueuedJob {
                owner: owner.into(),
                project: project.into(),
                seq: target,
            });
        }
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

    fn must_get(&self, owner: &str, project: &str, seq: u64) -> Result<&Job> {
        self.graphs
            .get(&format!("{owner}/{project}"))
            .and_then(|g| g.get(seq))
            .ok_or_else(|| CoreError::NotFound(format!("{owner}/{project}#{seq}")))
    }

    /// The single state-write path: §2.1 guard, then KV, then memory.
    async fn set_state(&mut self, job: &mut Job, to: JobState) -> Result<()> {
        assert_transition(job.state, to)?;
        job.state = to;
        self.jobs.put(job).await?;
        self.graphs
            .entry(job.project.clone())
            .or_default()
            .insert(job.clone());
        Ok(())
    }

    async fn next_task_id(&self, owner: &str, project: &str, job_seq: u64) -> Result<u64> {
        // Sequential within job (§1.2); safe as read-then-write: single writer.
        Ok(self.tasks.list_for_job(owner, project, job_seq).await?.len() as u64 + 1)
    }

    async fn kv_names(&self, owner: &str, project: &str) -> Result<KvNames> {
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

    async fn publish(
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

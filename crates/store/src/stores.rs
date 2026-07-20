//! Typed KV accessors (spec §1.4). Each store wraps one bucket handle and
//! speaks domain types. Read-modify-write operations (counters, rdeps append,
//! step append) are safe without CAS because the dispatcher is the sole writer
//! of these buckets (spec §3.1).

use crate::{Result, StoreError, keys};
use futures::TryStreamExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use types::{Job, ProjectRecord, StepRecord, Task};

fn nats_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Nats(e.to_string())
}

/// Shared plumbing over one KV bucket.
#[derive(Clone)]
pub struct Bucket {
    kv: async_nats::jetstream::kv::Store,
}

impl Bucket {
    pub(crate) fn new(kv: async_nats::jetstream::kv::Store) -> Self {
        Self { kv }
    }

    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.kv.get(key).await.map_err(nats_err)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    pub async fn put_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        self.kv.put(key, bytes.into()).await.map_err(nats_err)?;
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.kv.purge(key).await.map_err(nats_err)
    }

    /// All keys in the bucket starting with `prefix`.
    pub async fn keys_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let keys: Vec<String> = self
            .kv
            .keys()
            .await
            .map_err(nats_err)?
            .try_collect()
            .await
            .map_err(nats_err)?;
        Ok(keys.into_iter().filter(|k| k.starts_with(prefix)).collect())
    }

    pub async fn list_prefix<T: DeserializeOwned>(&self, prefix: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        for key in self.keys_with_prefix(prefix).await? {
            if let Some(v) = self.get_json(&key).await? {
                out.push(v);
            }
        }
        Ok(out)
    }
}

#[derive(Clone)]
pub struct JobStore(pub(crate) Bucket);

impl JobStore {
    pub async fn put(&self, job: &Job) -> Result<()> {
        let (owner, project) = split_project(&job.project)?;
        self.0
            .put_json(&keys::job_key(owner, project, job.id), job)
            .await
    }

    pub async fn get(&self, owner: &str, project: &str, seq: u64) -> Result<Option<Job>> {
        self.0.get_json(&keys::job_key(owner, project, seq)).await
    }

    pub async fn list(&self, owner: &str, project: &str) -> Result<Vec<Job>> {
        let mut jobs: Vec<Job> = self.0.list_prefix(&format!("{owner}.{project}.")).await?;
        jobs.sort_by_key(|j| j.id);
        Ok(jobs)
    }

    /// Every job across all projects — the startup reconciliation scan (§3.6).
    pub async fn list_all(&self) -> Result<Vec<Job>> {
        self.0.list_prefix("").await
    }
}

#[derive(Clone)]
pub struct TaskStore(pub(crate) Bucket);

impl TaskStore {
    pub async fn put(&self, task: &Task) -> Result<()> {
        let (owner, project) = split_project(&task.project)?;
        self.0
            .put_json(&keys::task_key(owner, project, task.job_seq, task.id), task)
            .await
    }

    pub async fn get(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
    ) -> Result<Option<Task>> {
        self.0
            .get_json(&keys::task_key(owner, project, job_seq, task_id))
            .await
    }

    pub async fn list_for_job(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
    ) -> Result<Vec<Task>> {
        let mut tasks: Vec<Task> = self
            .0
            .list_prefix(&format!("{owner}.{project}.{job_seq}."))
            .await?;
        tasks.sort_by_key(|t| t.id);
        Ok(tasks)
    }

    /// All tasks in a project — the operator inbox scan (spec §6.1
    /// `req.tasks.list.pending`); callers filter by kind/state.
    pub async fn list_for_project(&self, owner: &str, project: &str) -> Result<Vec<Task>> {
        let mut tasks: Vec<Task> = self.0.list_prefix(&format!("{owner}.{project}.")).await?;
        tasks.sort_by_key(|t| (t.job_seq, t.id));
        Ok(tasks)
    }
}

/// Inline review step log (spec §1.2, §4.5): one key per work task holding a
/// JSON array of `StepRecord`s.
#[derive(Clone)]
pub struct StepStore(pub(crate) Bucket);

impl StepStore {
    pub async fn list(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
    ) -> Result<Vec<StepRecord>> {
        Ok(self
            .0
            .get_json(&keys::step_key(owner, project, job_seq, task_id))
            .await?
            .unwrap_or_default())
    }

    pub async fn put(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
        steps: &[StepRecord],
    ) -> Result<()> {
        self.0
            .put_json(&keys::step_key(owner, project, job_seq, task_id), &steps)
            .await
    }

    /// Append or update by `step` number: a `step-started` report appends; the
    /// matching `step-completed` report overwrites the same record.
    pub async fn upsert(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
        record: StepRecord,
    ) -> Result<()> {
        let mut steps = self.list(owner, project, job_seq, task_id).await?;
        match steps.iter_mut().find(|s| s.step == record.step) {
            Some(existing) => *existing = record,
            None => steps.push(record),
        }
        self.put(owner, project, job_seq, task_id, &steps).await
    }
}

/// Per-project sequential ID counter (spec §1.1). Safe as read-modify-write:
/// single-writer dispatcher.
#[derive(Clone)]
pub struct CounterStore(pub(crate) Bucket);

impl CounterStore {
    pub async fn next(&self, owner: &str, project: &str) -> Result<u64> {
        let key = format!("{owner}.{project}");
        let current: u64 = self.0.get_json(&key).await?.unwrap_or(0);
        let next = current + 1;
        self.0.put_json(&key, &next).await?;
        Ok(next)
    }
}

/// Inverse dependency index (spec §1.4): JSON array of job IDs that declare the
/// indexed job as an input. Derived cache — rebuilt on dispatcher startup.
#[derive(Clone)]
pub struct RdepsStore(pub(crate) Bucket);

impl RdepsStore {
    pub async fn get(&self, owner: &str, project: &str, seq: u64) -> Result<Vec<u64>> {
        Ok(self
            .0
            .get_json(&keys::job_key(owner, project, seq))
            .await?
            .unwrap_or_default())
    }

    pub async fn append(&self, owner: &str, project: &str, seq: u64, dependent: u64) -> Result<()> {
        let mut deps = self.get(owner, project, seq).await?;
        if !deps.contains(&dependent) {
            deps.push(dependent);
            self.0
                .put_json(&keys::job_key(owner, project, seq), &deps)
                .await?;
        }
        Ok(())
    }

    pub async fn put(&self, owner: &str, project: &str, seq: u64, deps: &[u64]) -> Result<()> {
        self.0
            .put_json(&keys::job_key(owner, project, seq), &deps)
            .await
    }
}

/// Platform-level project records (linked-origin projects). Absence of a
/// record means a classic self-hosted project — only linked projects (and any
/// future project-level platform state) get an entry.
#[derive(Clone)]
pub struct ProjectStore(pub(crate) Bucket);

impl ProjectStore {
    pub async fn put(&self, owner: &str, project: &str, record: &ProjectRecord) -> Result<()> {
        self.0.put_json(&format!("{owner}.{project}"), record).await
    }

    pub async fn get(&self, owner: &str, project: &str) -> Result<Option<ProjectRecord>> {
        self.0.get_json(&format!("{owner}.{project}")).await
    }

    /// Every `(owner.project key, record)` — the startup hold-restore scan.
    pub async fn list_all(&self) -> Result<Vec<(String, ProjectRecord)>> {
        let mut out = Vec::new();
        for key in self.0.keys_with_prefix("").await? {
            if let Some(record) = self.0.get_json(&key).await? {
                out.push((key, record));
            }
        }
        Ok(out)
    }
}

/// Split a `"{owner}/{repo}"` project slug into KV key components (spec §1.1
/// naming conventions).
pub fn split_project(project: &str) -> Result<(&str, &str)> {
    project
        .split_once('/')
        .filter(|(o, p)| !o.is_empty() && !p.is_empty())
        .ok_or_else(|| {
            StoreError::InvalidKey(format!("project slug {project:?} is not owner/repo"))
        })
}

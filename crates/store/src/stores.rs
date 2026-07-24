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

    /// Watch one key (or a `*`/`>`-wildcard key filter) for changes. Powers the
    /// watch-based test waits (#206).
    ///
    /// Uses `watch()` (`DeliverPolicy::New`): the stream stays open and delivers
    /// **every future** revision's value for the matching key(s) — a `Put` as
    /// `Some(value)`, a `Delete`/`Purge` as `None`. Delivering each revision
    /// (not just a bare "changed" pulse) is what lets a wait catch a *transient*
    /// state — e.g. a task that flips through `Pending` between relaunch
    /// attempts — that a re-read of the latest value would race past.
    ///
    /// New does **not** replay the current value, so a caller waiting on an
    /// already-present state must pair this with an initial read taken *after*
    /// the watch is created (so no put is lost in the gap). `watch_with_history`
    /// is deliberately avoided: it terminates immediately when no key matches at
    /// creation, which is exactly the wait-for-a-future-key case.
    pub async fn watch(&self, key: &str) -> Result<KvWatch> {
        use async_nats::jetstream::kv::Operation;
        use futures::StreamExt as _;
        let watch = self.kv.watch(key).await.map_err(nats_err)?;
        // A transport error ends the stream, so a bounded wait falls through to
        // its own named timeout instead of looping on errors.
        let inner = watch
            .take_while(|e| futures::future::ready(e.is_ok()))
            .map(|e| {
                let entry = e.expect("take_while kept only Ok items");
                match entry.operation {
                    Operation::Put => Some(entry.value.to_vec()),
                    Operation::Delete | Operation::Purge => None,
                }
            })
            .boxed();
        Ok(KvWatch { inner })
    }
}

/// A live watch over one KV key (or wildcard key filter). Test waits block on
/// this instead of sleep-polling (#206). Each item is one KV revision's value
/// (`Some(bytes)` for a put, `None` for a delete/purge).
pub struct KvWatch {
    inner: futures::stream::BoxStream<'static, Option<Vec<u8>>>,
}

impl KvWatch {
    /// Resolve when a watched key next changes; `false` once the underlying
    /// watch has ended (transport dropped), so the caller should stop waiting.
    /// Use this for a *re-read* wait, where the change is only a trigger to
    /// re-query some other surface (e.g. an HTTP endpoint).
    pub async fn changed(&mut self) -> bool {
        use futures::StreamExt as _;
        self.inner.next().await.is_some()
    }

    /// The next revision's value: outer `None` = the watch ended;
    /// `Some(None)` = a delete/purge; `Some(Some(bytes))` = a put's value.
    /// Use this to inspect *each* revision so a transient state is not missed.
    pub async fn next_value(&mut self) -> Option<Option<Vec<u8>>> {
        use futures::StreamExt as _;
        self.inner.next().await
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

    /// Watch one job's record for changes — the watch-based `wait_for_state`
    /// backing (#206). Create it before the first [`JobStore::get`] to avoid a
    /// lost wakeup.
    pub async fn watch(&self, owner: &str, project: &str, seq: u64) -> Result<KvWatch> {
        self.0.watch(&keys::job_key(owner, project, seq)).await
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

    /// Watch every task under one job (any task id) for changes — the
    /// watch-based `wait_for_task` backing (#206).
    pub async fn watch_job(&self, owner: &str, project: &str, job_seq: u64) -> Result<KvWatch> {
        self.0
            .watch(&format!("{owner}.{project}.{job_seq}.*"))
            .await
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

    /// Drop `dependent` from `seq`'s reverse-dependency list — the inverse of
    /// [`RdepsStore::append`], used when a Draft edit removes an upstream so the
    /// KV index does not keep a stale edge (spec §2.1, §2.3).
    pub async fn remove(&self, owner: &str, project: &str, seq: u64, dependent: u64) -> Result<()> {
        let mut deps = self.get(owner, project, seq).await?;
        if let Some(pos) = deps.iter().position(|&d| d == dependent) {
            deps.remove(pos);
            self.0
                .put_json(&keys::job_key(owner, project, seq), &deps)
                .await?;
        }
        Ok(())
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

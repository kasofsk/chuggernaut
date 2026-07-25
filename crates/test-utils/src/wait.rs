//! Watch-based test waits (#206, principle 3).
//!
//! Tier-2 tests used to wait by sleep-polling (`for _ in 0..400 { get; sleep }`).
//! That is slow and racy on a shared server. These helpers wait by *watcher*
//! instead — a KV watch ([`store::KvWatch`]) blocks until the watched key
//! changes — and every wait carries a hard [`tokio::time::timeout`] whose panic
//! message names what it was waiting for.
//!
//! Two watch styles, because a KV watch (`DeliverPolicy::New`) delivers each
//! *future* revision's value but never replays the current one:
//!
//! - [`job_state`] / [`job_where`] / [`task_where`] — the workhorses. They take
//!   an initial read *after* creating the watch (so no put is lost in the gap),
//!   then test the predicate against **each delivered revision** — catching a
//!   *transient* state (a task flipping through `Pending` between relaunch
//!   attempts) that a re-read of the latest value would race past.
//! - [`on_kv`] — a *re-read* wait: block on a change, then re-query some other
//!   surface (an HTTP endpoint, a snapshot key). For stable outcomes only.
//!
//! And for state no KV watch can see:
//!
//! - [`poll`] / [`poll_async`] — a *tightened* poll (10 ms + hard timeout +
//!   named message) for in-memory `FakeBackend` state or an RPC-liveness probe.

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;
use store::NatsStore;
use types::{Job, JobState, Task};

/// Default hard timeout for a wait before it panics. A wait normally resolves
/// in well under a second; the ceiling exists to fail loud (with a named
/// message) instead of hanging. It is set above the nominal 30s because a whole
/// `cargo test --workspace` runs many binaries — including tier-3 real-container
/// suites — against one shared NATS server per binary, and under that peak CPU
/// contention a tier-2 job can take tens of seconds. 60s keeps the hard-timeout
/// guarantee with margin so a slow-but-progressing wait is not failed spuriously.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Wait until job `seq` reaches `want`, returning the record (#206 principle 3).
pub async fn job_state(
    store: &NatsStore,
    owner: &str,
    project: &str,
    seq: u64,
    want: JobState,
) -> Job {
    job_where(
        store,
        owner,
        project,
        seq,
        format!("job {seq} to reach {want:?}"),
        move |j| j.state == want,
    )
    .await
}

/// Wait until job `seq` satisfies `pred`, returning the record. Watches the job
/// key: an initial read (taken after the watch is created, so no put is lost)
/// then every delivered revision, bounded by [`DEFAULT_TIMEOUT`].
// TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
#[allow(clippy::unwrap_used)]
pub async fn job_where(
    store: &NatsStore,
    owner: &str,
    project: &str,
    seq: u64,
    desc: impl Display,
    pred: impl Fn(&Job) -> bool,
) -> Job {
    let jobs = store.jobs().await.unwrap();
    let watch = jobs.watch(owner, project, seq).await.unwrap();
    let initial = || async { jobs.get(owner, project, seq).await.unwrap() };
    kv_wait::<Job, _, _, _, _>(watch, DEFAULT_TIMEOUT, desc, initial, |j| {
        pred(j).then(|| j.clone())
    })
    .await
}

/// Wait until some task of job `seq` satisfies `pred`, returning it. Watches the
/// job's task keys (initial scan + every delivered revision), so a transient
/// task state is caught rather than raced past.
// TODO(style): test-harness code — STYLE.md's test exemption is scoped to test targets, so the debt is annotated rather than assumed.
#[allow(clippy::unwrap_used)]
pub async fn task_where(
    store: &NatsStore,
    owner: &str,
    project: &str,
    seq: u64,
    desc: impl Display,
    pred: impl Fn(&Task) -> bool,
) -> Task {
    let tasks = store.tasks().await.unwrap();
    let watch = tasks.watch_job(owner, project, seq).await.unwrap();
    let initial = || async {
        tasks
            .list_for_job(owner, project, seq)
            .await
            .unwrap()
            .into_iter()
            .find(|t| pred(t))
    };
    kv_wait::<Task, _, _, _, _>(watch, DEFAULT_TIMEOUT, desc, initial, |t| {
        pred(t).then(|| t.clone())
    })
    .await
}

/// Core value-based wait: an `initial` scan (run once, after the watch already
/// exists, so no put is lost) plus a `check` against each delivered revision's
/// value (`V`). Returns the first `Some`; panics naming `desc` on timeout.
pub async fn kv_wait<V, T, FInit, FutInit, FCheck>(
    mut watch: store::KvWatch,
    timeout: Duration,
    desc: impl Display,
    mut initial: FInit,
    mut check: FCheck,
) -> T
where
    V: serde::de::DeserializeOwned,
    FInit: FnMut() -> FutInit,
    FutInit: Future<Output = Option<V>>,
    FCheck: FnMut(&V) -> Option<T>,
{
    let run = async {
        // The watch (DeliverPolicy::New) was created before this read, so a put
        // landing in the gap is still delivered below — nothing is lost.
        if let Some(value) = initial().await
            && let Some(found) = check(&value)
        {
            return found;
        }
        loop {
            match watch.next_value().await {
                Some(Some(bytes)) => {
                    if let Ok(value) = serde_json::from_slice::<V>(&bytes)
                        && let Some(found) = check(&value)
                    {
                        return found;
                    }
                }
                // A delete/purge — nothing to test.
                Some(None) => {}
                // New watches do not end on their own; a `None` means the
                // transport dropped. Re-scan and nudge, still under the timeout.
                None => {
                    if let Some(value) = initial().await
                        && let Some(found) = check(&value)
                    {
                        return found;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(value) => value,
        Err(_) => panic!("timed out after {timeout:?} waiting for {desc}"),
    }
}

/// Block on a KV `signal` (from [`store::JobStore::watch`],
/// [`store::TaskStore::watch_job`], or [`store::Bucket::watch`]), re-running
/// `check` on entry and after every change until it returns `Some`. Bounded by
/// `timeout`; on expiry it panics naming `desc`.
///
/// A *re-read* wait: `check` re-queries some surface (an HTTP endpoint, a
/// snapshot key) — the change is only a trigger. For **stable** outcomes; a
/// transient KV state should use [`job_where`]/[`task_where`] instead, which
/// inspect the value that changed rather than re-reading the latest.
pub async fn on_kv<T, F, Fut>(
    mut signal: store::KvWatch,
    timeout: Duration,
    desc: impl Display,
    mut check: F,
) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let run = async {
        loop {
            if let Some(value) = check().await {
                return value;
            }
            if !signal.changed().await {
                // The watch ended (transport dropped). Fall back to a short
                // nudge so we re-check; still bounded by the outer timeout.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(value) => value,
        Err(_) => panic!("timed out after {timeout:?} waiting for {desc}"),
    }
}

/// [`on_kv`] with [`DEFAULT_TIMEOUT`].
pub async fn on_kv_default<T, F, Fut>(signal: store::KvWatch, desc: impl Display, check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    on_kv(signal, DEFAULT_TIMEOUT, desc, check).await
}

/// A tightened poll for waits that observe **in-memory** state a KV watch
/// cannot see (e.g. `FakeBackend` call logs): 10 ms interval, hard `timeout`,
/// and a panic that names `desc`.
pub async fn poll<T, F>(timeout: Duration, desc: impl Display, mut check: F) -> T
where
    F: FnMut() -> Option<T>,
{
    let run = async {
        loop {
            if let Some(value) = check() {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(value) => value,
        Err(_) => panic!("timed out after {timeout:?} waiting for {desc}"),
    }
}

/// [`poll`] with [`DEFAULT_TIMEOUT`].
pub async fn poll_default<T, F>(desc: impl Display, check: F) -> T
where
    F: FnMut() -> Option<T>,
{
    poll(DEFAULT_TIMEOUT, desc, check).await
}

/// Like [`poll`] but for an **async** predicate that no KV watch can express —
/// e.g. an RPC-liveness probe against a worker daemon's NATS subscription.
/// 10 ms between attempts, hard `timeout`, named-`desc` panic.
pub async fn poll_async<T, F, Fut>(timeout: Duration, desc: impl Display, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let run = async {
        loop {
            if let Some(value) = check().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(value) => value,
        Err(_) => panic!("timed out after {timeout:?} waiting for {desc}"),
    }
}

/// [`poll_async`] with [`DEFAULT_TIMEOUT`].
pub async fn poll_async_default<T, F, Fut>(desc: impl Display, check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    poll_async(DEFAULT_TIMEOUT, desc, check).await
}

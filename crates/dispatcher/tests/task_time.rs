//! Tier-2 tests for the job record's task time (`Job::task_time_ms`): the
//! recompute the dispatcher runs at its single task-write path.
//!
//! The summing rule itself is pure and pinned in `types` (`task_time_ms`);
//! what needs a real store is the *write* half — that completing a task moves
//! the owning job's total, that recomputing is stable rather than cumulative,
//! and that the in-memory graph copy moves with the KV record (a stale graph
//! copy would have the next transition write the old value straight back).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{DateTime, Duration, Utc};
use dispatcher::core::{Core, CoreConfig, CreateSpec};
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{Task, TaskKind, TaskPhase, TaskState};

mod common;
use common::assert_invariants;

const BUILD_YAML: &str = r#"
name: build
image: img:latest
work:
  type: command
  run: ./build.sh
"#;

async fn setup() -> Option<(NatsStore, Core, u64)> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/build.yaml", BUILD_YAML.as_bytes(), "add build")
        .await;
    clone.push("main").await;
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let job = core
        .create_job(CreateSpec {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "build".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            knowledge_tags: vec![],
            eval: vec![],
            timeout: None,
            model: None,
            factory: None,
            members: vec![],
            inputs: Default::default(),
            groups: vec![],
            draft: false,
        })
        .await
        .unwrap();
    assert_invariants(&core);
    Some((store, core, job.id))
}

fn base() -> DateTime<Utc> {
    "2026-07-24T09:00:00Z".parse().unwrap()
}

/// A task with the given id/cycle and an optional span, in minutes past a fixed
/// epoch. `None` for `started_min` models the records that never ran — a parked
/// human attempt or a launch that was cancelled while queued.
fn task(
    id: u64,
    job_seq: u64,
    cycle: u32,
    started_min: Option<i64>,
    done_min: Option<i64>,
) -> Task {
    let at = |m: i64| base() + Duration::minutes(m);
    Task {
        id,
        job_seq,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle,
        kind: TaskKind::Command {
            run: "./build.sh".into(),
        },
        state: TaskState::Done,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: None,
        rework_reason: None,
        infra_loss: false,
        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: base(),
        started_at: started_min.map(at),
        completed_at: done_min.map(at),
    }
}

async fn stored_task_time(store: &NatsStore, seq: u64) -> Option<u64> {
    store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", seq)
        .await
        .unwrap()
        .unwrap()
        .task_time_ms
}

const MIN_MS: u64 = 60 * 1000;

#[tokio::test]
async fn completing_a_task_recomputes_the_job_total_and_never_accumulates() {
    let Some((store, mut core, seq)) = setup().await else {
        return;
    };
    // A fresh job has run nothing, so it has no total to show.
    assert_eq!(stored_task_time(&store, seq).await, None);

    // Cycle 1's work task: 10 minutes of work.
    core.task_put(&task(1, seq, 1, Some(0), Some(10)))
        .await
        .unwrap();
    assert_invariants(&core);
    assert_eq!(stored_task_time(&store, seq).await, Some(10 * MIN_MS));

    // A rework cycle 30 minutes later adds its own 5 minutes — the gap between
    // the two is waiting, and must not appear in the total.
    core.task_put(&task(2, seq, 2, Some(40), Some(45)))
        .await
        .unwrap();
    assert_invariants(&core);
    assert_eq!(stored_task_time(&store, seq).await, Some(15 * MIN_MS));

    // Writing an already-counted task again is a recompute, not a `+=`: the
    // total is stable. This is the property that lets a lost write self-heal.
    core.task_put(&task(1, seq, 1, Some(0), Some(10)))
        .await
        .unwrap();
    assert_invariants(&core);
    assert_eq!(stored_task_time(&store, seq).await, Some(15 * MIN_MS));

    // A task that never started contributes nothing.
    let mut parked = task(3, seq, 3, None, None);
    parked.state = TaskState::Pending;
    core.task_put(&parked).await.unwrap();
    assert_invariants(&core);
    assert_eq!(stored_task_time(&store, seq).await, Some(15 * MIN_MS));

    // The in-memory graph — the copy every later job write starts from — moved
    // with the record, so the next transition cannot revert the total.
    assert_eq!(
        core.graph("acme", "api")
            .unwrap()
            .get(seq)
            .unwrap()
            .task_time_ms,
        Some(15 * MIN_MS)
    );
}

#[tokio::test]
async fn task_time_survives_the_terminal_transition() {
    let Some((store, mut core, seq)) = setup().await else {
        return;
    };
    core.task_put(&task(1, seq, 1, Some(0), Some(10)))
        .await
        .unwrap();
    assert_invariants(&core);
    // Revoke is the shortest public path to a terminal state-write; the stamp
    // it sets must not carry the job's total away with it.
    core.revoke_job("acme", "api", seq).await.unwrap();
    assert_invariants(&core);
    let job = store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", seq)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.state, types::JobState::Revoked);
    assert!(job.completed_at.is_some());
    assert_eq!(job.task_time_ms, Some(10 * MIN_MS));
}

#[tokio::test]
async fn revoking_keeps_the_span_of_the_attempt_it_closes() {
    let Some((store, mut core, seq)) = setup().await else {
        return;
    };
    core.task_put(&task(1, seq, 1, Some(0), Some(10)))
        .await
        .unwrap();
    assert_invariants(&core);

    // A claimed human attempt: Pending, but with a real `started_at`, so the
    // revoke path's own `close_pending_tasks` stamps it with a span rather
    // than closing an empty record. Nothing is written for this job after the
    // revoke — Revoked is terminal — so a total lost here is lost for good.
    let mut claimed = task(2, seq, 2, None, None);
    claimed.state = TaskState::Pending;
    claimed.performed_by = Some(types::Performer::Human);
    claimed.started_at = Some(Utc::now() - Duration::minutes(5));
    core.task_put(&claimed).await.unwrap();
    assert_invariants(&core);
    assert_eq!(stored_task_time(&store, seq).await, Some(10 * MIN_MS));

    core.revoke_job("acme", "api", seq).await.unwrap();
    assert_invariants(&core);

    // 10 minutes of finished work plus the ~5 minutes the closed attempt ran.
    let total = stored_task_time(&store, seq).await.unwrap();
    assert!(
        (15 * MIN_MS..16 * MIN_MS).contains(&total),
        "expected ~15 min, got {total} ms"
    );
    // `set_state` dual-writes KV and the graph, so both must carry the total.
    assert_eq!(
        core.graph("acme", "api")
            .unwrap()
            .get(seq)
            .unwrap()
            .task_time_ms,
        Some(total)
    );
}

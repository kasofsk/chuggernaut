//! Tier-2 tests for the live-output handler (`req.tasks.output`, spec §4.2):
//! a running task's container tail, the `running:false` fallback cue for a
//! finished task, 404 before a container exists, an error envelope for a
//! wedged node, and — the whole point of serving it off the core actor — a
//! stalled tail never blocking the rest of the `req.tasks.>` family.

use dispatcher::core::{Core, CoreConfig, spawn};
use dispatcher::handlers::spawn_tasks_handler;
use std::sync::Arc;
use std::time::{Duration, Instant};
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{Task, TaskKind, TaskPhase, TaskState};

fn running_task(id: u64, container_id: Option<&str>) -> Task {
    Task {
        id,
        job_seq: 1,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Command {
            run: "cargo build".into(),
        },
        state: TaskState::Running,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: container_id.map(String::from),
        rework_reason: None,
        infra_loss: false,

        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
    }
}

async fn setup(server_url: &str) -> (NatsStore, Arc<FakeBackend>) {
    let store = NatsStore::connect_namespaced(server_url, &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    // A real (empty) repo just so Core::new has somewhere to load from; the
    // output handler itself only touches the tasks KV and the backend.
    let repo = TempRepo::create("acme", "api").await;
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let backend = Arc::new(FakeBackend::new());
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server_url.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    spawn_tasks_handler(&store, handle, backend.clone())
        .await
        .unwrap();
    (store, backend)
}

async fn output(store: &NatsStore, task_id: u64, since: u64) -> serde_json::Value {
    let subject = store::subjects::tasks_output("acme", "api", 1, task_id);
    let payload = serde_json::json!({ "since": since });
    let reply = store
        .request_timeout(
            &subject,
            &serde_json::to_vec(&payload).unwrap(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    serde_json::from_slice(&reply.payload).unwrap()
}

#[tokio::test]
async fn output_tails_running_serves_fallback_and_404s() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let (store, backend) = setup(server.url()).await;
    backend.put_logs(b"compiling chuggernaut v0.1.0\ncompiling store\n".to_vec());
    let tasks = store.tasks().await.unwrap();

    // A running task with a live container → its tail, running: true, cursor > 0.
    tasks.put(&running_task(1, Some("fake/c1"))).await.unwrap();
    let v = output(&store, 1, 0).await;
    assert_eq!(v["running"], true);
    assert!(
        v["data"].as_str().unwrap().contains("compiling store"),
        "live tail missing output: {v}"
    );
    let offset = v["offset"].as_u64().unwrap();
    assert!(offset > 0);
    // Polling from the cursor: caught up, empty data, same offset (monotonic).
    let v = output(&store, 1, offset).await;
    assert_eq!(v["running"], true);
    assert_eq!(v["data"], "");
    assert_eq!(v["offset"].as_u64().unwrap(), offset);

    // A finished task → running: false (the api's cue to serve the artifact).
    let mut done = running_task(2, Some("fake/c2"));
    done.state = TaskState::Done;
    tasks.put(&done).await.unwrap();
    let v = output(&store, 2, 0).await;
    assert_eq!(v["running"], false);

    // A running task with no container yet → 404.
    tasks.put(&running_task(3, None)).await.unwrap();
    let v = output(&store, 3, 0).await;
    assert_eq!(v["error"]["status"], 404);

    // An unknown task → 404.
    let v = output(&store, 999, 0).await;
    assert_eq!(v["error"]["status"], 404);
}

#[tokio::test]
async fn wedged_node_errors_without_blocking_other_requests() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let (store, backend) = setup(server.url()).await;
    let tasks = store.tasks().await.unwrap();
    tasks.put(&running_task(1, Some("fake/c1"))).await.unwrap();

    // A wedged node: the tail stalls well past a normal request. Fire it in the
    // background and leave it hanging.
    backend.stall_logs_tail(Duration::from_secs(3));
    let bg_store = store.clone();
    let stalled = tokio::spawn(async move { output(&bg_store, 1, 0).await });
    tokio::time::sleep(Duration::from_millis(100)).await; // let it start & stall

    // Another leg of req.tasks.> must answer promptly — the stalled tail is
    // spawned off the subscription loop, so it never wedges list/resolve.
    let start = Instant::now();
    let list = store
        .request_timeout(
            &store::subjects::tasks_list("acme", "api", 1),
            b"{}",
            Duration::from_secs(2),
        )
        .await
        .unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "task list blocked behind a stalled output tail: {:?}",
        start.elapsed()
    );
    let listed: serde_json::Value = serde_json::from_slice(&list.payload).unwrap();
    assert!(listed.is_array(), "expected a task list: {listed}");
    stalled.abort();

    // An unreachable node surfaces an error envelope, not a hang.
    backend.stall_logs_tail(Duration::ZERO);
    backend.fail_logs_tail("worker node unreachable");
    let v = output(&store, 1, 0).await;
    assert_eq!(v["error"]["status"], 502, "wedged node should be 502: {v}");
}

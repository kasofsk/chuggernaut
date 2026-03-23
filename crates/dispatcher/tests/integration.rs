use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ImageExt;
use testcontainers_modules::nats::{Nats, NatsServerCmd};

use forge2_dispatcher::{
    config::Config, handlers, jobs, nats_init, recovery, state::DispatcherState,
};
use forge2_types::*;

// ---------------------------------------------------------------------------
// Shared NATS container — one per test process, leaked for lifetime
// ---------------------------------------------------------------------------

static TEST_NATS_PORT: tokio::sync::OnceCell<u16> = tokio::sync::OnceCell::const_new();

async fn test_nats_port() -> u16 {
    *TEST_NATS_PORT
        .get_or_init(|| async {
            let nats_cmd = NatsServerCmd::default().with_jetstream();
            let container = Nats::default()
                .with_cmd(&nats_cmd)
                .start()
                .await
                .unwrap();
            let port = container.get_host_port_ipv4(4222).await.unwrap();
            // Leak the container so it lives for the entire test process
            Box::leak(Box::new(container));
            port
        })
        .await
}

/// Each test gets its own UUID-namespaced dispatcher state.
/// All KV buckets and NATS subjects are prefixed so tests run in parallel
/// without interfering with each other.
async fn setup() -> Arc<DispatcherState> {
    let port = test_nats_port().await;
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let config = Config {
        nats_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 60,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        blacklist_ttl_secs: 3600,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());

    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, config.blacklist_ttl_secs, Some(&prefix))
        .await
        .unwrap();

    DispatcherState::new_namespaced(config, client, js, kv, prefix)
}

// ---------------------------------------------------------------------------
// Job creation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_job_on_deck() {
    let state = setup().await;
    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Test job".to_string(),
        body: "Test body".to_string(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    assert_eq!(key, "test.repo.1");
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);
}

#[tokio::test]
async fn create_job_on_ice() {
    let state = setup().await;
    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Held".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: Some(JobState::OnIce),
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnIce);
}

#[tokio::test]
async fn create_job_sequential_keys() {
    let state = setup().await;
    let make = || CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Job".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let k1 = jobs::create_job(&state, make()).await.unwrap();
    let k2 = jobs::create_job(&state, make()).await.unwrap();
    let k3 = jobs::create_job(&state, make()).await.unwrap();
    assert_eq!(k1, "test.repo.1");
    assert_eq!(k2, "test.repo.2");
    assert_eq!(k3, "test.repo.3");
}

#[tokio::test]
async fn create_job_rejects_dots_in_name() {
    let state = setup().await;
    let req = CreateJobRequest {
        repo: "bad.owner/repo".to_string(),
        title: "Bad".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    assert!(jobs::create_job(&state, req).await.is_err());
}

// ---------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deps_blocked_then_unblocked() {
    let state = setup().await;
    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);

    // Complete A → B unblocks
    jobs::transition_job(&state, &key_a, JobState::Done, "test", None).await.unwrap();
    let unblocked = forge2_dispatcher::deps::propagate_unblock(&state, &key_a).await.unwrap();
    assert_eq!(unblocked, vec![key_b.clone()]);
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::OnDeck);
}

#[tokio::test]
async fn diamond_deps_partial_unblock() {
    let state = setup().await;
    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![])).await.unwrap();
    let key_c = jobs::create_job(&state, make("C", vec![1, 2])).await.unwrap();
    assert_eq!(state.jobs.get(&key_c).unwrap().state, JobState::Blocked);

    // Complete A — C stays blocked (B not done)
    jobs::transition_job(&state, &key_a, JobState::Done, "test", None).await.unwrap();
    let unblocked = forge2_dispatcher::deps::propagate_unblock(&state, &key_a).await.unwrap();
    assert!(unblocked.is_empty());
    assert_eq!(state.jobs.get(&key_c).unwrap().state, JobState::Blocked);

    // Complete B — C unblocks
    jobs::transition_job(&state, &key_b, JobState::Done, "test", None).await.unwrap();
    let unblocked = forge2_dispatcher::deps::propagate_unblock(&state, &key_b).await.unwrap();
    assert_eq!(unblocked, vec![key_c.clone()]);
    assert_eq!(state.jobs.get(&key_c).unwrap().state, JobState::OnDeck);
}

// ---------------------------------------------------------------------------
// Worker lifecycle via NATS (uses namespaced subjects)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_register_via_nats() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let reg = WorkerRegistration {
        worker_id: "w1".to_string(),
        capabilities: vec!["rust".to_string()],
        worker_type: "sim".to_string(),
        platform: vec!["linux".to_string()],
    };
    let payload = Bytes::from(serde_json::to_vec(&reg).unwrap());
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request(subjects::WORKER_REGISTER, payload),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!reply.payload.is_empty());

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(state.workers.contains_key("w1"));
    assert_eq!(state.workers.get("w1").unwrap().state, WorkerState::Idle);
}

#[tokio::test]
async fn worker_idle_triggers_assignment() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    // Create a job
    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Assign me".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Subscribe to assignment channel (auto-prefixed)
    let assign_sub = state.nats.subscribe(&subjects::dispatch_assign("w1")).await.unwrap();

    // Register worker
    let reg = WorkerRegistration {
        worker_id: "w1".to_string(),
        capabilities: vec![],
        worker_type: "sim".to_string(),
        platform: vec![],
    };
    state.nats.request(subjects::WORKER_REGISTER, Bytes::from(serde_json::to_vec(&reg).unwrap())).await.unwrap();

    // Publish idle
    let idle = IdleEvent { worker_id: "w1".to_string() };
    state.nats.publish(subjects::WORKER_IDLE, Bytes::from(serde_json::to_vec(&idle).unwrap())).await.unwrap();

    // Wait for assignment
    let msg = tokio::time::timeout(Duration::from_secs(5), assign_sub.into_future())
        .await.unwrap().0.unwrap();
    let assignment: Assignment = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(assignment.job.key, key);

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);
}

#[tokio::test]
async fn worker_yield_to_in_review() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Yield test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Register + assign directly
    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request(subjects::WORKER_REGISTER, Bytes::from(serde_json::to_vec(&reg).unwrap())).await.unwrap();
    forge2_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    // Worker yields
    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
    };
    state.nats.publish(subjects::WORKER_OUTCOME, Bytes::from(serde_json::to_vec(&outcome).unwrap())).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    let job = state.jobs.get(&key).unwrap();
    assert_eq!(job.state, JobState::InReview);
    assert_eq!(job.pr_url.as_deref(), Some("http://forgejo/test/repo/pulls/1"));
}

#[tokio::test]
async fn worker_fail_schedules_retry() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Fail test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request(subjects::WORKER_REGISTER, Bytes::from(serde_json::to_vec(&reg).unwrap())).await.unwrap();
    forge2_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail { reason: "compile error".to_string(), logs: None },
    };
    state.nats.publish(subjects::WORKER_OUTCOME, Bytes::from(serde_json::to_vec(&outcome).unwrap())).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    let job = state.jobs.get(&key).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.retry_count, 1);
    assert!(job.retry_after.is_some());
}

#[tokio::test]
async fn worker_abandon_blacklists() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Abandon test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request(subjects::WORKER_REGISTER, Bytes::from(serde_json::to_vec(&reg).unwrap())).await.unwrap();
    forge2_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Abandon {},
    };
    state.nats.publish(subjects::WORKER_OUTCOME, Bytes::from(serde_json::to_vec(&outcome).unwrap())).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);

    let bl_key = format!("{key}.w1");
    assert!(state.kv.abandon_blacklist.entry(&bl_key).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Admin commands via NATS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_close_unblocks_dependents() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);

    let close = CloseJobRequest { job_key: key_a.clone(), revoke: false };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request(subjects::ADMIN_CLOSE_JOB, Bytes::from(serde_json::to_vec(&close).unwrap())),
    ).await.unwrap().unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key_a).unwrap().state, JobState::Done);
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::OnDeck);
}

#[tokio::test]
async fn admin_revoke_keeps_dependents_blocked() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    let close = CloseJobRequest { job_key: key_a.clone(), revoke: true };
    state.nats.request(subjects::ADMIN_CLOSE_JOB, Bytes::from(serde_json::to_vec(&close).unwrap())).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key_a).unwrap().state, JobState::Revoked);
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);
}

// ---------------------------------------------------------------------------
// Heartbeat
// ---------------------------------------------------------------------------

#[tokio::test]
async fn heartbeat_renews_lease() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "HB test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request(subjects::WORKER_REGISTER, Bytes::from(serde_json::to_vec(&reg).unwrap())).await.unwrap();
    forge2_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let (before, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let hb = WorkerHeartbeat { worker_id: "w1".to_string(), job_key: key.clone() };
    state.nats.publish(subjects::WORKER_HEARTBEAT, Bytes::from(serde_json::to_vec(&hb).unwrap())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (after, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().unwrap();
    assert!(after.lease_deadline > before.lease_deadline);
}

// ---------------------------------------------------------------------------
// Recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_rebuilds_state() {
    let state = setup().await;

    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key1 = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key2 = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    // Simulate restart: clear in-memory state
    state.jobs.clear();
    state.workers.clear();
    { let mut g = state.graph.write().await; *g = forge2_dispatcher::state::DepGraph::new(); }

    recovery::recover(&state).await.unwrap();

    assert_eq!(state.jobs.len(), 2);
    assert!(state.jobs.contains_key(&key1));
    assert!(state.jobs.contains_key(&key2));

    let graph = state.graph.read().await;
    assert_eq!(graph.dag.edge_count(), 1);
}

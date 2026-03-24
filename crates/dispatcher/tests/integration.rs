use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use testcontainers::core::{CmdWaitFor, ExecCommand, IntoContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};
use testcontainers_modules::nats::{Nats, NatsServerCmd};

use chuggernaut_dispatcher::{
    config::Config, handlers, jobs, nats_init, recovery, state::DispatcherState,
};
use chuggernaut_types::*;

// ---------------------------------------------------------------------------
// Shared NATS container — one per test process, leaked for lifetime.
// Started on a dedicated OS thread to avoid tokio runtime conflicts
// when multiple #[tokio::test] instances race to initialise.
// ---------------------------------------------------------------------------

static TEST_NATS_PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();

fn test_nats_port() -> u16 {
    *TEST_NATS_PORT.get_or_init(|| {
        std::thread::spawn(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
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
        })
        .join()
        .unwrap()
    })
}

/// Each test gets its own UUID-namespaced dispatcher state.
/// All KV buckets and NATS subjects are prefixed so tests run in parallel
/// without interfering with each other.
async fn setup() -> Arc<DispatcherState> {
    let port = test_nats_port();
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
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
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
    let unblocked = chuggernaut_dispatcher::deps::propagate_unblock(&state, &key_a).await.unwrap();
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
    let unblocked = chuggernaut_dispatcher::deps::propagate_unblock(&state, &key_a).await.unwrap();
    assert!(unblocked.is_empty());
    assert_eq!(state.jobs.get(&key_c).unwrap().state, JobState::Blocked);

    // Complete B — C unblocks
    jobs::transition_job(&state, &key_b, JobState::Done, "test", None).await.unwrap();
    let unblocked = chuggernaut_dispatcher::deps::propagate_unblock(&state, &key_b).await.unwrap();
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
    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::WORKER_REGISTER, &reg),
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
    let assign_sub = state.nats.subscribe_dynamic(&subjects::DISPATCH_ASSIGN, "w1").await.unwrap();

    // Register worker
    let reg = WorkerRegistration {
        worker_id: "w1".to_string(),
        capabilities: vec![],
        worker_type: "sim".to_string(),
        platform: vec![],
    };
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();

    // Publish idle
    let idle = IdleEvent { worker_id: "w1".to_string() };
    state.nats.publish_msg(&subjects::WORKER_IDLE, &idle).await.unwrap();

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
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    // Worker yields
    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();

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
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail { reason: "compile error".to_string(), logs: None },
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();

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
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Abandon {},
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();

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
        state.nats.request_msg(&subjects::ADMIN_CLOSE_JOB, &close),
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
    state.nats.request_msg(&subjects::ADMIN_CLOSE_JOB, &close).await.unwrap();

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
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let (before, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let hb = WorkerHeartbeat { worker_id: "w1".to_string(), job_key: key.clone() };
    state.nats.publish_msg(&subjects::WORKER_HEARTBEAT, &hb).await.unwrap();
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
    { let mut g = state.graph.write().await; *g = chuggernaut_dispatcher::state::DepGraph::new(); }

    recovery::recover(&state).await.unwrap();

    assert_eq!(state.jobs.len(), 2);
    assert!(state.jobs.contains_key(&key1));
    assert!(state.jobs.contains_key(&key2));

    let graph = state.graph.read().await;
    assert_eq!(graph.dag.edge_count(), 1);
}

// ---------------------------------------------------------------------------
// Review decisions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn review_approved_completes_job_and_unblocks_deps() {
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

    // A (no deps) → B depends on A
    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);

    // Register worker + assign A + yield (→ InReview)
    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key_a, "w1", false, None).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key_a.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key_a).unwrap().state, JobState::InReview);

    // Publish ReviewDecision Approved
    let decision = ReviewDecision {
        job_key: key_a.clone(),
        decision: DecisionType::Approved,
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A → Done, B → OnDeck (unblocked)
    assert_eq!(state.jobs.get(&key_a).unwrap().state, JobState::Done);
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::OnDeck);
}

#[tokio::test]
async fn review_changes_requested_transitions_job() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Review CR".to_string(),
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

    // Register + assign + yield → InReview
    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

    // Publish ChangesRequested
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested { feedback: "fix the tests".to_string() },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::ChangesRequested);
}

// ---------------------------------------------------------------------------
// Preemption
// ---------------------------------------------------------------------------

#[tokio::test]
async fn high_priority_job_sends_preempt_notice() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    // Create a low-priority job
    let low_req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Low prio".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 30,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let low_key = jobs::create_job(&state, low_req).await.unwrap();

    // Register worker and assign the low-priority job
    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &low_key, "w1", false, None).await.unwrap();
    assert_eq!(state.jobs.get(&low_key).unwrap().state, JobState::OnTheStack);

    // Subscribe to preempt notices for w1
    let preempt_sub = state.nats.subscribe_dynamic(&subjects::DISPATCH_PREEMPT, "w1").await.unwrap();

    // Create a high-priority job (priority > 50 triggers preemption)
    let high_req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "High prio".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 80,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let high_key = jobs::create_job(&state, high_req).await.unwrap();

    // Trigger assignment attempt for the high-priority job (no idle workers → preemption)
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &high_key).await.unwrap();

    // Should receive preempt notice
    let msg = tokio::time::timeout(Duration::from_secs(5), preempt_sub.into_future())
        .await.unwrap().0.unwrap();
    let notice: PreemptNotice = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(notice.new_job_key, high_key);
}

// ---------------------------------------------------------------------------
// Monitor: lease expiry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn monitor_lease_expiry_fails_job() {
    // Use very short lease + scan interval so the test completes quickly
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let config = Config {
        nats_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 1,
        default_timeout_secs: 3600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 1,
        job_retention_secs: 86400,
        activity_limit: 50,
        blacklist_ttl_secs: 3600,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, config.blacklist_ttl_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    handlers::start_handlers(state.clone()).await.unwrap();
    chuggernaut_dispatcher::monitor::start(state.clone());

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Lease test".to_string(),
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

    // Register worker and assign
    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    // Don't send heartbeats — wait for lease to expire (1s lease + 1s scan interval)
    tokio::time::sleep(Duration::from_secs(4)).await;

    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);
}

// ---------------------------------------------------------------------------
// Admin requeue from Failed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_requeue_from_failed() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Requeue test".to_string(),
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

    // Assign + fail the job
    let reg = WorkerRegistration { worker_id: "w1".to_string(), capabilities: vec![], worker_type: "sim".to_string(), platform: vec![] };
    state.nats.request_msg(&subjects::WORKER_REGISTER, &reg).await.unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&state, &key, "w1", false, None).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail { reason: "compile error".to_string(), logs: None },
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);

    // Admin requeue to OnDeck
    let requeue = RequeueRequest { job_key: key.clone(), target: RequeueTarget::OnDeck };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_REQUEUE, &requeue),
    ).await.unwrap().unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);
}

// ---------------------------------------------------------------------------
// Action dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn action_dispatch_creates_claim_and_transitions() {
    // This test verifies the dispatcher's action dispatch path WITHOUT a real
    // Forgejo runner. It creates a Forgejo instance with a noop workflow file,
    // then dispatches an action-type job. The dispatch should create a claim
    // and transition the job to OnTheStack.
    //
    // The action won't actually run (no runner), but we verify the dispatcher
    // side of the protocol is correct.

    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    // Start Forgejo for this test
    let forgejo = testcontainers::GenericImage::new("codeberg.org/forgejo/forgejo", "14")
        .with_exposed_port(3000.tcp())
        .with_wait_for(WaitFor::seconds(5))
        .with_env_var("FORGEJO__database__DB_TYPE", "sqlite3")
        .with_env_var("FORGEJO__security__INSTALL_LOCK", "true")
        .with_env_var("FORGEJO__service__DISABLE_REGISTRATION", "true")
        .with_env_var("FORGEJO__actions__ENABLED", "true")
        .with_env_var("FORGEJO__log__LEVEL", "Warn")
        .with_startup_timeout(Duration::from_secs(120))
        .start()
        .await
        .unwrap();

    let forgejo_port = forgejo.get_host_port_ipv4(3000.tcp()).await.unwrap();
    let forgejo_url = format!("http://127.0.0.1:{forgejo_port}");

    // Wait for API
    let http = reqwest::Client::new();
    for _ in 0..60 {
        if http.get(format!("{forgejo_url}/api/v1/version"))
            .send().await.map(|r| r.status().is_success()).unwrap_or(false) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Small extra wait for Forgejo to fully initialize
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create admin user — don't require exit code 0 so we can read stderr
    let mut exec_result = forgejo.exec(
        ExecCommand::new([
            "su-exec", "git", "forgejo", "admin", "user", "create",
            "--admin", "--username", "testadmin", "--password", "testadmin",
            "--email", "testadmin@test.local", "--must-change-password=false",
        ]),
    ).await.unwrap();
    let stdout = exec_result.stdout_to_vec().await.unwrap();
    let stderr = exec_result.stderr_to_vec().await.unwrap();
    let exit = exec_result.exit_code().await.unwrap();
    eprintln!("exec exit={exit:?} stdout={} stderr={}",
        String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
    assert_eq!(exit, Some(0), "admin user creation failed");

    let token_resp: serde_json::Value = http
        .post(format!("{forgejo_url}/api/v1/users/testadmin/tokens"))
        .basic_auth("testadmin", Some("testadmin"))
        .json(&serde_json::json!({"name": "test", "scopes": ["all"]}))
        .send().await.unwrap()
        .json().await.unwrap();
    let token = token_resp["sha1"].as_str().unwrap().to_string();

    // Create org + repo + workflow
    let org = format!("org{}", &prefix[..8]);
    http.post(format!("{forgejo_url}/api/v1/orgs"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"username": &org, "visibility": "public"}))
        .send().await.unwrap();

    http.post(format!("{forgejo_url}/api/v1/orgs/{org}/repos"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"name": "repo", "default_branch": "main", "auto_init": true}))
        .send().await.unwrap();

    // Push a noop workflow so dispatch_workflow succeeds
    let workflow = "name: work\non:\n  workflow_dispatch:\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    http.post(format!("{forgejo_url}/api/v1/repos/{org}/repo/contents/.forgejo/workflows/work.yml"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({
            "message": "add workflow",
            "content": base64_encode(workflow)
        }))
        .send().await.unwrap();

    // Set up dispatcher with Forgejo config
    let config = Config {
        nats_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 60,
        default_timeout_secs: 600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        blacklist_ttl_secs: 3600,
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
        action_workflow: "work.yml".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, config.blacklist_ttl_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);
    handlers::start_handlers(state.clone()).await.unwrap();

    // Create an action-type job
    let req = CreateJobRequest {
        repo: format!("{org}/repo"),
        title: "Action dispatch test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: Some("action".to_string()),
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Trigger assignment — should dispatch action directly
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key).await.unwrap();

    // Verify: job is OnTheStack, claim exists with action- prefix
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    let (claim, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await.unwrap().unwrap();
    assert!(claim.worker_id.starts_with("action-"), "worker_id should be action-{key}");
}

// ---------------------------------------------------------------------------
// Action runner isolation test: Forgejo + runner + NATS, no dispatcher
// ---------------------------------------------------------------------------

/// Focused test: run a Forgejo Action that executes chuggernaut-worker,
/// which publishes WorkerOutcome to NATS. No dispatcher involved.
///
/// Run: `cargo test -p chuggernaut-dispatcher --test integration action_runner_publishes -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn action_runner_publishes_outcome_to_nats() {
    let nats_port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{nats_port}");
    let nats_internal_url = format!("nats://host.docker.internal:{nats_port}");

    // Start Forgejo
    let forgejo = GenericImage::new("codeberg.org/forgejo/forgejo", "14")
        .with_exposed_port(3000.tcp())
        .with_wait_for(WaitFor::seconds(5))
        .with_env_var("FORGEJO__database__DB_TYPE", "sqlite3")
        .with_env_var("FORGEJO__security__INSTALL_LOCK", "true")
        .with_env_var("FORGEJO__service__DISABLE_REGISTRATION", "true")
        .with_env_var("FORGEJO__actions__ENABLED", "true")
        .with_env_var("FORGEJO__log__LEVEL", "Warn")
        .with_startup_timeout(Duration::from_secs(120))
        .start()
        .await
        .unwrap();

    let forgejo_port = forgejo.get_host_port_ipv4(3000.tcp()).await.unwrap();
    let forgejo_url = format!("http://127.0.0.1:{forgejo_port}");
    let forgejo_internal_url = format!("http://host.docker.internal:{forgejo_port}");

    // Wait for API
    let http = reqwest::Client::new();
    for _ in 0..60 {
        if http
            .get(format!("{forgejo_url}/api/v1/version"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create admin + token
    forgejo
        .exec(
            ExecCommand::new([
                "su-exec", "git", "forgejo", "admin", "user", "create",
                "--admin", "--username", "testadmin", "--password", "testadmin",
                "--email", "testadmin@test.local", "--must-change-password=false",
            ])
            .with_cmd_ready_condition(CmdWaitFor::exit_code(0)),
        )
        .await
        .unwrap();

    let token_resp: serde_json::Value = http
        .post(format!("{forgejo_url}/api/v1/users/testadmin/tokens"))
        .basic_auth("testadmin", Some("testadmin"))
        .json(&serde_json::json!({"name": "test", "scopes": ["all"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = token_resp["sha1"].as_str().unwrap().to_string();

    // Get runner registration token
    let reg_resp: serde_json::Value = http
        .get(format!(
            "{forgejo_url}/api/v1/admin/runners/registration-token"
        ))
        .header("Authorization", format!("token {token}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let runner_reg_token = reg_resp["token"].as_str().unwrap().to_string();

    // Create org + repo
    let org = "testorg";
    http.post(format!("{forgejo_url}/api/v1/orgs"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"username": org, "visibility": "public"}))
        .send()
        .await
        .unwrap();

    http.post(format!("{forgejo_url}/api/v1/orgs/{org}/repos"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"name": "repo", "default_branch": "main", "auto_init": true}))
        .send()
        .await
        .unwrap();

    // Push a simple workflow — first just echo to verify the runner works
    let workflow = format!(
        r#"name: test-action
on:
  workflow_dispatch:
    inputs:
      job_key:
        required: true
        type: string
      nats_url:
        required: true
        type: string
jobs:
  work:
    runs-on: ubuntu-latest
    steps:
      - name: Run worker
        run: |
          chuggernaut-worker \
            --job-key "${{{{ inputs.job_key }}}}" \
            --nats-url "${{{{ inputs.nats_url }}}}" \
            --forgejo-url "{forgejo_internal_url}" \
            --forgejo-token "${{{{ secrets.CHUGGERNAUT_FORGEJO_TOKEN }}}}" \
            --command mock-claude \
            --heartbeat-interval-secs 5
"#
    );

    http.post(format!(
            "{forgejo_url}/api/v1/repos/{org}/repo/contents/.forgejo/workflows/work.yml"
        ))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({
            "message": "add workflow",
            "content": base64_encode(&workflow)
        }))
        .send()
        .await
        .unwrap();

    // Set secret
    let secret_resp = http
        .put(format!(
            "{forgejo_url}/api/v1/repos/{org}/repo/actions/secrets/CHUGGERNAUT_FORGEJO_TOKEN"
        ))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"data": &token}))
        .send()
        .await
        .unwrap();
    eprintln!("Secret set: status={}", secret_resp.status());

    // Start runner
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let runner_config_path = workspace_root
        .join("infra/runner/config.yaml")
        .canonicalize()
        .unwrap();

    let labels = "ubuntu-latest:docker://chuggernaut-runner-env:latest";
    let register_and_run = format!(
        "cd /data && forgejo-runner register --no-interactive --instance '{forgejo_internal_url}' --token '{runner_reg_token}' --name runner --labels '{labels}' && forgejo-runner daemon"
    );

    let _runner = GenericImage::new("data.forgejo.org/forgejo/runner", "11")
        .with_user("root")
        .with_mount(Mount::bind_mount(
            "/var/run/docker.sock",
            "/var/run/docker.sock",
        ))
        .with_mount(Mount::bind_mount(
            runner_config_path.to_str().unwrap(),
            "/data/config.yaml",
        ))
        .with_cmd(["sh", "-c", &register_and_run])
        .with_startup_timeout(Duration::from_secs(60))
        .start()
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(15)).await;

    // Subscribe to NATS worker outcome BEFORE dispatching
    let nats_client = async_nats::connect(&nats_url).await.unwrap();
    let mut outcome_sub = nats_client
        .subscribe("chuggernaut.worker.outcome")
        .await
        .unwrap();

    // Also subscribe to heartbeats to see if the worker starts
    let mut hb_sub = nats_client
        .subscribe("chuggernaut.worker.heartbeat")
        .await
        .unwrap();

    // Subscribe to channel outbox to verify MCP channel_send works
    let job_key = "testorg.repo.1";
    let mut channel_sub = nats_client
        .subscribe(format!("chuggernaut.channel.{job_key}.outbox"))
        .await
        .unwrap();

    // Dispatch the workflow
    let forgejo_client = chuggernaut_forgejo_api::ForgejoClient::new(&forgejo_url, &token);
    let dispatch_result = forgejo_client
        .dispatch_workflow(
            org,
            "repo",
            "work.yml",
            &chuggernaut_forgejo_api::DispatchWorkflowOption {
                ref_field: "main".to_string(),
                inputs: Some(serde_json::json!({
                    "job_key": job_key,
                    "nats_url": nats_internal_url,
                })),
            },
        )
        .await
        .unwrap();
    let run_id = dispatch_result.id.unwrap_or(0);
    eprintln!("Workflow dispatched: run_id={run_id}");

    // Poll action status + check for NATS messages
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let mut got_heartbeat = false;
    let mut got_outcome = false;
    let mut got_channel_msg = false;
    let mut action_status = String::new();

    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }

        // Check action run status by ID
        if run_id > 0 {
            match forgejo_client.get_action_run(org, "repo", run_id).await {
                Ok(run) => {
                    if run.status != action_status {
                        eprintln!("Action status: {} (run_id={})", run.status, run_id);
                        action_status = run.status.clone();
                    }
                    if run.status == "success" || run.status == "failure" {
                        eprintln!("Action finished: {}", run.status);
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Failed to get action run: {e}");
                }
            }
        }

        // Check for heartbeats
        if let Ok(msg) =
            tokio::time::timeout(Duration::from_millis(500), hb_sub.next()).await
        {
            if msg.is_some() && !got_heartbeat {
                eprintln!("Got heartbeat from worker!");
                got_heartbeat = true;
            }
        }

        // Check for outcome
        if let Ok(msg) =
            tokio::time::timeout(Duration::from_millis(100), outcome_sub.next()).await
        {
            if let Some(msg) = msg {
                let outcome: WorkerOutcome =
                    serde_json::from_slice(&msg.payload).unwrap();
                eprintln!("Got outcome: {:?}", outcome.outcome);
                got_outcome = true;
                break;
            }
        }

        // Check for channel messages
        if let Ok(msg) =
            tokio::time::timeout(Duration::from_millis(100), channel_sub.next()).await
        {
            if let Some(msg) = msg {
                if let Ok(channel_msg) = serde_json::from_slice::<chuggernaut_types::ChannelMessage>(&msg.payload) {
                    eprintln!("Got channel message: sender={} body={}", channel_msg.sender, channel_msg.body);
                    got_channel_msg = true;
                }
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    eprintln!("Results: heartbeat={got_heartbeat} outcome={got_outcome} channel={got_channel_msg} action_status={action_status}");

    assert!(got_heartbeat, "should have received heartbeat from worker");
    assert!(got_outcome, "should have received WorkerOutcome from worker");
    // Channel message is a stretch goal — requires MCP bridge to work end-to-end
    if !got_channel_msg {
        eprintln!("NOTE: no channel message received (MCP bridge not yet exercised in action)");
    }

    // Check status KV
    let js = async_nats::jetstream::new(nats_client.clone());
    if let Ok(kv) = js.get_key_value("chuggernaut_channels").await {
        if let Ok(Some(entry)) = kv.entry(job_key).await {
            let status: chuggernaut_types::ChannelStatus = serde_json::from_slice(&entry.value).unwrap();
            eprintln!("Channel status: {} progress={:?}", status.status, status.progress);
            assert_eq!(status.status, "done");
        } else {
            eprintln!("No status KV entry found (channel KV bucket may not have been created by worker)");
        }
    }
}

// ---------------------------------------------------------------------------
// Full E2E: dispatcher → Forgejo Action → runner → worker → outcome
// ---------------------------------------------------------------------------

/// Full end-to-end test with a real Forgejo Actions runner.
/// Requires: Docker socket available, `chuggernaut-runner-env` image built.
///
/// Run: `cargo test -p chuggernaut-dispatcher --test integration e2e_full -- --test-threads=1 --ignored`
#[tokio::test]
#[ignore] // requires pre-built runner-env image + Docker socket
async fn e2e_full_action_pipeline() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    // Start Forgejo
    let forgejo = GenericImage::new("codeberg.org/forgejo/forgejo", "14")
        .with_exposed_port(3000.tcp())
        .with_wait_for(WaitFor::seconds(5))
        .with_env_var("FORGEJO__database__DB_TYPE", "sqlite3")
        .with_env_var("FORGEJO__security__INSTALL_LOCK", "true")
        .with_env_var("FORGEJO__service__DISABLE_REGISTRATION", "true")
        .with_env_var("FORGEJO__actions__ENABLED", "true")
        .with_env_var("FORGEJO__actions__DEFAULT_ACTIONS_URL", "https://code.forgejo.org")
        .with_env_var("FORGEJO__log__LEVEL", "Warn")
        .with_startup_timeout(Duration::from_secs(120))
        .start()
        .await
        .unwrap();

    let forgejo_port = forgejo.get_host_port_ipv4(3000.tcp()).await.unwrap();
    let forgejo_url = format!("http://127.0.0.1:{forgejo_port}");
    // Internal URL the runner uses (container-to-container via host network)
    let forgejo_internal_url = format!("http://host.docker.internal:{forgejo_port}");

    // Wait for API
    let http = reqwest::Client::new();
    for _ in 0..60 {
        if http
            .get(format!("{forgejo_url}/api/v1/version"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create admin + token
    forgejo
        .exec(
            ExecCommand::new([
                "su-exec", "git", "forgejo", "admin", "user", "create",
                "--admin", "--username", "testadmin", "--password", "testadmin",
                "--email", "testadmin@test.local", "--must-change-password=false",
            ])
            .with_cmd_ready_condition(CmdWaitFor::exit_code(0)),
        )
        .await
        .expect("admin user creation failed");

    let token_resp = http
        .post(format!("{forgejo_url}/api/v1/users/testadmin/tokens"))
        .basic_auth("testadmin", Some("testadmin"))
        .json(&serde_json::json!({"name": "e2e", "scopes": ["all"]}))
        .send()
        .await
        .unwrap();
    let status = token_resp.status();
    let body = token_resp.text().await.unwrap();
    eprintln!("E2E token create: status={status} body={body}");
    let token_json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let token = token_json["sha1"].as_str().unwrap().to_string();

    // Get runner registration token
    let reg_token_resp: serde_json::Value = http
        .get(format!("{forgejo_url}/api/v1/admin/runners/registration-token"))
        .header("Authorization", format!("token {token}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let runner_reg_token = reg_token_resp["token"].as_str().unwrap().to_string();

    // Create org + repo
    let org = format!("org{}", &prefix[..8]);
    http.post(format!("{forgejo_url}/api/v1/orgs"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"username": &org, "visibility": "public"}))
        .send()
        .await
        .unwrap();

    http.post(format!("{forgejo_url}/api/v1/orgs/{org}/repos"))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"name": "repo", "default_branch": "main", "auto_init": true}))
        .send()
        .await
        .unwrap();

    // Push workflow file
    let workflow = format!(
        r#"name: chuggernaut-work
on:
  workflow_dispatch:
    inputs:
      job_key:
        required: true
        type: string
      nats_url:
        required: true
        type: string
jobs:
  work:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Execute
        run: |
          echo "mock work for ${{{{ inputs.job_key }}}}" > work.txt
          git add -A
          git commit -m "work: ${{{{ inputs.job_key }}}}"
          git push origin HEAD
"#
    );
    http.post(format!(
            "{forgejo_url}/api/v1/repos/{org}/repo/contents/.forgejo/workflows/work.yml"
        ))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({
            "message": "add workflow",
            "content": base64_encode(&workflow)
        }))
        .send()
        .await
        .unwrap();

    // Start runner with Docker socket passthrough
    // Get absolute path to runner config
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let runner_config_path = workspace_root.join("infra/runner/config.yaml")
        .canonicalize()
        .expect("infra/runner/config.yaml not found");

    let e2e_labels = "ubuntu-latest:docker://chuggernaut-runner-env:latest";
    let e2e_register_and_run = format!(
        "forgejo-runner register --no-interactive --instance \"$INSTANCE\" --token \"$TOKEN\" --name e2e-runner --labels '{e2e_labels}' -c /data/config.yaml && forgejo-runner daemon -c /data/config.yaml -c /config.yaml"
    );

    let e2e_labels = "ubuntu-latest:docker://chuggernaut-runner-env:latest";
    let e2e_register_and_run = format!(
        "cd /data && forgejo-runner register --no-interactive --instance '{forgejo_internal_url}' --token '{runner_reg_token}' --name e2e-runner --labels '{e2e_labels}' && forgejo-runner daemon"
    );

    let _runner = GenericImage::new("data.forgejo.org/forgejo/runner", "11")
        .with_user("root")
        .with_mount(Mount::bind_mount(
            "/var/run/docker.sock",
            "/var/run/docker.sock",
        ))
        .with_mount(Mount::bind_mount(
            runner_config_path.to_str().unwrap(),
            "/data/config.yaml",
        ))
        .with_cmd(["sh", "-c", &e2e_register_and_run])
        .with_startup_timeout(Duration::from_secs(60))
        .start()
        .await
        .unwrap();

    // Give runner time to register with Forgejo
    tokio::time::sleep(Duration::from_secs(15)).await;

    // Check if runner registered
    let runners_resp = http
        .get(format!("{forgejo_url}/api/v1/admin/runners"))
        .header("Authorization", format!("token {token}"))
        .send()
        .await
        .unwrap();
    let runners_body = runners_resp.text().await.unwrap();
    eprintln!("E2E: registered runners = {runners_body}");

    // NATS URL the worker binary will use from inside a Docker container
    let nats_internal_url = format!("nats://host.docker.internal:{port}");

    // Push workflow that runs chuggernaut-worker with mock-claude
    let workflow = format!(
        r#"name: chuggernaut-work
on:
  workflow_dispatch:
    inputs:
      job_key:
        required: true
        type: string
      nats_url:
        required: true
        type: string
jobs:
  work:
    runs-on: ubuntu-latest
    steps:
      - name: Execute
        run: |
          chuggernaut-worker \
            --job-key "${{{{ inputs.job_key }}}}" \
            --nats-url "${{{{ inputs.nats_url }}}}" \
            --forgejo-url "{forgejo_internal_url}" \
            --forgejo-token "${{{{ secrets.CHUGGERNAUT_FORGEJO_TOKEN }}}}" \
            --command mock-claude \
            --heartbeat-interval-secs 5
"#
    );
    http.post(format!(
            "{forgejo_url}/api/v1/repos/{org}/repo/contents/.forgejo/workflows/work.yml"
        ))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({
            "message": "add workflow",
            "content": base64_encode(&workflow)
        }))
        .send()
        .await
        .unwrap();

    // Set repo secret for Forgejo token
    http.put(format!(
            "{forgejo_url}/api/v1/repos/{org}/repo/actions/secrets/CHUGGERNAUT_FORGEJO_TOKEN"
        ))
        .header("Authorization", format!("token {token}"))
        .json(&serde_json::json!({"data": &token}))
        .send()
        .await
        .unwrap();

    // Set up dispatcher — NO prefix, so the worker binary's unprefixed NATS
    // messages reach the dispatcher directly
    let config = Config {
        nats_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 120,
        default_timeout_secs: 300,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 5,
        job_retention_secs: 86400,
        activity_limit: 50,
        blacklist_ttl_secs: 3600,
        forgejo_url: Some(forgejo_url.clone()),
        forgejo_token: Some(token.clone()),
        action_workflow: "work.yml".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize(&js, config.lease_secs, config.blacklist_ttl_secs)
        .await
        .unwrap();
    let state = DispatcherState::new(config, client, js, kv);
    handlers::start_handlers(state.clone()).await.unwrap();

    // The action dispatch uses the internal NATS URL (reachable from Docker containers)
    // We need to override the nats_url in the dispatch inputs. Let's patch the
    // action_dispatch to use the internal URL by temporarily updating the config.
    // Actually — action_dispatch reads nats_url from state.config. Let's just
    // dispatch manually with the correct URL.

    // Create action-type job
    let req = CreateJobRequest {
        repo: format!("{org}/repo"),
        title: "E2E test".to_string(),
        body: "Full pipeline test".to_string(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: Some("action".to_string()),
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    eprintln!("E2E: created job {key}");

    // Dispatch action manually so we can pass the internal NATS URL
    let job = state.jobs.get(&key).unwrap().clone();
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, job.timeout_secs)
        .await
        .unwrap();
    chuggernaut_dispatcher::jobs::transition_job(
        &state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id),
    )
    .await
    .unwrap();

    let forgejo_client = chuggernaut_forgejo_api::ForgejoClient::new(&forgejo_url, &token);
    forgejo_client
        .dispatch_workflow(
            &org,
            "repo",
            "work.yml",
            &chuggernaut_forgejo_api::DispatchWorkflowOption {
                ref_field: "main".to_string(),
                inputs: Some(serde_json::json!({
                    "job_key": key,
                    "nats_url": nats_internal_url,
                })),
            },
        )
        .await
        .expect("workflow dispatch failed");

    eprintln!("E2E: action dispatched, waiting for worker outcome...");

    // Check action runs after a short delay
    tokio::time::sleep(Duration::from_secs(5)).await;
    let runs_resp = http
        .get(format!("{forgejo_url}/api/v1/repos/{org}/repo/actions/runs"))
        .header("Authorization", format!("token {token}"))
        .send()
        .await
        .unwrap();
    let runs_body = runs_resp.text().await.unwrap();
    eprintln!("E2E: action runs = {runs_body}");

    // Wait for the worker binary (inside the action) to report an outcome
    // The worker publishes WorkerOutcome to chuggernaut.worker.outcome
    // The dispatcher's handler picks it up and transitions the job
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::time::Instant::now() > deadline {
            let job_state = state.jobs.get(&key).unwrap().state;
            // Check action run status before panicking
            let runs_resp = http
                .get(format!("{forgejo_url}/api/v1/repos/{org}/repo/actions/runs"))
                .header("Authorization", format!("token {token}"))
                .send().await.unwrap();
            let runs = runs_resp.text().await.unwrap();
            panic!("E2E timeout: job in {job_state:?} after 60s. Action runs: {runs}");
        }

        let job_state = state.jobs.get(&key).unwrap().state;
        match job_state {
            JobState::InReview => {
                eprintln!("E2E: job reached InReview — worker completed successfully!");
                break;
            }
            JobState::Failed => {
                eprintln!("E2E: job failed");
                break;
            }
            JobState::Done => {
                eprintln!("E2E: job done!");
                break;
            }
            _ => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }

    let final_state = state.jobs.get(&key).unwrap().state;
    eprintln!("E2E: final job state = {final_state:?}");

    // The worker should have yielded with a PR URL, transitioning to InReview
    assert!(
        final_state == JobState::InReview || final_state == JobState::Done,
        "expected InReview or Done, got {final_state:?}"
    );

    // If InReview, check PR was created
    if final_state == JobState::InReview {
        let pr_url = state.jobs.get(&key).unwrap().pr_url.clone();
        assert!(pr_url.is_some(), "job should have a PR URL");
        eprintln!("E2E: PR URL = {}", pr_url.unwrap());
    }
}

fn base64_encode(s: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = s.as_bytes();
    let mut result = String::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | bytes[i + 2] as u32;
        result.push(CHARS[(n >> 18 & 63) as usize] as char);
        result.push(CHARS[(n >> 12 & 63) as usize] as char);
        result.push(CHARS[(n >> 6 & 63) as usize] as char);
        result.push(CHARS[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        result.push(CHARS[(n >> 18 & 63) as usize] as char);
        result.push(CHARS[(n >> 12 & 63) as usize] as char);
        result.push(CHARS[(n >> 6 & 63) as usize] as char);
        result.push('=');
    } else if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        result.push(CHARS[(n >> 18 & 63) as usize] as char);
        result.push(CHARS[(n >> 12 & 63) as usize] as char);
        result.push('=');
        result.push('=');
    }
    result
}

use std::sync::Arc;
use std::time::Duration;

use chuggernaut_test_utils as test_utils;
use futures::StreamExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ImageExt;
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
                test_utils::register_container_cleanup(container.id());
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
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 60,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());

    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
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
// Worker outcome processing
// ---------------------------------------------------------------------------

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
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate job being on-the-stack with a claim (as action_dispatch would do)
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    // Worker yields
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: None,
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
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack with claim
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail { reason: "compile error".to_string(), logs: None },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    let job = state.jobs.get(&key).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.retry_count, 1);
    assert!(job.retry_after.is_some());
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
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack with claim
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    let (before, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let hb = WorkerHeartbeat { worker_id: worker_id.clone(), job_key: key.clone() };
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

    // Simulate on-the-stack + yield → InReview
    let worker_id = format!("action-{key_a}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key_a, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key_a, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key_a.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key_a).unwrap().state, JobState::InReview);

    // Publish ReviewDecision Approved
    let decision = ReviewDecision {
        job_key: key_a.clone(),
        decision: DecisionType::Approved,
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: None,
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
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack + yield → InReview
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

    // Publish ChangesRequested
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested { feedback: "fix the tests".to_string() },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::ChangesRequested);
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
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 1,
        default_timeout_secs: 3600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 1,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
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
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack with claim
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();
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
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack with claim + fail
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail { reason: "compile error".to_string(), logs: None },
        token_usage: None,
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

    // Start Forgejo via test-utils
    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);

    // Create admin user + token
    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    // Create org + repo + workflow
    let org = format!("org{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    // Push a noop workflow so dispatch_workflow succeeds
    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", workflow, "add workflow",
    ).await;

    let review_workflow = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/review.yml", review_workflow, "add review workflow",
    ).await;

    // Set up dispatcher with Forgejo config
    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 60,
        default_timeout_secs: 600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
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

/// Rework dispatch: ChangesRequested → new action dispatched with review_feedback
#[tokio::test]
async fn rework_dispatches_new_action_with_feedback() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    // Start Forgejo via test-utils
    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("rw{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    // Push workflow with review_feedback + is_rework inputs
    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", workflow, "add workflow",
    ).await;

    let review_workflow = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/review.yml", review_workflow, "add review workflow",
    ).await;

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 60,
        default_timeout_secs: 600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);
    handlers::start_handlers(state.clone()).await.unwrap();

    // Create job and simulate initial work cycle: OnDeck → OnTheStack → InReview
    let req = CreateJobRequest {
        repo: format!("{org}/repo"),
        title: "Rework dispatch test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::High,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Dispatch initial action → OnTheStack
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    // Simulate yield → InReview
    let worker_id = format!("action-{key}");
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: format!("http://forgejo/{org}/repo/pulls/1") },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

    // Publish ChangesRequested with feedback
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested { feedback: "fix the tests please".to_string() },
        pr_url: Some(format!("http://forgejo/{org}/repo/pulls/1")),
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The handler transitions to ChangesRequested, then dispatches a rework action.
    // The rework action dispatch creates a new claim and transitions to OnTheStack.
    let job_state = state.jobs.get(&key).unwrap().state;
    assert!(
        job_state == JobState::OnTheStack,
        "expected OnTheStack after rework dispatch, got {job_state:?}"
    );

    // Verify claim exists for the rework
    let (claim, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await.unwrap().unwrap();
    assert!(claim.worker_id.starts_with("action-"), "rework claim should have action- prefix");
}

// ---------------------------------------------------------------------------
// Review dispatch: yield triggers review action
// ---------------------------------------------------------------------------

/// Verify that when a worker yields, the dispatcher dispatches a review action
/// to Forgejo and records a "review_dispatched" journal entry.
#[tokio::test]
async fn yield_dispatches_review_action() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("rv{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    // Push both work and review workflows
    let work_wf = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", work_wf, "add work workflow",
    ).await;

    let review_wf = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/review.yml", review_wf, "add review workflow",
    ).await;

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 60,
        default_timeout_secs: 600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);
    handlers::start_handlers(state.clone()).await.unwrap();

    // Create job, dispatch work action, simulate yield
    let req = CreateJobRequest {
        repo: format!("{org}/repo"),
        title: "Review dispatch test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::High,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    // Worker yields with PR URL
    let worker_id = format!("action-{key}");
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: format!("http://forgejo/{org}/repo/pulls/1") },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Job should be InReview
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

    // Verify review dispatch happened by checking journal
    let mut found_review_dispatch = false;
    if let Ok(keys) = state.kv.journal.keys().await {
        tokio::pin!(keys);
        while let Some(Ok(jk)) = keys.next().await {
            if let Ok(Some((entry, _))) = jobs::kv_get::<JournalEntry>(&state.kv.journal, &jk).await {
                if entry.action == "review_dispatched" && entry.job_key.as_deref() == Some(&key) {
                    found_review_dispatch = true;
                    // Verify details contain the workflow and pr_url
                    let details = entry.details.unwrap_or_default();
                    assert!(details.contains("review.yml"), "journal should mention review workflow");
                    assert!(details.contains("pr_url"), "journal should mention pr_url");
                    break;
                }
            }
        }
    }
    assert!(found_review_dispatch, "expected review_dispatched journal entry");
}

// ---------------------------------------------------------------------------
// Token usage: work + review tokens accumulated on Job
// ---------------------------------------------------------------------------

#[tokio::test]
async fn token_usage_accumulated_from_work_and_review() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Token tracking test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate work: claim → OnTheStack → yield with token usage
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    let work_tokens = TokenUsage {
        input_tokens: 25000,
        output_tokens: 8000,
        cache_read_tokens: 12000,
        cache_write_tokens: 3000,
    };
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: Some(work_tokens.clone()),
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify work token record was stored
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::InReview);
    assert_eq!(job.token_usage.len(), 1);
    assert_eq!(job.token_usage[0].action_type, "work");
    assert_eq!(job.token_usage[0].token_usage.input_tokens, 25000);
    assert_eq!(job.token_usage[0].token_usage.output_tokens, 8000);

    // Now simulate review decision with token usage
    let review_tokens = TokenUsage {
        input_tokens: 15000,
        output_tokens: 3500,
        cache_read_tokens: 8000,
        cache_write_tokens: 2000,
    };
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::Approved,
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: Some(review_tokens.clone()),
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify both records exist on the job
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::Done);
    assert_eq!(job.token_usage.len(), 2, "expected 2 token records (work + review)");

    assert_eq!(job.token_usage[0].action_type, "work");
    assert_eq!(job.token_usage[0].token_usage.input_tokens, 25000);

    assert_eq!(job.token_usage[1].action_type, "review");
    assert_eq!(job.token_usage[1].token_usage.input_tokens, 15000);
    assert_eq!(job.token_usage[1].token_usage.output_tokens, 3500);

    // Also verify via KV (persistence)
    let (kv_job, _) = jobs::kv_get::<Job>(&state.kv.jobs, &key).await.unwrap().unwrap();
    assert_eq!(kv_job.token_usage.len(), 2);
}

/// Verify that a rework cycle accumulates multiple token records.
#[tokio::test]
async fn token_usage_across_rework_cycle() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Rework token test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // First work cycle
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "action_dispatched", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: Some(TokenUsage { input_tokens: 20000, output_tokens: 5000, cache_read_tokens: 0, cache_write_tokens: 0 }),
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // First review: changes requested
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested { feedback: "fix tests".to_string() },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: Some(TokenUsage { input_tokens: 10000, output_tokens: 2000, cache_read_tokens: 0, cache_write_tokens: 0 }),
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Job should be in ChangesRequested (no Forgejo to dispatch rework, so it stays there)
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::ChangesRequested);
    assert_eq!(job.token_usage.len(), 2, "expected work + review records after first cycle");
    assert_eq!(job.token_usage[0].action_type, "work");
    assert_eq!(job.token_usage[1].action_type, "review");

    // Simulate rework: manually claim + transition + yield again
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "rework_dispatched", Some(&worker_id)).await.unwrap();

    let outcome2 = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: Some(TokenUsage { input_tokens: 8000, output_tokens: 3000, cache_read_tokens: 0, cache_write_tokens: 0 }),
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome2).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Second review: approved
    let decision2 = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::Approved,
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: Some(TokenUsage { input_tokens: 9000, output_tokens: 1500, cache_read_tokens: 0, cache_write_tokens: 0 }),
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision2).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::Done);
    assert_eq!(job.token_usage.len(), 4, "expected 4 records: work, review, work(rework), review");

    assert_eq!(job.token_usage[0].action_type, "work");
    assert_eq!(job.token_usage[0].token_usage.input_tokens, 20000);
    assert_eq!(job.token_usage[1].action_type, "review");
    assert_eq!(job.token_usage[1].token_usage.input_tokens, 10000);
    assert_eq!(job.token_usage[2].action_type, "work");
    assert_eq!(job.token_usage[2].token_usage.input_tokens, 8000);
    assert_eq!(job.token_usage[3].action_type, "review");
    assert_eq!(job.token_usage[3].token_usage.input_tokens, 9000);
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

    // Start Forgejo via test-utils
    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);
    let forgejo_internal_url = test_utils::forgejo_internal_url(forgejo_port);

    // Create admin + token
    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    // Get runner registration token
    let runner_reg_token = test_utils::get_runner_registration_token(forgejo_port, &token).await;

    // Create org + repo
    let org = "testorg";
    test_utils::create_test_repo(forgejo_port, &token, org, "repo").await;

    // Push workflow with all inputs (including rework fields)
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
      review_feedback:
        type: string
        default: ""
      is_rework:
        type: string
        default: "false"
jobs:
  work:
    runs-on: ubuntu-latest
    steps:
      - name: Verify networking
        run: |
          set -e
          nats_host=$(echo "${{{{ inputs.nats_url }}}}" | sed 's|nats://||' | cut -d: -f1)
          nats_port=$(echo "${{{{ inputs.nats_url }}}}" | sed 's|nats://||' | cut -d: -f2)
          echo "Checking NATS at $nats_host:$nats_port ..."
          timeout 5 bash -c "echo > /dev/tcp/$nats_host/$nats_port" || {{ echo "FATAL: cannot reach NATS"; exit 1; }}
          echo "Checking Forgejo at {forgejo_internal_url} ..."
          curl -sf --max-time 5 "{forgejo_internal_url}/api/v1/version" || {{ echo "FATAL: cannot reach Forgejo"; exit 1; }}
          echo "Networking OK"
      - name: Run worker
        run: |
          chuggernaut-worker \
            --job-key "${{{{ inputs.job_key }}}}" \
            --nats-url "${{{{ inputs.nats_url }}}}" \
            --forgejo-url "{forgejo_internal_url}" \
            --forgejo-token "${{{{ secrets.CHUGGERNAUT_FORGEJO_TOKEN }}}}" \
            --command mock-claude \
            --heartbeat-interval-secs 5
        env:
          CHUGGERNAUT_REVIEW_FEEDBACK: ${{{{ inputs.review_feedback }}}}
          CHUGGERNAUT_IS_REWORK: ${{{{ inputs.is_rework }}}}
"#
    );

    test_utils::push_file(
        forgejo_port, &token, org, "repo",
        ".forgejo/workflows/work.yml", &workflow, "add workflow",
    ).await;

    // Set secret
    test_utils::set_repo_secret(
        forgejo_port, &token, org, "repo",
        "CHUGGERNAUT_FORGEJO_TOKEN", &token,
    ).await;

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

    let _runner = test_utils::start_runner(
        forgejo_port,
        &runner_reg_token,
        runner_config_path.to_str().unwrap(),
    ).await;

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

        if got_outcome {
            break;
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Drain any remaining channel messages that arrived around the same time as the outcome
    if !got_channel_msg {
        if let Ok(Some(msg)) =
            tokio::time::timeout(Duration::from_secs(2), channel_sub.next()).await
        {
            if let Ok(channel_msg) = serde_json::from_slice::<chuggernaut_types::ChannelMessage>(&msg.payload) {
                eprintln!("Got channel message (post-loop): sender={} body={}", channel_msg.sender, channel_msg.body);
                got_channel_msg = true;
            }
        }
    }

    eprintln!("Results: heartbeat={got_heartbeat} outcome={got_outcome} channel={got_channel_msg} action_status={action_status}");

    assert!(got_heartbeat, "should have received heartbeat from worker");
    assert!(got_outcome, "should have received WorkerOutcome from worker");
    assert!(got_channel_msg, "should have received channel_send message on NATS outbox");

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

    // Start Forgejo via test-utils
    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);
    let forgejo_internal_url = test_utils::forgejo_internal_url(forgejo_port);

    // Create admin + token
    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    // Get runner registration token
    let runner_reg_token = test_utils::get_runner_registration_token(forgejo_port, &token).await;

    // Create org + repo
    let org = format!("org{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

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
      review_feedback:
        type: string
        default: ""
      is_rework:
        type: string
        default: "false"
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
        env:
          CHUGGERNAUT_REVIEW_FEEDBACK: ${{{{ inputs.review_feedback }}}}
          CHUGGERNAUT_IS_REWORK: ${{{{ inputs.is_rework }}}}
"#
    );
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", &workflow, "add workflow",
    ).await;

    // Set repo secret for Forgejo token
    test_utils::set_repo_secret(
        forgejo_port, &token, &org, "repo",
        "CHUGGERNAUT_FORGEJO_TOKEN", &token,
    ).await;

    // Start runner via test-utils
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let runner_config_path = workspace_root.join("infra/runner/config.yaml")
        .canonicalize()
        .expect("infra/runner/config.yaml not found");

    let _runner = test_utils::start_runner(
        forgejo_port,
        &runner_reg_token,
        runner_config_path.to_str().unwrap(),
    ).await;

    // Give runner time to register with Forgejo
    tokio::time::sleep(Duration::from_secs(15)).await;

    // Check if runner registered
    let http = reqwest::Client::new();
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

    // Set up dispatcher — NO prefix, so the worker binary's unprefixed NATS
    // messages reach the dispatcher directly
    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_internal_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 120,
        default_timeout_secs: 300,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 5,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url.clone()),
        forgejo_token: Some(token.clone()),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize(&js, config.lease_secs)
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
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    eprintln!("E2E: created job {key}");

    // Dispatcher auto-dispatches via try_assign_job (nats_worker_url = Docker-internal)
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    eprintln!("E2E: action dispatched via dispatcher, waiting for worker outcome...");

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

// ---------------------------------------------------------------------------
// Full E2E: work → review (changes_requested) → rework → review (approved) → Done
// ---------------------------------------------------------------------------

/// Complete lifecycle E2E: creates a job, runs a real work action via Forgejo runner,
/// injects review decisions to drive the rework cycle, runs a second work action,
/// and verifies the job reaches Done with token usage records.
///
/// Run: `cargo test -p chuggernaut-dispatcher --test integration e2e_full_review_cycle -- --test-threads=1 --ignored --nocapture`
#[tokio::test]
#[ignore] // requires pre-built runner-env image + Docker socket
async fn e2e_full_review_cycle() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let nats_internal_url = format!("nats://host.docker.internal:{port}");

    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);
    let forgejo_internal_url = test_utils::forgejo_internal_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let runner_reg_token = test_utils::get_runner_registration_token(forgejo_port, &token).await;

    let org = format!("e2e{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    // Push work workflow (runs mock-claude for git work)
    let work_wf = format!(
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
      review_feedback:
        type: string
        default: ""
      is_rework:
        type: string
        default: "false"
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
        env:
          CHUGGERNAUT_REVIEW_FEEDBACK: ${{{{ inputs.review_feedback }}}}
          CHUGGERNAUT_IS_REWORK: ${{{{ inputs.is_rework }}}}
"#
    );
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", &work_wf, "add work workflow",
    ).await;

    test_utils::set_repo_secret(
        forgejo_port, &token, &org, "repo",
        "CHUGGERNAUT_FORGEJO_TOKEN", &token,
    ).await;

    // Start runner
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let runner_config_path = workspace_root.join("infra/runner/config.yaml").canonicalize().unwrap();
    let _runner = test_utils::start_runner(forgejo_port, &runner_reg_token, runner_config_path.to_str().unwrap()).await;
    tokio::time::sleep(Duration::from_secs(15)).await;

    // Set up dispatcher (unprefixed so worker NATS messages reach it)
    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_internal_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 120,
        default_timeout_secs: 300,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 5,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url.clone()),
        forgejo_token: Some(token.clone()),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize(&js, config.lease_secs).await.unwrap();
    let state = DispatcherState::new(config, client, js, kv);
    handlers::start_handlers(state.clone()).await.unwrap();

    // --- Phase 1: Create job and dispatch first work action ---
    let req = CreateJobRequest {
        repo: format!("{org}/repo"),
        title: "E2E review cycle".to_string(),
        body: "Full lifecycle test".to_string(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::High,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    eprintln!("E2E-review: created job {key}");

    // Dispatcher auto-dispatches via try_assign_job (nats_worker_url = Docker-internal)
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);
    eprintln!("E2E-review: first work action dispatched via dispatcher");

    // Wait for InReview
    wait_for_state(&state, &key, JobState::InReview, 90).await;
    let pr_url = state.jobs.get(&key).unwrap().pr_url.clone().expect("should have PR URL");
    eprintln!("E2E-review: reached InReview, PR = {pr_url}");

    // --- Phase 2: Inject ChangesRequested review decision ---
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested { feedback: "please add tests".to_string() },
        pr_url: Some(pr_url.clone()),
        token_usage: Some(TokenUsage { input_tokens: 15000, output_tokens: 3000, cache_read_tokens: 0, cache_write_tokens: 0 }),
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    eprintln!("E2E-review: injected ChangesRequested");

    // Wait for job to leave InReview (handler transitions to ChangesRequested → OnTheStack)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let current = state.jobs.get(&key).unwrap().state;
        if current != JobState::InReview {
            eprintln!("E2E-review: left InReview, now in {current:?}");
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("E2E-review: timed out waiting to leave InReview");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Wait for InReview again (rework action yields)
    wait_for_state(&state, &key, JobState::InReview, 90).await;
    eprintln!("E2E-review: reached InReview after rework");

    // --- Phase 3: Inject Approved review decision ---
    let decision2 = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::Approved,
        pr_url: Some(pr_url.clone()),
        token_usage: Some(TokenUsage { input_tokens: 12000, output_tokens: 2000, cache_read_tokens: 0, cache_write_tokens: 0 }),
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision2).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let final_state = state.jobs.get(&key).unwrap().state;
    eprintln!("E2E-review: final state = {final_state:?}");
    assert_eq!(final_state, JobState::Done, "job should be Done after approval");

    // --- Verify token usage records ---
    let job = state.jobs.get(&key).unwrap().clone();
    eprintln!("E2E-review: token_usage records = {}", job.token_usage.len());
    for (i, record) in job.token_usage.iter().enumerate() {
        eprintln!("  [{i}] type={} input={} output={}", record.action_type, record.token_usage.input_tokens, record.token_usage.output_tokens);
    }

    // We expect at least the 2 review records we injected (work outcomes from the real
    // action don't have token_usage since mock-claude doesn't report it)
    let review_records: Vec<_> = job.token_usage.iter().filter(|r| r.action_type == "review").collect();
    assert_eq!(review_records.len(), 2, "expected 2 review token records");
    assert_eq!(review_records[0].token_usage.input_tokens, 15000);
    assert_eq!(review_records[1].token_usage.input_tokens, 12000);

    eprintln!("E2E-review: PASSED — full work → review → rework → review → Done cycle");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Like `setup()` but accepts overrides for Config fields.
async fn setup_with_config(overrides: impl FnOnce(&mut Config)) -> Arc<DispatcherState> {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let mut config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 60,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };
    overrides(&mut config);

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
    DispatcherState::new_namespaced(config, client, js, kv, prefix)
}

/// Start the HTTP server and return the base URL.
async fn start_http(state: Arc<DispatcherState>) -> String {
    let app = chuggernaut_dispatcher::http::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Helper: wait for a job to reach the given state, with timeout.
async fn wait_for_state(state: &Arc<DispatcherState>, key: &str, target: JobState, timeout_secs: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if tokio::time::Instant::now() > deadline {
            let current = state.jobs.get(key).unwrap().state;
            panic!("timeout waiting for {target:?}: job is in {current:?} after {timeout_secs}s");
        }
        let current = state.jobs.get(key).unwrap().state;
        if current == target {
            return;
        }
        if current == JobState::Failed {
            panic!("job failed while waiting for {target:?}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

// ===========================================================================
// NEW COVERAGE TESTS
// ===========================================================================

// ---------------------------------------------------------------------------
// Group 1: State machine edges
// ---------------------------------------------------------------------------

#[tokio::test]
async fn escalated_review_transitions_job() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Escalation test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack → yield → InReview
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

    // Publish Escalated decision
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::Escalated { reviewer_login: "human".to_string() },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Escalated);
}

#[tokio::test]
async fn escalated_then_approved_via_close() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Escalated close".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Get to Escalated
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();
    jobs::transition_job(&state, &key, JobState::InReview, "test", None).await.unwrap();
    jobs::transition_job(&state, &key, JobState::Escalated, "test", None).await.unwrap();

    // Admin close → Done
    let close = CloseJobRequest { job_key: key.clone(), revoke: false };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_CLOSE_JOB, &close),
    ).await.unwrap().unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Done);
}

#[tokio::test]
async fn escalated_then_changes_requested() {
    let state = setup().await;

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Esc → CR".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Direct transitions to Escalated
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();
    jobs::transition_job(&state, &key, JobState::InReview, "test", None).await.unwrap();
    jobs::transition_job(&state, &key, JobState::Escalated, "test", None).await.unwrap();

    // Escalated → ChangesRequested
    let result = jobs::transition_job(&state, &key, JobState::ChangesRequested, "human_review", None).await;
    assert!(result.is_ok());
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::ChangesRequested);
}

#[tokio::test]
async fn requeue_from_failed_to_on_ice() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Failed → OnIce".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Get to Failed
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail { reason: "error".to_string(), logs: None },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);

    // Requeue to OnIce
    let requeue = RequeueRequest { job_key: key.clone(), target: RequeueTarget::OnIce };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_REQUEUE, &requeue),
    ).await.unwrap().unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnIce);
}

#[tokio::test]
async fn thaw_from_on_ice_via_requeue() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Thaw".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: Some(JobState::OnIce),
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnIce);

    let requeue = RequeueRequest { job_key: key.clone(), target: RequeueTarget::OnDeck };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_REQUEUE, &requeue),
    ).await.unwrap().unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);
}

#[tokio::test]
async fn invalid_transition_rejected() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Invalid".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Close job → Done
    let close = CloseJobRequest { job_key: key.clone(), revoke: false };
    state.nats.request_msg(&subjects::ADMIN_CLOSE_JOB, &close).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Done);

    // Try Done → OnTheStack (invalid)
    let result = jobs::transition_job(&state, &key, JobState::OnTheStack, "test", None).await;
    assert!(result.is_err(), "Done → OnTheStack should be rejected");
}

// ---------------------------------------------------------------------------
// Group 3: Error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn action_dispatch_failure_releases_claim() {
    // Use a bad Forgejo URL that will fail immediately
    let state = setup_with_config(|c| {
        c.forgejo_url = Some("http://127.0.0.1:1".to_string());
        c.forgejo_token = Some("fake-token".to_string());
    }).await;

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Dispatch fail".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // try_assign_job should fail internally but return Ok(false)
    let assigned = chuggernaut_dispatcher::assignment::try_assign_job(&state, &key).await.unwrap();
    assert!(!assigned, "assignment should fail with bad forgejo URL");

    // Job should be Failed (action_dispatch releases claim and transitions to Failed)
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);

    // Claim should be released (may be tombstoned — kv_get fails on tombstone, which is fine)
    let claim = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap_or(None);
    assert!(claim.is_none(), "claim should be released after dispatch failure");
}

#[tokio::test]
async fn dependency_cycle_rejected() {
    let state = setup().await;

    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };

    // Create A (no deps), B (deps on A)
    let _key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    // Try to add A → B dep (would create cycle: A→B and B→A)
    let result = chuggernaut_dispatcher::deps::create_deps(
        &state,
        &_key_a,
        &[key_b.clone()],
    ).await;
    assert!(result.is_err(), "adding cycle should fail");
}

#[tokio::test]
async fn heartbeat_from_wrong_worker_ignored() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Wrong worker HB".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    let (before, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send heartbeat from a different worker
    let hb = WorkerHeartbeat { worker_id: "wrong-worker".to_string(), job_key: key.clone() };
    state.nats.publish_msg(&subjects::WORKER_HEARTBEAT, &hb).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (after, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().unwrap();
    assert_eq!(before.lease_deadline, after.lease_deadline, "lease should not be renewed by wrong worker");
    assert_eq!(after.worker_id, worker_id, "claim owner should not change");
}

#[tokio::test]
async fn rework_limit_not_enforced_yet() {
    // Documents current behavior: rework_limit config exists but is not checked.
    // Multiple rework cycles succeed without limit.
    let state = setup_with_config(|c| c.rework_limit = 1).await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Rework limit".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Do 3 rework cycles (exceeding rework_limit=1)
    for i in 0..3 {
        let worker_id = format!("action-{key}");
        chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
        jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

        let outcome = WorkerOutcome {
            worker_id: worker_id.clone(),
            job_key: key.clone(),
            outcome: OutcomeType::Yield { pr_url: format!("http://forgejo/test/repo/pulls/{}", i + 1) },
            token_usage: None,
        };
        state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

        // Changes requested
        let decision = ReviewDecision {
            job_key: key.clone(),
            decision: DecisionType::ChangesRequested { feedback: format!("fix #{}", i + 1) },
            pr_url: Some(format!("http://forgejo/test/repo/pulls/{}", i + 1)),
            token_usage: None,
        };
        state.nats.publish_msg(&subjects::REVIEW_DECISION, &decision).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Should be ChangesRequested (rework_limit not enforced, no Forgejo to auto-dispatch)
        assert_eq!(
            state.jobs.get(&key).unwrap().state,
            JobState::ChangesRequested,
            "cycle {i}: rework_limit should not block transitions"
        );
    }
}

// ---------------------------------------------------------------------------
// Group 7: Token usage + concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nil_token_usage_not_appended() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Nil tokens".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: None, // no token usage
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::InReview);
    assert!(job.token_usage.is_empty(), "nil token_usage should not create a record");
}

#[tokio::test]
async fn concurrent_heartbeats_benign() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Concurrent HB".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    // Send 10 heartbeats concurrently
    let mut handles = Vec::new();
    for _ in 0..10 {
        let s = state.clone();
        let w = worker_id.clone();
        let k = key.clone();
        handles.push(tokio::spawn(async move {
            let hb = WorkerHeartbeat { worker_id: w, job_key: k };
            s.nats.publish_msg(&subjects::WORKER_HEARTBEAT, &hb).await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Claim should still be valid
    let (claim, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().unwrap();
    assert_eq!(claim.worker_id, worker_id);
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);
}

#[tokio::test]
async fn duplicate_outcome_handled() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Dup outcome".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield { pr_url: "http://forgejo/test/repo/pulls/1".to_string() },
        token_usage: None,
    };

    // Send same outcome twice
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();

    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Should be InReview (first succeeds, second is a no-op or graceful error)
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);
}

// ---------------------------------------------------------------------------
// Group 5: Recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_cleans_stale_claims() {
    let state = setup().await;

    // Create a job that stays OnDeck
    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Stale claim".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);

    // Write a stale claim directly to KV (job is NOT OnTheStack)
    let stale_claim = ClaimState {
        worker_id: "stale-worker".to_string(),
        claimed_at: chrono::Utc::now(),
        last_heartbeat: chrono::Utc::now(),
        lease_deadline: chrono::Utc::now() + chrono::Duration::seconds(3600),
        timeout_secs: 3600,
        lease_secs: 5,
    };
    jobs::kv_put(&state.kv.claims, &key, &stale_claim).await.unwrap();

    // Verify claim exists
    assert!(jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap().is_some());

    // Run recovery
    recovery::recover(&state).await.unwrap();

    // Stale claim should be deleted (tombstoned — kv_get may fail on tombstone)
    let claim_after = jobs::kv_get::<ClaimState>(&state.kv.claims, &key).await.unwrap_or(None);
    assert!(claim_after.is_none(), "stale claim should be cleaned up by recovery");
}

#[tokio::test]
async fn recovery_fails_claimless_on_the_stack() {
    let state = setup().await;

    // Create job and directly write OnTheStack state to KV (without creating a claim)
    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Claimless".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Directly update the job to OnTheStack in KV (bypassing claim requirement)
    let (mut job, rev) = jobs::kv_get::<Job>(&state.kv.jobs, &key).await.unwrap().unwrap();
    job.state = JobState::OnTheStack;
    jobs::kv_cas_update(&state.kv.jobs, &key, &job, rev, 3).await.unwrap();

    // Clear in-memory state to simulate restart
    state.jobs.clear();
    { let mut g = state.graph.write().await; *g = chuggernaut_dispatcher::state::DepGraph::new(); }

    // Recover — should detect OnTheStack job with no claim and fail it
    recovery::recover(&state).await.unwrap();

    // Job should be Failed (claimless on-the-stack)
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);
}

#[tokio::test]
async fn recovery_repairs_reverse_deps() {
    let state = setup().await;

    // Create two jobs with B depending on A
    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    // Corrupt: remove the reverse dep on A (A should have depended_on_by=[B])
    let (mut dep_a, rev) = jobs::kv_get::<DepRecord>(&state.kv.deps, &key_a).await.unwrap().unwrap();
    dep_a.depended_on_by.clear();
    jobs::kv_cas_update(&state.kv.deps, &key_a, &dep_a, rev, 3).await.unwrap();

    // Clear in-memory state
    state.jobs.clear();
    { let mut g = state.graph.write().await; *g = chuggernaut_dispatcher::state::DepGraph::new(); }

    // Recover
    recovery::recover(&state).await.unwrap();

    // Verify reverse dep was repaired
    let (dep_a_after, _) = jobs::kv_get::<DepRecord>(&state.kv.deps, &key_a).await.unwrap().unwrap();
    assert!(
        dep_a_after.depended_on_by.contains(&key_b),
        "recovery should repair reverse dep: A.depended_on_by should contain B"
    );
}

// ---------------------------------------------------------------------------
// Group 4: Monitor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn monitor_job_timeout() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 120, // long lease so it doesn't expire
        default_timeout_secs: 1, // very short timeout
        cas_max_retries: 3,
        monitor_scan_interval_secs: 1,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    handlers::start_handlers(state.clone()).await.unwrap();
    chuggernaut_dispatcher::monitor::start(state.clone());

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Timeout test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 1, // 1 second timeout
        review: ReviewLevel::High,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Acquire claim with 1s timeout
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 1).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    // Wait for monitor to detect timeout
    tokio::time::sleep(Duration::from_secs(4)).await;

    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);
}

#[tokio::test]
async fn monitor_orphan_detection() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 120,
        default_timeout_secs: 3600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 1,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    // Subscribe to orphan events before starting monitor
    let mut orphan_sub = state.nats.subscribe("chuggernaut.monitor.orphan").await.unwrap();

    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Orphan test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Set job to OnTheStack directly in memory (no claim created — this is the orphan condition)
    {
        let mut job = state.jobs.get(&key).unwrap().clone();
        job.state = JobState::OnTheStack;
        state.jobs.insert(key.clone(), job);
    }

    // Start monitor
    chuggernaut_dispatcher::monitor::start(state.clone());

    // Wait for orphan event
    let got_orphan = tokio::time::timeout(Duration::from_secs(5), orphan_sub.next()).await;
    assert!(got_orphan.is_ok(), "should receive orphan detection event");
}

#[tokio::test]
async fn monitor_retry_eligible_transitions_to_on_deck() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 3600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 1,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Retry eligible".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate failure via outcome
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail { reason: "error".to_string(), logs: None },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);

    // Manually set retry_after to the past so the monitor picks it up
    {
        let mut job = state.jobs.get(&key).unwrap().clone();
        job.retry_after = Some(chrono::Utc::now() - chrono::Duration::seconds(10));
        // Update both in-memory and KV
        state.jobs.insert(key.clone(), job.clone());
        jobs::kv_put(&state.kv.jobs, &key, &job).await.unwrap();
    }

    // Start monitor
    chuggernaut_dispatcher::monitor::start(state.clone());

    // Wait for retry scan to transition to OnDeck
    tokio::time::sleep(Duration::from_secs(4)).await;

    assert_eq!(
        state.jobs.get(&key).unwrap().state,
        JobState::OnDeck,
        "monitor should transition retry-eligible job to OnDeck"
    );
}

#[tokio::test]
async fn monitor_archival_removes_done_job() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 3600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 1,
        job_retention_secs: 1, // 1 second retention for quick archival
        activity_limit: 50,
        forgejo_url: None,
        forgejo_token: None,
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Archival test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 0,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Close job → Done, backdate updated_at so retention window has passed
    jobs::transition_job(&state, &key, JobState::Done, "test", None).await.unwrap();
    {
        let mut job = state.jobs.get(&key).unwrap().clone();
        job.updated_at = chrono::Utc::now() - chrono::Duration::seconds(10);
        state.jobs.insert(key.clone(), job.clone());
        jobs::kv_put(&state.kv.jobs, &key, &job).await.unwrap();
    }

    assert!(state.jobs.contains_key(&key));

    // Start monitor
    chuggernaut_dispatcher::monitor::start(state.clone());

    // Wait for archival scan
    tokio::time::sleep(Duration::from_secs(4)).await;

    assert!(
        !state.jobs.contains_key(&key),
        "archived job should be removed from in-memory index"
    );
}

// ---------------------------------------------------------------------------
// Group 6: HTTP API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_list_jobs() {
    let state = setup().await;
    let make = |title: &str| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    jobs::create_job(&state, make("A")).await.unwrap();
    jobs::create_job(&state, make("B")).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp: JobListResponse = client.get(format!("{base}/jobs"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(resp.jobs.len(), 2);
}

#[tokio::test]
async fn http_list_jobs_filter_by_state() {
    let state = setup().await;
    let make = |title: &str, initial: Option<JobState>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: initial,
    };
    jobs::create_job(&state, make("OnDeck1", None)).await.unwrap();
    jobs::create_job(&state, make("OnDeck2", None)).await.unwrap();
    jobs::create_job(&state, make("OnIce1", Some(JobState::OnIce))).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();

    // Filter by on-deck (kebab-case)
    let resp: JobListResponse = client.get(format!("{base}/jobs?state=on-deck"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(resp.jobs.len(), 2);
    for job in &resp.jobs {
        assert_eq!(job.state, JobState::OnDeck);
    }

    // Filter by on-ice
    let resp: JobListResponse = client.get(format!("{base}/jobs?state=on-ice"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(resp.jobs.len(), 1);
    assert_eq!(resp.jobs[0].state, JobState::OnIce);
}

#[tokio::test]
async fn http_get_job_detail() {
    let state = setup().await;
    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Detail test".to_string(),
        body: "some body".to_string(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp: JobDetailResponse = client.get(format!("{base}/jobs/{key}"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(resp.job.key, key);
    assert_eq!(resp.job.title, "Detail test");
    assert_eq!(resp.job.body, "some body");
    assert!(resp.claim.is_none());
}

#[tokio::test]
async fn http_get_job_not_found() {
    let state = setup().await;
    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp = client.get(format!("{base}/jobs/nonexistent"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn http_create_job() {
    let state = setup().await;
    let base = start_http(state.clone()).await;
    let client = reqwest::Client::new();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "HTTP create".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let resp = client.post(format!("{base}/jobs"))
        .json(&req)
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);

    let body: CreateJobResponse = resp.json().await.unwrap();
    assert_eq!(body.key, "test.repo.1");
    assert!(state.jobs.contains_key(&body.key));
}

#[tokio::test]
async fn http_requeue_job() {
    let state = setup().await;

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Requeue".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: Some(JobState::OnIce),
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state.clone()).await;
    let client = reqwest::Client::new();

    let resp = client.post(format!("{base}/jobs/{key}/requeue"))
        .json(&serde_json::json!({"target": "on-deck"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);
}

#[tokio::test]
async fn http_close_job() {
    let state = setup().await;

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Close".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state.clone()).await;
    let client = reqwest::Client::new();

    let resp = client.post(format!("{base}/jobs/{key}/close"))
        .json(&serde_json::json!({"revoke": false}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Done);
}

#[tokio::test]
async fn http_get_journal() {
    let state = setup().await;

    // Create a job which writes journal entries
    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Journal".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp = client.get(format!("{base}/journal"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: JournalListResponse = resp.json().await.unwrap();
    // create_job writes a journal entry
    assert!(!body.entries.is_empty(), "journal should have entries after job creation");
}

#[tokio::test]
async fn http_get_job_deps() {
    let state = setup().await;

    let make = |title: &str, deps: Vec<u64>| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: deps,
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let _key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp: JobDepsResponse = client.get(format!("{base}/jobs/{key_b}/deps"))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(resp.dependencies.len(), 1);
    assert_eq!(resp.dependencies[0].key, _key_a);
    assert!(!resp.all_done);
}

#[tokio::test]
async fn http_channel_send() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Channel".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp = client.post(format!("{base}/jobs/{key}/channel/send"))
        .json(&serde_json::json!({"message": "hello"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn http_channel_send_missing_job() {
    let state = setup().await;
    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp = client.post(format!("{base}/jobs/nonexistent/channel/send"))
        .json(&serde_json::json!({"message": "hello"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn http_sse_snapshot() {
    let state = setup().await;

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "SSE test".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();

    // SSE endpoint returns a stream; read the first chunk
    let mut resp = client.get(format!("{base}/events"))
        .header("Accept", "text/event-stream")
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // Read first chunk (should contain snapshot event)
    let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
        .await.unwrap().unwrap().unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("event: snapshot"), "first SSE event should be snapshot, got: {text}");
    assert!(text.contains("jobs"), "snapshot should contain jobs array");
}

// ---------------------------------------------------------------------------
// Group 2: Assignment & capacity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn capacity_limit_prevents_assignment() {
    let state = setup_with_config(|c| c.max_concurrent_actions = 1).await;

    let make = |title: &str| CreateJobRequest {
        repo: "test/repo".to_string(),
        title: title.to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
    };
    let key1 = jobs::create_job(&state, make("Job1")).await.unwrap();
    let key2 = jobs::create_job(&state, make("Job2")).await.unwrap();

    // Manually put job1 on-the-stack to fill the single slot
    let worker_id = format!("action-{key1}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key1, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key1, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();

    // Try to assign job2 — should fail due to capacity
    let assigned = chuggernaut_dispatcher::assignment::try_assign_job(&state, &key2).await.unwrap();
    assert!(!assigned, "should be at capacity");
    assert_eq!(state.jobs.get(&key2).unwrap().state, JobState::OnDeck);
}

#[tokio::test]
async fn dispatch_next_respects_priority() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("pri{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", workflow, "add workflow",
    ).await;

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 60,
        default_timeout_secs: 600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 1, // only 1 slot
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    // Create 3 jobs with different priorities
    let make = |title: &str, pri: u8| CreateJobRequest {
        repo: format!("{org}/repo"),
        title: title.to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: pri,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };

    let key_low = jobs::create_job(&state, make("Low", 10)).await.unwrap();
    let key_high = jobs::create_job(&state, make("High", 90)).await.unwrap();
    let key_mid = jobs::create_job(&state, make("Mid", 50)).await.unwrap();

    // Dispatch next — should pick highest priority
    chuggernaut_dispatcher::assignment::try_dispatch_next(&state).await.unwrap();

    assert_eq!(state.jobs.get(&key_high).unwrap().state, JobState::OnTheStack, "highest priority job should be dispatched");
    assert_eq!(state.jobs.get(&key_mid).unwrap().state, JobState::OnDeck, "mid priority should stay on-deck");
    assert_eq!(state.jobs.get(&key_low).unwrap().state, JobState::OnDeck, "low priority should stay on-deck");
}

#[tokio::test]
async fn dispatch_next_after_yield() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("dn{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", workflow, "add workflow",
    ).await;

    let review_workflow = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/review.yml", review_workflow, "add review workflow",
    ).await;

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 60,
        default_timeout_secs: 600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 1, // only 1 slot
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);
    handlers::start_handlers(state.clone()).await.unwrap();

    let make = |title: &str| CreateJobRequest {
        repo: format!("{org}/repo"),
        title: title.to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };

    let key1 = jobs::create_job(&state, make("First")).await.unwrap();
    let key2 = jobs::create_job(&state, make("Second")).await.unwrap();

    // Dispatch first job
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key1).await.unwrap();
    assert_eq!(state.jobs.get(&key1).unwrap().state, JobState::OnTheStack);
    assert_eq!(state.jobs.get(&key2).unwrap().state, JobState::OnDeck);

    // Yield first job — should free slot and auto-dispatch second
    let worker_id = format!("action-{key1}");
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key1.clone(),
        outcome: OutcomeType::Yield { pr_url: format!("http://forgejo/{org}/repo/pulls/1") },
        token_usage: None,
    };
    state.nats.publish_msg(&subjects::WORKER_OUTCOME, &outcome).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    assert_eq!(state.jobs.get(&key1).unwrap().state, JobState::InReview);
    assert_eq!(
        state.jobs.get(&key2).unwrap().state,
        JobState::OnTheStack,
        "second job should be auto-dispatched after first yields"
    );
}

#[tokio::test]
async fn changes_requested_in_dispatch_queue() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let forgejo_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("cr{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port, &token, &org, "repo",
        ".forgejo/workflows/work.yml", workflow, "add workflow",
    ).await;

    let config = Config {
        nats_url: nats_url.clone(),
        nats_worker_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 60,
        default_timeout_secs: 600,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 1,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await.unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    // Create a ChangesRequested job (high priority) and an OnDeck job (low priority)
    let req_cr = CreateJobRequest {
        repo: format!("{org}/repo"),
        title: "CR job".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 90,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };
    let key_cr = jobs::create_job(&state, req_cr).await.unwrap();

    // Simulate work cycle → ChangesRequested (release claim so dispatch_action can re-acquire)
    let worker_id = format!("action-{key_cr}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key_cr, &worker_id, 3600).await.unwrap();
    jobs::transition_job(&state, &key_cr, JobState::OnTheStack, "test", Some(&worker_id)).await.unwrap();
    chuggernaut_dispatcher::claims::release_claim(&state, &key_cr).await.unwrap();
    jobs::transition_job(&state, &key_cr, JobState::InReview, "test", None).await.unwrap();
    jobs::transition_job(&state, &key_cr, JobState::ChangesRequested, "test", None).await.unwrap();

    let req_od = CreateJobRequest {
        repo: format!("{org}/repo"),
        title: "OnDeck job".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 10,
        capabilities: vec![],
        platform: None,
        timeout_secs: 300,
        review: ReviewLevel::Low,
        max_retries: 0,
        initial_state: None,
    };
    let key_od = jobs::create_job(&state, req_od).await.unwrap();

    // Dispatch next — ChangesRequested job has higher priority
    chuggernaut_dispatcher::assignment::try_dispatch_next(&state).await.unwrap();

    assert_eq!(
        state.jobs.get(&key_cr).unwrap().state,
        JobState::OnTheStack,
        "ChangesRequested job with higher priority should be dispatched"
    );
    assert_eq!(state.jobs.get(&key_od).unwrap().state, JobState::OnDeck);
}

mod common;
use common::*;

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
    let git_url = test_utils::forgejo_host_url(forgejo_port);

    // Create admin user + token
    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    // Create org + repo + workflow
    let org = format!("org{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    // Push a noop workflow so dispatch_workflow succeeds
    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        workflow,
        "add workflow",
    )
    .await;

    let review_workflow = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/review.yml",
        review_workflow,
        "add review workflow",
    )
    .await;

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
        git_url: Some(git_url),
        git_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Trigger assignment — should dispatch action directly
    chuggernaut_dispatcher::assignment::assign_job(&state, &key)
        .await
        .unwrap();

    // Verify: job is OnTheStack, claim exists with action- prefix
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    let (claim, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap()
        .unwrap();
    assert!(
        claim.worker_id.starts_with("action-"),
        "worker_id should be action-{key}"
    );
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
    let git_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("rw{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    // Push workflow with review_feedback + is_rework inputs
    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        workflow,
        "add workflow",
    )
    .await;

    let review_workflow = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/review.yml",
        review_workflow,
        "add review workflow",
    )
    .await;

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
        git_url: Some(git_url),
        git_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 10,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Dispatch initial action → OnTheStack
    chuggernaut_dispatcher::assignment::assign_job(&state, &key)
        .await
        .unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    // Simulate yield → InReview
    let worker_id = format!("action-{key}");
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: format!("http://forgejo/{org}/repo/pulls/1"),
            partial: false,
        },
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

    // Publish ChangesRequested with feedback
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested {
            feedback: "fix the tests please".to_string(),
        },
        pr_url: Some(format!("http://forgejo/{org}/repo/pulls/1")),
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision)
        .await
        .unwrap();
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
        .await
        .unwrap()
        .unwrap();
    assert!(
        claim.worker_id.starts_with("action-"),
        "rework claim should have action- prefix"
    );
}

// ---------------------------------------------------------------------------
// Assignment & capacity
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
        claude_args: None,
        rework_limit: None,
    };
    let key1 = jobs::create_job(&state, make("Job1")).await.unwrap();
    let key2 = jobs::create_job(&state, make("Job2")).await.unwrap();

    // Manually put job1 on-the-stack to fill the single slot
    let worker_id = format!("action-{key1}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key1, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(
        &state,
        &key1,
        JobState::OnTheStack,
        "test",
        Some(&worker_id),
    )
    .await
    .unwrap();

    // Try to assign job2 — should fail due to capacity
    let assigned = chuggernaut_dispatcher::assignment::assign_job(&state, &key2)
        .await
        .unwrap();
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
    let git_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("pri{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        workflow,
        "add workflow",
    )
    .await;

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
        git_url: Some(git_url),
        git_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 1, // only 1 slot
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };

    let key_low = jobs::create_job(&state, make("Low", 10)).await.unwrap();
    let key_high = jobs::create_job(&state, make("High", 90)).await.unwrap();
    let key_mid = jobs::create_job(&state, make("Mid", 50)).await.unwrap();

    // Dispatch next — should pick highest priority
    chuggernaut_dispatcher::assignment::dispatch_next(&state)
        .await
        .unwrap();

    assert_eq!(
        state.jobs.get(&key_high).unwrap().state,
        JobState::OnTheStack,
        "highest priority job should be dispatched"
    );
    assert_eq!(
        state.jobs.get(&key_mid).unwrap().state,
        JobState::OnDeck,
        "mid priority should stay on-deck"
    );
    assert_eq!(
        state.jobs.get(&key_low).unwrap().state,
        JobState::OnDeck,
        "low priority should stay on-deck"
    );
}

#[tokio::test]
async fn dispatch_next_after_yield() {
    let port = test_nats_port();
    let nats_url = format!("nats://127.0.0.1:{port}");
    let prefix = uuid::Uuid::new_v4().simple().to_string();

    let forgejo = test_utils::start_forgejo().await;
    let forgejo_port = test_utils::forgejo_port(&forgejo).await;
    let git_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("dn{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        workflow,
        "add workflow",
    )
    .await;

    let review_workflow = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/review.yml",
        review_workflow,
        "add review workflow",
    )
    .await;

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
        git_url: Some(git_url),
        git_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 1, // only 1 slot
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };

    let key1 = jobs::create_job(&state, make("First")).await.unwrap();
    let key2 = jobs::create_job(&state, make("Second")).await.unwrap();

    // Dispatch first job
    chuggernaut_dispatcher::assignment::assign_job(&state, &key1)
        .await
        .unwrap();
    assert_eq!(state.jobs.get(&key1).unwrap().state, JobState::OnTheStack);
    assert_eq!(state.jobs.get(&key2).unwrap().state, JobState::OnDeck);

    // Yield first job — should free slot and auto-dispatch second
    let worker_id = format!("action-{key1}");
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key1.clone(),
        outcome: OutcomeType::Yield {
            pr_url: format!("http://forgejo/{org}/repo/pulls/1"),
            partial: false,
        },
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
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
    let git_url = test_utils::forgejo_host_url(forgejo_port);

    let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, false).await;
    let token = creds.admin_token;

    let org = format!("cr{}", &prefix[..8]);
    test_utils::create_test_repo(forgejo_port, &token, &org, "repo").await;

    let workflow = "name: work\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      review_feedback:\n        type: string\n        default: \"\"\n      is_rework:\n        type: string\n        default: \"false\"\n      runner_label:\n        type: string\n        default: \"ubuntu-latest\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        workflow,
        "add workflow",
    )
    .await;

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
        git_url: Some(git_url),
        git_token: Some(token),
        action_workflow: "work.yml".to_string(),
        action_runner_label: "ubuntu-latest".to_string(),
        max_concurrent_actions: 1,
        review_workflow: "review.yml".to_string(),
        review_runner_label: "ubuntu-latest".to_string(),
        rework_limit: 3,
        human_login: "you".to_string(),
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key_cr = jobs::create_job(&state, req_cr).await.unwrap();

    // Simulate work cycle → ChangesRequested (release claim so dispatch_action can re-acquire)
    let worker_id = format!("action-{key_cr}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key_cr, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(
        &state,
        &key_cr,
        JobState::OnTheStack,
        "test",
        Some(&worker_id),
    )
    .await
    .unwrap();
    chuggernaut_dispatcher::claims::release_claim(&state, &key_cr)
        .await
        .unwrap();
    jobs::transition_job(&state, &key_cr, JobState::InReview, "test", None)
        .await
        .unwrap();
    jobs::transition_job(&state, &key_cr, JobState::ChangesRequested, "test", None)
        .await
        .unwrap();

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
        claude_args: None,
        rework_limit: None,
    };
    let key_od = jobs::create_job(&state, req_od).await.unwrap();

    // Dispatch next — ChangesRequested job has higher priority
    chuggernaut_dispatcher::assignment::dispatch_next(&state)
        .await
        .unwrap();

    assert_eq!(
        state.jobs.get(&key_cr).unwrap().state,
        JobState::OnTheStack,
        "ChangesRequested job with higher priority should be dispatched"
    );
    assert_eq!(state.jobs.get(&key_od).unwrap().state, JobState::OnDeck);
}

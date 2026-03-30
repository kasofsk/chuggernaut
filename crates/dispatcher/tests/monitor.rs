mod common;
use common::*;

/// Poll in-memory state until `pred` returns true, or fail after `timeout`.
/// `describe` returns a human-readable snapshot of what the predicate is checking.
async fn poll_until(
    label: &str,
    timeout: Duration,
    pred: impl Fn() -> bool,
    describe: impl Fn() -> String,
) {
    let result = tokio::time::timeout(timeout, async {
        loop {
            if pred() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if result.is_err() {
        panic!(
            "{label}: timed out after {timeout:?} — last state: {}",
            describe()
        );
    }
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
        git_url: None,
        git_token: None,
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
        archive_threshold: 200,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack with claim
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(
        &state,
        &key,
        JobState::OnTheStack,
        "action_dispatched",
        Some(&worker_id),
    )
    .await
    .unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    // Don't send heartbeats — wait for lease to expire (1s lease + 1s scan interval)
    let s = state.clone();
    let s2 = state.clone();
    let k2 = key.clone();
    poll_until(
        "lease expiry → Failed",
        Duration::from_secs(10),
        move || s.jobs.get(&key).map(|j| j.state) == Some(JobState::Failed),
        move || format!("{:?}", s2.jobs.get(&k2).map(|j| j.state)),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Monitor: job timeout
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
        lease_secs: 120,         // long lease so it doesn't expire
        default_timeout_secs: 1, // very short timeout
        cas_max_retries: 3,
        monitor_scan_interval_secs: 1,
        job_retention_secs: 86400,
        activity_limit: 50,
        git_url: None,
        git_token: None,
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
        archive_threshold: 200,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Acquire claim with 1s timeout
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 1)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();

    // Wait for monitor to detect timeout
    let s = state.clone();
    let s2 = state.clone();
    let k2 = key.clone();
    poll_until(
        "job timeout → Failed",
        Duration::from_secs(10),
        move || s.jobs.get(&key).map(|j| j.state) == Some(JobState::Failed),
        move || format!("{:?}", s2.jobs.get(&k2).map(|j| j.state)),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Monitor: orphan detection
// ---------------------------------------------------------------------------

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
        git_url: None,
        git_token: None,
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
        archive_threshold: 200,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    // Subscribe to orphan events before starting monitor
    let mut orphan_sub = state
        .nats
        .subscribe("chuggernaut.monitor.orphan")
        .await
        .unwrap();

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
        claude_args: None,
        rework_limit: None,
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
    let got_orphan = tokio::time::timeout(Duration::from_secs(10), orphan_sub.next()).await;
    assert!(got_orphan.is_ok(), "should receive orphan detection event");
}

// ---------------------------------------------------------------------------
// Monitor: retry eligible transitions to OnDeck
// ---------------------------------------------------------------------------

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
        git_url: None,
        git_token: None,
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
        archive_threshold: 200,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate failure via outcome
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Fail {
            reason: "error".to_string(),
            logs: None,
        },
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();

    let s = state.clone();
    let k = key.clone();
    let s2 = state.clone();
    let k2 = key.clone();
    poll_until(
        "worker outcome → Failed",
        Duration::from_secs(5),
        move || s.jobs.get(&k).map(|j| j.state) == Some(JobState::Failed),
        move || format!("{:?}", s2.jobs.get(&k2).map(|j| j.state)),
    )
    .await;

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
    let s = state.clone();
    let s2 = state.clone();
    let k2 = key.clone();
    poll_until(
        "monitor retry → OnDeck",
        Duration::from_secs(10),
        move || s.jobs.get(&key).map(|j| j.state) == Some(JobState::OnDeck),
        move || format!("{:?}", s2.jobs.get(&k2).map(|j| j.state)),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Monitor: archival removes done job
// ---------------------------------------------------------------------------

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
        git_url: None,
        git_token: None,
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
        archive_threshold: 0, // disable threshold so archival fires with 1 job
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Close job → Done, backdate updated_at so retention window has passed
    jobs::transition_job(&state, &key, JobState::Done, "test", None)
        .await
        .unwrap();
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
    let s = state.clone();
    let s2 = state.clone();
    let k2 = key.clone();
    poll_until(
        "archival removes job",
        Duration::from_secs(10),
        move || !s.jobs.contains_key(&key),
        move || format!("still_exists={}", s2.jobs.contains_key(&k2)),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Monitor: lease expiry schedules retry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn monitor_lease_expiry_schedules_retry() {
    // Lease expiry should set retry_count and retry_after so the monitor
    // can later pick the job up for auto-retry (same as worker-reported failures).
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
        git_url: None,
        git_token: None,
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
        archive_threshold: 200,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize_with_prefix(&js, config.lease_secs, Some(&prefix))
        .await
        .unwrap();
    let state = DispatcherState::new_namespaced(config, client, js, kv, prefix);

    handlers::start_handlers(state.clone()).await.unwrap();
    chuggernaut_dispatcher::monitor::start(state.clone());

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Lease retry test".to_string(),
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
    let key = jobs::create_job(&state, req).await.unwrap();

    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(
        &state,
        &key,
        JobState::OnTheStack,
        "action_dispatched",
        Some(&worker_id),
    )
    .await
    .unwrap();

    // Wait for lease to expire and monitor to process it
    let s = state.clone();
    let k = key.clone();
    let s2 = state.clone();
    let k2 = key.clone();
    poll_until(
        "lease expiry → Failed",
        Duration::from_secs(10),
        move || s.jobs.get(&k).map(|j| j.state) == Some(JobState::Failed),
        move || format!("{:?}", s2.jobs.get(&k2).map(|j| j.state)),
    )
    .await;

    let job = state.jobs.get(&key).unwrap();
    assert_eq!(
        job.retry_count, 1,
        "lease expiry should increment retry_count"
    );
    assert!(
        job.retry_after.is_some(),
        "lease expiry should set retry_after for auto-retry"
    );
}

// ---------------------------------------------------------------------------
// schedule_auto_retry fixes Failed jobs with missing retry_after
// ---------------------------------------------------------------------------

#[tokio::test]
async fn schedule_auto_retry_fixes_missing_retry_after() {
    let state = setup().await;

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Missing retry_after".to_string(),
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
    let key = jobs::create_job(&state, req).await.unwrap();

    // Manually set job to Failed in both KV and memory, with no retry_after
    // (simulates the old recovery bug)
    {
        let (mut job, _rev) = jobs::kv_get::<Job>(&state.kv.jobs, &key)
            .await
            .unwrap()
            .unwrap();
        job.state = JobState::Failed;
        job.retry_count = 0;
        job.retry_after = None;
        jobs::kv_put(&state.kv.jobs, &key, &job).await.unwrap();
        state.jobs.insert(key.clone(), job);
    }

    // Verify precondition
    let before = state.jobs.get(&key).unwrap().clone();
    assert_eq!(before.state, JobState::Failed);
    assert!(before.retry_after.is_none());
    assert_eq!(before.retry_count, 0);

    // Call schedule_auto_retry directly (this is what the monitor safety-net calls)
    chuggernaut_dispatcher::assignment::schedule_auto_retry(&state, &key).await;

    let after = state.jobs.get(&key).unwrap().clone();
    assert_eq!(after.retry_count, 1, "retry_count should be incremented");
    assert!(
        after.retry_after.is_some(),
        "retry_after must be set so scan_retry picks it up"
    );
}

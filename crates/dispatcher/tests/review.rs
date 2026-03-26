mod common;
use common::*;

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
        claude_args: None,
    };

    // A (no deps) → B depends on A
    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);

    // Simulate on-the-stack + yield → InReview
    let worker_id = format!("action-{key_a}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key_a, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(
        &state,
        &key_a,
        JobState::OnTheStack,
        "action_dispatched",
        Some(&worker_id),
    )
    .await
    .unwrap();

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key_a.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
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
    assert_eq!(state.jobs.get(&key_a).unwrap().state, JobState::InReview);

    // Publish ReviewDecision Approved
    let decision = ReviewDecision {
        job_key: key_a.clone(),
        decision: DecisionType::Approved,
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision)
        .await
        .unwrap();
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack + yield → InReview
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

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
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

    // Publish ChangesRequested
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested {
            feedback: "fix the tests".to_string(),
        },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        state.jobs.get(&key).unwrap().state,
        JobState::ChangesRequested
    );
}

// ---------------------------------------------------------------------------
// Escalated review
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack → yield → InReview
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
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
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

    // Publish Escalated decision
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::Escalated {
            reviewer_login: "human".to_string(),
        },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision)
        .await
        .unwrap();
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Get to Escalated
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::InReview, "test", None)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::Escalated, "test", None)
        .await
        .unwrap();

    // Admin close → Done
    let close = CloseJobRequest {
        job_key: key.clone(),
        revoke: false,
    };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_CLOSE_JOB, &close),
    )
    .await
    .unwrap()
    .unwrap();

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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Direct transitions to Escalated
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::InReview, "test", None)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::Escalated, "test", None)
        .await
        .unwrap();

    // Escalated → ChangesRequested
    let result = jobs::transition_job(
        &state,
        &key,
        JobState::ChangesRequested,
        "human_review",
        None,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(
        state.jobs.get(&key).unwrap().state,
        JobState::ChangesRequested
    );
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
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        work_wf,
        "add work workflow",
    )
    .await;

    let review_wf = "name: review\non:\n  workflow_dispatch:\n    inputs:\n      job_key:\n        type: string\n      nats_url:\n        type: string\n      pr_url:\n        type: string\n      review_level:\n        type: string\n        default: \"high\"\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/review.yml",
        review_wf,
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
        forgejo_url: Some(forgejo_url),
        forgejo_token: Some(token),
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key)
        .await
        .unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    // Worker yields with PR URL
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
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Job should be InReview with CI pending (review deferred until CI passes)
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::InReview);
    assert_eq!(job.ci_status, Some(CiStatus::Pending));
    assert!(job.ci_check_since.is_some());

    // Review should NOT be dispatched yet (no journal entry for review_dispatched)
    let mut found_review_dispatch = false;
    if let Ok(keys) = state.kv.journal.keys().await {
        tokio::pin!(keys);
        while let Some(Ok(jk)) = keys.next().await {
            if let Ok(Some((entry, _))) = jobs::kv_get::<JournalEntry>(&state.kv.journal, &jk).await
                && entry.action == "review_dispatched"
                && entry.job_key.as_deref() == Some(&key)
            {
                found_review_dispatch = true;
                break;
            }
        }
    }
    assert!(
        !found_review_dispatch,
        "review should NOT be dispatched before CI passes"
    );

    // Simulate CI passing via CiCheckEvent
    let ci_event = CiCheckEvent {
        job_key: key.clone(),
        pr_url: format!("http://forgejo/{org}/repo/pulls/1"),
        ci_status: CiStatus::Success,
        detected_at: chrono::Utc::now(),
    };
    state
        .nats
        .publish_msg(&subjects::MONITOR_CI_CHECK, &ci_event)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Now verify review was dispatched and CI status updated
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.ci_status, Some(CiStatus::Success));

    // Check journal for ci_passed entry
    let mut found_ci_passed = false;
    if let Ok(keys) = state.kv.journal.keys().await {
        tokio::pin!(keys);
        while let Some(Ok(jk)) = keys.next().await {
            if let Ok(Some((entry, _))) = jobs::kv_get::<JournalEntry>(&state.kv.journal, &jk).await
                && entry.action == "ci_passed"
                && entry.job_key.as_deref() == Some(&key)
            {
                found_ci_passed = true;
                break;
            }
        }
    }
    assert!(found_ci_passed, "expected ci_passed journal entry");
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate work: claim → OnTheStack → yield with token usage
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

    let work_tokens = TokenUsage {
        input_tokens: 25000,
        output_tokens: 8000,
        cache_read_tokens: 12000,
        cache_write_tokens: 3000,
    };
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
            partial: false,
        },
        token_usage: Some(work_tokens.clone()),
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
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
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify both records exist on the job
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::Done);
    assert_eq!(
        job.token_usage.len(),
        2,
        "expected 2 token records (work + review)"
    );

    assert_eq!(job.token_usage[0].action_type, "work");
    assert_eq!(job.token_usage[0].token_usage.input_tokens, 25000);

    assert_eq!(job.token_usage[1].action_type, "review");
    assert_eq!(job.token_usage[1].token_usage.input_tokens, 15000);
    assert_eq!(job.token_usage[1].token_usage.output_tokens, 3500);

    // Also verify via KV (persistence)
    let (kv_job, _) = jobs::kv_get::<Job>(&state.kv.jobs, &key)
        .await
        .unwrap()
        .unwrap();
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // First work cycle
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

    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
            partial: false,
        },
        token_usage: Some(TokenUsage {
            input_tokens: 20000,
            output_tokens: 5000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }),
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // First review: changes requested
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested {
            feedback: "fix tests".to_string(),
        },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: Some(TokenUsage {
            input_tokens: 10000,
            output_tokens: 2000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }),
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Job should be in ChangesRequested (no Forgejo to dispatch rework, so it stays there)
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::ChangesRequested);
    assert_eq!(
        job.token_usage.len(),
        2,
        "expected work + review records after first cycle"
    );
    assert_eq!(job.token_usage[0].action_type, "work");
    assert_eq!(job.token_usage[1].action_type, "review");

    // Simulate rework: manually claim + transition + yield again
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(
        &state,
        &key,
        JobState::OnTheStack,
        "rework_dispatched",
        Some(&worker_id),
    )
    .await
    .unwrap();

    let outcome2 = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
            partial: false,
        },
        token_usage: Some(TokenUsage {
            input_tokens: 8000,
            output_tokens: 3000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }),
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome2)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Second review: approved
    let decision2 = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::Approved,
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: Some(TokenUsage {
            input_tokens: 9000,
            output_tokens: 1500,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }),
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision2)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::Done);
    assert_eq!(
        job.token_usage.len(),
        4,
        "expected 4 records: work, review, work(rework), review"
    );

    assert_eq!(job.token_usage[0].action_type, "work");
    assert_eq!(job.token_usage[0].token_usage.input_tokens, 20000);
    assert_eq!(job.token_usage[1].action_type, "review");
    assert_eq!(job.token_usage[1].token_usage.input_tokens, 10000);
    assert_eq!(job.token_usage[2].action_type, "work");
    assert_eq!(job.token_usage[2].token_usage.input_tokens, 8000);
    assert_eq!(job.token_usage[3].action_type, "review");
    assert_eq!(job.token_usage[3].token_usage.input_tokens, 9000);
}

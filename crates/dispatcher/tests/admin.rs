mod common;
use common::*;

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
        claude_args: None,
        rework_limit: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);

    let close = CloseJobRequest {
        job_key: key_a.clone(),
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
        claude_args: None,
        rework_limit: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    let close = CloseJobRequest {
        job_key: key_a.clone(),
        revoke: true,
    };
    state
        .nats
        .request_msg(&subjects::ADMIN_CLOSE_JOB, &close)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key_a).unwrap().state, JobState::Revoked);
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate on-the-stack with claim + fail
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
        outcome: OutcomeType::Fail {
            reason: "compile error".to_string(),
            logs: None,
        },
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);

    // Admin requeue to OnDeck
    let requeue = RequeueRequest {
        job_key: key.clone(),
        target: RequeueTarget::OnDeck,
    };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_REQUEUE, &requeue),
    )
    .await
    .unwrap()
    .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);
}

// ---------------------------------------------------------------------------
// Requeue / thaw
// ---------------------------------------------------------------------------

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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Get to Failed
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
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);

    // Requeue to OnIce
    let requeue = RequeueRequest {
        job_key: key.clone(),
        target: RequeueTarget::OnIce,
    };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_REQUEUE, &requeue),
    )
    .await
    .unwrap()
    .unwrap();

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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnIce);

    let requeue = RequeueRequest {
        job_key: key.clone(),
        target: RequeueTarget::OnDeck,
    };
    let _reply = tokio::time::timeout(
        Duration::from_secs(5),
        state.nats.request_msg(&subjects::ADMIN_REQUEUE, &requeue),
    )
    .await
    .unwrap()
    .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnDeck);
}

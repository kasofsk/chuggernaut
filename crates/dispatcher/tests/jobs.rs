mod common;
use common::*;

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
        claude_args: None,
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
        claude_args: None,
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
        claude_args: None,
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
        claude_args: None,
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
        claude_args: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();
    assert_eq!(state.jobs.get(&key_b).unwrap().state, JobState::Blocked);

    // Complete A → B unblocks
    jobs::transition_job(&state, &key_a, JobState::Done, "test", None)
        .await
        .unwrap();
    let unblocked = chuggernaut_dispatcher::deps::propagate_unblock(&state, &key_a)
        .await
        .unwrap();
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
        claude_args: None,
    };

    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![])).await.unwrap();
    let key_c = jobs::create_job(&state, make("C", vec![1, 2]))
        .await
        .unwrap();
    assert_eq!(state.jobs.get(&key_c).unwrap().state, JobState::Blocked);

    // Complete A — C stays blocked (B not done)
    jobs::transition_job(&state, &key_a, JobState::Done, "test", None)
        .await
        .unwrap();
    let unblocked = chuggernaut_dispatcher::deps::propagate_unblock(&state, &key_a)
        .await
        .unwrap();
    assert!(unblocked.is_empty());
    assert_eq!(state.jobs.get(&key_c).unwrap().state, JobState::Blocked);

    // Complete B — C unblocks
    jobs::transition_job(&state, &key_b, JobState::Done, "test", None)
        .await
        .unwrap();
    let unblocked = chuggernaut_dispatcher::deps::propagate_unblock(&state, &key_b)
        .await
        .unwrap();
    assert_eq!(unblocked, vec![key_c.clone()]);
    assert_eq!(state.jobs.get(&key_c).unwrap().state, JobState::OnDeck);
}

// ---------------------------------------------------------------------------
// State machine edge tests
// ---------------------------------------------------------------------------

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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Close job → Done
    let close = CloseJobRequest {
        job_key: key.clone(),
        revoke: false,
    };
    state
        .nats
        .request_msg(&subjects::ADMIN_CLOSE_JOB, &close)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Done);

    // Try Done → OnTheStack (invalid)
    let result = jobs::transition_job(&state, &key, JobState::OnTheStack, "test", None).await;
    assert!(result.is_err(), "Done → OnTheStack should be rejected");
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
        claude_args: None,
    };

    // Create A (no deps), B (deps on A)
    let _key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    // Try to add A → B dep (would create cycle: A→B and B→A)
    let result =
        chuggernaut_dispatcher::deps::create_deps(&state, &_key_a, std::slice::from_ref(&key_b))
            .await;
    assert!(result.is_err(), "adding cycle should fail");
}

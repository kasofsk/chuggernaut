mod common;
use common::*;

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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Simulate job being on-the-stack with a claim (as action_dispatch would do)
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

    // Worker yields
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
    let job = state.jobs.get(&key).unwrap();
    assert_eq!(job.state, JobState::InReview);
    assert_eq!(
        job.pr_url.as_deref(),
        Some("http://forgejo/test/repo/pulls/1")
    );
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
        claude_args: None,
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
    let job = state.jobs.get(&key).unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.retry_count, 1);
    assert!(job.retry_after.is_some());
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
        claude_args: None,
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

    let (before, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let hb = WorkerHeartbeat {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        token_usage: None,
        cost_usd: None,
        turns: None,
        rate_limit: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_HEARTBEAT, &hb)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (after, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap()
        .unwrap();
    assert!(after.lease_deadline > before.lease_deadline);
}

// ---------------------------------------------------------------------------
// Partial yield: continuation dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partial_yield_dispatches_continuation() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Partial yield test".to_string(),
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

    // Simulate on-the-stack
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

    // Worker yields with partial=true
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
            partial: true,
        },
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Job should be back on OnDeck (or OnTheStack if dispatch succeeded — but without
    // Forgejo, dispatch will fail and job stays OnDeck)
    let job = state.jobs.get(&key).unwrap().clone();
    // continuation_count should be incremented
    assert_eq!(job.continuation_count, 1);
    assert_eq!(
        job.pr_url.as_deref(),
        Some("http://forgejo/test/repo/pulls/1")
    );
    // ci_status should NOT be set (partial yield skips CI check)
    assert!(job.ci_status.is_none());

    // Check journal for partial_yield entry
    let mut found_partial = false;
    if let Ok(keys) = state.kv.journal.keys().await {
        tokio::pin!(keys);
        while let Some(Ok(jk)) = keys.next().await {
            if let Ok(Some((entry, _))) = jobs::kv_get::<JournalEntry>(&state.kv.journal, &jk).await
                && entry.action == "partial_yield"
                && entry.job_key.as_deref() == Some(&key)
            {
                found_partial = true;
                break;
            }
        }
    }
    assert!(found_partial, "expected partial_yield journal entry");
}

// ---------------------------------------------------------------------------
// Partial yield: max continuations reached → proceeds to CI check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partial_yield_max_continuations_proceeds_to_review() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Max continuations test".to_string(),
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

    // Set continuation_count to max_continuations (3)
    jobs::kv_cas_rmw::<Job, _>(&state.kv.jobs, &key, state.config.cas_max_retries, |job| {
        job.continuation_count = state.config.max_continuations;
        Ok(())
    })
    .await
    .unwrap();
    if let Ok(Some((j, _))) = jobs::kv_get::<Job>(&state.kv.jobs, &key).await {
        state.jobs.insert(key.clone(), j);
    }

    // Simulate on-the-stack
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

    // Worker yields with partial=true but max continuations reached
    let outcome = WorkerOutcome {
        worker_id: worker_id.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
            partial: true,
        },
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Should proceed to InReview with CI pending (same as non-partial)
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::InReview);
    assert_eq!(job.ci_status, Some(CiStatus::Pending));
    assert!(job.ci_check_since.is_some());
    // continuation_count should NOT have been incremented
    assert_eq!(job.continuation_count, state.config.max_continuations);
}

// ---------------------------------------------------------------------------
// CI failure: transitions to ChangesRequested
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ci_failure_triggers_changes_requested() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "CI failure test".to_string(),
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

    // Walk through the state machine: OnDeck → OnTheStack → InReview
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();
    chuggernaut_dispatcher::claims::release_claim(&state, &key)
        .await
        .unwrap();

    // Set CI pending fields and transition to InReview
    jobs::kv_cas_rmw::<Job, _>(&state.kv.jobs, &key, state.config.cas_max_retries, |job| {
        job.ci_status = Some(CiStatus::Pending);
        job.ci_check_since = Some(chrono::Utc::now());
        job.pr_url = Some("http://forgejo/test/repo/pulls/1".to_string());
        Ok(())
    })
    .await
    .unwrap();
    if let Ok(Some((j, _))) = jobs::kv_get::<Job>(&state.kv.jobs, &key).await {
        state.jobs.insert(key.clone(), j);
    }
    jobs::transition_job(&state, &key, JobState::InReview, "worker_yield", None)
        .await
        .unwrap();

    // Simulate CI failure event
    let ci_event = CiCheckEvent {
        job_key: key.clone(),
        pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
        ci_status: CiStatus::Failure,
        detected_at: chrono::Utc::now(),
    };
    // Route directly to assignment task (monitor events skip NATS handlers).
    chuggernaut_dispatcher::assignment::request_dispatch(
        &state,
        chuggernaut_dispatcher::state::DispatchRequest::CiCheck(ci_event),
    );
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Job should transition to ChangesRequested
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::ChangesRequested);
    assert_eq!(job.ci_status, Some(CiStatus::Failure));

    // Check journal for ci_failed entry
    let mut found_ci_failed = false;
    if let Ok(keys) = state.kv.journal.keys().await {
        tokio::pin!(keys);
        while let Some(Ok(jk)) = keys.next().await {
            if let Ok(Some((entry, _))) = jobs::kv_get::<JournalEntry>(&state.kv.journal, &jk).await
                && entry.action == "ci_failed"
                && entry.job_key.as_deref() == Some(&key)
            {
                found_ci_failed = true;
                break;
            }
        }
    }
    assert!(found_ci_failed, "expected ci_failed journal entry");

    // Check activity for ci_failed entry
    if let Ok(Some((log, _))) = jobs::kv_get::<ActivityLog>(&state.kv.activities, &key).await {
        assert!(
            log.entries.iter().any(|e| e.kind == "ci_failed"),
            "expected ci_failed activity entry"
        );
    }
}

// ---------------------------------------------------------------------------
// CI error variant also triggers ChangesRequested
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ci_error_triggers_changes_requested() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "CI error test".to_string(),
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

    // Walk through: OnDeck → OnTheStack → InReview
    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();
    chuggernaut_dispatcher::claims::release_claim(&state, &key)
        .await
        .unwrap();

    jobs::kv_cas_rmw::<Job, _>(&state.kv.jobs, &key, state.config.cas_max_retries, |job| {
        job.ci_status = Some(CiStatus::Pending);
        job.ci_check_since = Some(chrono::Utc::now());
        job.pr_url = Some("http://forgejo/test/repo/pulls/1".to_string());
        Ok(())
    })
    .await
    .unwrap();
    if let Ok(Some((j, _))) = jobs::kv_get::<Job>(&state.kv.jobs, &key).await {
        state.jobs.insert(key.clone(), j);
    }
    jobs::transition_job(&state, &key, JobState::InReview, "worker_yield", None)
        .await
        .unwrap();

    // CI error (not failure)
    let ci_event = CiCheckEvent {
        job_key: key.clone(),
        pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
        ci_status: CiStatus::Error,
        detected_at: chrono::Utc::now(),
    };
    // Route directly to assignment task (monitor events skip NATS handlers).
    chuggernaut_dispatcher::assignment::request_dispatch(
        &state,
        chuggernaut_dispatcher::state::DispatchRequest::CiCheck(ci_event),
    );
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::ChangesRequested);
    assert_eq!(job.ci_status, Some(CiStatus::Error));
}

// ---------------------------------------------------------------------------
// CI check on non-InReview job is ignored
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ci_check_ignored_if_not_in_review() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let req = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "CI check ignore test".to_string(),
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
    // Job is in OnDeck state

    // Send CI check event for a job that isn't InReview
    let ci_event = CiCheckEvent {
        job_key: key.clone(),
        pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
        ci_status: CiStatus::Failure,
        detected_at: chrono::Utc::now(),
    };
    state
        .nats
        .publish_msg(&subjects::MONITOR_CI_CHECK, &ci_event)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Job should still be OnDeck (event ignored)
    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::OnDeck);
}

// ---------------------------------------------------------------------------
// Error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn action_dispatch_failure_releases_claim() {
    // Use a bad Forgejo URL that will fail immediately
    let state = setup_with_config(|c| {
        c.git_url = Some("http://127.0.0.1:1".to_string());
        c.git_token = Some("fake-token".to_string());
    })
    .await;

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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // try_assign_job should fail internally but return Ok(false)
    let assigned = chuggernaut_dispatcher::assignment::assign_job(&state, &key)
        .await
        .unwrap();
    assert!(!assigned, "assignment should fail with bad forgejo URL");

    // Job should be Failed (action_dispatch releases claim and transitions to Failed)
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::Failed);

    // Claim should be released (may be tombstoned — kv_get fails on tombstone, which is fine)
    let claim = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap_or(None);
    assert!(
        claim.is_none(),
        "claim should be released after dispatch failure"
    );
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();

    let (before, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap()
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send heartbeat from a different worker
    let hb = WorkerHeartbeat {
        worker_id: "wrong-worker".to_string(),
        job_key: key.clone(),
        token_usage: None,
        cost_usd: None,
        turns: None,
        rate_limit: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_HEARTBEAT, &hb)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (after, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before.lease_deadline, after.lease_deadline,
        "lease should not be renewed by wrong worker"
    );
    assert_eq!(after.worker_id, worker_id, "claim owner should not change");
}

#[tokio::test]
async fn rework_limit_escalates_when_exceeded() {
    // rework_limit=1 means the first ChangesRequested dispatches rework,
    // but the second ChangesRequested escalates to a human reviewer.
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // --- Cycle 0: should succeed (rework_count=0 < limit=1) ---
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

    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested {
            feedback: "fix #1".to_string(),
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

    // First cycle: ChangesRequested (rework dispatched, no Forgejo to actually run it)
    assert_eq!(
        state.jobs.get(&key).unwrap().state,
        JobState::ChangesRequested,
        "cycle 0: should transition to ChangesRequested"
    );
    assert_eq!(state.jobs.get(&key).unwrap().rework_count, 1);

    // --- Cycle 1: should escalate (rework_count=1 >= limit=1) ---
    let worker_id2 = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id2, 3600)
        .await
        .unwrap();
    jobs::transition_job(
        &state,
        &key,
        JobState::OnTheStack,
        "test",
        Some(&worker_id2),
    )
    .await
    .unwrap();

    let outcome2 = WorkerOutcome {
        worker_id: worker_id2.clone(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: "http://forgejo/test/repo/pulls/1".to_string(),
            partial: false,
        },
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome2)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);

    let decision2 = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested {
            feedback: "fix #2".to_string(),
        },
        pr_url: Some("http://forgejo/test/repo/pulls/1".to_string()),
        token_usage: None,
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision2)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Second cycle: Escalated (rework_count >= rework_limit)
    assert_eq!(
        state.jobs.get(&key).unwrap().state,
        JobState::Escalated,
        "cycle 1: should escalate when rework_limit exceeded"
    );
}

// ---------------------------------------------------------------------------
// Token usage + concurrency
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

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
        token_usage: None, // no token usage
    };
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let job = state.jobs.get(&key).unwrap().clone();
    assert_eq!(job.state, JobState::InReview);
    assert!(
        job.token_usage.is_empty(),
        "nil token_usage should not create a record"
    );
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let worker_id = format!("action-{key}");
    chuggernaut_dispatcher::claims::acquire_claim(&state, &key, &worker_id, 3600)
        .await
        .unwrap();
    jobs::transition_job(&state, &key, JobState::OnTheStack, "test", Some(&worker_id))
        .await
        .unwrap();

    // Send 10 heartbeats concurrently
    let mut handles = Vec::new();
    for _ in 0..10 {
        let s = state.clone();
        let w = worker_id.clone();
        let k = key.clone();
        handles.push(tokio::spawn(async move {
            let hb = WorkerHeartbeat {
                worker_id: w,
                job_key: k,
                token_usage: None,
                cost_usd: None,
                turns: None,
                rate_limit: None,
            };
            s.nats
                .publish_msg(&subjects::WORKER_HEARTBEAT, &hb)
                .await
                .unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Claim should still be valid
    let (claim, _) = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap()
        .unwrap();
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

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

    // Send same outcome twice
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Should be InReview (first succeeds, second is a no-op or graceful error)
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::InReview);
}

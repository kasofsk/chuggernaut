mod common;
use common::*;

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
        claude_args: None,
        rework_limit: None,
    };
    let key1 = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key2 = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    // Simulate restart: clear in-memory state
    state.jobs.clear();
    {
        let mut g = state.graph.write().await;
        *g = chuggernaut_dispatcher::state::DepGraph::new();
    }

    recovery::recover(&state).await.unwrap();

    assert_eq!(state.jobs.len(), 2);
    assert!(state.jobs.contains_key(&key1));
    assert!(state.jobs.contains_key(&key2));

    let graph = state.graph.read().await;
    assert_eq!(graph.dag.edge_count(), 1);
}

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
        claude_args: None,
        rework_limit: None,
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
    jobs::kv_put(&state.kv.claims, &key, &stale_claim)
        .await
        .unwrap();

    // Verify claim exists
    assert!(
        jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
            .await
            .unwrap()
            .is_some()
    );

    // Run recovery
    recovery::recover(&state).await.unwrap();

    // Stale claim should be deleted (tombstoned — kv_get may fail on tombstone)
    let claim_after = jobs::kv_get::<ClaimState>(&state.kv.claims, &key)
        .await
        .unwrap_or(None);
    assert!(
        claim_after.is_none(),
        "stale claim should be cleaned up by recovery"
    );
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    // Directly update the job to OnTheStack in KV (bypassing claim requirement)
    let (mut job, _rev) = jobs::kv_get::<Job>(&state.kv.jobs, &key)
        .await
        .unwrap()
        .unwrap();
    job.state = JobState::OnTheStack;
    jobs::kv_put(&state.kv.jobs, &key, &job).await.unwrap();

    // Clear in-memory state to simulate restart
    state.jobs.clear();
    {
        let mut g = state.graph.write().await;
        *g = chuggernaut_dispatcher::state::DepGraph::new();
    }

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
        claude_args: None,
        rework_limit: None,
    };
    let key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    // Corrupt: remove the reverse dep on A (A should have depended_on_by=[B])
    let (mut dep_a, _rev) = jobs::kv_get::<DepRecord>(&state.kv.deps, &key_a)
        .await
        .unwrap()
        .unwrap();
    dep_a.depended_on_by.clear();
    jobs::kv_put(&state.kv.deps, &key_a, &dep_a).await.unwrap();

    // Clear in-memory state
    state.jobs.clear();
    {
        let mut g = state.graph.write().await;
        *g = chuggernaut_dispatcher::state::DepGraph::new();
    }

    // Recover
    recovery::recover(&state).await.unwrap();

    // Verify reverse dep was repaired
    let (dep_a_after, _) = jobs::kv_get::<DepRecord>(&state.kv.deps, &key_a)
        .await
        .unwrap()
        .unwrap();
    assert!(
        dep_a_after.depended_on_by.contains(&key_b),
        "recovery should repair reverse dep: A.depended_on_by should contain B"
    );
}

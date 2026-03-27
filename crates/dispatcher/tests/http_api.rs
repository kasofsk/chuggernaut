mod common;
use common::*;

// ---------------------------------------------------------------------------
// HTTP API
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
        claude_args: None,
        rework_limit: None,
    };
    jobs::create_job(&state, make("A")).await.unwrap();
    jobs::create_job(&state, make("B")).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp: JobListResponse = client
        .get(format!("{base}/jobs"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    jobs::create_job(&state, make("OnDeck1", None))
        .await
        .unwrap();
    jobs::create_job(&state, make("OnDeck2", None))
        .await
        .unwrap();
    jobs::create_job(&state, make("OnIce1", Some(JobState::OnIce)))
        .await
        .unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();

    // Filter by on-deck (kebab-case)
    let resp: JobListResponse = client
        .get(format!("{base}/jobs?state=on-deck"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp.jobs.len(), 2);
    for job in &resp.jobs {
        assert_eq!(job.state, JobState::OnDeck);
    }

    // Filter by on-ice
    let resp: JobListResponse = client
        .get(format!("{base}/jobs?state=on-ice"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp: JobDetailResponse = client
        .get(format!("{base}/jobs/{key}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
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
    let resp = client
        .get(format!("{base}/jobs/nonexistent"))
        .send()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let resp = client
        .post(format!("{base}/jobs"))
        .json(&req)
        .send()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state.clone()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/jobs/{key}/requeue"))
        .json(&serde_json::json!({"target": "on-deck"}))
        .send()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state.clone()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/jobs/{key}/close"))
        .json(&serde_json::json!({"revoke": false}))
        .send()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp = client.get(format!("{base}/journal")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: JournalListResponse = resp.json().await.unwrap();
    // create_job writes a journal entry
    assert!(
        !body.entries.is_empty(),
        "journal should have entries after job creation"
    );
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
        claude_args: None,
        rework_limit: None,
    };
    let _key_a = jobs::create_job(&state, make("A", vec![])).await.unwrap();
    let key_b = jobs::create_job(&state, make("B", vec![1])).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp: JobDepsResponse = client
        .get(format!("{base}/jobs/{key_b}/deps"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/jobs/{key}/channel/send"))
        .json(&serde_json::json!({"message": "hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn http_channel_send_missing_job() {
    let state = setup().await;
    let base = start_http(state).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/jobs/nonexistent/channel/send"))
        .json(&serde_json::json!({"message": "hello"}))
        .send()
        .await
        .unwrap();
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
        claude_args: None,
        rework_limit: None,
    };
    jobs::create_job(&state, req).await.unwrap();

    let base = start_http(state).await;
    let client = reqwest::Client::new();

    // SSE endpoint returns a stream; read the first chunk
    let mut resp = client
        .get(format!("{base}/events"))
        .header("Accept", "text/event-stream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Read first chunk (should contain snapshot event)
    let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(
        text.contains("event: snapshot"),
        "first SSE event should be snapshot, got: {text}"
    );
    assert!(text.contains("jobs"), "snapshot should contain jobs array");
}

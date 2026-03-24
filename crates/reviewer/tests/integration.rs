use std::sync::Arc;
use std::time::Duration;

use chuggernaut_test_utils as test_utils;

use chuggernaut_dispatcher::{
    config::Config as DispatcherConfig, handlers, http, jobs, nats_init,
    state::DispatcherState,
};
use chuggernaut_forgejo_api::ForgejoClient;
use chuggernaut_reviewer::{config::Config as ReviewerConfig, nats_setup, state::ReviewerState};
use chuggernaut_types::*;

// ---------------------------------------------------------------------------
// Shared infrastructure — one NATS + one Forgejo per test process
// ---------------------------------------------------------------------------

struct TestInfra {
    nats_port: u16,
    forgejo_port: u16,
    admin_token: String,
    reviewer_token: String,
}

static TEST_INFRA: std::sync::OnceLock<TestInfra> = std::sync::OnceLock::new();

fn test_infra() -> &'static TestInfra {
    TEST_INFRA.get_or_init(|| {
        std::thread::spawn(|| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                // Start NATS with JetStream
                let nats = test_utils::start_nats().await;
                let nats_port = test_utils::nats_port(&nats).await;
                Box::leak(Box::new(nats));

                // Start Forgejo
                let forgejo = test_utils::start_forgejo().await;
                let forgejo_port = test_utils::forgejo_port(&forgejo).await;

                // Create admin + reviewer users
                let creds = test_utils::setup_forgejo_users(&forgejo, forgejo_port, true).await;

                Box::leak(Box::new(forgejo));

                TestInfra {
                    nats_port,
                    forgejo_port,
                    admin_token: creds.admin_token,
                    reviewer_token: creds.reviewer_token.unwrap(),
                }
            })
        })
        .join()
        .unwrap()
    })
}

// ---------------------------------------------------------------------------
// Per-test setup: namespaced dispatcher + reviewer + Forgejo org/repo
// ---------------------------------------------------------------------------

struct TestEnv {
    dispatcher_state: Arc<DispatcherState>,
    reviewer_state: Arc<ReviewerState>,
    /// Admin Forgejo client (creates orgs, repos, PRs)
    forgejo: ForgejoClient,
    /// Reviewer Forgejo client (submits reviews — must be different user to avoid "approve own PR")
    reviewer_forgejo: ForgejoClient,
    forgejo_url: String,
    org: String,
    repo: String,
    #[allow(dead_code)]
    dispatcher_url: String,
}

async fn setup() -> TestEnv {
    let infra = test_infra();
    let prefix = uuid::Uuid::new_v4().simple().to_string();
    let nats_url = format!("nats://127.0.0.1:{}", infra.nats_port);
    let forgejo_url = test_utils::forgejo_host_url(infra.forgejo_port);

    // Reuse the shared tokens from test_infra
    let token = infra.admin_token.clone();
    let reviewer_token = infra.reviewer_token.clone();

    let forgejo = ForgejoClient::new(&forgejo_url, &token);
    let reviewer_forgejo = ForgejoClient::new(&forgejo_url, &reviewer_token);

    // Create namespaced org + repo
    let org = format!("org{}", &prefix[..8]);
    let repo_name = "repo";

    test_utils::create_test_repo(infra.forgejo_port, &token, &org, repo_name).await;

    // Create a noop review workflow so dispatch_workflow succeeds.
    let workflow_content = "name: review-work\non:\n  workflow_dispatch:\njobs:\n  noop:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo noop\n";
    test_utils::push_file(
        infra.forgejo_port,
        &token,
        &org,
        repo_name,
        ".forgejo/workflows/review-work.yml",
        workflow_content,
        "add noop review workflow",
    )
    .await;

    // Set up dispatcher
    let dispatcher_config = DispatcherConfig {
        nats_url: nats_url.clone(),
        http_listen: "127.0.0.1:0".to_string(),
        lease_secs: 5,
        default_timeout_secs: 60,
        cas_max_retries: 3,
        monitor_scan_interval_secs: 100,
        job_retention_secs: 86400,
        activity_limit: 50,
        blacklist_ttl_secs: 3600,
        forgejo_url: Some(forgejo_url.clone()),
        forgejo_token: Some(token.clone()),
        action_workflow: "review-work.yml".to_string(),
    };

    let nats_client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(nats_client.clone());
    let kv = nats_init::initialize_with_prefix(
        &js,
        dispatcher_config.lease_secs,
        dispatcher_config.blacklist_ttl_secs,
        Some(&prefix),
    )
    .await
    .unwrap();

    let dispatcher_state =
        DispatcherState::new_namespaced(dispatcher_config, nats_client, js, kv, prefix.clone());

    // Start dispatcher handlers
    handlers::start_handlers(dispatcher_state.clone())
        .await
        .unwrap();

    // Start dispatcher HTTP server on random port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let dispatcher_addr = listener.local_addr().unwrap();
    let dispatcher_url = format!("http://{dispatcher_addr}");
    let app = http::router(dispatcher_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Set up reviewer
    let reviewer_nats = async_nats::connect(&nats_url).await.unwrap();
    let reviewer_js = async_nats::jetstream::new(reviewer_nats.clone());
    let reviewer_kv = nats_setup::initialize(&reviewer_js, 300).await.unwrap();

    let reviewer_config = ReviewerConfig {
        nats_url: nats_url.clone(),
        forgejo_url: forgejo_url.clone(),
        forgejo_token: token,
        dispatcher_url: dispatcher_url.clone(),
        human_login: test_utils::ADMIN_USER.to_string(),
        delay_secs: 0, // no delay in tests
        workflow: "review-work.yml".to_string(),
        runner: "ubuntu-latest".to_string(),
        action_timeout_secs: 10,
        poll_secs: 1,
        escalation_poll_secs: 1,
        rework_limit: 3,
        merge_lock_ttl_secs: 300,
    };

    let reviewer_state =
        ReviewerState::new(reviewer_config, reviewer_nats, reviewer_js, reviewer_kv);

    // Bridge: forward reviewer's unprefixed NATS messages to the dispatcher's
    // prefixed namespace. The reviewer publishes to plain subjects (e.g.,
    // "chuggernaut.review.decision") but the dispatcher listens on prefixed subjects
    // (e.g., "{uuid}_chuggernaut.review.decision").
    {
        let ds = dispatcher_state.clone();
        let bridge_nats = async_nats::connect(&nats_url).await.unwrap();
        let mut sub = bridge_nats
            .subscribe(subjects::REVIEW_DECISION.name)
            .await
            .unwrap();
        tokio::spawn(async move {
            while let Some(msg) = futures::StreamExt::next(&mut sub).await {
                let _ = ds
                    .nats
                    .publish(subjects::REVIEW_DECISION.name, msg.payload)
                    .await;
            }
        });
    }

    TestEnv {
        dispatcher_state,
        reviewer_state,
        forgejo,
        reviewer_forgejo,
        forgejo_url,
        org,
        repo: repo_name.to_string(),
        dispatcher_url,
    }
}

fn repo_full(env: &TestEnv) -> String {
    format!("{}/{}", env.org, env.repo)
}

// ---------------------------------------------------------------------------
// Helper: create a job, assign to worker, yield with a real PR
// ---------------------------------------------------------------------------

async fn create_job_and_yield_pr(env: &TestEnv) -> (String, String) {
    create_job_and_yield_pr_with_worker(env, "w1").await
}

async fn create_job_and_yield_pr_with_worker(env: &TestEnv, worker_id: &str) -> (String, String) {
    let repo_full = repo_full(env);
    let req = CreateJobRequest {
        repo: repo_full.clone(),
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
    let key = jobs::create_job(&env.dispatcher_state, req).await.unwrap();

    // Register worker + assign
    let reg = WorkerRegistration {
        worker_id: worker_id.to_string(),
        capabilities: vec![],
        worker_type: "sim".to_string(),
        platform: vec![],
    };
    env.dispatcher_state
        .nats
        .request_msg(&subjects::WORKER_REGISTER, &reg)
        .await
        .unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&env.dispatcher_state, &key, worker_id, false, None)
        .await
        .unwrap();

    // Create a real branch + PR in Forgejo
    let branch_name = format!("chuggernaut/{key}");

    // Get main branch SHA
    let main_branch: serde_json::Value = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/{}/branches/main",
            env.forgejo_url, repo_full
        ))
        .header(
            "Authorization",
            format!("token {}", env.reviewer_state.config.forgejo_token),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let _main_sha = main_branch["commit"]["id"].as_str().unwrap();

    // Create branch
    reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{}/branches",
            env.forgejo_url, repo_full
        ))
        .header(
            "Authorization",
            format!("token {}", env.reviewer_state.config.forgejo_token),
        )
        .json(&serde_json::json!({
            "new_branch_name": branch_name,
            "old_branch_name": "main"
        }))
        .send()
        .await
        .unwrap();

    // Create a file on the branch (so the PR has a diff)
    reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{}/contents/{}",
            env.forgejo_url,
            repo_full,
            format!("{key}.txt")
        ))
        .header(
            "Authorization",
            format!("token {}", env.reviewer_state.config.forgejo_token),
        )
        .json(&serde_json::json!({
            "message": format!("work for {key}"),
            "content": test_utils::base64_encode("hello world"),
            "branch": branch_name
        }))
        .send()
        .await
        .unwrap();

    // Create PR
    let pr = env
        .forgejo
        .create_pull_request(
            &env.org,
            &env.repo,
            &chuggernaut_forgejo_api::CreatePullRequestOption {
                title: format!("PR for {key}"),
                body: Some("test".to_string()),
                head: branch_name,
                base: "main".to_string(),
            },
        )
        .await
        .unwrap();

    // Worker yields with the PR URL
    let outcome = WorkerOutcome {
        worker_id: worker_id.to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: pr.html_url.clone(),
        },
    };
    env.dispatcher_state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        env.dispatcher_state.jobs.get(&key).unwrap().state,
        JobState::InReview
    );

    (key, pr.html_url)
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Helper: submit a review after a delay (while process_job runs concurrently).
/// The delay lets process_job reach the polling loop so the review isn't filtered as stale.
async fn submit_review_after_delay(
    fg: &ForgejoClient,
    pr_url: &str,
    event: &str,
    body: &str,
    delay: Duration,
) {
    tokio::time::sleep(delay).await;
    let (owner, repo, pr_index) =
        chuggernaut_reviewer::merge::parse_pr_url(pr_url).unwrap();
    fg.submit_review(
        &owner,
        &repo,
        pr_index,
        &chuggernaut_forgejo_api::CreatePullReviewOptions {
            body: body.to_string(),
            event: event.to_string(),
        },
    )
    .await
    .unwrap();
}

/// Review approved via Forgejo API → reviewer detects → merges → job Done
#[tokio::test]
async fn review_approved_merges_and_completes() {
    let env = setup().await;
    let (key, pr_url) = create_job_and_yield_pr(&env).await;

    // Submit APPROVED review concurrently — delayed so process_job's polling finds it
    let fg = env.reviewer_forgejo.clone();
    let url = pr_url.clone();
    let review_task = tokio::spawn(async move {
        submit_review_after_delay(&fg, &url, "APPROVED", "LGTM", Duration::from_secs(2)).await;
    });

    chuggernaut_reviewer::review::process_job(&env.reviewer_state, &key)
        .await
        .unwrap();
    review_task.await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        env.dispatcher_state.jobs.get(&key).unwrap().state,
        JobState::Done
    );

    let (owner, repo, pr_index) =
        chuggernaut_reviewer::merge::parse_pr_url(&pr_url).unwrap();
    let pr = env.forgejo.get_pull_request(&owner, &repo, pr_index).await.unwrap();
    assert!(pr.merged);
}

/// Review with changes_requested → reviewer detects → job ChangesRequested
#[tokio::test]
async fn review_changes_requested_routes_rework() {
    let env = setup().await;
    let (key, pr_url) = create_job_and_yield_pr(&env).await;

    let fg = env.reviewer_forgejo.clone();
    let url = pr_url.clone();
    let review_task = tokio::spawn(async move {
        submit_review_after_delay(&fg, &url, "REQUEST_CHANGES", "Please fix", Duration::from_secs(2)).await;
    });

    chuggernaut_reviewer::review::process_job(&env.reviewer_state, &key)
        .await
        .unwrap();
    review_task.await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        env.dispatcher_state.jobs.get(&key).unwrap().state,
        JobState::ChangesRequested
    );
}

/// Human review level → reviewer escalates directly without running action
#[tokio::test]
async fn human_review_level_escalates_then_human_approves() {
    let env = setup().await;
    let repo_full = repo_full(&env);

    // Create job with ReviewLevel::Human
    let req = CreateJobRequest {
        repo: repo_full.clone(),
        title: "Human review job".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 50,
        capabilities: vec![],
        worker_type: None,
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::Human,
        max_retries: 3,
        initial_state: None,
    };
    let key = jobs::create_job(&env.dispatcher_state, req).await.unwrap();

    // Register + assign + yield with PR
    let reg = WorkerRegistration {
        worker_id: "w1".to_string(),
        capabilities: vec![],
        worker_type: "sim".to_string(),
        platform: vec![],
    };
    env.dispatcher_state
        .nats
        .request_msg(&subjects::WORKER_REGISTER, &reg)
        .await
        .unwrap();
    chuggernaut_dispatcher::assignment::assign_job(&env.dispatcher_state, &key, "w1", false, None)
        .await
        .unwrap();

    // Create branch + file + PR
    let branch_name = format!("chuggernaut/{key}");
    reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{}/branches",
            env.forgejo_url, repo_full
        ))
        .header(
            "Authorization",
            format!("token {}", env.reviewer_state.config.forgejo_token),
        )
        .json(&serde_json::json!({
            "new_branch_name": branch_name,
            "old_branch_name": "main"
        }))
        .send()
        .await
        .unwrap();

    reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{}/contents/{}.txt",
            env.forgejo_url, repo_full, key
        ))
        .header(
            "Authorization",
            format!("token {}", env.reviewer_state.config.forgejo_token),
        )
        .json(&serde_json::json!({
            "message": format!("work for {key}"),
            "content": test_utils::base64_encode("human review test"),
            "branch": branch_name
        }))
        .send()
        .await
        .unwrap();

    let pr = env
        .forgejo
        .create_pull_request(
            &env.org,
            &env.repo,
            &chuggernaut_forgejo_api::CreatePullRequestOption {
                title: format!("PR for {key}"),
                body: None,
                head: branch_name,
                base: "main".to_string(),
            },
        )
        .await
        .unwrap();

    let outcome = WorkerOutcome {
        worker_id: "w1".to_string(),
        job_key: key.clone(),
        outcome: OutcomeType::Yield {
            pr_url: pr.html_url.clone(),
        },
    };
    env.dispatcher_state
        .nats
        .publish_msg(&subjects::WORKER_OUTCOME, &outcome)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Submit human approval concurrently — delayed so escalation's polling finds it
    let fg = env.reviewer_forgejo.clone();
    let html_url = pr.html_url.clone();
    let review_task = tokio::spawn(async move {
        submit_review_after_delay(&fg, &html_url, "APPROVED", "Looks good", Duration::from_secs(2)).await;
    });

    // Run the reviewer — should escalate (Human level), then poll and find the approval
    chuggernaut_reviewer::review::process_job(&env.reviewer_state, &key)
        .await
        .unwrap();
    review_task.await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        env.dispatcher_state.jobs.get(&key).unwrap().state,
        JobState::Done
    );

    // Verify PR was merged
    let (owner, repo, pr_index) =
        chuggernaut_reviewer::merge::parse_pr_url(&pr.html_url).unwrap();
    let merged_pr = env
        .forgejo
        .get_pull_request(&owner, &repo, pr_index)
        .await
        .unwrap();
    assert!(merged_pr.merged);
}

/// Two approved jobs in the same repo → both merge sequentially through the queue.
/// Reviews are submitted concurrently (after a delay) so the reviewer's polling loop
/// finds them after it starts.
#[tokio::test]
async fn merge_queue_serializes_concurrent_merges() {
    let env = setup().await;

    // Create two jobs with separate workers
    let (key1, pr_url1) = create_job_and_yield_pr_with_worker(&env, "w1").await;
    let (key2, pr_url2) = create_job_and_yield_pr_with_worker(&env, "w2").await;

    // Helper: submit a review after a delay, while process_job runs concurrently
    async fn submit_review_delayed(fg: &ForgejoClient, pr_url: &str, delay: Duration) {
        tokio::time::sleep(delay).await;
        let (owner, repo, pr_index) =
            chuggernaut_reviewer::merge::parse_pr_url(pr_url).unwrap();
        fg.submit_review(
            &owner,
            &repo,
            pr_index,
            &chuggernaut_forgejo_api::CreatePullReviewOptions {
                body: "LGTM".to_string(),
                event: "APPROVED".to_string(),
            },
        )
        .await
        .unwrap();
    }

    // Process job1: spawn delayed review, then run process_job
    let fg1 = env.reviewer_forgejo.clone();
    let url1c = pr_url1.clone();
    let review_task1 = tokio::spawn(async move {
        submit_review_delayed(&fg1, &url1c, Duration::from_secs(2)).await;
    });
    chuggernaut_reviewer::review::process_job(&env.reviewer_state, &key1)
        .await
        .unwrap();
    review_task1.await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        env.dispatcher_state.jobs.get(&key1).unwrap().state,
        JobState::Done
    );

    // Process job2: same pattern — spawn delayed review, then process
    let fg2 = env.reviewer_forgejo.clone();
    let url2c = pr_url2.clone();
    let review_task2 = tokio::spawn(async move {
        submit_review_delayed(&fg2, &url2c, Duration::from_secs(2)).await;
    });
    chuggernaut_reviewer::review::process_job(&env.reviewer_state, &key2)
        .await
        .unwrap();
    review_task2.await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        env.dispatcher_state.jobs.get(&key2).unwrap().state,
        JobState::Done
    );

    // Verify both PRs are merged in Forgejo
    let (owner, repo, pr_index1) =
        chuggernaut_reviewer::merge::parse_pr_url(&pr_url1).unwrap();
    let pr1 = env
        .forgejo
        .get_pull_request(&owner, &repo, pr_index1)
        .await
        .unwrap();
    assert!(pr1.merged);

    let (_, _, pr_index2) =
        chuggernaut_reviewer::merge::parse_pr_url(&pr_url2).unwrap();
    let pr2 = env
        .forgejo
        .get_pull_request(&owner, &repo, pr_index2)
        .await
        .unwrap();
    assert!(pr2.merged);
}

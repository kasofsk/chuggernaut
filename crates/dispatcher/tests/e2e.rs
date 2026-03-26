mod common;
use common::*;

// ---------------------------------------------------------------------------
// Action runner isolation test: Forgejo + runner + NATS, no dispatcher
// ---------------------------------------------------------------------------

/// Focused test: run a Forgejo Action that executes chuggernaut-worker,
/// which publishes WorkerOutcome to NATS. No dispatcher involved.
///
/// Run: `cargo test -p chuggernaut-dispatcher --test e2e action_runner_publishes -- --ignored --nocapture`
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
            --forgejo-token "${{{{ secrets.CHUGGERNAUT_WORKER_TOKEN }}}}" \
            --command mock-claude \
            --heartbeat-interval-secs 5
        env:
          CHUGGERNAUT_REVIEW_FEEDBACK: ${{{{ inputs.review_feedback }}}}
          CHUGGERNAUT_IS_REWORK: ${{{{ inputs.is_rework }}}}
"#
    );

    test_utils::push_file(
        forgejo_port,
        &token,
        org,
        "repo",
        ".forgejo/workflows/work.yml",
        &workflow,
        "add workflow",
    )
    .await;

    // Set secret
    test_utils::set_repo_secret(
        forgejo_port,
        &token,
        org,
        "repo",
        "CHUGGERNAUT_WORKER_TOKEN",
        &token,
    )
    .await;

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
    )
    .await;

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
        if let Ok(msg) = tokio::time::timeout(Duration::from_millis(500), hb_sub.next()).await
            && msg.is_some()
            && !got_heartbeat
        {
            eprintln!("Got heartbeat from worker!");
            got_heartbeat = true;
        }

        // Check for outcome
        if let Ok(Some(msg)) =
            tokio::time::timeout(Duration::from_millis(100), outcome_sub.next()).await
        {
            let outcome: WorkerOutcome = serde_json::from_slice(&msg.payload).unwrap();
            eprintln!("Got outcome: {:?}", outcome.outcome);
            got_outcome = true;
        }

        // Check for channel messages
        if let Ok(Some(msg)) =
            tokio::time::timeout(Duration::from_millis(100), channel_sub.next()).await
            && let Ok(channel_msg) =
                serde_json::from_slice::<chuggernaut_types::ChannelMessage>(&msg.payload)
        {
            eprintln!(
                "Got channel message: sender={} body={}",
                channel_msg.sender, channel_msg.body
            );
            got_channel_msg = true;
        }

        if got_outcome {
            break;
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Drain any remaining channel messages that arrived around the same time as the outcome
    if !got_channel_msg
        && let Ok(Some(msg)) =
            tokio::time::timeout(Duration::from_secs(2), channel_sub.next()).await
        && let Ok(channel_msg) =
            serde_json::from_slice::<chuggernaut_types::ChannelMessage>(&msg.payload)
    {
        eprintln!(
            "Got channel message (post-loop): sender={} body={}",
            channel_msg.sender, channel_msg.body
        );
        got_channel_msg = true;
    }

    eprintln!(
        "Results: heartbeat={got_heartbeat} outcome={got_outcome} channel={got_channel_msg} action_status={action_status}"
    );

    assert!(got_heartbeat, "should have received heartbeat from worker");
    assert!(
        got_outcome,
        "should have received WorkerOutcome from worker"
    );
    assert!(
        got_channel_msg,
        "should have received channel_send message on NATS outbox"
    );

    // Check status KV
    let js = async_nats::jetstream::new(nats_client.clone());
    if let Ok(kv) = js.get_key_value("chuggernaut_channels").await {
        if let Ok(Some(entry)) = kv.entry(job_key).await {
            let status: chuggernaut_types::ChannelStatus =
                serde_json::from_slice(&entry.value).unwrap();
            eprintln!(
                "Channel status: {} progress={:?}",
                status.status, status.progress
            );
            assert_eq!(status.status, "done");
        } else {
            eprintln!(
                "No status KV entry found (channel KV bucket may not have been created by worker)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Full E2E: dispatcher → Forgejo Action → runner → worker → outcome
// ---------------------------------------------------------------------------

/// Full end-to-end test with a real Forgejo Actions runner.
/// Requires: Docker socket available, `chuggernaut-runner-env` image built.
///
/// Run: `cargo test -p chuggernaut-dispatcher --test e2e e2e_full -- --test-threads=1 --ignored`
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
            --forgejo-token "${{{{ secrets.CHUGGERNAUT_WORKER_TOKEN }}}}" \
            --command mock-claude \
            --heartbeat-interval-secs 5
        env:
          CHUGGERNAUT_REVIEW_FEEDBACK: ${{{{ inputs.review_feedback }}}}
          CHUGGERNAUT_IS_REWORK: ${{{{ inputs.is_rework }}}}
"#
    );
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        &workflow,
        "add workflow",
    )
    .await;

    // Set repo secret for Forgejo token
    test_utils::set_repo_secret(
        forgejo_port,
        &token,
        &org,
        "repo",
        "CHUGGERNAUT_WORKER_TOKEN",
        &token,
    )
    .await;

    // Start runner via test-utils
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let runner_config_path = workspace_root
        .join("infra/runner/config.yaml")
        .canonicalize()
        .expect("infra/runner/config.yaml not found");

    let _runner = test_utils::start_runner(
        forgejo_port,
        &runner_reg_token,
        runner_config_path.to_str().unwrap(),
    )
    .await;

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
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
    };

    let client = async_nats::connect(&nats_url).await.unwrap();
    let js = async_nats::jetstream::new(client.clone());
    let kv = nats_init::initialize(&js, config.lease_secs).await.unwrap();
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    eprintln!("E2E: created job {key}");

    // Dispatcher auto-dispatches via try_assign_job (nats_worker_url = Docker-internal)
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key)
        .await
        .unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);

    eprintln!("E2E: action dispatched via dispatcher, waiting for worker outcome...");

    // Check action runs after a short delay
    tokio::time::sleep(Duration::from_secs(5)).await;
    let runs_resp = http
        .get(format!(
            "{forgejo_url}/api/v1/repos/{org}/repo/actions/runs"
        ))
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
                .get(format!(
                    "{forgejo_url}/api/v1/repos/{org}/repo/actions/runs"
                ))
                .header("Authorization", format!("token {token}"))
                .send()
                .await
                .unwrap();
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
/// Run: `cargo test -p chuggernaut-dispatcher --test e2e e2e_full_review_cycle -- --test-threads=1 --ignored --nocapture`
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
            --forgejo-token "${{{{ secrets.CHUGGERNAUT_WORKER_TOKEN }}}}" \
            --command mock-claude \
            --heartbeat-interval-secs 5
        env:
          CHUGGERNAUT_REVIEW_FEEDBACK: ${{{{ inputs.review_feedback }}}}
          CHUGGERNAUT_IS_REWORK: ${{{{ inputs.is_rework }}}}
"#
    );
    test_utils::push_file(
        forgejo_port,
        &token,
        &org,
        "repo",
        ".forgejo/workflows/work.yml",
        &work_wf,
        "add work workflow",
    )
    .await;

    test_utils::set_repo_secret(
        forgejo_port,
        &token,
        &org,
        "repo",
        "CHUGGERNAUT_WORKER_TOKEN",
        &token,
    )
    .await;

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
    )
    .await;
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
        allowed_claude_flags: chuggernaut_types::default_allowed_claude_flags(),
        pause_on_overage: true,
        runner_label_map: std::collections::HashMap::new(),
        max_continuations: 3,
        ci_poll_timeout_secs: 120,
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
        claude_args: None,
    };
    let key = jobs::create_job(&state, req).await.unwrap();
    eprintln!("E2E-review: created job {key}");

    // Dispatcher auto-dispatches via try_assign_job (nats_worker_url = Docker-internal)
    chuggernaut_dispatcher::assignment::try_assign_job(&state, &key)
        .await
        .unwrap();
    assert_eq!(state.jobs.get(&key).unwrap().state, JobState::OnTheStack);
    eprintln!("E2E-review: first work action dispatched via dispatcher");

    // Wait for InReview
    wait_for_state(&state, &key, JobState::InReview, 90).await;
    let pr_url = state
        .jobs
        .get(&key)
        .unwrap()
        .pr_url
        .clone()
        .expect("should have PR URL");
    eprintln!("E2E-review: reached InReview, PR = {pr_url}");

    // --- Phase 2: Inject ChangesRequested review decision ---
    let decision = ReviewDecision {
        job_key: key.clone(),
        decision: DecisionType::ChangesRequested {
            feedback: "please add tests".to_string(),
        },
        pr_url: Some(pr_url.clone()),
        token_usage: Some(TokenUsage {
            input_tokens: 15000,
            output_tokens: 3000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }),
    };
    state
        .nats
        .publish_msg(&subjects::REVIEW_DECISION, &decision)
        .await
        .unwrap();
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
        token_usage: Some(TokenUsage {
            input_tokens: 12000,
            output_tokens: 2000,
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

    let final_state = state.jobs.get(&key).unwrap().state;
    eprintln!("E2E-review: final state = {final_state:?}");
    assert_eq!(
        final_state,
        JobState::Done,
        "job should be Done after approval"
    );

    // --- Verify token usage records ---
    let job = state.jobs.get(&key).unwrap().clone();
    eprintln!(
        "E2E-review: token_usage records = {}",
        job.token_usage.len()
    );
    for (i, record) in job.token_usage.iter().enumerate() {
        eprintln!(
            "  [{i}] type={} input={} output={}",
            record.action_type, record.token_usage.input_tokens, record.token_usage.output_tokens
        );
    }

    // We expect at least the 2 review records we injected (work outcomes from the real
    // action don't have token_usage since mock-claude doesn't report it)
    let review_records: Vec<_> = job
        .token_usage
        .iter()
        .filter(|r| r.action_type == "review")
        .collect();
    assert_eq!(review_records.len(), 2, "expected 2 review token records");
    assert_eq!(review_records[0].token_usage.input_tokens, 15000);
    assert_eq!(review_records[1].token_usage.input_tokens, 12000);

    eprintln!("E2E-review: PASSED — full work → review → rework → review → Done cycle");
}

// ---------------------------------------------------------------------------
// Rapid seeding — create handler must not block on action dispatch
// ---------------------------------------------------------------------------

/// Simulates the CLI seed command: rapidly creates 11 jobs (1 root + 10 with deps)
/// via NATS request-reply. All replies must arrive within 2 seconds total.
/// This catches regressions where the create handler blocks on Forgejo dispatch.
#[tokio::test]
async fn rapid_seed_does_not_block() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let nats = &state.nats;

    // Job 1: root (no deps) → OnDeck
    let req1 = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Root job".to_string(),
        body: "Scaffold".to_string(),
        depends_on: vec![],
        priority: 90,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: None,
        claude_args: None,
    };

    let start = tokio::time::Instant::now();

    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        nats.request_msg(&subjects::ADMIN_CREATE_JOB, &req1),
    )
    .await
    .expect("root job reply timed out")
    .unwrap();

    let resp: serde_json::Value = serde_json::from_slice(&reply.payload).unwrap();
    let root_key = resp["key"].as_str().unwrap().to_string();
    assert!(!root_key.is_empty(), "root job should return a key");

    // Extract root job's sequence number for deps
    let (_, _, root_seq) = parse_job_key(&root_key).unwrap();

    // Jobs 2-11: all depend on root → should be Blocked
    let mut dep_keys = Vec::new();
    for i in 2..=11 {
        let req = CreateJobRequest {
            repo: "test/repo".to_string(),
            title: format!("Dep job {i}"),
            body: "Depends on root".to_string(),
            depends_on: vec![root_seq],
            priority: 50,
            capabilities: vec![],
            platform: None,
            timeout_secs: 3600,
            review: ReviewLevel::High,
            max_retries: 3,
            initial_state: None,
            claude_args: None,
        };

        let reply = tokio::time::timeout(
            Duration::from_secs(2),
            nats.request_msg(&subjects::ADMIN_CREATE_JOB, &req),
        )
        .await
        .unwrap_or_else(|_| panic!("job {i} reply timed out"))
        .unwrap();

        let resp: serde_json::Value = serde_json::from_slice(&reply.payload).unwrap();
        let key = resp["key"].as_str().unwrap().to_string();
        assert!(!key.is_empty(), "job {i} should return a key");
        dep_keys.push(key);
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "all 11 jobs should be created in under 2 seconds, took {:?}",
        elapsed
    );

    // Verify states
    let root_state = state.jobs.get(&root_key).unwrap().state;
    assert_eq!(
        root_state,
        JobState::OnDeck,
        "root job (no deps) should be OnDeck"
    );

    for key in &dep_keys {
        let job_state = state.jobs.get(key).unwrap().state;
        assert_eq!(
            job_state,
            JobState::Blocked,
            "dep job {key} should be Blocked, not {job_state:?}"
        );
    }

    // Verify total count
    assert_eq!(state.jobs.len(), 11);
}

/// Same as above but with initial_state=OnIce for root — no dispatch at all.
#[tokio::test]
async fn rapid_seed_on_ice_no_dispatch() {
    let state = setup().await;
    handlers::start_handlers(state.clone()).await.unwrap();

    let nats = &state.nats;
    let start = tokio::time::Instant::now();

    // Root on-ice
    let req1 = CreateJobRequest {
        repo: "test/repo".to_string(),
        title: "Root on-ice".to_string(),
        body: String::new(),
        depends_on: vec![],
        priority: 90,
        capabilities: vec![],
        platform: None,
        timeout_secs: 3600,
        review: ReviewLevel::High,
        max_retries: 3,
        initial_state: Some(JobState::OnIce),
        claude_args: None,
    };

    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        nats.request_msg(&subjects::ADMIN_CREATE_JOB, &req1),
    )
    .await
    .expect("on-ice root timed out")
    .unwrap();

    let resp: serde_json::Value = serde_json::from_slice(&reply.payload).unwrap();
    let root_key = resp["key"].as_str().unwrap().to_string();
    let (_, _, root_seq) = parse_job_key(&root_key).unwrap();

    // 10 dep jobs
    for i in 2..=11 {
        let req = CreateJobRequest {
            repo: "test/repo".to_string(),
            title: format!("Dep job {i}"),
            body: String::new(),
            depends_on: vec![root_seq],
            priority: 50,
            capabilities: vec![],
            platform: None,
            timeout_secs: 3600,
            review: ReviewLevel::High,
            max_retries: 3,
            initial_state: None,
            claude_args: None,
        };

        let reply = tokio::time::timeout(
            Duration::from_secs(2),
            nats.request_msg(&subjects::ADMIN_CREATE_JOB, &req),
        )
        .await
        .unwrap_or_else(|_| panic!("job {i} timed out"))
        .unwrap();

        let resp: serde_json::Value = serde_json::from_slice(&reply.payload).unwrap();
        assert!(resp.get("key").is_some(), "job {i} should return a key");
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "on-ice seed of 11 jobs should be near-instant, took {:?}",
        elapsed
    );

    assert_eq!(state.jobs.get(&root_key).unwrap().state, JobState::OnIce);
    assert_eq!(state.jobs.len(), 11);
}

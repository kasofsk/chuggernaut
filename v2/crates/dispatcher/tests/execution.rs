//! Tier-2 execution tests: the actor core driving work + evaluation with
//! FakeBackend/FakeProvider over real NATS and real bare repos. Covers the
//! happy path (agent commits → eval passes → squash-merge → Done), work retry
//! exhaustion, and the eval-failure rework loop with §4.3 context injection.

use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateJobRequest, EvalSubmission, spawn};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{JobState, TaskPhase, TaskState};

const IMPL_CMD_EVAL: &str = r#"
name: impl-cmd
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: tests
    type: command
    run: ./ci.sh
"#;

const IMPL_AGENT_EVAL: &str = r#"
name: impl-agent
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
rework_budget: 1
eval:
  - name: reviewer
    type: agent
    prompt: prompts/eval.md
"#;

const FLAKY: &str = r#"
name: flaky
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
work_retries: 1
"#;

struct Rig {
    _server: test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
    handle: CoreHandle,
}

async fn rig() -> Option<Rig> {
    let server = test_utils::nats::NatsTestServer::spawn()?;
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in [
        ("jobs/impl-cmd.yaml", IMPL_CMD_EVAL),
        ("jobs/impl-agent.yaml", IMPL_AGENT_EVAL),
        ("jobs/flaky.yaml", FLAKY),
        ("prompts/impl.md", "implement it"),
        ("prompts/eval.md", "review it"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let backend = Arc::new(FakeBackend::new());
    let provider = Arc::new(FakeProvider::new());
    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend.clone(),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    Some(Rig { _server: server, store, repo, backend, provider, handle })
}

fn req(r#type: &str) -> CreateJobRequest {
    CreateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        inputs: HashMap::new(),
        knowledge_tags: vec![],
        factory: None,
    }
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
    let jobs = store.jobs().await.unwrap();
    for _ in 0..100 {
        if let Some(job) = jobs.get("acme", "api", seq).await.unwrap() {
            if job.state == want {
                return job;
            }
            assert!(
                !matches!(job.state, JobState::Escalated | JobState::Revoked) || want == job.state,
                "job reached terminal-ish {:?} while waiting for {want:?}",
                job.state
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {want:?}");
}

#[tokio::test]
async fn agent_work_commits_eval_passes_squash_merges_to_done() {
    let Some(rig) = rig().await else { return };

    // The "agent" commits a file to its job branch, like a real container.
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("src/new.rs", b"pub fn f() {}", "implement").await;
        clone.push(&branch).await;
    });
    // Command evaluator: exit 0 with structured findings.
    rig.backend.put_file("/workspace/eval-result.json", br#"{"coverage": 91}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // The squash-merge landed the agent's commit on main.
    let merged = rig
        .repo
        .manager
        .read_file_at("acme", "api", "main", "src/new.rs")
        .await
        .unwrap();
    assert_eq!(merged.as_deref(), Some("pub fn f() {}"));

    // Task log: work Done, eval Done with structured result.
    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].phase, TaskPhase::Work);
    assert_eq!(tasks[0].state, TaskState::Done);
    assert_eq!(tasks[1].phase, TaskPhase::Evaluation);
    match &tasks[1].result {
        Some(types::TaskResult::Command { pass: true, structured: Some(s), .. }) => {
            assert_eq!(s["coverage"], 91);
        }
        other => panic!("unexpected eval result: {other:?}"),
    }

    let events = event_types(&rig.store).await;
    for e in ["job-started", "job-evaluation-started", "job-done"] {
        assert!(events.contains(&e.to_string()), "missing {e}: {events:?}");
    }
}

#[tokio::test]
async fn work_failure_retries_with_reset_then_escalates() {
    let Some(rig) = rig().await else { return };
    rig.provider.script_exits([2, 3]); // work_retries: 1 → both attempts fail

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    // attempt 1 Failed, attempt 2 Failed, Human escalation task Pending
    assert_eq!(tasks.len(), 3);
    assert_eq!((tasks[0].attempt, tasks[0].state), (1, TaskState::Failed));
    assert_eq!((tasks[1].attempt, tasks[1].state), (2, TaskState::Failed));
    assert!(matches!(tasks[2].kind, types::TaskKind::Human { .. }));
    assert_eq!(tasks[2].state, TaskState::Pending);
    assert_eq!(rig.provider.runs().len(), 2);
}

#[tokio::test]
async fn eval_failure_reworks_with_context_then_passes() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    // Run order: work c1, eval c1 (fail w/ findings), work c2, eval c2 (pass).
    // Task ids are sequential per job: 1=work, 2=eval, 3=work, 4=eval.
    rig.provider.on_run(|_| async {}); // work cycle 1
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval("acme", "api", 1, 2, EvalSubmission {
            pass: false,
            structured: Some(serde_json::json!({"issues": ["missing tests"]})),
            token_usage: None,
        })
        .await
        .unwrap();
    });
    rig.provider.on_run(|_| async {}); // work cycle 2
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval("acme", "api", 1, 4, EvalSubmission {
            pass: true,
            structured: None,
            token_usage: None,
        })
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // Cycle 2's work run received the cycle-1 findings (§4.3).
    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 4);
    assert!(runs[0].eval_context.is_empty());
    let rework = &runs[2];
    assert_eq!(rework.eval_context.len(), 1);
    assert!(!rework.eval_context[0].pass);
    assert!(rework.prompt.contains("Rework Context"));
    assert!(rework.prompt.contains("missing tests"));

    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    assert_eq!(tasks.len(), 4);
    assert_eq!(tasks[2].cycle, 2);
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-rework-started".to_string()));
}

async fn event_types(store: &NatsStore) -> Vec<String> {
    store
        .read_stream("job-events", 100)
        .await
        .unwrap()
        .iter()
        .map(|p| {
            let v: serde_json::Value = serde_json::from_slice(p).unwrap();
            v["event_type"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

const IMPL_SECRET: &str = r#"
name: impl-secret
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
secrets: [DEPLOY_KEY]
"#;

/// Full launch wiring (§4.2/§8.2): the channel MCP binary is injected with
/// its config entry, and declared secrets arrive age-decrypted in the env.
#[tokio::test]
async fn agent_launch_carries_channel_mcp_and_decrypted_secrets() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else { return };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone.commit_file("jobs/impl-secret.yaml", IMPL_SECRET.as_bytes(), "t").await;
    clone.commit_file("prompts/impl.md", b"implement", "p").await;
    clone.push("main").await;

    // Encrypted write with the public key (the API layer's path)…
    let (identity, public_key) = store::secrets::generate_age_keypair();
    let secrets_bucket = store.raw_bucket(store::buckets::SECRETS).await.unwrap();
    {
        use store::secrets::SecretStore;
        let api_side = store::secrets::AgeSecretStore::for_api(secrets_bucket, &public_key).unwrap();
        api_side.set("acme", "api", "DEPLOY_KEY", "s3cret-value").await.unwrap();
    }

    let fake_binary = repo.bare_path().parent().unwrap().join("chuggernaut-channel");
    tokio::fs::write(&fake_binary, b"#!/bin/sh\nexit 0\n").await.unwrap();

    let provider = Arc::new(FakeProvider::new());
    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            channel_binary: Some(fake_binary),
            age_identity: Some(identity),
            nats_account_seed: Some(nkeys::KeyPair::new_account().seed().unwrap()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);

    let job = handle.create_job(req("impl-secret")).await.unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&store, job.id, JobState::Done).await;

    let runs = provider.runs();
    assert_eq!(runs.len(), 1);
    // …decrypted read at launch (the dispatcher's path).
    assert_eq!(runs[0].env.get("DEPLOY_KEY").map(String::as_str), Some("s3cret-value"));
    assert_eq!(runs[0].mcp_servers.len(), 1);
    assert_eq!(runs[0].mcp_servers[0].command, "/usr/local/bin/chuggernaut-channel");
    assert!(runs[0].mcp_servers[0].env.contains_key("NATS_URL"));
    // §7.4: per-launch scoped credentials, forwarded to the channel binary.
    let creds = runs[0].env.get("NATS_CREDS").expect("NATS_CREDS in container env");
    assert!(creds.contains("BEGIN NATS USER JWT"));
    assert_eq!(runs[0].mcp_servers[0].env.get("NATS_CREDS"), Some(creds));
    assert_eq!(runs[0].env.get("CHANNEL_ROLE").map(String::as_str), Some("work"));
    assert_eq!(runs[0].files.len(), 1);
    assert_eq!(runs[0].files[0].container_path, "/usr/local/bin/chuggernaut-channel");
    assert_eq!(runs[0].files[0].mode, 0o755);
}

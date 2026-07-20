//! Tier-2 execution tests: the actor core driving work + evaluation with
//! FakeBackend/FakeProvider over real NATS and real bare repos. Covers the
//! happy path (agent commits → eval passes → squash-merge → Done), work retry
//! exhaustion, and the eval-failure rework loop with §4.3 context injection.

use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateJobRequest, EvalSubmission, spawn};
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
knowledge: [rust]
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
    rig_with_artifacts(None).await
}

/// `artifacts_identity` enables transcript/log capture; the provider then
/// launches through the backend so runs report a container id to harvest from.
async fn rig_with_artifacts(artifacts_identity: Option<String>) -> Option<Rig> {
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
        ("tags/rust.md", "# rust\nrust conventions here"),
        ("tags/style.md", "# style\nhouse style here"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let backend = Arc::new(FakeBackend::new());
    let provider = Arc::new(if artifacts_identity.is_some() {
        FakeProvider::with_backend(backend.clone())
    } else {
        FakeProvider::new()
    });
    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend.clone(),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            artifacts_identity,
            // Enables the operator-dispatched triage action (§1.2).
            triage_image: Some("triage:latest".into()),
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
        title: String::new(),
        description: String::new(),
        deps: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
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
                !matches!(
                    job.state,
                    JobState::Escalated | JobState::Stalled | JobState::Revoked
                ) || want == job.state,
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
            abort: false,
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
            abort: false,
            structured: None,
            token_usage: None,
        })
        .await
        .unwrap();
    });

    let mut create = req("impl-agent");
    create.title = "Add fortune file".into();
    create.description = "Create fortune.txt with an aphorism.".into();
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // Cycle 2's work run received the cycle-1 findings (§4.3).
    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 4);
    assert!(runs[0].eval_context.is_empty());
    // The §4.3 job brief reaches the work agent AND the agent evaluator.
    for run in [&runs[0], &runs[1]] {
        assert!(run.prompt.contains("Job Brief"), "{}", run.prompt);
        assert!(run.prompt.contains("Add fortune file"), "{}", run.prompt);
        assert!(run.prompt.contains("aphorism"), "{}", run.prompt);
    }
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
        // Platform agent credential (reserved global/agents scope): reaches
        // every agent container without any declaration in the job type.
        api_side.set("global", "agents", "PROVIDER_TOKEN", "tok-123").await.unwrap();
    }

    let fake_binary = repo.bare_path().parent().unwrap().join("chuggernaut-channel");
    tokio::fs::write(&fake_binary, b"#!/bin/sh\nexit 0\n").await.unwrap();

    // SSH front enabled: generate a CA so launches carry a job cert (§5.2).
    let ssh_ca = repo.bare_path().parent().unwrap().join("ssh_ca");
    let status = tokio::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f", ssh_ca.to_str().unwrap()])
        .status()
        .await
        .unwrap();
    assert!(status.success());

    let provider = Arc::new(FakeProvider::new());
    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "ssh://git@forge.example".into(),
            nats_url: server.url().into(),
            channel_binary: Some(fake_binary),
            age_identity: Some(identity),
            nats_account_seed: Some(nkeys::KeyPair::new_account().seed().unwrap()),
            ssh_ca: Some(ssh_ca),
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
    // Platform agent credential injected without being declared anywhere.
    assert_eq!(runs[0].env.get("PROVIDER_TOKEN").map(String::as_str), Some("tok-123"));
    assert_eq!(runs[0].mcp_servers.len(), 1);
    assert_eq!(runs[0].mcp_servers[0].command, "/usr/local/bin/chuggernaut-channel");
    assert!(runs[0].mcp_servers[0].env.contains_key("NATS_URL"));
    // §7.4: per-launch scoped credentials, forwarded to the channel binary.
    let creds = runs[0].env.get("NATS_CREDS").expect("NATS_CREDS in container env");
    assert!(creds.contains("BEGIN NATS USER JWT"));
    assert_eq!(runs[0].mcp_servers[0].env.get("NATS_CREDS"), Some(creds));
    assert_eq!(runs[0].env.get("CHANNEL_ROLE").map(String::as_str), Some("work"));
    // Channel binary + §5.2 job SSH credential (key 0600 + cert).
    let paths: Vec<&str> = runs[0].files.iter().map(|f| f.container_path.as_str()).collect();
    assert_eq!(
        paths,
        ["/usr/local/bin/chuggernaut-channel", "/chuggernaut/ssh/id", "/chuggernaut/ssh/id-cert.pub"]
    );
    assert_eq!(runs[0].files[0].mode, 0o755);
    assert_eq!(runs[0].files[1].mode, 0o600);
    let cert = String::from_utf8(runs[0].files[2].contents.clone()).unwrap();
    assert!(cert.starts_with("ssh-ed25519-cert-v01@openssh.com"));
    assert!(
        runs[0].env.get("GIT_SSH_COMMAND").unwrap().contains("/chuggernaut/ssh/id"),
        "GIT_SSH_COMMAND must reference the injected key"
    );
}

/// The artifacts a job leaves behind. Before this, an agent's session transcript
/// died with its container: the provider dropped the container id, so nothing
/// could name the file even though the container itself was never removed.
#[tokio::test]
async fn agent_run_captures_transcript_logs_and_measured_usage() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity.clone())).await else { return };

    // stdout as the real CLI emits it under `--output-format json`: the result
    // object carries the authoritative usage.
    rig.backend.put_logs(
        br#"Cloning into '/workspace'...
{"type":"result","subtype":"success","is_error":false,"session_id":"s","total_cost_usd":0.01,"usage":{"input_tokens":1200,"cache_creation_input_tokens":300,"cache_read_input_tokens":400,"output_tokens":56}}"#
            .to_vec(),
    );

    let bare = rig.repo.bare_path();
    let backend = rig.backend.clone();
    rig.provider.on_run(move |cfg| async move {
        // The CLI writes its transcript keyed by the session id the dispatcher
        // chose; put it exactly where `--session-id` + CLAUDE_CONFIG_DIR say.
        backend.put_file(
            &agent::transcript_path(&cfg.session_id),
            br#"{"type":"user","message":"do it"}"#.to_vec(),
        );
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("src/new.rs", b"pub fn f() {}", "implement").await;
        clone.push(&branch).await;
    });
    rig.backend.put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig.store.tasks().await.unwrap();
    let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
    let work = log.iter().find(|t| t.phase == TaskPhase::Work).unwrap();

    // The session id is persisted, so the transcript stays addressable after a
    // restart rather than being lost with the process.
    let session_id = work.session_id.clone().expect("work task records a session id");

    let artifacts = rig
        .store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();

    let transcript = artifacts
        .get("acme", "api", job.id, work.id, store::ArtifactKind::SessionTranscript)
        .await
        .unwrap()
        .expect("transcript captured");
    assert_eq!(transcript, br#"{"type":"user","message":"do it"}"#);

    let stdout = artifacts
        .get("acme", "api", job.id, work.id, store::ArtifactKind::Stdout)
        .await
        .unwrap()
        .expect("stdout captured");
    assert!(String::from_utf8_lossy(&stdout).contains("Cloning into"));

    // Usage is measured from the CLI's own result object, not self-reported —
    // the agent here never called submit_result with a token_usage.
    match work.result.as_ref().expect("work result") {
        types::TaskResult::Work { token_usage, .. } => {
            let u = token_usage.as_ref().expect("measured usage");
            assert_eq!(u.input_tokens, 1200);
            assert_eq!(u.output_tokens, 56);
            assert_eq!(u.cache_write_tokens, Some(300));
            assert_eq!(u.cache_read_tokens, Some(400));
        }
        other => panic!("unexpected work result: {other:?}"),
    }

    // The command evaluator's container logs are captured too — TaskResult
    // carries no output, so this is the only record of what it printed.
    let eval = log.iter().find(|t| t.phase == TaskPhase::Evaluation).unwrap();
    assert!(
        artifacts
            .get("acme", "api", job.id, eval.id, store::ArtifactKind::Stdout)
            .await
            .unwrap()
            .is_some(),
        "eval container logs captured"
    );
    // Command evals run no agent, so they have no session.
    assert!(eval.session_id.is_none());
    assert!(!session_id.is_empty());
}

// ── Lifecycle generalization (design-lifecycle.md) ───────────────────────

const DEPLOY_NONE: &str = r#"
name: deploy-none
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
wrap_up:
  type: none
eval:
  - name: smoke
    type: command
    run: ./smoke.sh
"#;

/// `finalize: none`: eval-pass goes straight to Done — no squash-merge, the
/// job branch is scratch and is deleted unmerged.
#[tokio::test]
async fn finalize_none_completes_without_merging() {
    let Some(rig) = rig().await else { return };
    let clone = rig.repo.clone_branch("main").await;
    clone.commit_file("jobs/deploy-none.yaml", DEPLOY_NONE.as_bytes(), "type").await;
    clone.push("main").await;

    // The "agent" commits scratch to its branch, like a deploy that jots notes.
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("deploy.log", b"deployed v1", "scratch").await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("deploy-none")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // The evaluator still ran; nothing landed on main; the branch is gone.
    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[1].phase, TaskPhase::Evaluation);
    assert_eq!(tasks[1].state, TaskState::Done);
    let scratch = rig
        .repo
        .manager
        .read_file_at("acme", "api", "main", "deploy.log")
        .await
        .unwrap();
    assert_eq!(scratch, None, "scratch branch content must not merge");
    assert!(
        rig.repo.manager.resolve_ref("acme", "api", &job.branch).await.is_err(),
        "job branch deleted at Done"
    );
}

/// Abort verdict: a required evaluator declaring the work unsalvageable
/// escalates immediately — the remaining rework budget is not consumed.
#[tokio::test]
async fn eval_abort_escalates_without_consuming_rework_budget() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    rig.provider.on_run(|_| async {}); // work cycle 1
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval("acme", "api", 1, 2, EvalSubmission {
            pass: false,
            abort: true,
            structured: Some(serde_json::json!({"reason": "endpoint spec references a retired API"})),
            token_usage: None,
        })
        .await
        .unwrap();
    });

    // impl-agent has rework_budget: 1 — abort must not spend it.
    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    assert_eq!(rig.provider.runs().len(), 2, "no cycle-2 work after abort");
    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    assert_eq!(tasks.len(), 3);
    match &tasks[1].result {
        Some(types::TaskResult::Agent { pass: false, abort: true, .. }) => {}
        other => panic!("unexpected eval result: {other:?}"),
    }
    match &tasks[2].kind {
        types::TaskKind::Human { prompt } => {
            assert!(prompt.contains("not satisfiable by rework"), "{prompt}");
            assert!(prompt.contains("retired API"), "findings forwarded: {prompt}");
        }
        other => panic!("expected escalation task, got {other:?}"),
    }
}

/// Additive per-job evaluators: layered on top of the type's list and executed
/// like declared ones.
#[tokio::test]
async fn job_level_evaluators_run_alongside_type_evaluators() {
    let Some(rig) = rig().await else { return };

    let mut r = req("flaky"); // type declares no evaluators
    r.eval = vec![types::Evaluator {
        name: "extra-ci".into(),
        r#type: types::EvaluatorType::Command,
        image: None, // falls back to the type's top-level image
        run: Some("./ci.sh".into()),
        prompt: None,
        provider: None,
        model: None,
        secrets: vec![],
        required: None,
    }];
    let job = rig.handle.create_job(r).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    let eval = tasks.iter().find(|t| t.phase == TaskPhase::Evaluation).expect("eval task");
    assert_eq!(eval.evaluator.as_deref(), Some("extra-ci"));
    assert_eq!(eval.state, TaskState::Done);
}

/// §4.4 upfront knowledge injection: the union of the type's `knowledge:`
/// defaults and the job's tags rides the work agent's system prompt, read
/// from tags/{tag}.md at base_ref. Unknown tags are skipped.
#[tokio::test]
async fn knowledge_tags_inject_into_work_system_prompt() {
    let Some(rig) = rig().await else { return };

    let mut create = req("flaky"); // type declares knowledge: [rust]
    create.knowledge_tags = vec!["style".into(), "no-such-tag".into()];
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    let system = runs[0].system_prompt.as_deref().expect("knowledge block");
    assert!(system.contains("Project Knowledge"), "{system}");
    assert!(system.contains("rust conventions here"), "type default: {system}");
    assert!(system.contains("house style here"), "job tag: {system}");
    assert!(!system.contains("no-such-tag"), "missing tags are skipped: {system}");
}

/// The type's evaluators are a floor: a job evaluator colliding with a
/// declared name is a release-time validation error.
#[tokio::test]
async fn job_evaluator_name_collision_fails_release() {
    let Some(rig) = rig().await else { return };

    let mut r = req("impl-cmd"); // type declares evaluator "tests"
    r.eval = vec![types::Evaluator {
        name: "tests".into(),
        r#type: types::EvaluatorType::Command,
        image: None,
        run: Some("./sneaky.sh".into()),
        prompt: None,
        provider: None,
        model: None,
        secrets: vec![],
        required: None,
    }];
    let job = rig.handle.create_job(r).await.unwrap(); // creation always lands Frozen
    let err = rig.handle.release_job("acme", "api", job.id).await.unwrap_err();
    assert!(err.to_string().contains("collides"), "{err}");
}

/// Unexpected wrap-up failure → triage, and the merge queue moves on instead
/// of wedging (design-lifecycle.md). Simulated by deleting the job branch
/// out from under finalization.
#[tokio::test]
async fn finalize_hard_failure_escalates_instead_of_wedging() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    let bare = rig.repo.bare_path();

    rig.provider.on_run(|_| async {}); // work cycle 1
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        // Sabotage wrap-up: the branch vanishes before the squash-merge.
        let out = tokio::process::Command::new("git")
            .args(["-C", bare.to_str().unwrap(), "update-ref", "-d", "refs/heads/job/1"])
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        h.submit_eval("acme", "api", 1, 2, EvalSubmission {
            pass: true,
            abort: false,
            structured: None,
            token_usage: None,
        })
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    match &tasks.last().unwrap().kind {
        types::TaskKind::Human { prompt } => {
            assert!(prompt.contains("wrap-up failed unexpectedly"), "{prompt}");
        }
        other => panic!("expected escalation task, got {other:?}"),
    }
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-escalated".to_string()));
}

// ── Issue #31: per-job timeout override + operator-dispatched triage ────────

/// The per-job `Job.timeout` override (§1.1) drives the Work agent's own run
/// timeout, while the evaluator keeps the type default — the override is
/// Work-scoped. Asserted at the mechanism: the recorded run configs.
#[tokio::test]
async fn work_timeout_override_applies_to_work_not_eval() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    rig.provider.on_run(|_| async {}); // work c1
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval("acme", "api", 1, 2, EvalSubmission {
            pass: true, abort: false, structured: None, token_usage: None,
        })
        .await
        .unwrap();
    });

    let mut create = req("impl-agent");
    create.timeout = Some("45m".into());
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 2);
    // Work: the 45m override. Eval: the type default (no `resources` → 1h).
    assert_eq!(runs[0].task_timeout, Duration::from_secs(45 * 60));
    assert_eq!(runs[1].task_timeout, Duration::from_secs(3600));
}

/// A Work task that outlives the per-job override is killed by the §3.5 timeout
/// scan — the override applies at kill time, escalating the job.
#[tokio::test]
async fn work_timeout_override_times_out_running_work_task() {
    let Some(rig) = rig().await else { return };
    // The work "container" never exits on its own — the scan must end it.
    rig.provider.on_run(|_| async { tokio::time::sleep(Duration::from_secs(30)).await });

    let mut create = req("impl-agent"); // no work_retries → escalates on first fail
    create.timeout = Some("1s".into());
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    // Age the Running work task past the 1s override, then scan.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    let work = tasks.iter().find(|t| t.phase == TaskPhase::Work).unwrap();
    assert_eq!(work.state, TaskState::Failed, "the override should have timed out the work task");
}

/// The evaluator keeps the type default even when the job carries a short work
/// override: a scan after the (short) override elapses must not touch the
/// Evaluation-phase task — the override does not leak past Work.
#[tokio::test]
async fn eval_task_ignores_work_timeout_override() {
    let Some(rig) = rig().await else { return };
    rig.provider.on_run(|_| async {}); // work c1: exits immediately
    // Eval "container" blocks so it is Running when the scan fires.
    rig.provider.on_run(|_| async { tokio::time::sleep(Duration::from_secs(30)).await });

    let mut create = req("impl-agent");
    create.timeout = Some("1s".into());
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;

    // Age well past the 1s work override; the eval task's 1h default protects it.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();

    let job_now = rig.store.jobs().await.unwrap().get("acme", "api", job.id).await.unwrap().unwrap();
    assert_eq!(job_now.state, JobState::Evaluation, "eval must survive the work override");
    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    let eval = tasks.iter().find(|t| t.phase == TaskPhase::Evaluation).unwrap();
    assert_eq!(eval.state, TaskState::Running, "the work override must not time out the eval task");
}

/// A malformed `Job.timeout` is rejected at release (§1.1: parseability
/// validated at release, not creation) — creation still succeeds.
#[tokio::test]
async fn malformed_timeout_override_rejected_at_release() {
    let Some(rig) = rig().await else { return };
    let mut create = req("impl-agent");
    create.timeout = Some("2 hours".into()); // not a valid duration string
    let job = rig.handle.create_job(create).await.unwrap(); // creation is permissive
    let err = rig.handle.release_job("acme", "api", job.id).await.unwrap_err();
    match err {
        dispatcher::core::CoreError::Validation(errs) => {
            assert!(errs.iter().any(|e| e.field == "timeout"), "{errs:?}");
        }
        other => panic!("expected a validation error on timeout, got {other:?}"),
    }
}

/// Operator-dispatched triage (§1.2) over an Escalated job: creates a Triage
/// agent task, captures the assessment from the CLI's JSON result (no channel
/// MCP), and leaves the job Escalated — purely advisory.
#[tokio::test]
async fn triage_on_escalated_job_records_assessment_and_leaves_escalated() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity)).await else { return };

    // Drive the job to Escalated: both work attempts fail (flaky: work_retries 1).
    rig.provider.script_exits([2, 3]);
    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    // The triage agent's assessment rides the CLI's JSON `result` on stdout.
    rig.backend.put_logs(
        br#"{"type":"result","subtype":"success","is_error":false,"result":"Root cause: the work container exited non-zero on both attempts. Recommend Revoke.","session_id":"t","usage":{"input_tokens":10,"output_tokens":20}}"#
            .to_vec(),
    );

    rig.handle.triage_job("acme", "api", job.id).await.unwrap();

    // Poll for the Triage task to land with a recorded assessment.
    let tasks = rig.store.tasks().await.unwrap();
    let mut triage = None;
    for _ in 0..100 {
        let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
        if let Some(t) = log.iter().find(|t| t.phase == TaskPhase::Triage && t.result.is_some()) {
            triage = Some(t.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let triage = triage.expect("triage task recorded a result");
    assert_eq!(triage.state, TaskState::Done);
    match triage.result.as_ref().unwrap() {
        types::TaskResult::Triage { assessment, .. } => {
            assert!(assessment.contains("Recommend Revoke"), "{assessment}");
        }
        other => panic!("expected a Triage result, got {other:?}"),
    }

    // Advisory: the job state is untouched.
    let job_now = rig.store.jobs().await.unwrap().get("acme", "api", job.id).await.unwrap().unwrap();
    assert_eq!(job_now.state, JobState::Escalated);

    // The run used the platform triage image and carried no channel MCP.
    let last = rig.provider.runs().pop().unwrap();
    assert_eq!(last.image, "triage:latest");
    assert!(last.mcp_servers.is_empty(), "triage runs without the channel MCP");
}

/// Triage is rejected unless the job is Escalated or Stalled (§1.2).
#[tokio::test]
async fn triage_rejected_on_non_intervention_state() {
    let Some(rig) = rig().await else { return };
    // A freshly created job is Frozen.
    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    let err = rig.handle.triage_job("acme", "api", job.id).await.unwrap_err();
    assert!(matches!(err, dispatcher::core::CoreError::Conflict(_)), "{err:?}");
}

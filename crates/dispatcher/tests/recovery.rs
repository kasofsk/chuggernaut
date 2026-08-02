//! Tier-2 tests for restart reconciliation (§3.6) and the timeout/deadline
//! scans (§3.5). Crash states are constructed directly in KV — the task log
//! is the source of truth the dispatcher recovers from.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateSpec, EvalSubmission};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{
    EscalationAction, Job, JobState, Task, TaskKind, TaskPhase, TaskResolution, TaskResult,
    TaskState,
};

mod common;
use common::{assert_invariants_of, spawn_checked};

const FLAKY: &str = r#"
name: flaky
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
work_retries: 1
"#;

const SLOW: &str = r#"
name: slow
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
resources:
  task_timeout: 1s
"#;

const CMD_WORK: &str = r#"
name: cmd-work
image: img:latest
work:
  type: command
  run: ./build.sh
work_retries: 1
"#;

const AGENT_EVAL: &str = r#"
name: agent-eval
image: img:latest
work:
  type: command
  run: ./build.sh
eval:
  - name: reviewer
    type: agent
    prompt: prompts/eval.md
"#;

const MANUAL_DEADLINE: &str = r#"
name: manual-deadline
work:
  type: human
  prompt: prompts/manual.md
job_deadline: 1s
"#;

const CMD_WORK_SLOW: &str = r#"
name: cmd-work-slow
image: img:latest
work:
  type: command
  run: ./deploy.sh
resources:
  task_timeout: 1s
"#;

const REWORK_AGENT: &str = r#"
name: rework-agent
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

struct Rig {
    _server: &'static test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
    provider: Arc<FakeProvider>,
    handle: CoreHandle,
    /// Invariant violations the actor logged, drained by
    /// `assert_invariants_of` (refactor-plan B1a).
    invariants: InvariantSink,
}

async fn rig() -> Option<Rig> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in [
        ("jobs/flaky.yaml", FLAKY),
        ("jobs/slow.yaml", SLOW),
        ("jobs/manual-deadline.yaml", MANUAL_DEADLINE),
        ("prompts/impl.md", "implement it"),
        ("prompts/manual.md", "do it by hand"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let provider = Arc::new(FakeProvider::new());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, invariants) = spawn_checked(core);
    Some(Rig {
        _server: server,
        store,
        repo,
        provider,
        handle,
        invariants,
    })
}

fn req(r#type: &str) -> CreateSpec {
    CreateSpec {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        require_approval: false,
        timeout: None,
        model: None,
        factory: None,
        schedule: None,
        members: vec![],
        inputs: Default::default(),
        groups: vec![],
        draft: false,
    }
}

/// Registers a work run that commits a stub to the job branch, so a relaunched
/// agent attempt produces output and clears the §3.2 empty-output guard.
fn commit_on_run(provider: &FakeProvider, bare: std::path::PathBuf) {
    provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        let body = format!("// work produced on {branch}\n");
        clone
            .commit_file("src/work.rs", body.as_bytes(), "work")
            .await;
        clone.push(&branch).await;
    });
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> Job {
    test_utils::wait::job_state(store, "acme", "api", seq, want).await
}

/// A dispatcher died mid-Work: the job record says Work, the task log has a
/// Running task, and no container exists. Reconciliation treats it as a
/// failure, retries per `work_retries`, and the job completes.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_recovers_orphaned_running_work_task() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/impl.md".into(),
            },
            state: TaskState::Running,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    let provider = Arc::new(FakeProvider::new());
    commit_on_run(&provider, repo.bare_path());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert_eq!((tasks[1].attempt, tasks[1].state), (2, TaskState::Done));
    assert_eq!(provider.runs().len(), 1);
}

/// Job #155: the re-review context block is rebuilt from persisted records, not
/// in-memory state. We construct the crash state a dispatcher would leave after
/// cycle 1 (work + a FAILING reviewer that recorded its `reviewed_tip`) and a
/// completed cycle-2 work task, then a fresh core reconciles: recovering the
/// Done cycle-2 work re-enters Evaluation and launches the cycle-2 reviewer,
/// whose prompt must still carry the prior findings, the last-reviewed SHA, and
/// the delta — proving nothing depended on the crashed dispatcher's memory.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_rebuilds_re_review_context_from_persisted_records() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/rework-agent.yaml", REWORK_AGENT.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone
        .commit_file("prompts/eval.md", b"review it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    let c1 = clone_branch_from(&repo.bare_path(), "job/1").await;
    c1.commit_file("src/a.rs", b"pub fn a() {}", "cycle 1")
        .await;
    c1.push("job/1").await;
    let tip1 = repo
        .manager
        .resolve_ref("acme", "api", "job/1")
        .await
        .unwrap();
    let c2 = clone_branch_from(&repo.bare_path(), "job/1").await;
    c2.commit_file("src/b.rs", b"pub fn b() {}", "cycle 2")
        .await;
    c2.push("job/1").await;
    let tip2 = repo
        .manager
        .resolve_ref("acme", "api", "job/1")
        .await
        .unwrap();

    let jobs = store.jobs().await.unwrap();
    jobs.put(&Job {
        id: 1,
        project: "acme/api".into(),
        r#type: "rework-agent".into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        members: vec![],
        batch_id: None,
        state: JobState::Work,
        branch: "job/1".into(),
        base_ref: Some(head.clone()),
        knowledge_tags: vec![],
        eval: vec![],
        require_approval: false,
        timeout: None,
        model: None,
        claim_next: false,
        escalation: None,
        factory: None,
        schedule: None,
        created_at: Utc::now(),
        ready_at: Some(Utc::now()),
        completed_at: None,
        inputs: Default::default(),
        groups: vec![],
        task_time_ms: None,
    })
    .await
    .unwrap();

    let tasks = store.tasks().await.unwrap();
    let base_task = |id: u64, cycle: u32| Task {
        id,
        job_seq: 1,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle,
        kind: TaskKind::Agent {
            provider: "claude".into(),
            model: None,
            prompt: "prompts/impl.md".into(),
        },
        state: TaskState::Done,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: None,
        rework_reason: None,
        infra_loss: false,
        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        completed_at: Some(Utc::now()),
    };
    tasks
        .put(&Task {
            result: Some(TaskResult::Work {
                summary: Some("cycle 1 work".into()),
                structured: None,
                token_usage: None,
                cover_html: None,
            }),
            ..base_task(1, 1)
        })
        .await
        .unwrap();
    tasks
        .put(&Task {
            phase: TaskPhase::Evaluation,
            evaluator: Some("reviewer".into()),
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/eval.md".into(),
            },
            reviewed_tip: Some(tip1.clone()),
            result: Some(TaskResult::Agent {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["needs docstrings"]})),
                token_usage: None,
                cover_html: None,
            }),
            ..base_task(2, 1)
        })
        .await
        .unwrap();
    tasks
        .put(&Task {
            cycle: 2,
            rework_reason: Some(types::ReworkReason::EvalFailure),
            result: Some(TaskResult::Work {
                summary: Some("cycle 2 work".into()),
                structured: None,
                token_usage: None,
                cover_html: None,
            }),
            ..base_task(3, 2)
        })
        .await
        .unwrap();

    let provider = Arc::new(FakeProvider::new());
    provider.on_run(|_cfg| async {});
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    let prompt = test_utils::wait::poll_default("cycle-2 reviewer to launch after restart", || {
        provider.runs().first().map(|r| r.prompt.clone())
    })
    .await;
    assert_invariants_of(&sink);

    assert!(
        prompt.contains("Re-Review Context"),
        "re-review context missing after restart: {prompt}"
    );
    assert!(
        prompt.contains("needs docstrings"),
        "prior findings not rebuilt from records: {prompt}"
    );
    assert!(
        prompt.contains(&tip1) && prompt.contains("Last-reviewed tip"),
        "last-reviewed SHA not rebuilt from records: {prompt}"
    );
    assert!(
        prompt.contains("```diff") && prompt.contains("src/b.rs"),
        "delta diff (tip1..tip2) not rebuilt from records: {prompt}"
    );
    assert_ne!(tip1, tip2);
}

/// §3.6 infra-loss accounting: a dispatcher died mid-Work and, by the time it
/// restarted, the work container was GONE (docker pruned it, the node rebooted,
/// colima restarted). That is an infrastructure loss, NOT a real failure —
/// reconciliation relaunches the attempt WITHOUT spending a `work_retries`
/// budget, and stamps the retired task/event with the infra reason. The
/// distinguishing fact vs `restart_recovers_orphaned_running_work_task` is that
/// a container id WAS recorded, so the container demonstrably existed.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_infra_loss_relaunches_work_without_burning_budget() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/impl.md".into(),
            },
            state: TaskState::Running,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: Some("pruned-work-container".into()),
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    let provider = Arc::new(FakeProvider::new());
    commit_on_run(&provider, repo.bare_path());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert!(tasks[0].infra_loss, "orphan carries the infra-loss marker");
    assert_eq!(
        (tasks[1].attempt, tasks[1].state),
        (1, TaskState::Done),
        "infra relaunch keeps the same attempt: {tasks:?}"
    );
    assert!(!tasks[1].infra_loss);
    assert_eq!(provider.runs().len(), 1);
}

/// §3.6 infra-loss cap: an environment that keeps eating the container (a node
/// stuck in a reboot loop) must not relaunch forever. After
/// `INFRA_RELAUNCH_CAP` losses the job escalates with reason `infra_loss`
/// rather than a `work_retries`-exhausted failure. The prior losses are seeded
/// directly (three restarts compressed into one crash state).
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_repeated_infra_loss_escalates_with_infra_loss() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    let tasks = store.tasks().await.unwrap();
    let mk = |id: u64, state: TaskState, infra_loss: bool, container: Option<&str>| Task {
        id,
        job_seq: 1,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Agent {
            provider: "claude".into(),
            model: None,
            prompt: "prompts/impl.md".into(),
        },
        state,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: container.map(Into::into),
        rework_reason: None,
        infra_loss,

        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        completed_at: None,
    };
    for id in 1..=3 {
        tasks
            .put(&mk(id, TaskState::Failed, true, None))
            .await
            .unwrap();
    }
    tasks
        .put(&mk(4, TaskState::Running, false, Some("still-vanishing")))
        .await
        .unwrap();

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    let job = wait_for_state(&store, 1, JobState::Escalated).await;
    assert_invariants_of(&sink);
    assert_eq!(
        job.escalation.as_ref().map(|e| e.reason.as_str()),
        Some("infra_loss"),
        "the cap escalates with reason=infra_loss, not work_retries_exhausted"
    );
    let log = tasks.list_for_job("acme", "api", 1).await.unwrap();
    let work: Vec<&Task> = log.iter().filter(|t| t.phase == TaskPhase::Work).collect();
    assert!(
        work.iter().all(|t| t.attempt == 1),
        "no infra relaunch ever spent a work_retries budget: {work:?}"
    );
    assert_eq!(
        work.iter().filter(|t| t.infra_loss).count(),
        4,
        "four infra losses recorded before escalation: {work:?}"
    );
}

/// Regression guard: a REAL nonzero exit found at reconcile still burns the
/// `work_retries` budget — the infra-loss path must not swallow genuine
/// failures. Here the container is present-and-exited(1), not gone, so the exit
/// code is authoritative and the retry advances to attempt 2.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_real_nonzero_exit_still_burns_budget() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/impl.md".into(),
            },
            state: TaskState::Running,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: Some("exited-nonzero".into()),
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.seed_exited("exited-nonzero", 1);
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let provider = Arc::new(FakeProvider::new());
    commit_on_run(&provider, repo.bare_path());
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend,
        provider,
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert!(!tasks[0].infra_loss, "a real exit is not an infra loss");
    assert_eq!((tasks[1].attempt, tasks[1].state), (2, TaskState::Done));
}

/// A dispatcher died with a command work task queued under capacity pressure
/// (§3.5): the job is Work, the task Pending with no container — exactly what
/// `defer_launch` persists. Reconciliation re-queues it (§3.6), and once the
/// now-available fleet accepts it, the *same* task launches and the job lands —
/// no new attempt, no retry consumed.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_requeues_queued_pending_work_task() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/cmd-work.yaml", CMD_WORK.as_bytes(), "type")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "cmd-work".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Command {
                run: "./build.sh".into(),
            },
            state: TaskState::Pending,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        })
        .await
        .unwrap();

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    let works: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Work)
        .collect();
    assert_eq!(
        works.len(),
        1,
        "re-queued the existing task, not a new attempt"
    );
    assert_eq!(
        (works[0].id, works[0].attempt, works[0].state),
        (1, 1, TaskState::Done),
    );
    assert_eq!(works[0].pending_reason, None);
    assert_eq!(works[0].queued_at, None);
}

/// Seed a job mid-Work with a single capacity-deferred command work task
/// (Pending, no container) carrying a persisted `queued_at` — exactly what
/// `defer_launch` leaves behind before a crash. Used by the restart-fairness and
/// timeout-clock tests below.
async fn seed_queued_command_work(
    store: &NatsStore,
    seq: u64,
    base_ref: &str,
    queued_at: chrono::DateTime<Utc>,
) {
    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: seq,
            project: "acme/api".into(),
            r#type: "cmd-work".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: format!("job/{seq}"),
            base_ref: Some(base_ref.into()),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: queued_at,
            ready_at: Some(queued_at),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: seq,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Command {
                run: "./build.sh".into(),
            },
            state: TaskState::Pending,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            pending_reason: Some(types::PendingReason::QueuedForCapacity),
            queued_at: Some(queued_at),
            session_id: None,
            reviewed_tip: None,
            result: None,
            created_at: queued_at,
            started_at: None,
            completed_at: None,
        })
        .await
        .unwrap();
}

/// §3.5 restart FIFO fairness (addendum): the launch queue's order lives only in
/// memory, so a restart must rebuild it from the *persisted* `queued_at`, not
/// from reconcile's job-iteration order. Three capacity-deferred command work
/// tasks are seeded so the newest-queued job has the lowest seq — enqueue order
/// is the reverse of graph order. Once the fleet frees, they must relaunch
/// oldest-first (job/3, job/2, job/1), matching the original enqueue order.
#[tokio::test]
async fn restart_relaunches_queued_tasks_in_persisted_fifo_order() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/cmd-work.yaml", CMD_WORK.as_bytes(), "type")
        .await;
    clone.push("main").await;
    let head = repo.head().await;

    let now = Utc::now();
    for (seq, age_secs) in [(1u64, 0i64), (2, 60), (3, 120)] {
        repo.create_job_branch(seq, &head).await;
        let queued_at = now - chrono::Duration::seconds(age_secs);
        seed_queued_command_work(&store, seq, &head, queued_at).await;
    }

    let backend = Arc::new(FakeBackend::new());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    test_utils::wait::poll_default("3 launches to drain from the queue", || {
        (backend.launches().len() >= 3).then_some(())
    })
    .await;
    assert_invariants_of(&sink);
    let branches: Vec<String> = backend
        .launches()
        .iter()
        .take(3)
        .map(|c| c.env.get("JOB_BRANCH").cloned().unwrap_or_default())
        .collect();
    assert_eq!(
        branches,
        vec!["job/3".to_string(), "job/2".into(), "job/1".into()],
        "queued launches relaunch oldest-first, from persisted queued_at",
    );
}

/// §3.5 restart timeout-clock survival (addendum): the max-queue-wait backstop
/// must measure from the *persisted* `queued_at`, not process-local time —
/// otherwise frequent auto-deploys reset the clock every restart and a genuinely
/// wedged launch never escalates. A task queued 25m before the restart, under a
/// 20m max wait, escalates on the very first post-restart scan; a reset clock
/// (wait ~0) would leave it Pending.
#[tokio::test]
async fn restart_preserves_queue_wait_clock_for_timeout() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/cmd-work.yaml", CMD_WORK.as_bytes(), "type")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    let queued_at = Utc::now() - chrono::Duration::minutes(25);
    seed_queued_command_work(&store, 1, &head, queued_at).await;

    let backend = Arc::new(FakeBackend::new());
    backend.fail_launch_no_capacity_if(|_| Some("no free slots on any node".into()));
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            launch_queue_max_wait: Some(Duration::from_secs(20 * 60)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, sink) = spawn_checked(core);

    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    wait_for_state(&store, 1, JobState::Escalated).await;
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.phase == TaskPhase::Work && t.state == TaskState::Failed),
        "the queued task failed on the persisted-clock timeout: {tasks:?}",
    );
    assert_invariants_of(&sink);
}

/// A dispatcher died with an **agent** evaluator queued under capacity pressure
/// (§3.5, #140): the job is Evaluation, the eval task Pending (kind Agent) with
/// no container — what `defer_launch` persists for an agent eval. Reconciliation
/// must re-queue it (not drop it as it once did for non-command kinds), and once
/// the fleet has capacity the *same* task relaunches through the provider, the
/// evaluator passes, and the job lands.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_requeues_queued_pending_agent_eval() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/agent-eval.yaml", AGENT_EVAL.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/eval.md", b"review it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "agent-eval".into(),
            title: String::new(),
            description: String::new(),
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Evaluation,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
            cover_html: None,
        })
        .await
        .unwrap();
    let tasks = store.tasks().await.unwrap();
    tasks
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Command {
                run: "./build.sh".into(),
            },
            state: TaskState::Done,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            session_id: None,
            reviewed_tip: None,
            result: Some(types::TaskResult::Command {
                pass: true,
                exit_code: 0,
                output: String::new(),
                structured: None,
            }),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
            pending_reason: None,
            queued_at: None,
        })
        .await
        .unwrap();
    tasks
        .put(&Task {
            id: 2,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Evaluation,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "review it".into(),
            },
            state: TaskState::Pending,
            attempt: 1,
            evaluator: Some("reviewer".into()),
            label: Some("reviewer".into()),
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            session_id: Some("sess-eval-1".into()),
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            pending_reason: Some(types::PendingReason::QueuedForCapacity),
            queued_at: Some(Utc::now()),
        })
        .await
        .unwrap();

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let backend = Arc::new(FakeBackend::new());
    let full = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let f = full.clone();
    backend.fail_launch_no_capacity_if(move |cfg| {
        (f.load(std::sync::atomic::Ordering::SeqCst) && cfg.cmd.iter().any(|c| c == "agent"))
            .then(|| "no free slots on any node".to_string())
    });
    let provider = Arc::new(FakeProvider::with_backend(backend.clone()));
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
    let (handle, sink) = spawn_checked(core);
    let h = handle.clone();
    provider.on_run(move |_| {
        let h = h.clone();
        async move {
            h.submit_eval(
                "acme",
                "api",
                1,
                2,
                EvalSubmission {
                    pass: true,
                    abort: false,
                    structured: None,
                    token_usage: None,
                    cover_html: None,
                },
            )
            .await
            .unwrap();
        }
    });

    full.store(false, std::sync::atomic::Ordering::SeqCst);
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    wait_for_state(&store, 1, JobState::Done).await;
    let evals: Vec<_> = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert_eq!(
        evals.len(),
        1,
        "re-queued the existing eval task, not a new attempt"
    );
    assert_eq!((evals[0].id, evals[0].attempt), (2, 1));
    assert_eq!(evals[0].state, TaskState::Done);
    assert_invariants_of(&sink);
}

/// §3.6 startup sweep: exited `chuggernaut.managed` containers left behind by
/// a crash/restart are reclaimed at boot — but only when their task is terminal
/// (or gone entirely). A container a live task may still resume is kept.
/// This is the other half of the disk-leak fix: task-exit removal covers the
/// happy path, the sweep covers containers orphaned by a crash before that ran.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn startup_sweep_removes_only_terminal_and_orphan_containers() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Escalated,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    let mk_task = |id: u64, state: TaskState, container: &str| Task {
        id,
        job_seq: 1,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Agent {
            provider: "claude".into(),
            model: None,
            prompt: "prompts/impl.md".into(),
        },
        state,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: Some(container.into()),
        rework_reason: None,
        infra_loss: false,

        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        completed_at: None,
    };
    let tasks = store.tasks().await.unwrap();
    tasks
        .put(&mk_task(1, TaskState::Done, "local/c-terminal"))
        .await
        .unwrap();
    tasks
        .put(&mk_task(2, TaskState::Pending, "local/c-live"))
        .await
        .unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.seed_managed_exited([
        "local/c-terminal".to_string(),
        "local/c-live".to_string(),
        "local/c-orphan".to_string(),
    ]);

    let provider = Arc::new(FakeProvider::new());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
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
    let (_handle, sink) = spawn_checked(core);

    let mut removed = test_utils::wait::poll_default("2 containers reclaimed by the sweep", || {
        let removed = backend.removed();
        (removed.len() >= 2).then_some(removed)
    })
    .await;
    assert_invariants_of(&sink);
    removed.sort();
    assert_eq!(
        removed,
        vec!["local/c-orphan".to_string(), "local/c-terminal".to_string()],
        "terminal-task and orphan containers reclaimed; live-task container kept"
    );
}

/// Shared fixture for the §3.6 fleet-sweep tests: a store carrying the `flaky`
/// job type plus one Escalated job (id 51) whose tasks the caller supplies.
/// Escalated so step-2 recovery leaves it (and its tasks) untouched, isolating
/// the running-container sweep. Returns the store, the spawned core handle, and
/// the backend (already seeded by the caller before `spawn`).
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn fleet_sweep_core(
    tasks: Vec<Task>,
    backend: Arc<FakeBackend>,
) -> Option<(
    NatsStore,
    &'static test_utils::nats::NatsTestServer,
    CoreHandle,
    TempRepo,
    InvariantSink,
)> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 51,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Escalated,
            branch: "job/51".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    let task_store = store.tasks().await.unwrap();
    for task in &tasks {
        task_store.put(task).await.unwrap();
    }

    let provider = Arc::new(FakeProvider::new());
    commit_on_run(&provider, repo.bare_path());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend,
        provider,
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, sink) = spawn_checked(core);
    Some((store, server, handle, repo, sink))
}

fn work_task(id: u64, state: TaskState, container_id: Option<&str>) -> Task {
    Task {
        id,
        job_seq: 51,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Agent {
            provider: "claude".into(),
            model: None,
            prompt: "prompts/impl.md".into(),
        },
        state,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: container_id.map(Into::into),
        rework_reason: None,
        infra_loss: false,

        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        completed_at: None,
    }
}

async fn wait_killed(backend: &FakeBackend, want: usize) -> Vec<String> {
    test_utils::wait::poll_default(format!("{want} killed container(s)"), || {
        let killed = backend.killed();
        (killed.len() >= want).then_some(killed)
    })
    .await
}

/// §3.6 fleet sweep: a container still running after a crash-restart but owned
/// by no live task is reaped, freeing its slot. This is the durable fix for the
/// 2026-07-22 incident, where pre-upgrade in-flight tasks were failed as
/// container-gone while their containers kept running and holding fleet slots
/// until an operator manually removed them. The reap emits a `container-reaped`
/// platform event attributed to the owning job.
#[tokio::test]
async fn startup_fleet_sweep_reaps_orphan_running_container() {
    let backend = Arc::new(FakeBackend::new());
    let tasks = vec![work_task(2, TaskState::Failed, Some("dev-air/c-orphan"))];
    backend.seed_managed_running([container::RunningContainer {
        id: "dev-air/c-orphan".into(),
        project: Some("acme/api".into()),
        job: Some(51),
        task: Some(2),
    }]);
    let Some((store, _server, _handle, _repo, sink)) =
        fleet_sweep_core(tasks, backend.clone()).await
    else {
        return;
    };

    let killed = wait_killed(&backend, 1).await;
    assert_invariants_of(&sink);
    assert_eq!(
        killed,
        vec!["dev-air/c-orphan".to_string()],
        "the orphan container is reaped to free its slot"
    );

    let reaped: Vec<serde_json::Value> = store
        .read_stream("job-events", 100)
        .await
        .unwrap()
        .iter()
        .map(|p| serde_json::from_slice(p).unwrap())
        .filter(|v: &serde_json::Value| v["event_type"] == "container-reaped")
        .collect();
    assert_eq!(
        reaped.len(),
        1,
        "one reap event (only the identified orphan)"
    );
    assert_eq!(reaped[0]["job_seq"], 51);
    assert_eq!(reaped[0]["container_id"], "dev-air/c-orphan");
    assert!(
        reaped[0]["detail"]
            .as_str()
            .unwrap()
            .contains("reaped orphan container dev-air/c-orphan for 51/2"),
        "detail names the container and job/task: {}",
        reaped[0]["detail"]
    );
}

/// §3.6 fleet-sweep re-attach regression guard: a running container owned by a
/// live `Running` task must NOT be reaped — step 2 recovery re-attaches its
/// monitor. Matched either by the `(project, job, task)` identity labels or,
/// for a container whose task predates those labels, by the task's recorded
/// `container_id`.
#[tokio::test]
async fn startup_fleet_sweep_keeps_container_of_running_task() {
    let backend = Arc::new(FakeBackend::new());
    let tasks = vec![
        work_task(1, TaskState::Running, Some("dev-air/c-live")),
        work_task(3, TaskState::Running, Some("dev-air/c-live-legacy")),
    ];
    backend.seed_managed_running([
        container::RunningContainer {
            id: "dev-air/c-live".into(),
            project: Some("acme/api".into()),
            job: Some(51),
            task: Some(1),
        },
        container::RunningContainer {
            id: "dev-air/c-live-legacy".into(),
            project: None,
            job: None,
            task: None,
        },
    ]);
    let Some((_store, _server, _handle, _repo, sink)) =
        fleet_sweep_core(tasks, backend.clone()).await
    else {
        return;
    };

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_invariants_of(&sink);
    assert!(
        backend.killed().is_empty(),
        "containers of live Running tasks are re-attached, never reaped: {:?}",
        backend.killed()
    );
}

/// §3.6 fleet-sweep ownership guard (#268): a running container that carries
/// the managed marker but **no identity labels** was not launched by the
/// dispatcher — most plausibly it inherited the marker from its image, which is
/// how the long-lived `chug-worker` daemon became reapable and every dispatcher
/// restart killed the worker fleet. The marker alone is not ownership; the
/// identity labels every launch stamps beside it are. Nothing here is named
/// `chug-worker` on purpose — the rule is about what the label means.
#[tokio::test]
async fn startup_fleet_sweep_spares_container_without_identity_labels() {
    let backend = Arc::new(FakeBackend::new());
    let tasks = vec![work_task(2, TaskState::Failed, Some("dev-air/c-orphan"))];
    backend.seed_managed_running([container::RunningContainer {
        id: "dev-air/c-daemon".into(),
        project: None,
        job: None,
        task: None,
    }]);
    let Some((_store, _server, _handle, _repo, sink)) =
        fleet_sweep_core(tasks, backend.clone()).await
    else {
        return;
    };

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_invariants_of(&sink);
    assert!(
        backend.killed().is_empty(),
        "a marker-bearing container with no identity is not the dispatcher's to reap: {:?}",
        backend.killed()
    );
}

/// §3.6 fleet sweep is best-effort: a backend that cannot list running
/// containers (an unreachable node) is logged and skipped — the dispatcher
/// still starts and serves work rather than crashing on the error.
#[tokio::test]
async fn startup_fleet_sweep_tolerates_backend_error() {
    let backend = Arc::new(FakeBackend::new());
    backend.fail_list_managed_running("dev-air unreachable");
    let tasks = vec![work_task(2, TaskState::Failed, Some("dev-air/c-orphan"))];
    let Some((store, _server, handle, _repo, sink)) =
        fleet_sweep_core(tasks, backend.clone()).await
    else {
        return;
    };

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        backend.killed().is_empty(),
        "a list error reaps nothing (log, continue)"
    );

    let job = handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);
    wait_for_state(&store, job.id, JobState::Done).await;
    assert_invariants_of(&sink);
}

/// A dispatcher died mid-wrap-up: the job record says WrapUp (eval already
/// passed) and the job was parked in the in-memory merge queue, which is lost
/// on restart. Reconciliation re-enters it into the queue and it lands
/// (§2.1 WrapUp; §3.6 step 3). No gate was in flight, so the fast path squashes.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_lands_job_orphaned_in_wrapup() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::WrapUp,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/impl.md".into(),
            },
            state: TaskState::Done,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: Some(types::TaskResult::Work {
                summary: None,
                structured: None,
                token_usage: None,
                cover_html: None,
            }),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        })
        .await
        .unwrap();

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);
}

/// The §3.6 wrap-up-command gap: a dispatcher died AFTER the squash landed on
/// main but BEFORE the `wrap_up.run` publish finished — the job is in WrapUp
/// with a Running WrapUp command task whose container is gone. Reconciliation
/// must NOT re-drive the merge queue (the merge already landed); it must
/// relaunch the pending publish, which then carries the job to Done. This is
/// the correctness requirement that killed the first attempt as churn.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_during_wrapup_relaunches_pending_publish() {
    const WEBPUB: &str = r#"
name: webpub
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
wrap_up:
  run: ./tasks/web-publish.sh
"#;
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/webpub.yaml", WEBPUB.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone
        .commit_file("web/src/app.tsx", b"<App/>", "job/1: webpub")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "webpub".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::WrapUp,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    let tasks = store.tasks().await.unwrap();
    tasks
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/impl.md".into(),
            },
            state: TaskState::Done,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: Some(types::TaskResult::Work {
                summary: None,
                structured: None,
                token_usage: None,
                cover_html: None,
            }),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        })
        .await
        .unwrap();
    tasks
        .put(&Task {
            id: 2,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::WrapUp,
            cycle: 1,
            kind: TaskKind::Command {
                run: "./tasks/web-publish.sh".into(),
            },
            state: TaskState::Running,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: Some("dead-publish-container".into()),
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    let backend = Arc::new(FakeBackend::new());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);

    let launches = backend.launches();
    assert_eq!(launches.len(), 1, "the pending publish is relaunched once");
    assert_eq!(
        launches[0].env.get("JOB_BRANCH").map(String::as_str),
        Some("main"),
        "the relaunched publish runs against merged main"
    );

    let log = tasks.list_for_job("acme", "api", 1).await.unwrap();
    let wrapups: Vec<&Task> = log
        .iter()
        .filter(|t| t.phase == TaskPhase::WrapUp)
        .collect();
    assert_eq!(wrapups.len(), 2, "orphan + relaunch: {log:?}");
    assert!(
        wrapups
            .iter()
            .any(|t| t.attempt == 2 && t.state == TaskState::Done),
        "the relaunched publish (attempt 2) completed: {wrapups:?}"
    );
    assert!(
        !wrapups.iter().any(|t| t.state == TaskState::Running),
        "no publish task is left Running: {wrapups:?}"
    );
}

/// Upstream reached Done while the dispatcher was dead: reconciliation
/// unblocks the dependent and runs it.
#[tokio::test]
async fn restart_unblocks_dependent_whose_deps_completed() {
    let Some(rig) = rig().await else { return };
    commit_on_run(&rig.provider, rig.repo.bare_path());
    commit_on_run(&rig.provider, rig.repo.bare_path());

    let up = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let down = rig
        .handle
        .create_job(CreateSpec {
            deps: vec![],
            ..req("flaky")
        })
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", up.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, up.id, JobState::Done).await;

    let jobs = rig.store.jobs().await.unwrap();
    let mut blocked = jobs.get("acme", "api", down.id).await.unwrap().unwrap();
    blocked.state = JobState::Blocked;
    blocked.deps = vec![up.id];
    jobs.put(&blocked).await.unwrap();

    let repos_root = rig
        .repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        rig.store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        rig.provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: rig._server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle2, sink2) = spawn_checked(core);
    wait_for_state(&rig.store, down.id, JobState::Done).await;
    assert_invariants_of(&rig.invariants);
    assert_invariants_of(&sink2);
}

/// A hung work container is killed at `task_timeout` and the failure path
/// applies — no retries on this type, so the job escalates.
#[tokio::test]
async fn task_timeout_kills_and_fails_hung_work() {
    let Some(rig) = rig().await else { return };
    rig.provider.on_run(|_| async {
        futures::future::pending::<()>().await;
    });

    let job = rig.handle.create_job(req("slow")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert_invariants_of(&rig.invariants);
}

/// Human tasks never time out, but `job_deadline` summons a human exactly
/// once (§3.5 one-shot rule).
#[tokio::test]
async fn job_deadline_escalates_once_for_stalled_human_work() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(req("manual-deadline")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let deadline_task = tasks.last().unwrap();
    assert!(
        matches!(&deadline_task.kind, TaskKind::Human { prompt } if prompt.starts_with("[deadline]"))
    );

    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            deadline_task.id,
            TaskResolution::Escalation {
                action: EscalationAction::Retry,
                structured: None,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = rig
        .store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.state,
        JobState::Work,
        "one-shot: no second deadline escalation"
    );
    assert_invariants_of(&rig.invariants);
}

/// `submit_result` arrives while the container is still running (§4.2
/// ack-then-exit), so a dispatcher restart in that window used to drop the
/// agent's summary on the floor: it lived only in `ExecState`, which rebuilds
/// blank. The summary is the squash commit's message body, so the work landed
/// on `main` unexplained.
///
/// The submission is now persisted to the task record on arrival and recovered
/// from the task log on restart.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_preserves_the_submitted_summary_for_the_squash_commit() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;

    repo.create_job_branch(1, &head).await;
    let work = repo.clone_branch("job/1").await;
    work.commit_file("src/f.rs", b"fn f() {}", "impl").await;
    work.push("job/1").await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "flaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: "job/1".into(),
            base_ref: Some(head),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/impl.md".into(),
            },
            state: TaskState::Done,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,

            pending_reason: None,
            queued_at: None,
            session_id: Some("da08d5f3-844e-430e-8363-39b4882f437b".into()),
            reviewed_tip: None,
            result: Some(types::TaskResult::Work {
                summary: Some("added f() with tests".into()),
                structured: None,
                token_usage: None,
                cover_html: None,
            }),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        })
        .await
        .unwrap();

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.bare_path())
        .args(["log", "-1", "--format=%B", "main"])
        .output()
        .unwrap();
    let message = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        message.contains("added f() with tests"),
        "summary lost across restart: {message:?}"
    );

    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert_eq!(
        tasks[0].session_id.as_deref(),
        Some("da08d5f3-844e-430e-8363-39b4882f437b")
    );
}

/// §3.6 graceful drain: a SIGTERM (deploy `kickstart -k`) drains the actor and
/// flushes memory-only state to KV so records are true at exit, then a fresh
/// core re-attaches every still-Running task rather than failing it.
///
/// The gap the drain closes: a work container launches and reports its id via a
/// `TaskContainerStarted` message, but that message can still be in the mailbox
/// when SIGTERM lands — leaving the task record with `container_id: None`, which
/// reconciliation reads as container-gone and turns into a synthetic -1. The
/// drain sweeps the mailbox and then audits every Running task, stamping its real
/// id from the live fleet. On restart the task re-attaches (same container, still
/// Running) with zero reconcile-failure and zero synthetic -1.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn drain_flushes_container_id_so_restart_reattaches_running_work() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let backend = Arc::new(FakeBackend::new());
    let provider = Arc::new(FakeProvider::with_backend(backend.clone()));
    provider.on_run(|_| async { futures::future::pending::<()>().await });
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root.clone()),
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
    let (handle, sink) = spawn_checked(core);

    let job = handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);
    wait_for_state(&store, job.id, JobState::Work).await;

    let work = test_utils::wait::task_where(
        &store,
        "acme",
        "api",
        job.id,
        "running work task with a container id",
        |t| t.phase == TaskPhase::Work && t.state == TaskState::Running && t.container_id.is_some(),
    )
    .await;
    let real_cid = work.container_id.clone().unwrap();
    let tasks = store.tasks().await.unwrap();

    let mut racing = work.clone();
    racing.container_id = None;
    tasks.put(&racing).await.unwrap();
    backend.seed_managed_running([container::RunningContainer {
        id: real_cid.clone(),
        project: Some("acme/api".into()),
        job: Some(job.id),
        task: Some(work.id),
    }]);

    handle.drain().await.unwrap();
    assert_invariants_of(&sink);
    let flushed = tasks
        .get("acme", "api", job.id, work.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(flushed.state, TaskState::Running);
    assert_eq!(
        flushed.container_id.as_deref(),
        Some(real_cid.as_str()),
        "drain stamped the running container's id back onto the task"
    );

    let backend2 = Arc::new(FakeBackend::new());
    backend2.seed_running([real_cid.clone()]);
    let core2 = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend2.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle2, sink2) = spawn_checked(core2);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
    let work_tasks: Vec<&Task> = log.iter().filter(|t| t.phase == TaskPhase::Work).collect();
    assert_eq!(
        work_tasks.len(),
        1,
        "re-attached the existing task, no relaunch: {log:?}"
    );
    assert_eq!(work_tasks[0].state, TaskState::Running);
    assert_eq!(
        work_tasks[0].container_id.as_deref(),
        Some(real_cid.as_str())
    );
    assert!(
        !work_tasks[0].infra_loss,
        "a re-attached container is not an infra loss: {work_tasks:?}"
    );
    let after = store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, JobState::Work, "job stays in Work, mid-task");
    assert_eq!(backend2.killed(), Vec::<String>::new(), "nothing reaped");
    assert_invariants_of(&sink);
    assert_invariants_of(&sink2);
}

/// §3.6 re-attach harvest (ticket #187, self-deploy report loss on deploy #209):
/// a command WORK container the OLD dispatcher launched is still Running when the
/// NEW dispatcher reconciles, so it re-attaches (§3.6) rather than relaunching —
/// and at exit it must harvest the `@chug:leg`/`@chug:report` stream into the
/// task's structured result *exactly* as the launch-path monitor does. Before the
/// fix the re-attach monitor threaded `structured: None`, so a self-deploy — which
/// always spans its own dispatcher restart — lost its report every time.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn restart_reattach_harvests_command_work_deploy_report() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/cmd-work.yaml", CMD_WORK.as_bytes(), "type")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    let cid = "reattach/deploy-1";
    store
        .jobs()
        .await
        .unwrap()
        .put(&Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "cmd-work".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: JobState::Work,
            branch: "job/1".into(),
            base_ref: Some(head.clone()),
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        })
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&Task {
            id: 1,
            job_seq: 1,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Command {
                run: "./build.sh".into(),
            },
            state: TaskState::Running,
            attempt: 1,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: Some(cid.into()),
            rework_reason: None,
            infra_loss: false,
            pending_reason: None,
            queued_at: None,
            session_id: None,
            reviewed_tip: None,
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.seed_running([cid.to_string()]);
    backend.put_logs(
        concat!(
            "update: deploying abc123\n",
            "@chug:leg {\"name\":\"build-dispatcher\",\"status\":\"ok\",\"secs\":41}\n",
            "@chug:leg {\"name\":\"restart-verify\",\"status\":\"ok\",\"secs\":12}\n",
            "@chug:leg {\"name\":\"sha-advance\",\"status\":\"ok\",\"secs\":1}\n",
            "@chug:report {\"from_sha\":\"prev999\",\"to_sha\":\"abc123\",\"rollback\":false,\"health\":\"ok\"}\n",
        )
        .as_bytes()
        .to_vec(),
    );
    backend.finish_running(cid, 0);

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle, sink) = spawn_checked(core);

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);
    let works: Vec<Task> = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Work)
        .collect();
    assert_eq!(works.len(), 1, "re-attached, not relaunched: {works:?}");
    assert_eq!(works[0].state, TaskState::Done);
    let Some(TaskResult::Work {
        structured: Some(value),
        ..
    }) = &works[0].result
    else {
        panic!(
            "re-attached command work must carry its harvested deploy report, got {:?}",
            works[0].result
        );
    };
    let report: types::DeployReport = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(report.legs.len(), 3, "all three legs harvested: {report:?}");
    assert_eq!(report.legs[1].name, "restart-verify");
    assert_eq!(report.to_sha.as_deref(), Some("abc123"));
    assert_eq!(report.health.as_deref(), Some("ok"));
    assert!(!report.rollback);
    assert_eq!(backend.removed(), vec![cid.to_string()]);
}

/// What a deploy's `update.sh` prints: human progress, the per-leg `@chug:leg`
/// lines and the closing `@chug:report` envelope (ticket #187).
const DEPLOY_STDOUT: &str = concat!(
    "update: deploying abc123\n",
    "@chug:leg {\"name\":\"build-dispatcher\",\"status\":\"ok\",\"secs\":41}\n",
    "update: worker 'nuc' refresh NOT confirmed on abc123 — FAILING deploy\n",
    "@chug:leg {\"name\":\"worker-refresh:nuc\",\"status\":\"failed\",\"secs\":900,",
    "\"error\":\"refresh not confirmed\"}\n",
    "@chug:report {\"from_sha\":\"prev999\",\"to_sha\":\"abc123\",\"rollback\":false}\n",
);

/// Seed the crash state §3.6 reconciles from: job 1 of `job_type` sitting in
/// Work with one Running command task pointing at `cid`, started at
/// `started_at`. Written straight to KV — the task log is the source of truth a
/// restarted dispatcher recovers from.
async fn seed_command_work_crash_state(
    store: &NatsStore,
    repo: &TempRepo,
    job_type: &str,
    yaml: &str,
    cid: &str,
    started_at: chrono::DateTime<Utc>,
) {
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file(&format!("jobs/{job_type}.yaml"), yaml.as_bytes(), "type")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;
    store
        .jobs()
        .await
        .unwrap()
        .put(&crash_state_job(job_type, head))
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&crash_state_task(cid, started_at))
        .await
        .unwrap();
}

/// Job 1 as the crashed dispatcher left it: mid-Work on its own branch.
fn crash_state_job(job_type: &str, head: String) -> Job {
    Job {
        id: 1,
        project: "acme/api".into(),
        r#type: job_type.into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        members: vec![],
        batch_id: None,
        state: JobState::Work,
        branch: "job/1".into(),
        base_ref: Some(head),
        knowledge_tags: vec![],
        eval: vec![],
        require_approval: false,
        timeout: None,
        model: None,
        claim_next: false,
        escalation: None,
        factory: None,
        schedule: None,
        created_at: Utc::now(),
        ready_at: Some(Utc::now()),
        completed_at: None,
        inputs: Default::default(),
        groups: vec![],
        task_time_ms: None,
    }
}

/// Its work task: Running, with the container id the restart reconciles against.
fn crash_state_task(cid: &str, started_at: chrono::DateTime<Utc>) -> Task {
    Task {
        id: 1,
        job_seq: 1,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Command {
            run: "./deploy.sh".into(),
        },
        state: TaskState::Running,
        attempt: 1,
        evaluator: None,
        label: None,
        stage: 0,
        performed_by: None,
        container_id: Some(cid.into()),
        rework_reason: None,
        infra_loss: false,
        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: Utc::now(),
        started_at: Some(started_at),
        completed_at: None,
    }
}

/// A core with artifact capture enabled, so a test can read back the
/// `stdout.log` a harvest stored. Returns its handle and the identity to open
/// the artifact store with.
async fn spawn_core_capturing_artifacts(
    store: &NatsStore,
    repo: &TempRepo,
    backend: Arc<FakeBackend>,
    nats_url: &str,
) -> (CoreHandle, String, InvariantSink) {
    let (identity, _) = store::secrets::generate_age_keypair();
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend,
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: nats_url.into(),
            artifacts_identity: Some(identity.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, sink) = spawn_checked(core);
    (handle, identity, sink)
}

/// The stored `stdout.log` for a task, polled until it lands (harvests run off
/// the actor thread) or the bound elapses.
async fn wait_for_stdout_artifact(store: &NatsStore, identity: &str, task_id: u64) -> Vec<u8> {
    let artifacts = store
        .artifacts(store::ArtifactCrypto::with_identity(identity).unwrap())
        .await
        .unwrap();
    for _ in 0..100 {
        if let Some(bytes) = artifacts
            .get("acme", "api", 1, task_id, store::ArtifactKind::Stdout)
            .await
            .unwrap()
        {
            return bytes;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("task {task_id} stored no stdout.log artifact within the bound");
}

/// §3.6 harvest of a container that exited during the downtime (ticket #270,
/// deploy #267): a self-deploy restarts the dispatcher that supervises it, and
/// its ssh session — hence its work container — ends within seconds of that
/// restart, so reconciliation routinely finds the container already Exited
/// rather than Running. That arm used to synthesize the exit inline and skip the
/// harvest entirely, leaving the deploy with an empty `stdout.log`, empty task
/// output and no legs: the deploy job recorded nothing about its own deploy. It
/// must harvest exactly as the still-Running arm does.
#[tokio::test]
async fn restart_harvests_command_work_that_exited_during_the_downtime() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let cid = "exited/deploy-1";
    seed_command_work_crash_state(&store, &repo, "cmd-work", CMD_WORK, cid, Utc::now()).await;

    let backend = Arc::new(FakeBackend::new());
    backend.seed_exited(cid, 0);
    backend.put_logs(DEPLOY_STDOUT.as_bytes().to_vec());

    let (_handle, identity, sink) =
        spawn_core_capturing_artifacts(&store, &repo, backend.clone(), server.url()).await;

    wait_for_state(&store, 1, JobState::Done).await;
    assert_invariants_of(&sink);

    let stdout = wait_for_stdout_artifact(&store, &identity, 1).await;
    let text = String::from_utf8_lossy(&stdout);
    assert!(
        text.contains("update: deploying abc123")
            && text.contains("worker 'nuc' refresh NOT confirmed"),
        "harvested stdout.log missing the deploy's output: {text}"
    );

    let works: Vec<Task> = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Work)
        .collect();
    assert_eq!(works.len(), 1, "re-attached, not relaunched: {works:?}");
    let Some(TaskResult::Work {
        structured: Some(value),
        ..
    }) = &works[0].result
    else {
        panic!(
            "an exited-during-downtime command work task must still carry its \
             harvested deploy report, got {:?}",
            works[0].result
        );
    };
    let report: types::DeployReport = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(report.legs.len(), 2, "both legs harvested: {report:?}");
    assert_eq!(report.legs[1].name, "worker-refresh:nuc");
    assert_eq!(report.to_sha.as_deref(), Some("abc123"));
    assert_eq!(backend.removed(), vec![cid.to_string()]);
}

/// A task the timeout scan gives up on still leaves its log behind (ticket
/// #270). The launch-path monitor normally harvests at exit, but it is parked in
/// `backend.wait` — and when the node holding the container is what broke
/// (deploy #267: the worker daemons died mid-deploy, so every poll answered with
/// a transport error) that wait never returns. Without a harvest on the timeout
/// path itself, the record of a deploy the dispatcher killed at `task_timeout`
/// is `{"artifacts":[]}` — precisely the #267 post-mortem's problem.
#[tokio::test]
async fn task_timeout_harvests_the_container_log_before_giving_up() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let cid = "wedged/deploy-1";
    seed_command_work_crash_state(
        &store,
        &repo,
        "cmd-work-slow",
        CMD_WORK_SLOW,
        cid,
        Utc::now() - chrono::Duration::seconds(60),
    )
    .await;

    let backend = Arc::new(FakeBackend::new());
    backend.seed_running([cid.to_string()]);
    backend.put_logs(DEPLOY_STDOUT.as_bytes().to_vec());

    let (handle, identity, sink) =
        spawn_core_capturing_artifacts(&store, &repo, backend.clone(), server.url()).await;
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);

    wait_for_state(&store, 1, JobState::Escalated).await;
    assert_eq!(backend.killed(), vec![cid.to_string()], "container killed");
    let stdout = wait_for_stdout_artifact(&store, &identity, 1).await;
    assert!(
        String::from_utf8_lossy(&stdout).contains("worker 'nuc' refresh NOT confirmed"),
        "a timed-out task must still leave its container log as stdout.log"
    );
    assert_invariants_of(&sink);
}

/// §3.6 graceful drain: even with a busy mailbox the drain completes well under
/// its ~10s bound. Draining is non-blocking on the mailbox — it sweeps what is
/// present and returns — so a backlog of in-flight requests cannot wedge exit.
#[tokio::test]
async fn drain_completes_promptly_with_a_busy_mailbox() {
    let Some(rig) = rig().await else { return };

    let mut inflight = Vec::new();
    for _ in 0..64 {
        let h = rig.handle.clone();
        inflight.push(tokio::spawn(async move {
            let _ = h.ping().await;
        }));
    }
    let drained = tokio::time::timeout(Duration::from_secs(5), rig.handle.drain()).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(drained, Ok(Ok(()))),
        "drain did not complete under its bound with a busy mailbox: {drained:?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// §3.6 graceful drain robustness: a drain cut short (here by a 1ms deadline,
/// standing in for launchd's SIGKILL) never corrupts or regresses records — a
/// Running task with an in-flight launch stays Running and parseable, exactly as
/// it was before the drain (no worse than today). The drain only ever adds truth.
#[tokio::test]
async fn cut_short_drain_leaves_records_no_worse() {
    let Some(rig) = rig().await else { return };
    rig.provider
        .on_run(|_| async { futures::future::pending::<()>().await });

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::task_where(
        &rig.store,
        "acme",
        "api",
        job.id,
        "running work task",
        |t| t.phase == TaskPhase::Work && t.state == TaskState::Running,
    )
    .await;

    let _ = tokio::time::timeout(Duration::from_millis(1), rig.handle.drain()).await;
    assert_invariants_of(&rig.invariants);

    let log = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert!(!log.is_empty(), "task log still reads back");
    assert!(
        log.iter().all(|t| t.state != TaskState::Failed),
        "a cut-short drain must never fail a task: {log:?}"
    );
    let work = log
        .iter()
        .find(|t| t.phase == TaskPhase::Work)
        .expect("work task");
    assert_eq!(
        work.state,
        TaskState::Running,
        "the in-flight work task is left exactly as it was"
    );
    assert_invariants_of(&rig.invariants);
}

/// C1 heal: the escalation shim commits the Stalled/Escalated transition
/// before the PutTask effect (the §2.1 record is the decision; artifacts are
/// downstream), so a crash between the two leaves a parked job with an empty
/// operator inbox — a shape `reconcile` previously had "nothing to recover"
/// for. Restart reconciliation re-derives the Pending Human task from the WHY
/// stamped on the job record.
#[tokio::test]
async fn restart_recreates_missing_escalation_task_from_stamped_record() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let jobs = rig.store.jobs().await.unwrap();
    let mut stalled = jobs.get("acme", "api", job.id).await.unwrap().unwrap();
    stalled.state = JobState::Stalled;
    stalled.escalation = Some(types::Escalation {
        reason: "revalidation_failed".into(),
        detail: "missing dep".into(),
        failing_task: None,
        at: Utc::now(),
    });
    jobs.put(&stalled).await.unwrap();

    let repos_root = rig
        .repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        rig.store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        rig.provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: rig._server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (_handle2, sink2) = spawn_checked(core);

    let tasks_store = rig.store.tasks().await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let healed = loop {
        let tasks = tasks_store
            .list_for_job("acme", "api", job.id)
            .await
            .unwrap();
        if let Some(t) = tasks
            .iter()
            .find(|t| t.phase == TaskPhase::Escalation && t.state == TaskState::Pending)
        {
            break t.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "escalation task was not healed within 5s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        matches!(&healed.kind, TaskKind::Human { prompt } if prompt == "missing dep"),
        "healed task must carry the stamped detail, got {:?}",
        healed.kind,
    );
    let after = jobs.get("acme", "api", job.id).await.unwrap().unwrap();
    assert_eq!(after.state, JobState::Stalled);
    assert_eq!(
        after.escalation.as_ref().unwrap().reason,
        "revalidation_failed"
    );
    assert_invariants_of(&rig.invariants);
    assert_invariants_of(&sink2);
}

//! Tier-2 tests for restart reconciliation (§3.6) and the timeout/deadline
//! scans (§3.5). Crash states are constructed directly in KV — the task log
//! is the source of truth the dispatcher recovers from.

use chrono::Utc;
use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateJobRequest, EvalSubmission, spawn};
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{
    EscalationAction, Job, JobState, Task, TaskKind, TaskPhase, TaskResolution, TaskState,
};

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

// Command work + agent eval: the shape where an agent evaluator can be parked
// Pending under capacity pressure (§3.5, #140) and must be re-queued on restart.
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

struct Rig {
    _server: test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
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
    let handle = spawn(core);
    Some(Rig {
        _server: server,
        store,
        repo,
        provider,
        handle,
    })
}

fn req(r#type: &str) -> CreateJobRequest {
    CreateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        factory: None,
        members: vec![],
        draft: false,
    }
}

/// Registers a work run that commits a stub to the job branch, so a relaunched
/// agent attempt produces output and clears the §3.2 empty-output guard.
fn commit_on_run(provider: &FakeProvider, bare: std::path::PathBuf) {
    provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        // Branch-derived content so the commit always diffs, even when a prior
        // job already merged this stub path to the base.
        let body = format!("// work produced on {branch}\n");
        clone
            .commit_file("src/work.rs", body.as_bytes(), "work")
            .await;
        clone.push(&branch).await;
    });
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> Job {
    let jobs = store.jobs().await.unwrap();
    for _ in 0..100 {
        if let Some(job) = jobs.get("acme", "api", seq).await.unwrap()
            && job.state == want
        {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {want:?}");
}

/// A dispatcher died mid-Work: the job record says Work, the task log has a
/// Running task, and no container exists. Reconciliation treats it as a
/// failure, retries per `work_retries`, and the job completes.
#[tokio::test]
async fn restart_recovers_orphaned_running_work_task() {
    // Fresh infra WITHOUT spawning a core yet — the crash state comes first.
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
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
            session_id: None, // gone with the crashed dispatcher
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    // "Restart": a fresh core reconciles and finishes the job.
    let provider = Arc::new(FakeProvider::new()); // retry exits 0
    commit_on_run(&provider, repo.bare_path()); // …producing output (§3.2 guard)
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
    let _handle = spawn(core);

    wait_for_state(&store, 1, JobState::Done).await;
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].state, TaskState::Failed); // the orphan
    assert_eq!((tasks[1].attempt, tasks[1].state), (2, TaskState::Done));
    assert_eq!(provider.runs().len(), 1); // only the retry ran here
}

/// §3.6 infra-loss accounting: a dispatcher died mid-Work and, by the time it
/// restarted, the work container was GONE (docker pruned it, the node rebooted,
/// colima restarted). That is an infrastructure loss, NOT a real failure —
/// reconciliation relaunches the attempt WITHOUT spending a `work_retries`
/// budget, and stamps the retired task/event with the infra reason. The
/// distinguishing fact vs `restart_recovers_orphaned_running_work_task` is that
/// a container id WAS recorded, so the container demonstrably existed.
#[tokio::test]
async fn restart_infra_loss_relaunches_work_without_burning_budget() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();
    // Running work task with a recorded container id that the fresh backend has
    // never heard of: inspect → not found → infra loss.
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
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    let provider = Arc::new(FakeProvider::new()); // relaunch exits 0
    commit_on_run(&provider, repo.bare_path()); // …producing output (§3.2 guard)
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
    let _handle = spawn(core);

    wait_for_state(&store, 1, JobState::Done).await;
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    // The orphan is retired as an infra loss, not a plain failure.
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert!(tasks[0].infra_loss, "orphan carries the infra-loss marker");
    // Budget UNCHANGED: the relaunch reuses attempt 1 (a real retry would be 2).
    assert_eq!(
        (tasks[1].attempt, tasks[1].state),
        (1, TaskState::Done),
        "infra relaunch keeps the same attempt: {tasks:?}"
    );
    assert!(!tasks[1].infra_loss);
    assert_eq!(provider.runs().len(), 1); // only the relaunch ran
}

/// §3.6 infra-loss cap: an environment that keeps eating the container (a node
/// stuck in a reboot loop) must not relaunch forever. After
/// `INFRA_RELAUNCH_CAP` losses the job escalates with reason `infra_loss`
/// rather than a `work_retries`-exhausted failure. The prior losses are seeded
/// directly (three restarts compressed into one crash state).
#[tokio::test]
async fn restart_repeated_infra_loss_escalates_with_infra_loss() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
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
        result: None,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        completed_at: None,
    };
    // Three already-retired infra losses (INFRA_RELAUNCH_CAP), then a fourth
    // attempt Running against a container the fresh backend has never seen.
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
    let _handle = spawn(core);

    let job = wait_for_state(&store, 1, JobState::Escalated).await;
    assert_eq!(
        job.escalation.as_ref().map(|e| e.reason.as_str()),
        Some("infra_loss"),
        "the cap escalates with reason=infra_loss, not work_retries_exhausted"
    );
    // The fourth attempt is retired as an infra loss too — no relaunch beyond it.
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
async fn restart_real_nonzero_exit_still_burns_budget() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
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
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    // The container is still known to the backend and exited(1) — a real
    // failure the crash merely lost, not a vanished container.
    let backend = Arc::new(FakeBackend::new());
    backend.seed_exited("exited-nonzero", 1);
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let provider = Arc::new(FakeProvider::new()); // the retry exits 0
    commit_on_run(&provider, repo.bare_path()); // …producing output (§3.2 guard)
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
    let _handle = spawn(core);

    wait_for_state(&store, 1, JobState::Done).await;
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    // Budget SPENT: a real nonzero exit advances to attempt 2, not a same-attempt
    // infra relaunch, and the orphan carries no infra-loss marker.
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
async fn restart_requeues_queued_pending_work_task() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();
    // The crash-time task: Pending, no container, not human — a queued launch.
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
            result: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        })
        .await
        .unwrap();

    // "Restart": a fresh core whose fleet now has capacity.
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
    let _handle = spawn(core);

    wait_for_state(&store, 1, JobState::Done).await;
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
    // The launch cleared the queued markers, so the record no longer reads as
    // waiting (the UI badge disappears live).
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: queued_at,
            ready_at: Some(queued_at),
            completed_at: None,
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
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/cmd-work.yaml", CMD_WORK.as_bytes(), "type")
        .await;
    clone.push("main").await;
    let head = repo.head().await;

    let now = Utc::now();
    // seq 1 queued most recently, seq 3 the oldest.
    for (seq, age_secs) in [(1u64, 0i64), (2, 60), (3, 120)] {
        repo.create_job_branch(seq, &head).await;
        let queued_at = now - chrono::Duration::seconds(age_secs);
        seed_queued_command_work(&store, seq, &head, queued_at).await;
    }

    // "Restart": a fresh core whose fleet has capacity, so the re-queued launches
    // drain immediately — in whatever order reconciliation left them.
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
    let _handle = spawn(core);

    // Drain launches the queue front-to-back, so the first three launches are the
    // three work commands in FIFO order.
    for _ in 0..100 {
        if backend.launches().len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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

    // Fleet stays full: the launch never gets a slot, so only the backstop can
    // retire it.
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
            // Below the 25m persisted wait, so a surviving clock fires at once.
            launch_queue_max_wait: Some(Duration::from_secs(20 * 60)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let handle = spawn(core);

    handle.trigger_scan().await.unwrap();
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
}

/// A dispatcher died with an **agent** evaluator queued under capacity pressure
/// (§3.5, #140): the job is Evaluation, the eval task Pending (kind Agent) with
/// no container — what `defer_launch` persists for an agent eval. Reconciliation
/// must re-queue it (not drop it as it once did for non-command kinds), and once
/// the fleet has capacity the *same* task relaunches through the provider, the
/// evaluator passes, and the job lands.
#[tokio::test]
async fn restart_requeues_queued_pending_agent_eval() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
            cover_html: None,
        })
        .await
        .unwrap();
    let tasks = store.tasks().await.unwrap();
    // Work already succeeded; only the eval remains.
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
    // The crash-time eval: Pending agent evaluator, no container — a queued
    // launch (§3.5), exactly what the agent NoCapacity path parks.
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
            result: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            pending_reason: Some(types::PendingReason::QueuedForCapacity),
            queued_at: Some(Utc::now()),
        })
        .await
        .unwrap();

    // "Restart": a fresh core whose provider launches through the backend, so the
    // agent eval's relaunch actually runs. The fleet stays full for agent
    // launches until we free it — that keeps the eval from running before the
    // submit_eval hook is wired (a queued-NoCapacity attempt consumes no hook),
    // so the pass is deterministic regardless of when the startup drain fires.
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
    let handle = spawn(core);
    // Wire the eval verdict now that the core exists: the relaunched agent run
    // submits a pass for task 2.
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
                },
            )
            .await
            .unwrap();
        }
    });

    // Free the fleet; the re-queued eval drains, relaunches the same task, and
    // passes. If reconciliation had dropped the queued agent eval, nothing
    // launches and the job wedges in Evaluation — this wait would time out.
    full.store(false, std::sync::atomic::Ordering::SeqCst);
    handle.trigger_scan().await.unwrap();
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
}

/// §3.6 startup sweep: exited `chuggernaut.managed` containers left behind by
/// a crash/restart are reclaimed at boot — but only when their task is terminal
/// (or gone entirely). A container a live task may still resume is kept.
/// This is the other half of the disk-leak fix: task-exit removal covers the
/// happy path, the sweep covers containers orphaned by a crash before that ran.
#[tokio::test]
async fn startup_sweep_removes_only_terminal_and_orphan_containers() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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

    // An escalated job (recovery leaves it alone) with two tasks: one already
    // terminal, one still live. Their containers exited when the process died.
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
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

    // The daemon reports three exited managed containers: the two above plus a
    // pure orphan with no task record at all (a crash before the task write).
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
    let _handle = spawn(core);

    // Reconciliation runs once at startup; wait for the sweep to settle.
    let mut removed = Vec::new();
    for _ in 0..100 {
        removed = backend.removed();
        if removed.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
async fn fleet_sweep_core(
    tasks: Vec<Task>,
    backend: Arc<FakeBackend>,
) -> Option<(NatsStore, test_utils::nats::NatsTestServer, CoreHandle)> {
    let server = test_utils::nats::NatsTestServer::spawn()?;
    let store = NatsStore::connect(server.url()).await.unwrap();
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();
    let task_store = store.tasks().await.unwrap();
    for task in &tasks {
        task_store.put(task).await.unwrap();
    }

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
    let handle = spawn(core);
    Some((store, server, handle))
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
        result: None,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        completed_at: None,
    }
}

async fn wait_killed(backend: &FakeBackend, want: usize) -> Vec<String> {
    let mut killed = Vec::new();
    for _ in 0..100 {
        killed = backend.killed();
        if killed.len() >= want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    killed
}

/// §3.6 fleet sweep: a container still running after a crash-restart but owned
/// by no live task is reaped, freeing its slot. This is the durable fix for the
/// 2026-07-22 incident, where pre-upgrade in-flight tasks were failed as
/// container-gone while their containers kept running and holding fleet slots
/// until an operator manually removed them. An identity-bearing orphan emits a
/// `container-reaped` platform event; an identity-less one (a pre-labels
/// container, exactly the incident's shape) is still reaped, event or not.
#[tokio::test]
async fn startup_fleet_sweep_reaps_orphan_running_container() {
    let backend = Arc::new(FakeBackend::new());
    // The pre-upgrade task recovery already failed as container-gone.
    let tasks = vec![work_task(2, TaskState::Failed, Some("dev-air/c-orphan"))];
    // Its container is still alive (identity resolves to job 51 / task 2), plus
    // a legacy container with no identity labels at all.
    backend.seed_managed_running([
        container::RunningContainer {
            id: "dev-air/c-orphan".into(),
            project: Some("acme/api".into()),
            job: Some(51),
            task: Some(2),
        },
        container::RunningContainer {
            id: "dev-air/c-legacy".into(),
            project: None,
            job: None,
            task: None,
        },
    ]);
    let Some((store, _server, _handle)) = fleet_sweep_core(tasks, backend.clone()).await else {
        return;
    };

    let mut killed = wait_killed(&backend, 2).await;
    killed.sort();
    assert_eq!(
        killed,
        vec![
            "dev-air/c-legacy".to_string(),
            "dev-air/c-orphan".to_string()
        ],
        "both orphan containers reaped to free their slots"
    );

    // The identity-bearing reap is attributed to its job as a platform event.
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
        // Identity resolves to the live task 1 — kept.
        container::RunningContainer {
            id: "dev-air/c-live".into(),
            project: Some("acme/api".into()),
            job: Some(51),
            task: Some(1),
        },
        // No identity labels, but its id is a live task's container_id — kept.
        container::RunningContainer {
            id: "dev-air/c-live-legacy".into(),
            project: None,
            job: None,
            task: None,
        },
    ]);
    let Some((_store, _server, _handle)) = fleet_sweep_core(tasks, backend.clone()).await else {
        return;
    };

    // Give the sweep ample time to run, then assert it reaped nothing.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        backend.killed().is_empty(),
        "containers of live Running tasks are re-attached, never reaped: {:?}",
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
    let Some((store, _server, handle)) = fleet_sweep_core(tasks, backend.clone()).await else {
        return;
    };

    // The failing sweep must reap nothing and must not wedge the actor loop:
    // a freshly created job still runs to completion.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        backend.killed().is_empty(),
        "a list error reaps nothing (log, continue)"
    );

    let job = handle.create_job(req("flaky")).await.unwrap();
    wait_for_state(&store, job.id, JobState::Done).await;
}

/// A dispatcher died mid-wrap-up: the job record says WrapUp (eval already
/// passed) and the job was parked in the in-memory merge queue, which is lost
/// on restart. Reconciliation re-enters it into the queue and it lands
/// (§2.1 WrapUp; §3.6 step 3). No gate was in flight, so the fast path squashes.
#[tokio::test]
async fn restart_lands_job_orphaned_in_wrapup() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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
    // Job branch at HEAD: nothing to merge → squash is a NoOp that still lands.
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();
    // A completed work task so ensure_exec_state can rebuild cycle/submission.
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
            result: Some(types::TaskResult::Work {
                summary: None,
                structured: None,
                token_usage: None,
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
    let _handle = spawn(core);

    // Recovery re-drives wrap-up and the job reaches Done.
    wait_for_state(&store, 1, JobState::Done).await;
}

/// The §3.6 wrap-up-command gap: a dispatcher died AFTER the squash landed on
/// main but BEFORE the `wrap_up.run` publish finished — the job is in WrapUp
/// with a Running WrapUp command task whose container is gone. Reconciliation
/// must NOT re-drive the merge queue (the merge already landed); it must
/// relaunch the pending publish, which then carries the job to Done. This is
/// the correctness requirement that killed the first attempt as churn.
#[tokio::test]
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
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/webpub.yaml", WEBPUB.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    // The squash already landed on main before the crash.
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();
    let tasks = store.tasks().await.unwrap();
    // Work completed before the crash.
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
            result: Some(types::TaskResult::Work {
                summary: None,
                structured: None,
                token_usage: None,
            }),
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: Some(Utc::now()),
        })
        .await
        .unwrap();
    // The publish task was in flight when the dispatcher died: Running, with a
    // container id that no longer exists in the fresh backend.
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
            result: None,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();

    let backend = Arc::new(FakeBackend::new()); // relaunched publish exits 0
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
    let _handle = spawn(core);

    // Recovery relaunches the publish and lands the job.
    wait_for_state(&store, 1, JobState::Done).await;

    // Exactly one relaunch happened — the publish, not a re-merge.
    let launches = backend.launches();
    assert_eq!(launches.len(), 1, "the pending publish is relaunched once");
    assert_eq!(
        launches[0].env.get("JOB_BRANCH").map(String::as_str),
        Some("main"),
        "the relaunched publish runs against merged main"
    );

    // The orphaned Running publish is retired; a fresh attempt completed it.
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
    // Both the upstream and the dependent are agent work: each run must produce
    // output to clear the §3.2 empty-output guard and reach Done.
    commit_on_run(&rig.provider, rig.repo.bare_path());
    commit_on_run(&rig.provider, rig.repo.bare_path());

    let up = rig.handle.create_job(req("flaky")).await.unwrap();
    let down = rig
        .handle
        .create_job(CreateJobRequest {
            deps: vec![],
            ..req("flaky")
        })
        .await
        .unwrap();
    // Wire down ← up by rewriting the record before release (creation API has
    // no input here because flaky declares none; use the graph as-is instead).
    // Simpler: up runs to Done; down was Blocked behind it via a crash state.
    rig.handle.release_job("acme", "api", up.id).await.unwrap();
    wait_for_state(&rig.store, up.id, JobState::Done).await;

    // Simulate: down was released Blocked before the crash (deps not Done yet),
    // then upstream finished. Write the Blocked record directly.
    let jobs = rig.store.jobs().await.unwrap();
    let mut blocked = jobs.get("acme", "api", down.id).await.unwrap().unwrap();
    blocked.state = JobState::Blocked;
    blocked.deps = vec![up.id];
    jobs.put(&blocked).await.unwrap();

    // Restart against the same store.
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
    let _handle2 = spawn(core);
    wait_for_state(&rig.store, down.id, JobState::Done).await;
}

/// A hung work container is killed at `task_timeout` and the failure path
/// applies — no retries on this type, so the job escalates.
#[tokio::test]
async fn task_timeout_kills_and_fails_hung_work() {
    let Some(rig) = rig().await else { return };
    // The "agent" hangs forever.
    rig.provider.on_run(|_| async {
        futures::future::pending::<()>().await;
    });

    let job = rig.handle.create_job(req("slow")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();
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
}

/// Human tasks never time out, but `job_deadline` summons a human exactly
/// once (§3.5 one-shot rule).
#[tokio::test]
async fn job_deadline_escalates_once_for_stalled_human_work() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(req("manual-deadline")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();
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

    // Operator retries; the deadline is now permanently disabled for this job.
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
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.handle.trigger_scan().await.unwrap();
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
async fn restart_preserves_the_submitted_summary_for_the_squash_commit() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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

    // The agent's commit is already on the job branch, as it would be.
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
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: Some(Utc::now()),
            completed_at: None,
        })
        .await
        .unwrap();
    // Crash state: the work task finished and its submission was persisted,
    // but the Work→Evaluation transition never happened.
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
            result: Some(types::TaskResult::Work {
                summary: Some("added f() with tests".into()),
                structured: None,
                token_usage: None,
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
    let _handle = spawn(core);

    wait_for_state(&store, 1, JobState::Done).await;

    // The summary survived the restart and reached the commit message *body*
    // (`build_squash_commit` passes it as a second `-m`; RepoManager::log only
    // formats `%s`, so read the full message here).
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

    // And the session id is still there, so the transcript stays addressable.
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
async fn drain_flushes_container_id_so_restart_reattaches_running_work() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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

    // A live dispatcher whose work agent hangs, so its work task stays Running
    // (with a real container id) while we drain it.
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
    let handle = spawn(core);

    let job = handle.create_job(req("flaky")).await.unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&store, job.id, JobState::Work).await;

    // Wait until the work task is Running with its container id stamped.
    let tasks = store.tasks().await.unwrap();
    let mut work = None;
    for _ in 0..100 {
        let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
        if let Some(t) = log
            .into_iter()
            .find(|t| t.phase == TaskPhase::Work && t.state == TaskState::Running)
            && t.container_id.is_some()
        {
            work = Some(t);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let work = work.expect("running work task with a container id");
    let real_cid = work.container_id.clone().unwrap();

    // Simulate the in-flight race: its `TaskContainerStarted` had not landed, so
    // the record carries no id — yet the fleet still reports the container
    // running under its identity labels.
    let mut racing = work.clone();
    racing.container_id = None;
    tasks.put(&racing).await.unwrap();
    backend.seed_managed_running([container::RunningContainer {
        id: real_cid.clone(),
        project: Some("acme/api".into()),
        job: Some(job.id),
        task: Some(work.id),
    }]);

    // Drain: the audit recovers the id, so the record is true at exit.
    handle.drain().await.unwrap();
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

    // Restart: a fresh core whose fleet still reports the container running.
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
    let _handle2 = spawn(core2);

    // Reconciliation re-attaches the container: the task stays Running on the
    // same id, with no retry, no reconcile-failure, and no synthetic -1.
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
}

/// §3.6 graceful drain: even with a busy mailbox the drain completes well under
/// its ~10s bound. Draining is non-blocking on the mailbox — it sweeps what is
/// present and returns — so a backlog of in-flight requests cannot wedge exit.
#[tokio::test]
async fn drain_completes_promptly_with_a_busy_mailbox() {
    let Some(rig) = rig().await else { return };

    // Flood the actor with in-flight requests, then drain immediately.
    let mut inflight = Vec::new();
    for _ in 0..64 {
        let h = rig.handle.clone();
        inflight.push(tokio::spawn(async move {
            let _ = h.ping().await;
        }));
    }
    let drained = tokio::time::timeout(Duration::from_secs(5), rig.handle.drain()).await;
    assert!(
        matches!(drained, Ok(Ok(()))),
        "drain did not complete under its bound with a busy mailbox: {drained:?}"
    );
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
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    // Cut the drain short almost immediately.
    let _ = tokio::time::timeout(Duration::from_millis(1), rig.handle.drain()).await;

    // Records remain parseable and the Running task was not regressed.
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
}

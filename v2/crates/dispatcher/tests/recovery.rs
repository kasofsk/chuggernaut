//! Tier-2 tests for restart reconciliation (§3.6) and the timeout/deadline
//! scans (§3.5). Crash states are constructed directly in KV — the task log
//! is the source of truth the dispatcher recovers from.

use chrono::Utc;
use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateJobRequest, spawn};
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::TempRepo;
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
    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig { repo_url_base: "file:///repos".into(), nats_url: server.url().into(), ..Default::default() },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    Some(Rig { _server: server, store, repo, provider, handle })
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
        factory: None,
    }
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
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else { return };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone.commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type").await;
    clone.commit_file("prompts/impl.md", b"implement it", "prompt").await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store.jobs().await.unwrap().put(&Job {
        id: 1,
        project: "acme/api".into(),
        r#type: "flaky".into(),
        title: String::new(),
        description: String::new(),
        deps: vec![],
        state: JobState::Work,
        branch: "job/1".into(),
        base_ref: Some(head),
        knowledge_tags: vec![],
        eval: vec![],
        factory: None,
        created_at: Utc::now(),
        ready_at: Some(Utc::now()),
    })
    .await
    .unwrap();
    store.tasks().await.unwrap().put(&Task {
        id: 1,
        job_seq: 1,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Agent { provider: "claude".into(), model: None, prompt: "prompts/impl.md".into() },
        state: TaskState::Running,
        attempt: 1,
        evaluator: None,
        container_id: None,
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
    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig { repo_url_base: "file:///repos".into(), nats_url: server.url().into(), ..Default::default() },
    )
    .await
    .unwrap();
    let _handle = spawn(core);

    wait_for_state(&store, 1, JobState::Done).await;
    let tasks = store.tasks().await.unwrap().list_for_job("acme", "api", 1).await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].state, TaskState::Failed); // the orphan
    assert_eq!((tasks[1].attempt, tasks[1].state), (2, TaskState::Done));
    assert_eq!(provider.runs().len(), 1); // only the retry ran here
}

/// Upstream reached Done while the dispatcher was dead: reconciliation
/// unblocks the dependent and runs it.
#[tokio::test]
async fn restart_unblocks_dependent_whose_deps_completed() {
    let Some(rig) = rig().await else { return };

    let up = rig.handle.create_job(req("flaky")).await.unwrap();
    let down = rig
        .handle
        .create_job(CreateJobRequest { deps: vec![], ..req("flaky") })
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
    let repos_root = rig.repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        rig.store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        rig.provider.clone(),
        CoreConfig { repo_url_base: "file:///repos".into(), nats_url: rig._server.url().into(), ..Default::default() },
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

    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
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

    let tasks = rig.store.tasks().await.unwrap().list_for_job("acme", "api", job.id).await.unwrap();
    let deadline_task = tasks.last().unwrap();
    assert!(matches!(&deadline_task.kind, TaskKind::Human { prompt } if prompt.starts_with("[deadline]")));

    // Operator retries; the deadline is now permanently disabled for this job.
    rig.handle
        .resolve_task("acme", "api", job.id, deadline_task.id,
            TaskResolution::Escalation { action: EscalationAction::Retry, structured: None },
            "david")
        .await
        .unwrap();
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.handle.trigger_scan().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = rig.store.jobs().await.unwrap().get("acme", "api", job.id).await.unwrap().unwrap();
    assert_eq!(after.state, JobState::Work, "one-shot: no second deadline escalation");
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
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else { return };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone.commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type").await;
    clone.commit_file("prompts/impl.md", b"implement it", "prompt").await;
    clone.push("main").await;
    let head = repo.head().await;

    // The agent's commit is already on the job branch, as it would be.
    repo.create_job_branch(1, &head).await;
    let work = repo.clone_branch("job/1").await;
    work.commit_file("src/f.rs", b"fn f() {}", "impl").await;
    work.push("job/1").await;

    store.jobs().await.unwrap().put(&Job {
        id: 1,
        project: "acme/api".into(),
        r#type: "flaky".into(),
        title: String::new(),
        description: String::new(),
        deps: vec![],
        state: JobState::Work,
        branch: "job/1".into(),
        base_ref: Some(head),
        knowledge_tags: vec![],
        eval: vec![],
        factory: None,
        created_at: Utc::now(),
        ready_at: Some(Utc::now()),
    })
    .await
    .unwrap();
    // Crash state: the work task finished and its submission was persisted,
    // but the Work→Evaluation transition never happened.
    store.tasks().await.unwrap().put(&Task {
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
        container_id: None,
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

    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
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
    let tasks = store.tasks().await.unwrap().list_for_job("acme", "api", 1).await.unwrap();
    assert_eq!(
        tasks[0].session_id.as_deref(),
        Some("da08d5f3-844e-430e-8363-39b4882f437b")
    );
}

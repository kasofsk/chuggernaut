//! Tier-2 execution tests: the actor core driving work + evaluation with
//! FakeBackend/FakeProvider over real NATS and real bare repos. Covers the
//! happy path (agent commits → eval passes → squash-merge → Done), work retry
//! exhaustion, and the eval-failure rework loop with §4.3 context injection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateSpec, EvalSubmission, WorkSubmission};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{EscalationAction, JobState, TaskPhase, TaskResolution, TaskState};

mod common;
use common::{assert_invariants_of, spawn_checked};

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

const IMPL_CMD_REWORK: &str = r#"
name: impl-cmd-rework
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
rework_budget: 1
eval:
  - name: ci
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

const CMD_WORK_AGENT_EVAL: &str = r#"
name: cmd-agent-eval
image: img:latest
work:
  type: command
  run: ./build.sh
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

const CMD_WORK: &str = r#"
name: cmd-work
image: img:latest
work:
  type: command
  run: ./build.sh
work_retries: 1
"#;

const STAGED: &str = r#"
name: staged
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
rework_budget: 1
eval:
  - name: review
    type: agent
    prompt: prompts/eval.md
    stage: 0
  - name: ci
    type: command
    run: ./ci.sh
    stage: 1
"#;

const STAGED_AGENTS: &str = r#"
name: staged-agents
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
rework_budget: 1
eval:
  - name: review
    type: agent
    prompt: prompts/eval.md
    stage: 0
  - name: review2
    type: agent
    prompt: prompts/eval.md
    stage: 1
"#;

const WEBPUB: &str = r#"
name: webpub
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
wrap_up:
  run: ./tasks/web-publish.sh
"#;

const WEBPUB_NAMED: &str = r#"
name: webpub-named
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
wrap_up:
  run: ./tasks/web-publish.sh
  name: publish
"#;

const STAGED_ADVISORY: &str = r#"
name: staged-advisory
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: review
    type: agent
    prompt: prompts/eval.md
    required: false
    stage: 0
  - name: ci
    type: command
    run: ./ci.sh
    stage: 1
"#;

struct Rig {
    _server: &'static test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
    handle: CoreHandle,
    /// Invariant violations the actor logged, drained by
    /// `assert_invariants_of` (refactor-plan B1a).
    invariants: InvariantSink,
}

async fn rig() -> Option<Rig> {
    rig_full(None, None).await
}

/// `artifacts_identity` enables transcript/log capture; the provider then
/// launches through the backend so runs report a container id to harvest from.
async fn rig_with_artifacts(artifacts_identity: Option<String>) -> Option<Rig> {
    rig_full(artifacts_identity, None).await
}

async fn rig_full(
    artifacts_identity: Option<String>,
    launch_queue_max_wait: Option<Duration>,
) -> Option<Rig> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in [
        ("jobs/impl-cmd.yaml", IMPL_CMD_EVAL),
        ("jobs/impl-cmd-rework.yaml", IMPL_CMD_REWORK),
        ("jobs/impl-agent.yaml", IMPL_AGENT_EVAL),
        ("jobs/flaky.yaml", FLAKY),
        ("jobs/cmd-work.yaml", CMD_WORK),
        ("jobs/cmd-agent-eval.yaml", CMD_WORK_AGENT_EVAL),
        ("jobs/staged.yaml", STAGED),
        ("jobs/staged-agents.yaml", STAGED_AGENTS),
        ("jobs/staged-advisory.yaml", STAGED_ADVISORY),
        ("jobs/webpub.yaml", WEBPUB),
        ("jobs/webpub-named.yaml", WEBPUB_NAMED),
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
            artifacts_identity,
            triage_image: Some("triage:latest".into()),
            launch_queue_max_wait,
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
        backend,
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

/// Registers a work run that commits a stub file to the job branch — the
/// minimal "the agent produced output" so the §3.2 empty-output guard is
/// satisfied and the job advances to Evaluation. Mirrors a real work
/// container's commit+push. First-cycle only: rework cycles inherit the branch.
fn commit_work(rig: &Rig) {
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        let body = format!("// work produced on {branch}\n");
        clone
            .commit_file("src/work.rs", body.as_bytes(), "work")
            .await;
        clone.push(&branch).await;
    });
}

/// The output archive stored against one task, `None` when that container's
/// [`OUTPUT_PATH`](dispatcher::platform_ops::harvest::OUTPUT_PATH) was never
/// read. Design #362 S1's scope tests ask this of a work task and of every
/// evaluator in the same job.
async fn stored_output(
    artifacts: &store::ArtifactStore,
    seq: u64,
    task_id: u64,
) -> Option<Vec<u8>> {
    artifacts
        .get("acme", "api", seq, task_id, store::ArtifactKind::Output)
        .await
        .unwrap()
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
    test_utils::wait::job_where(
        store,
        "acme",
        "api",
        seq,
        format!("job {seq} to reach {want:?}"),
        move |job| {
            if job.state == want {
                return true;
            }
            assert!(
                !matches!(
                    job.state,
                    JobState::Escalated | JobState::Stalled | JobState::Revoked
                ) || want == job.state,
                "job reached terminal-ish {:?} while waiting for {want:?}",
                job.state
            );
            false
        },
    )
    .await
}

#[tokio::test]
async fn agent_work_commits_eval_passes_squash_merges_to_done() {
    let Some(rig) = rig().await else { return };

    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/new.rs", b"pub fn f() {}", "implement")
            .await;
        clone.push(&branch).await;
    });
    rig.backend.put_file(
        "/workspace/eval-result.json",
        br#"{"coverage": 91}"#.to_vec(),
    );

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let merged = rig
        .repo
        .manager
        .read_file_at("acme", "api", "main", "src/new.rs")
        .await
        .unwrap();
    assert_eq!(merged.as_deref(), Some("pub fn f() {}"));

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].phase, TaskPhase::Work);
    assert_eq!(tasks[0].state, TaskState::Done);
    assert_eq!(tasks[1].phase, TaskPhase::Evaluation);
    match &tasks[1].result {
        Some(types::TaskResult::Command {
            pass: true,
            structured: Some(s),
            ..
        }) => {
            assert_eq!(s["coverage"], 91);
        }
        other => panic!("unexpected eval result: {other:?}"),
    }

    let events = event_types(&rig.store).await;
    for e in ["job-started", "job-evaluation-started", "job-done"] {
        assert!(events.contains(&e.to_string()), "missing {e}: {events:?}");
    }
    assert_invariants_of(&rig.invariants);
}

#[tokio::test]
async fn work_failure_retries_with_reset_then_escalates() {
    let Some(rig) = rig().await else { return };
    rig.provider.script_exits([2, 3]);

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
    assert_eq!(tasks.len(), 3);
    assert_eq!((tasks[0].attempt, tasks[0].state), (1, TaskState::Failed));
    assert_eq!((tasks[1].attempt, tasks[1].state), (2, TaskState::Failed));
    assert!(matches!(tasks[2].kind, types::TaskKind::Human { .. }));
    assert_eq!(tasks[2].state, TaskState::Pending);
    assert_eq!(rig.provider.runs().len(), 2);
    assert_invariants_of(&rig.invariants);
}

/// §3.2 empty-output guard: a work container that exits 0 but leaves the branch
/// empty AND submits no summary (the job-79 finish-line signature — a headless
/// agent that ended its turn before committing) is a genuine failure, not a
/// success. Each attempt fails with reason `no_output_produced`, a work retry is
/// burned and the attempt relaunches, and the job NEVER enters Evaluation.
#[tokio::test]
async fn work_exit0_empty_branch_empty_summary_fails_with_no_output() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    assert_eq!(
        rig.provider.runs().len(),
        2,
        "attempt + one relaunched retry"
    );

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let works: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Work && !matches!(t.kind, types::TaskKind::Human { .. }))
        .collect();
    assert_eq!(works.len(), 2, "attempt 1 + one retry, both failed");
    for t in &works {
        assert_eq!(t.state, TaskState::Failed);
        match &t.result {
            Some(types::TaskResult::Command {
                pass: false,
                output,
                structured: Some(s),
                ..
            }) => {
                assert_eq!(output, "exited without producing changes");
                assert_eq!(s["reason"], "no_output_produced");
            }
            other => panic!("expected a no-output failure result, got {other:?}"),
        }
    }
    assert!(
        !tasks.iter().any(|t| t.phase == TaskPhase::Evaluation),
        "no Evaluation task may exist: {tasks:?}"
    );
    assert_eq!(
        escalated.escalation.expect("escalation recorded").reason,
        "work_retries_exhausted"
    );

    let failed: Vec<_> = job_events(&rig.store, job.id)
        .await
        .into_iter()
        .filter(|e| e["event_type"] == "task-failed")
        .collect();
    assert!(
        failed.iter().any(|e| e["reason"] == "no_output_produced"),
        "a task-failed event must carry the machine reason: {failed:?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// §3.2 empty-output guard, exception path: an empty branch with a NON-empty
/// summary is a deliberate "no change is the correct outcome", not a finish-line
/// death — so it proceeds to Evaluation (here: no evaluators → straight to Done)
/// and no retry is burned.
#[tokio::test]
async fn work_exit0_empty_branch_with_summary_proceeds() {
    let Some(rig) = rig().await else { return };
    let h = rig.handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_result(
            "acme",
            "api",
            1,
            WorkSubmission {
                summary: Some("no code change is the correct outcome here".into()),
                structured: None,
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    assert_eq!(
        rig.provider.runs().len(),
        1,
        "no relaunch: the guard passed"
    );
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let work = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Work)
        .expect("work task");
    assert_eq!(work.state, TaskState::Done);
    match &work.result {
        Some(types::TaskResult::Work {
            summary: Some(s), ..
        }) => assert!(s.contains("no code change")),
        other => panic!("expected a Work result carrying the summary, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

/// §3.2 empty-output guard, regression: exit 0 WITH commits is unchanged even
/// when no summary is submitted — a real work run advances to Evaluation.
#[tokio::test]
async fn work_exit0_with_commits_and_no_summary_proceeds() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let work = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Work)
        .expect("work task");
    assert_eq!(
        work.state,
        TaskState::Done,
        "commits present → work succeeds"
    );
    assert_invariants_of(&rig.invariants);
}

/// Dogfood-#1 regression: an eval container that fails to *launch* must not
/// leave the task `Running` and the job wedged in `Evaluation`. The launch
/// error flows through the task-failure machinery: task Failed with the error
/// in its result → eval_retries → required infra failure → job Escalated.
#[tokio::test]
async fn eval_launch_failure_escalates_instead_of_stuck_running() {
    let Some(rig) = rig().await else { return };
    rig.backend
        .fail_launch_if(|_| Some("invalid memory limit \"5g\"".into()));
    commit_work(&rig);

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
    assert_eq!(tasks[0].phase, TaskPhase::Work);
    assert_eq!(tasks[0].state, TaskState::Done);
    let evals: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert_eq!(evals.len(), 2, "eval attempt + one eval_retries");
    for t in &evals {
        assert_eq!(t.state, TaskState::Failed);
        match &t.result {
            Some(types::TaskResult::Command {
                pass: false,
                output,
                ..
            }) => assert_eq!(
                output,
                "container launch failed: invalid memory limit \"5g\""
            ),
            other => panic!("expected launch-error result, got {other:?}"),
        }
    }
    assert!(
        tasks.iter().all(|t| t.state != TaskState::Running),
        "no task left Running: {tasks:?}"
    );
    assert!(
        tasks
            .iter()
            .any(|t| matches!(t.kind, types::TaskKind::Human { .. })
                && t.state == TaskState::Pending),
        "a Human escalation task names the failure"
    );
    assert_invariants_of(&rig.invariants);
}

/// Work-path parity: a command work container that fails to launch takes the
/// same route the agent path already does (provider error → exit -1) — task
/// Failed with the launch error recorded, work_retries consumed, then Escalated.
#[tokio::test]
async fn work_command_launch_failure_retries_then_escalates() {
    let Some(rig) = rig().await else { return };
    rig.backend
        .fail_launch_if(|_| Some("invalid memory limit \"5g\"".into()));

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let esc = escalated
        .escalation
        .expect("escalation recorded on the job");
    assert_eq!(esc.reason, "work_retries_exhausted");
    assert!(
        esc.detail.contains("no retries left"),
        "detail: {}",
        esc.detail
    );
    assert!(esc.failing_task.is_some(), "failing task recorded");

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let works: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Work && !matches!(t.kind, types::TaskKind::Human { .. }))
        .collect();
    assert_eq!(esc.failing_task, works.last().map(|t| t.id));
    assert_eq!(works.len(), 2, "attempt + one work_retries");
    for t in &works {
        assert_eq!(t.state, TaskState::Failed);
        match &t.result {
            Some(types::TaskResult::Command {
                pass: false,
                output,
                ..
            }) => assert_eq!(
                output,
                "container launch failed: invalid memory limit \"5g\""
            ),
            other => panic!("expected launch-error result, got {other:?}"),
        }
    }
    assert!(
        tasks.iter().all(|t| t.state != TaskState::Running),
        "no task left Running: {tasks:?}"
    );
    assert!(
        tasks
            .iter()
            .any(|t| matches!(t.kind, types::TaskKind::Human { .. })),
        "escalation task exists"
    );
    assert_invariants_of(&rig.invariants);
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn eval_failure_reworks_with_context_then_passes() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["missing tests"]})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    rig.provider.on_run(|_| async {});
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            4,
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
    });

    let mut create = req("impl-agent");
    create.title = "Add fortune file".into();
    create.description = "Create fortune.txt with an aphorism.".into();
    let job = rig.handle.create_job(create).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 4);
    assert!(runs[0].eval_context.is_empty());
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

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 4);
    assert_eq!(tasks[2].cycle, 2);
    assert_eq!(tasks[0].rework_reason, None);
    assert_eq!(
        tasks[2].rework_reason,
        Some(types::ReworkReason::EvalFailure)
    );
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-rework-started".to_string()));
    assert_invariants_of(&rig.invariants);
}

/// Job #155: a cycle-2 agent evaluator receives a re-review context block —
/// its prior verdict/findings, the SHA it last reviewed, the delta diff since,
/// and a job-history digest — while the cycle-1 review is unchanged.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn cycle2_evaluator_gets_prior_review_context() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    let bare = rig.repo.bare_path();

    let b = bare.clone();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&b, &branch).await;
        clone.commit_file("src/a.rs", b"pub fn a() {}", "a").await;
        clone.push(&branch).await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["needs docstrings"]})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    let b = bare.clone();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&b, &branch).await;
        clone.commit_file("src/b.rs", b"pub fn b() {}", "b").await;
        clone.push(&branch).await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            4,
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
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 4, "work c1, eval c1, work c2, eval c2");
    let eval_c1 = &runs[1];
    assert!(
        !eval_c1.prompt.contains("Re-Review Context"),
        "cycle-1 eval must not carry re-review context: {}",
        eval_c1.prompt
    );
    let eval_c2 = &runs[3];
    let p = &eval_c2.prompt;
    assert!(p.contains("Re-Review Context"), "{p}");
    assert!(
        p.contains("needs docstrings"),
        "prior findings missing: {p}"
    );
    assert!(p.contains("Last-reviewed tip"), "reviewed SHA missing: {p}");
    assert!(p.contains("```diff"), "delta diff block missing: {p}");
    assert!(
        p.contains("src/b.rs"),
        "delta should mention the new file: {p}"
    );
    assert!(
        p.contains("Job history at a glance"),
        "history digest missing: {p}"
    );
    assert!(p.contains("reviewer=fail"), "history verdict missing: {p}");
    assert_invariants_of(&rig.invariants);
}

/// Job #155 rebase fallback: when the cycle-2 review follows a conflict rework
/// (the branch was rebased onto a moved base), the re-review block says so and
/// omits a bogus delta rather than diffing across the rebase.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn cycle2_evaluator_after_rebase_gets_full_diff_note() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    let bare = rig.repo.bare_path();

    let b = bare.clone();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&b, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change", "implement")
            .await;
        clone.push(&branch).await;
        let main = clone_branch_from(&b, "main").await;
        main.commit_file("src/a.rs", b"conflicting land", "other")
            .await;
        main.push("main").await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: true,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["nit: naming"]})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    let b = bare.clone();
    rig.provider.on_run(move |cfg| async move {
        assert!(cfg.merge_conflict.is_some(), "conflict context expected");
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&b, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change v2", "again")
            .await;
        clone.push(&branch).await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            4,
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
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 4);
    let eval_c2 = &runs[3];
    let p = &eval_c2.prompt;
    assert!(p.contains("Re-Review Context"), "{p}");
    assert!(p.contains("nit: naming"), "prior findings missing: {p}");
    assert!(p.contains("rebased"), "rebase note missing: {p}");
    assert!(
        !p.contains("```diff"),
        "delta diff must be suppressed across a rebase: {p}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Job #155: an evaluator that first appears on cycle 2 gets the unchanged
/// cycle-1 (no-context) form — there is no prior review of its own to carry.
/// staged-agents fails its stage-0 `review` on cycle 1, so the stage-1 `review2`
/// never runs that cycle; on cycle 2 `review` carries re-review context while
/// `review2`, appearing for the first time, does not.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn evaluator_first_appearing_on_cycle2_gets_no_context() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    let bare = rig.repo.bare_path();

    let b = bare.clone();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&b, &branch).await;
        clone.commit_file("src/a.rs", b"pub fn a() {}", "a").await;
        clone.push(&branch).await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["fix stage 0"]})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    let b = bare.clone();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&b, &branch).await;
        clone.commit_file("src/b.rs", b"pub fn b() {}", "b").await;
        clone.push(&branch).await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            4,
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
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            5,
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
    });

    let job = rig.handle.create_job(req("staged-agents")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(
        runs.len(),
        5,
        "work c1, review c1, work c2, review c2, review2 c2"
    );
    let review_c2 = &runs[3];
    assert!(
        review_c2.prompt.contains("Re-Review Context"),
        "pre-existing evaluator must carry re-review context: {}",
        review_c2.prompt
    );
    let review2_c2 = &runs[4];
    assert!(
        !review2_c2.prompt.contains("Re-Review Context"),
        "an evaluator first appearing on cycle 2 gets the cycle-1 (no-context) form: {}",
        review2_c2.prompt
    );
    assert_invariants_of(&rig.invariants);
}

/// §3.2 crash recovery: an attempt that pushes commits then crashes is retried
/// on the SAME branch — the commits survive, the retry's prompt notes the
/// resume, and the recovered work lands on merge instead of being redone.
#[tokio::test]
async fn crashed_work_attempt_recovers_branch_and_notes_resume() {
    let Some(rig) = rig().await else { return };
    let bare = rig.repo.bare_path();

    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("wip.rs", b"partial work", "wip").await;
        clone.push(&branch).await;
    });
    rig.provider.script_exits([2, 0]);

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 2, "one crash, one recovered retry");
    assert!(
        !runs[0].prompt.contains("Resuming a Previous Attempt"),
        "the first attempt is not a resume: {}",
        runs[0].prompt
    );
    assert!(
        runs[1].prompt.contains("Resuming a Previous Attempt"),
        "the retry prompt must note the resume: {}",
        runs[1].prompt
    );

    let merged = rig
        .repo
        .manager
        .read_file_at("acme", "api", "main", "wip.rs")
        .await
        .unwrap();
    assert_eq!(
        merged.as_deref(),
        Some("partial work"),
        "the recovered commit should land on main"
    );
    assert_invariants_of(&rig.invariants);
}

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Poll the task log for the first task matching `pred`.
async fn wait_for_task(
    store: &NatsStore,
    seq: u64,
    pred: impl Fn(&types::Task) -> bool,
) -> types::Task {
    test_utils::wait::task_where(
        store,
        "acme",
        "api",
        seq,
        format!("a task of job {seq} matching predicate"),
        pred,
    )
    .await
}

/// A command evaluator that can't be placed (fleet full → `NoCapacity`) is
/// *queued* Pending, not Failed — no `eval_retries` burned, the job holds in
/// Evaluation — and launches when a slot frees. The periodic scan drives the
/// drain once capacity returns (spec §3.5).
#[tokio::test]
async fn eval_launch_queues_on_no_capacity_then_launches_when_freed() {
    let Some(rig) = rig().await else { return };
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/new.rs", b"pub fn f() {}", "impl")
            .await;
        clone.push(&branch).await;
    });
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let full = Arc::new(AtomicBool::new(true));
    let f = full.clone();
    rig.backend.fail_launch_no_capacity_if(move |_| {
        f.load(Ordering::SeqCst)
            .then(|| "no free slots on any node".to_string())
    });

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let eval = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Evaluation && t.state == TaskState::Pending
    })
    .await;
    assert_eq!(eval.attempt, 1, "queueing consumed no eval_retries");
    assert!(
        eval.container_id.is_none(),
        "a queued task holds no container"
    );
    let jobs = rig.store.jobs().await.unwrap();
    assert_eq!(
        jobs.get("acme", "api", job.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Evaluation,
        "queued, not escalated",
    );
    assert!(
        event_types(&rig.store)
            .await
            .contains(&"task-queued".into()),
        "a task-queued event surfaces the wait",
    );

    full.store(false, Ordering::SeqCst);
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let evals: Vec<_> = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert_eq!(
        evals.len(),
        1,
        "one eval task launched from the queue — no retry inflation"
    );
    assert_eq!(evals[0].state, TaskState::Done);
    assert_invariants_of(&rig.invariants);
}

/// Work-path parity: a command work container that can't be placed queues
/// Pending — no `work_retries` burned — and launches when a slot frees (§3.5).
#[tokio::test]
async fn command_work_queues_on_no_capacity_then_launches_when_freed() {
    let Some(rig) = rig().await else { return };
    let full = Arc::new(AtomicBool::new(true));
    let f = full.clone();
    rig.backend.fail_launch_no_capacity_if(move |_| {
        f.load(Ordering::SeqCst)
            .then(|| "no free slots on any node".to_string())
    });

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let work = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Work && t.state == TaskState::Pending
    })
    .await;
    assert_eq!(work.attempt, 1, "queueing consumed no work_retries");
    assert!(work.container_id.is_none());
    assert_eq!(
        work.pending_reason,
        Some(types::PendingReason::QueuedForCapacity)
    );
    assert!(work.queued_at.is_some());
    let snap = rig.handle.queue_snapshot("acme", "api").await.unwrap();
    assert_eq!(snap.depth, 1);
    assert_eq!(snap.entries.len(), 1);
    assert_eq!(snap.entries[0].position, 1);
    assert_eq!((snap.entries[0].seq, snap.entries[0].task_id), (job.id, 1));
    let jobs = rig.store.jobs().await.unwrap();
    assert_eq!(
        jobs.get("acme", "api", job.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Work,
    );
    assert!(
        event_types(&rig.store)
            .await
            .contains(&"task-queued".into())
    );

    full.store(false, Ordering::SeqCst);
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let works: Vec<_> = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Work)
        .collect();
    assert_eq!(works.len(), 1, "one work task launched from the queue");
    assert_eq!(works[0].state, TaskState::Done);
    assert_eq!(works[0].pending_reason, None);
    assert_eq!(works[0].queued_at, None);
    assert_eq!(
        rig.handle
            .queue_snapshot("acme", "api")
            .await
            .unwrap()
            .depth,
        0
    );
    assert_invariants_of(&rig.invariants);
}

/// A command WORK task whose stdout carries `@chug:leg`/`@chug:report` lines
/// (ticket #187) has them harvested into the work task's structured result: the
/// valid legs survive in order, a malformed leg line is dropped without failing
/// the harvest, ordinary output is ignored, and the envelope merges.
/// The FAILED sibling of the harvest test (#207 review finding): a command
/// work run that dies mid-deploy must still carry its harvested leg report —
/// which leg failed and which never ran is exactly what the record is for.
/// Previously the harvest landed only on the exit-0 path and a failed deploy
/// dropped it.
#[tokio::test]
async fn failed_command_work_still_carries_harvested_leg_report() {
    let Some(rig) = rig().await else { return };
    let logs = concat!(
        "@chug:leg {\"name\":\"build-dispatcher\",\"status\":\"ok\",\"secs\":41}\n",
        "@chug:leg {\"name\":\"build-images\",\"status\":\"failed\",\"secs\":7,\"error\":\"ENOSPC\"}\n",
        "@chug:leg {\"name\":\"restart-verify\",\"status\":\"skipped\"}\n",
        "@chug:report {\"from_sha\":\"prev999\",\"to_sha\":\"abc123\",\"rollback\":false}\n",
    );
    rig.backend.put_logs(logs.as_bytes().to_vec());
    rig.backend.script_exits([1]);

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let failed = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Work && t.state == TaskState::Failed
    })
    .await;
    let Some(types::TaskResult::Command {
        pass: false,
        exit_code: 1,
        structured: Some(value),
        ..
    }) = &failed.result
    else {
        panic!(
            "failed run must keep its harvested report, got {:?}",
            failed.result
        );
    };
    let report: types::DeployReport = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(report.legs.len(), 3);
    assert_eq!(report.legs[1].status, types::LegStatus::Failed);
    assert_eq!(report.legs[2].status, types::LegStatus::Skipped);
    assert_eq!(report.to_sha.as_deref(), Some("abc123"));
    assert_invariants_of(&rig.invariants);
}

#[tokio::test]
async fn command_work_harvests_deploy_legs_into_structured_result() {
    let Some(rig) = rig().await else { return };
    let logs = concat!(
        "update: deploying abc123\n",
        "@chug:leg {\"name\":\"build-dispatcher\",\"status\":\"ok\",\"secs\":41}\n",
        "some other output the harvest must ignore\n",
        "@chug:leg {\"name\":\"build-images\",\"status\":\"ok\",\"secs\":7}\n",
        "@chug:leg {not valid json at all}\n",
        "@chug:leg {\"name\":\"restart-verify\",\"status\":\"failed\",\"secs\":3,\"error\":\"health timed out\"}\n",
        "@chug:leg {\"name\":\"sha-advance\",\"status\":\"skipped\"}\n",
        "@chug:report {\"from_sha\":\"prev999\",\"to_sha\":\"abc123\",\"rollback\":true,\"health\":\"degraded\"}\n",
        "update: done\n",
    );
    rig.backend.put_logs(logs.as_bytes().to_vec());

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let works: Vec<_> = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Work)
        .collect();
    assert_eq!(works.len(), 1);
    let Some(types::TaskResult::Work {
        structured: Some(value),
        ..
    }) = &works[0].result
    else {
        panic!(
            "expected a structured deploy report, got {:?}",
            works[0].result
        );
    };
    let report: types::DeployReport = serde_json::from_value(value.clone()).unwrap();

    assert_eq!(report.legs.len(), 4, "malformed leg dropped: {report:?}");
    assert_eq!(report.legs[0].name, "build-dispatcher");
    assert_eq!(report.legs[0].status, types::LegStatus::Ok);
    assert_eq!(report.legs[0].secs, Some(41));
    assert_eq!(report.legs[1].name, "build-images");
    assert_eq!(report.legs[2].name, "restart-verify");
    assert_eq!(report.legs[2].status, types::LegStatus::Failed);
    assert_eq!(report.legs[2].error.as_deref(), Some("health timed out"));
    assert_eq!(report.legs[3].name, "sha-advance");
    assert_eq!(report.legs[3].status, types::LegStatus::Skipped);
    assert_eq!(report.legs[3].secs, None);
    assert_eq!(report.from_sha.as_deref(), Some("prev999"));
    assert_eq!(report.to_sha.as_deref(), Some("abc123"));
    assert!(report.rollback);
    assert_eq!(report.health.as_deref(), Some("degraded"));
    assert_invariants_of(&rig.invariants);
}

/// A command WORK task that emits no `@chug:` markers (an ordinary build/deploy
/// with plain output) gets no structured result — the harvest leaves an
/// ordinary command task untouched (ticket #187).
#[tokio::test]
async fn command_work_without_legs_has_no_structured_result() {
    let Some(rig) = rig().await else { return };
    rig.backend
        .put_logs(b"just some ordinary build output\nno markers here\n".to_vec());

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let work = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.phase == TaskPhase::Work)
        .unwrap();
    match work.result {
        Some(types::TaskResult::Work { structured, .. }) => {
            assert!(structured.is_none(), "no markers → no structured report");
        }
        other => panic!("expected a Work result, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

/// A launch wedged in the queue past the maximum wait escalates with the clear
/// `no_free_slots_timeout` reason — the backstop for a genuinely stuck fleet
/// (§3.5). The rig shrinks the max wait so the scan fires it immediately.
#[tokio::test]
async fn queued_launch_escalates_after_max_wait() {
    let Some(rig) = rig_full(None, Some(Duration::from_millis(300))).await else {
        return;
    };
    rig.backend
        .fail_launch_no_capacity_if(|_| Some("no free slots on any node".to_string()));

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Work && t.state == TaskState::Pending
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let events = event_types(&rig.store).await;
    assert!(events.contains(&"task-queued".into()));
    assert!(events.contains(&"job-escalated".into()));
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.phase == TaskPhase::Work && t.state == TaskState::Failed),
        "the queued task is failed on timeout, not left Pending: {tasks:?}",
    );
    assert!(
        tasks
            .iter()
            .any(|t| matches!(t.kind, types::TaskKind::Human { .. })
                && t.state == TaskState::Pending),
        "an escalation task is raised",
    );
    assert_invariants_of(&rig.invariants);
}

/// Priority inversion fix (#140): when the fleet is full and both an eval launch
/// and a work launch are queued, freeing a single slot must launch the *eval*
/// first — a finishing-phase launch drains ahead of queued work, so a job that
/// has finished its work never loses its evaluation slot to one that has not
/// started. The work launch stays queued.
#[tokio::test]
async fn queued_eval_drains_before_queued_work() {
    let Some(rig) = rig().await else { return };
    let full = Arc::new(AtomicBool::new(true));
    let permits = Arc::new(AtomicUsize::new(0));
    let (f, p) = (full.clone(), permits.clone());
    rig.backend.fail_launch_no_capacity_if(move |_| {
        if f.load(Ordering::SeqCst) {
            return Some("no free slots on any node".to_string());
        }
        (p.fetch_add(1, Ordering::SeqCst) >= 1).then(|| "no free slots on any node".to_string())
    });
    commit_work(&rig);
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let eval_job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .release_job("acme", "api", eval_job.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    let work_job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .release_job("acme", "api", work_job.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);

    wait_for_task(&rig.store, eval_job.id, |t| {
        t.phase == TaskPhase::Evaluation && t.state == TaskState::Pending
    })
    .await;
    wait_for_task(&rig.store, work_job.id, |t| {
        t.phase == TaskPhase::Work && t.state == TaskState::Pending
    })
    .await;

    full.store(false, Ordering::SeqCst);
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, eval_job.id, JobState::Done).await;

    let work = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", work_job.id)
        .await
        .unwrap();
    let work_task = work.iter().find(|t| t.phase == TaskPhase::Work).unwrap();
    assert_eq!(
        work_task.state,
        TaskState::Pending,
        "the queued work launch must wait behind the eval launch (#140)",
    );
    assert!(work_task.container_id.is_none());
    let work_state = rig
        .store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", work_job.id)
        .await
        .unwrap()
        .unwrap()
        .state;
    assert_eq!(work_state, JobState::Work, "work job still awaiting a slot");
    assert_invariants_of(&rig.invariants);
}

/// A starved *eval* launch that outwaits the queue escalates with the
/// `no_free_slots_timeout` reason — never by burning `eval_retries` on instant
/// retries (#140). Same backstop the work path uses; this pins the eval arm.
#[tokio::test]
async fn queued_eval_escalates_after_max_wait_not_retry_exhaustion() {
    let Some(rig) = rig_full(None, Some(Duration::from_millis(300))).await else {
        return;
    };
    rig.backend
        .fail_launch_no_capacity_if(|_| Some("no free slots on any node".to_string()));
    commit_work(&rig);

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let eval = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Evaluation && t.state == TaskState::Pending
    })
    .await;
    assert_eq!(eval.attempt, 1, "queueing burns no eval_retries");
    tokio::time::sleep(Duration::from_millis(400)).await;
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    assert_eq!(
        escalated.escalation.unwrap().reason,
        "no_free_slots_timeout",
        "starved eval escalates on the queue timeout, not eval_infra_failure",
    );
    assert_invariants_of(&rig.invariants);
}

/// An **agent** evaluator (the #125/#130 shape: `review-web` on a saturated web
/// fleet) queues on `NoCapacity` instead of instant-failing and burning
/// `eval_retries` (#140). The provider erases the variant, so the spawned run
/// signals it back and the actor parks the eval Pending; freeing a slot
/// relaunches the *same* task, which passes → the job lands. The command work
/// took the earlier slot, so only the eval — the finishing-phase launch — waits.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn agent_eval_queues_on_no_capacity_then_launches_when_freed() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_full(Some(identity), None).await else {
        return;
    };
    let full = Arc::new(AtomicBool::new(true));
    let f = full.clone();
    rig.backend.fail_launch_no_capacity_if(move |cfg| {
        (f.load(Ordering::SeqCst) && cfg.cmd.iter().any(|c| c == "agent"))
            .then(|| "no free slots on any node".to_string())
    });
    let h = rig.handle.clone();
    rig.provider.on_run(move |_| async move {
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
    });

    let job = rig.handle.create_job(req("cmd-agent-eval")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let eval = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Evaluation && t.state == TaskState::Pending
    })
    .await;
    assert_eq!(
        eval.attempt, 1,
        "queueing an agent eval burns no eval_retries"
    );
    assert!(
        eval.container_id.is_none(),
        "a queued task holds no container"
    );
    assert!(
        matches!(eval.kind, types::TaskKind::Agent { .. }),
        "the queued launch is the agent evaluator, not the command work",
    );
    assert_eq!(
        rig.store
            .jobs()
            .await
            .unwrap()
            .get("acme", "api", job.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Evaluation,
        "queued, not escalated as eval_infra_failure",
    );
    assert!(
        event_types(&rig.store)
            .await
            .contains(&"task-queued".into()),
        "a task-queued event surfaces the wait",
    );

    full.store(false, Ordering::SeqCst);
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let evals: Vec<_> = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert_eq!(
        evals.len(),
        1,
        "one eval task drained from the queue — no retry inflation"
    );
    assert_eq!(evals[0].state, TaskState::Done);
    assert_invariants_of(&rig.invariants);
}

/// A starved *agent* eval that outwaits the queue escalates with the
/// `no_free_slots_timeout` reason — never by burning `eval_retries` on instant
/// retries (#140). Pins the agent arm of the queue-timeout backstop, the exact
/// failure mode #125/#130 hit before the fix.
#[tokio::test]
async fn agent_eval_escalates_after_max_wait_not_retry_exhaustion() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_full(Some(identity), Some(Duration::from_millis(100))).await else {
        return;
    };
    rig.backend.fail_launch_no_capacity_if(|cfg| {
        cfg.cmd
            .iter()
            .any(|c| c == "agent")
            .then(|| "no free slots on any node".to_string())
    });

    let job = rig.handle.create_job(req("cmd-agent-eval")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    assert_eq!(
        escalated.escalation.unwrap().reason,
        "no_free_slots_timeout",
        "starved agent eval escalates on the queue timeout, not eval_infra_failure",
    );
    let eval = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Evaluation && t.state == TaskState::Failed
    })
    .await;
    assert_eq!(eval.attempt, 1, "queue starvation burns no eval_retries");
    let output = match eval.result {
        Some(types::TaskResult::Command { ref output, .. }) => output.clone(),
        ref other => panic!("expected the queue-timeout Command result, got {other:?}"),
    };
    assert!(
        output.contains("launch queue"),
        "failure records the queue wait: {output}"
    );
    assert_invariants_of(&rig.invariants);
}

/// A queued agent eval that is *resumed* but re-hits `NoCapacity` (it lost the
/// freed slot to a concurrent launch) re-defers instead of failing — and the
/// re-defer must **preserve** the original `queued_at` so the max-wait backstop
/// keeps accumulating rather than resetting on each resume (#140 rework). The
/// eval queues (T0), a slot frees, the first resume loses the race and
/// re-defers, and the next resume places. The re-defer keeps `queued_at == T0`
/// (never restamped), exactly one eval task runs at attempt 1 (no `eval_retries`
/// burned), and the resumed record drops its `QueuedForCapacity` badge.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn agent_eval_redefer_preserves_queue_time_and_burns_no_retries() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_full(Some(identity), None).await else {
        return;
    };
    let full = Arc::new(AtomicBool::new(true));
    let redefer = Arc::new(AtomicBool::new(true));
    let (f, rd) = (full.clone(), redefer.clone());
    rig.backend.fail_launch_no_capacity_if(move |cfg| {
        if !cfg.cmd.iter().any(|c| c == "agent") {
            return None;
        }
        if f.load(Ordering::SeqCst) || rd.swap(false, Ordering::SeqCst) {
            return Some("no free slots on any node".to_string());
        }
        None
    });
    let h = rig.handle.clone();
    rig.provider.on_run(move |_| async move {
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
    });

    let job = rig.handle.create_job(req("cmd-agent-eval")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let queued = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Evaluation && t.state == TaskState::Pending
    })
    .await;
    assert_eq!(queued.attempt, 1, "queueing burns no eval_retries");
    let first_queued_at = queued.queued_at.expect("a queued eval carries queued_at");
    tokio::time::sleep(Duration::from_millis(20)).await;

    full.store(false, Ordering::SeqCst);
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;
    assert!(
        !redefer.load(Ordering::SeqCst),
        "the resume→re-defer path ran (the one-shot slot-race loss was consumed)"
    );

    let evals: Vec<_> = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert_eq!(
        evals.len(),
        1,
        "one eval task drained across the re-defers — no retry inflation"
    );
    let eval = &evals[0];
    assert_eq!(eval.state, TaskState::Done);
    assert_eq!(
        eval.attempt, 1,
        "re-deferring on NoCapacity never burns an eval_retries attempt"
    );
    assert_eq!(
        eval.pending_reason, None,
        "a resumed-then-completed eval clears its QueuedForCapacity badge"
    );
    assert_eq!(
        eval.queued_at,
        Some(first_queued_at),
        "every re-defer preserved the original queued_at (backstop accumulates, no reset)"
    );
    assert_invariants_of(&rig.invariants);
}

/// Eval-exhaustion escalation + Retry (#141) resumes at Evaluation: it re-runs
/// the evaluators against the preserved branch and never launches a fresh work
/// task. The work attempt counter and cycle are untouched; a fresh eval fan-out
/// passes → the job lands.
#[tokio::test]
async fn eval_exhaustion_retry_reruns_evaluation_without_new_work() {
    let Some(rig) = rig().await else { return };
    let fail = Arc::new(AtomicBool::new(true));
    let f = fail.clone();
    rig.backend.fail_launch_if(move |_| {
        f.load(Ordering::SeqCst)
            .then(|| "invalid memory limit \"5g\"".to_string())
    });
    commit_work(&rig);
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks_now = || async {
        rig.store
            .tasks()
            .await
            .unwrap()
            .list_for_job("acme", "api", job.id)
            .await
            .unwrap()
    };
    let before = tasks_now().await;
    let work_before = before.iter().filter(|t| t.phase == TaskPhase::Work).count();
    assert_eq!(work_before, 1, "exactly one work task ran");
    let esc = before
        .iter()
        .find(|t| matches!(t.kind, types::TaskKind::Human { .. }) && t.state == TaskState::Pending)
        .expect("a Human escalation task");
    assert_eq!(esc.phase, TaskPhase::Escalation);

    fail.store(false, Ordering::SeqCst);
    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            esc.id,
            TaskResolution::Escalation {
                action: EscalationAction::Retry,
                structured: None,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let after = tasks_now().await;
    let works: Vec<_> = after
        .iter()
        .filter(|t| t.phase == TaskPhase::Work)
        .collect();
    assert_eq!(works.len(), 1, "Retry re-ran evaluation, not work (#141)");
    assert_eq!(works[0].attempt, 1, "work attempt counter untouched");
    assert!(
        after
            .iter()
            .any(|t| t.phase == TaskPhase::Evaluation && t.state == TaskState::Done),
        "the retried evaluation produced a passing eval task: {after:?}",
    );
    assert!(
        after.iter().all(|t| t.cycle == 1),
        "cycle stays 1 across the escalation retry: {after:?}",
    );
    assert_invariants_of(&rig.invariants);
}

/// A wrap-up task carries its label from job-type config (#146): the explicit
/// `wrap_up.name` when set, and a derived default (the script basename) when not.
#[tokio::test]
async fn wrap_up_task_carries_label() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    commit_work(&rig);
    let named = rig.handle.create_job(req("webpub-named")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .release_job("acme", "api", named.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, named.id, JobState::Done).await;
    let wrap = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", named.id)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.phase == TaskPhase::WrapUp)
        .expect("a wrap-up task");
    assert_eq!(wrap.label.as_deref(), Some("publish"));

    let derived = rig.handle.create_job(req("webpub")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .release_job("acme", "api", derived.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, derived.id, JobState::Done).await;
    let wrap = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", derived.id)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.phase == TaskPhase::WrapUp)
        .expect("a wrap-up task");
    assert_eq!(wrap.label.as_deref(), Some("web-publish"));
    assert_invariants_of(&rig.invariants);
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

/// Full event payloads (one job per rig, so no per-seq filtering is needed).
async fn job_events(store: &NatsStore, _seq: u64) -> Vec<serde_json::Value> {
    store
        .read_stream("job-events", 100)
        .await
        .unwrap()
        .iter()
        .map(|p| serde_json::from_slice(p).unwrap())
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
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn agent_launch_carries_channel_mcp_and_decrypted_secrets() {
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
        .commit_file("jobs/impl-secret.yaml", IMPL_SECRET.as_bytes(), "t")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement", "p")
        .await;
    clone.push("main").await;

    let (identity, public_key) = store::secrets::generate_age_keypair();
    let secrets_bucket = store.raw_bucket(store::buckets::SECRETS).await.unwrap();
    {
        use store::secrets::SecretStore;
        let api_side =
            store::secrets::AgeSecretStore::for_api(secrets_bucket, &public_key).unwrap();
        api_side
            .set("acme", "api", "DEPLOY_KEY", "s3cret-value")
            .await
            .unwrap();
        api_side
            .set("global", "agents", "PROVIDER_TOKEN", "tok-123")
            .await
            .unwrap();
    }

    let fake_binary = repo
        .bare_path()
        .parent()
        .unwrap()
        .join("chuggernaut-channel");
    tokio::fs::write(&fake_binary, b"#!/bin/sh\nexit 0\n")
        .await
        .unwrap();

    let ssh_ca = repo.bare_path().parent().unwrap().join("ssh_ca");
    let status = tokio::process::Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            ssh_ca.to_str().unwrap(),
        ])
        .status()
        .await
        .unwrap();
    assert!(status.success());

    let provider = Arc::new(FakeProvider::new());
    {
        let bare = repo.bare_path();
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
    let (handle, sink) = spawn_checked(core);

    let job = handle.create_job(req("impl-secret")).await.unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);
    wait_for_state(&store, job.id, JobState::Done).await;

    let runs = provider.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].env.get("DEPLOY_KEY").map(String::as_str),
        Some("s3cret-value")
    );
    assert_eq!(
        runs[0].env.get("PROVIDER_TOKEN").map(String::as_str),
        Some("tok-123")
    );
    assert_eq!(runs[0].mcp_servers.len(), 1);
    assert_eq!(
        runs[0].mcp_servers[0].command,
        "/usr/local/bin/chuggernaut-channel"
    );
    assert!(runs[0].mcp_servers[0].env.contains_key("NATS_URL"));
    let creds = runs[0]
        .env
        .get("NATS_CREDS")
        .expect("NATS_CREDS in container env");
    assert!(creds.contains("BEGIN NATS USER JWT"));
    assert_eq!(runs[0].mcp_servers[0].env.get("NATS_CREDS"), Some(creds));
    assert_eq!(
        runs[0].env.get("CHANNEL_ROLE").map(String::as_str),
        Some("work")
    );
    let paths: Vec<&str> = runs[0]
        .files
        .iter()
        .map(|f| f.container_path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "/usr/local/bin/chuggernaut-channel",
            "/chuggernaut/ssh/id",
            "/chuggernaut/ssh/id-cert.pub"
        ]
    );
    assert_eq!(runs[0].files[0].mode, 0o755);
    assert_eq!(runs[0].files[1].mode, 0o600);
    let cert = String::from_utf8(runs[0].files[2].contents.clone()).unwrap();
    assert!(cert.starts_with("ssh-ed25519-cert-v01@openssh.com"));
    assert!(
        runs[0]
            .env
            .get("GIT_SSH_COMMAND")
            .unwrap()
            .contains("/chuggernaut/ssh/id"),
        "GIT_SSH_COMMAND must reference the injected key"
    );
    assert_invariants_of(&sink);
}

/// The artifacts a job leaves behind. Before this, an agent's session transcript
/// died with its container: the provider dropped the container id, so nothing
/// could name the file even though the container itself was never removed.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn agent_run_captures_transcript_logs_and_measured_usage() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity.clone())).await else {
        return;
    };

    rig.backend.put_logs(
        br#"Cloning into '/workspace'...
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}],"usage":{"input_tokens":1200,"output_tokens":10}},"session_id":"s"}
{"type":"result","subtype":"success","is_error":false,"session_id":"s","total_cost_usd":0.01,"usage":{"input_tokens":1200,"cache_creation_input_tokens":300,"cache_read_input_tokens":400,"output_tokens":56}}"#
            .to_vec(),
    );

    let bare = rig.repo.bare_path();
    let backend = rig.backend.clone();
    rig.provider.on_run(move |cfg| async move {
        backend.put_file(
            &agent::transcript_path(&cfg.session_id),
            br#"{"type":"user","message":"do it"}"#.to_vec(),
        );
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/new.rs", b"pub fn f() {}", "implement")
            .await;
        clone.push(&branch).await;
    });
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig.store.tasks().await.unwrap();
    let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
    let work = log.iter().find(|t| t.phase == TaskPhase::Work).unwrap();

    let session_id = work
        .session_id
        .clone()
        .expect("work task records a session id");

    let artifacts = rig
        .store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();

    let transcript = artifacts
        .get(
            "acme",
            "api",
            job.id,
            work.id,
            store::ArtifactKind::SessionTranscript,
        )
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

    let eval = log
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation)
        .unwrap();
    assert!(
        artifacts
            .get("acme", "api", job.id, eval.id, store::ArtifactKind::Stdout)
            .await
            .unwrap()
            .is_some(),
        "eval container logs captured"
    );
    assert!(eval.session_id.is_none());
    assert!(!session_id.is_empty());

    let removed = rig.backend.removed();
    assert_eq!(
        removed.len(),
        rig.backend.launches().len(),
        "every launched container should be removed after its task exits"
    );
    assert_invariants_of(&rig.invariants);
}

/// Design #362 S1, the scope decision: a **work** container's
/// `/workspace/chug-output.tar.gz` is harvested and listed on its task, and
/// neither evaluator container — both of which this fake backend shows the very
/// same path — is read. The `staged` job type carries both evaluator shapes, so
/// the agent evaluator fails this the moment the hook moves inside the
/// `collect_agent` that the agent-work and agent-eval paths share, and the
/// command evaluator fails it the moment `MonitorKind::Eval` grows the call.
#[tokio::test]
async fn agent_work_output_is_harvested_and_neither_evaluator_is() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity.clone())).await else {
        return;
    };
    rig.backend.put_file(
        dispatcher::platform_ops::harvest::OUTPUT_PATH,
        b"gzipped-coverage-tree".to_vec(),
    );

    commit_work(&rig);
    let h = rig.handle.clone();
    rig.provider.on_run(move |_| async move {
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
    });

    let job = rig.handle.create_job(req("staged")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig.store.tasks().await.unwrap();
    let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
    let work = log.iter().find(|t| t.phase == TaskPhase::Work).unwrap();
    let evaluator = |name: &str| {
        log.iter()
            .find(|t| t.evaluator.as_deref() == Some(name))
            .unwrap()
            .id
    };

    let artifacts = rig
        .store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();
    assert_eq!(
        stored_output(&artifacts, job.id, work.id).await.as_deref(),
        Some(b"gzipped-coverage-tree".as_slice()),
    );
    assert!(
        artifacts
            .list_for_task("acme", "api", job.id, work.id)
            .await
            .unwrap()
            .contains(&store::ArtifactKind::Output),
        "the output must appear in the work task's artifact listing"
    );
    for (shape, id) in [("agent", evaluator("review")), ("command", evaluator("ci"))] {
        assert!(
            stored_output(&artifacts, job.id, id).await.is_none(),
            "a {shape} evaluator's output archive must NOT be harvested"
        );
    }
    assert_invariants_of(&rig.invariants);
}

/// The other half of #362 S1's scope: command work reaches the archive through
/// `MonitorKind::Logs`, which read only `logs` before this. The agent evaluator
/// in the same job is still not read.
#[tokio::test]
async fn command_work_output_is_harvested_and_the_agent_evaluator_is_not() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity.clone())).await else {
        return;
    };
    rig.backend.put_file(
        dispatcher::platform_ops::harvest::OUTPUT_PATH,
        b"coverage.lcov + coverage-html".to_vec(),
    );
    let h = rig.handle.clone();
    rig.provider.on_run(move |_| async move {
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
    });

    let job = rig.handle.create_job(req("cmd-agent-eval")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig.store.tasks().await.unwrap();
    let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
    let work = log.iter().find(|t| t.phase == TaskPhase::Work).unwrap();
    let eval = log
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation)
        .unwrap();

    let artifacts = rig
        .store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();
    assert_eq!(
        stored_output(&artifacts, job.id, work.id).await.as_deref(),
        Some(b"coverage.lcov + coverage-html".as_slice()),
    );
    assert!(
        stored_output(&artifacts, job.id, eval.id).await.is_none(),
        "an agent evaluator's output archive must NOT be harvested"
    );
    assert_invariants_of(&rig.invariants);
}

/// Design #362 R2: revoking a job drops its outputs and leaves the audit
/// record. A revoked job is still evidence of what an agent did.
#[tokio::test]
async fn revoke_deletes_outputs_and_keeps_the_audit_record() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity.clone())).await else {
        return;
    };
    rig.backend.put_file(
        dispatcher::platform_ops::harvest::OUTPUT_PATH,
        b"byproduct".to_vec(),
    );
    rig.backend
        .put_logs(b"Cloning into '/workspace'...".to_vec());
    rig.backend.script_exits([1, 1]);

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig.store.tasks().await.unwrap();
    let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
    let work = log.iter().find(|t| t.phase == TaskPhase::Work).unwrap();
    let artifacts = rig
        .store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();
    assert!(
        artifacts
            .get("acme", "api", job.id, work.id, store::ArtifactKind::Output)
            .await
            .unwrap()
            .is_some(),
        "output stored before the revoke"
    );

    rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Revoked).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let gone = artifacts
            .get("acme", "api", job.id, work.id, store::ArtifactKind::Output)
            .await
            .unwrap()
            .is_none();
        if gone || std::time::Instant::now() >= deadline {
            assert!(gone, "revoke must delete the job's outputs");
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        artifacts
            .get("acme", "api", job.id, work.id, store::ArtifactKind::Stdout)
            .await
            .unwrap()
            .is_some(),
        "revoke must NOT touch the audit record"
    );
    assert_invariants_of(&rig.invariants);
}

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
    clone
        .commit_file("jobs/deploy-none.yaml", DEPLOY_NONE.as_bytes(), "type")
        .await;
    clone.push("main").await;

    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("deploy.log", b"deployed v1", "scratch")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("deploy-none")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
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
        rig.repo
            .manager
            .resolve_ref("acme", "api", &job.branch)
            .await
            .is_err(),
        "job branch deleted at Done"
    );
    assert_invariants_of(&rig.invariants);
}

/// A web-style job with a `wrap_up.run` command (spec §3.2): eval passes, the
/// squash lands on main, and only THEN the publish command runs — against the
/// merged default branch — carrying the job to Done. The merged content is on
/// main before the publish launches, and the publish container clones the
/// default branch (not the scratch job branch).
#[tokio::test]
async fn wrap_up_command_runs_after_merge_against_main() {
    let Some(rig) = rig().await else { return };

    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("web/src/app.tsx", b"<App/>", "implement")
            .await;
        clone.push(&branch).await;
    });
    let repos_root = rig
        .repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    rig.backend.on_launch(move |_cfg| {
        let manager = vcs::RepoManager::new(repos_root.clone());
        async move {
            let merged = manager
                .read_file_at("acme", "api", "main", "web/src/app.tsx")
                .await
                .unwrap();
            assert_eq!(
                merged.as_deref(),
                Some("<App/>"),
                "the squash must land on main BEFORE the publish runs"
            );
        }
    });

    let job = rig.handle.create_job(req("webpub")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2, "work + wrap-up command");
    assert_eq!(tasks[0].phase, TaskPhase::Work);
    let wrapup = &tasks[1];
    assert_eq!(wrapup.phase, TaskPhase::WrapUp);
    assert_eq!(wrapup.state, TaskState::Done);
    assert!(matches!(wrapup.kind, types::TaskKind::Command { .. }));

    let launches = rig.backend.launches();
    assert_eq!(
        launches.len(),
        1,
        "only the publish launches through backend"
    );
    assert_eq!(
        launches[0].env.get("JOB_BRANCH").map(String::as_str),
        Some("main"),
        "publish runs against merged main, not the job branch"
    );

    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-done".to_string()), "{events:?}");
    assert_invariants_of(&rig.invariants);
}

/// A failed `wrap_up.run` command escalates the job — but the squash has
/// already landed, so the merge is NOT undone (spec §3.2 wrap-up failure).
#[tokio::test]
async fn wrap_up_command_failure_escalates_but_merge_stays() {
    let Some(rig) = rig().await else { return };

    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("web/src/app.tsx", b"<App/>", "implement")
            .await;
        clone.push(&branch).await;
    });
    rig.backend.script_exits([7]);

    let job = rig.handle.create_job(req("webpub")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let merged = rig
        .repo
        .manager
        .read_file_at("acme", "api", "main", "web/src/app.tsx")
        .await
        .unwrap();
    assert_eq!(
        merged.as_deref(),
        Some("<App/>"),
        "a failed publish must not un-merge the landed squash"
    );

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let wrapup = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::WrapUp)
        .expect("wrap-up task");
    assert_eq!(wrapup.state, TaskState::Failed);
    assert!(
        tasks
            .iter()
            .any(|t| matches!(t.kind, types::TaskKind::Human { .. })
                && t.state == TaskState::Pending),
        "an escalation task should be open: {tasks:?}"
    );
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-escalated".to_string()), "{events:?}");
    assert_invariants_of(&rig.invariants);
}

/// Wrap-up-failure escalation + Retry (#141) resumes at WrapUp: it re-runs only
/// the publish command (the squash already landed) and never launches a fresh
/// work task. The retry publish succeeds → the job lands.
#[tokio::test]
async fn wrap_up_failure_retry_reruns_only_publish() {
    let Some(rig) = rig().await else { return };
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("web/src/app.tsx", b"<App/>", "implement")
            .await;
        clone.push(&branch).await;
    });
    rig.backend.script_exits([7, 0]);

    let job = rig.handle.create_job(req("webpub")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let before = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let work_before = before.iter().filter(|t| t.phase == TaskPhase::Work).count();
    let esc = before
        .iter()
        .find(|t| matches!(t.kind, types::TaskKind::Human { .. }) && t.state == TaskState::Pending)
        .expect("a Human escalation task");
    assert_eq!(esc.phase, TaskPhase::Escalation);

    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            esc.id,
            TaskResolution::Escalation {
                action: EscalationAction::Retry,
                structured: None,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let after = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(
        after.iter().filter(|t| t.phase == TaskPhase::Work).count(),
        work_before,
        "Retry re-ran the publish, not work (#141)",
    );
    let wrapups: Vec<_> = after
        .iter()
        .filter(|t| t.phase == TaskPhase::WrapUp)
        .collect();
    assert_eq!(wrapups.len(), 2, "the failed publish plus the retried one");
    assert!(
        wrapups.iter().any(|t| t.state == TaskState::Done),
        "the retried publish landed the job: {wrapups:?}",
    );
    assert!(after.iter().all(|t| t.cycle == 1), "cycle untouched");
    assert_invariants_of(&rig.invariants);
}

/// A job revoked before it lands never runs its `wrap_up.run` command: the
/// publish only fires off a successful merge, never off a revoke.
#[tokio::test]
async fn revoked_job_never_runs_wrap_up_command() {
    let Some(rig) = rig().await else { return };

    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (s2, r2) = (started.clone(), release.clone());
    rig.provider.on_run(move |_cfg| async move {
        s2.notify_one();
        r2.notified().await;
    });

    let job = rig.handle.create_job(req("webpub")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    started.notified().await;
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    release.notify_one();

    wait_for_state(&rig.store, job.id, JobState::Revoked).await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert!(
        tasks.iter().all(|t| t.phase != TaskPhase::WrapUp),
        "a revoked job must never create a wrap-up command task: {tasks:?}"
    );
    assert!(
        rig.backend.launches().is_empty(),
        "no publish container should ever launch for a revoked job"
    );
    assert_invariants_of(&rig.invariants);
}

/// A run exit collected after its job was revoked is stale noise, not a
/// state transition: revoke removes the job's exec state but leaves the
/// Running task record, so a clean late exit used to drive the verdict path
/// into `enter_evaluation`'s exec-state expect and panic the core loop
/// (the 2026-07-23 outage — one revoked job's orphan took down the platform).
/// The neighbor test above has the same shape but never touches the handle
/// after the exit, so the panic went unobserved; this one probes liveness.
#[tokio::test]
async fn late_exit_after_revoke_is_ignored_and_core_survives() {
    let Some(rig) = rig().await else { return };

    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (s2, r2) = (started.clone(), release.clone());
    rig.provider.on_run(move |_cfg| async move {
        s2.notify_one();
        r2.notified().await;
    });

    let job = rig.handle.create_job(req("webpub")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    started.notified().await;
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Revoked).await;
    release.notify_one();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let jobs = rig.store.jobs().await.unwrap();
    let j = jobs.get("acme", "api", job.id).await.unwrap().unwrap();
    assert_eq!(
        j.state,
        JobState::Revoked,
        "stale exit must not resurrect the job"
    );
    let next = rig.handle.create_job(req("webpub")).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        next.is_ok(),
        "core loop died handling the stale exit: {next:?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Abort verdict: a required evaluator declaring the work unsalvageable
/// escalates immediately — the remaining rework budget is not consumed.
#[tokio::test]
async fn eval_abort_escalates_without_consuming_rework_budget() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: true,
                structured: Some(
                    serde_json::json!({"reason": "endpoint spec references a retired API"}),
                ),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    assert_eq!(rig.provider.runs().len(), 2, "no cycle-2 work after abort");
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 3);
    match &tasks[1].result {
        Some(types::TaskResult::Agent {
            pass: false,
            abort: true,
            ..
        }) => {}
        other => panic!("unexpected eval result: {other:?}"),
    }
    match &tasks[2].kind {
        types::TaskKind::Human { prompt } => {
            assert!(prompt.contains("not satisfiable by rework"), "{prompt}");
            assert!(
                prompt.contains("retired API"),
                "findings forwarded: {prompt}"
            );
        }
        other => panic!("expected escalation task, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

/// Staged evaluation happy path (spec §3.3): the stage-0 review runs first and
/// the stage-1 `ci` command evaluator is created only after it passes. Both
/// tasks carry their `stage`, and only two agent runs happen (work + review).
#[tokio::test]
async fn staged_eval_review_passes_then_ci_runs_and_merges() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    let bare = rig.repo.bare_path();

    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/new.rs", b"pub fn f() {}", "implement")
            .await;
        clone.push(&branch).await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
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
    });

    let job = rig.handle.create_job(req("staged")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[1].evaluator.as_deref(), Some("review"));
    assert_eq!(tasks[1].stage, 0);
    assert_eq!(tasks[2].evaluator.as_deref(), Some("ci"));
    assert_eq!(tasks[2].stage, 1);
    assert!(tasks[2].created_at >= tasks[1].completed_at.unwrap());
    assert_eq!(
        rig.provider.runs().len(),
        2,
        "only work + review run agents"
    );
    assert_invariants_of(&rig.invariants);
}

/// Required stage-0 failure short-circuits: the stage-1 `ci` task is never
/// created for that cycle, and the rework cycle restarts from stage 0 (review
/// again). Directly the acceptance case — a review-rejected change spends no CI.
#[tokio::test]
async fn staged_eval_required_review_fail_skips_ci_and_reworks_from_stage0() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["needs tests"]})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    rig.provider.on_run(|_| async {});
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            4,
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
    });

    let job = rig.handle.create_job(req("staged")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let ci = |cycle: u32| {
        tasks.iter().any(|t| {
            t.phase == TaskPhase::Evaluation
                && t.cycle == cycle
                && t.evaluator.as_deref() == Some("ci")
        })
    };
    assert!(!ci(1), "stage-1 ci must not run when stage-0 review fails");
    assert!(ci(2), "ci runs once review passes on the rework cycle");
    assert!(tasks.iter().any(|t| {
        t.phase == TaskPhase::Evaluation
            && t.cycle == 2
            && t.evaluator.as_deref() == Some("review")
            && t.stage == 0
    }));
    assert_invariants_of(&rig.invariants);
}

/// Advisory stage-0 failure does not block: a `required: false` review that
/// fails still lets the stage-1 `ci` evaluator run, and the required ci pass
/// carries the job to Done.
#[tokio::test]
async fn staged_eval_advisory_review_fail_still_runs_ci() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: false,
                structured: None,
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("staged-advisory")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 3);
    assert!(matches!(
        tasks[1].result,
        Some(types::TaskResult::Agent { pass: false, .. })
    ));
    assert_eq!(tasks[2].evaluator.as_deref(), Some("ci"));
    assert_eq!(tasks[2].state, TaskState::Done);
    assert_invariants_of(&rig.invariants);
}

/// An abort from a required stage-0 evaluator escalates immediately: no later
/// stage is created, and (like any abort) the rework budget is not consumed.
#[tokio::test]
async fn staged_eval_stage0_abort_escalates_without_ci() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: true,
                structured: Some(serde_json::json!({"reason": "wrong premise"})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("staged")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    assert_eq!(rig.provider.runs().len(), 2, "no rework after abort");
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 3);
    assert!(
        !tasks.iter().any(|t| t.evaluator.as_deref() == Some("ci")),
        "the stage-1 ci evaluator must never be created after a stage-0 abort"
    );
    assert!(matches!(tasks[2].kind, types::TaskKind::Human { .. }));
    assert_invariants_of(&rig.invariants);
}

/// Additive per-job evaluators: layered on top of the type's list and executed
/// like declared ones.
#[tokio::test]
async fn job_level_evaluators_run_alongside_type_evaluators() {
    let Some(rig) = rig().await else { return };

    commit_work(&rig);
    let mut r = req("flaky");
    r.eval = vec![types::Evaluator {
        name: "extra-ci".into(),
        r#type: types::EvaluatorType::Command,
        image: None,
        run: Some("./ci.sh".into()),
        prompt: None,
        provider: None,
        model: None,
        secrets: vec![],
        workload_identities: vec![],
        required: None,
        stage: 0,
    }];
    let job = rig.handle.create_job(r).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let eval = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation)
        .expect("eval task");
    assert_eq!(eval.evaluator.as_deref(), Some("extra-ci"));
    assert_eq!(eval.state, TaskState::Done);
    assert_invariants_of(&rig.invariants);
}

/// §4.4 upfront knowledge injection: the union of the type's `knowledge:`
/// defaults and the job's tags rides the work agent's system prompt, read
/// from tags/{tag}.md at base_ref. Unknown tags are skipped.
#[tokio::test]
async fn knowledge_tags_inject_into_work_system_prompt() {
    let Some(rig) = rig().await else { return };

    commit_work(&rig);
    let mut create = req("flaky");
    create.knowledge_tags = vec!["style".into(), "no-such-tag".into()];
    let job = rig.handle.create_job(create).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    let system = runs[0].system_prompt.as_deref().expect("knowledge block");
    assert!(system.contains("Project Knowledge"), "{system}");
    assert!(
        system.contains("rust conventions here"),
        "type default: {system}"
    );
    assert!(system.contains("house style here"), "job tag: {system}");
    assert!(
        !system.contains("no-such-tag"),
        "missing tags are skipped: {system}"
    );
    assert_invariants_of(&rig.invariants);
}

/// The type's evaluators are a floor: a job evaluator colliding with a
/// declared name is a release-time validation error.
#[tokio::test]
async fn job_evaluator_name_collision_fails_release() {
    let Some(rig) = rig().await else { return };

    let mut r = req("impl-cmd");
    r.eval = vec![types::Evaluator {
        name: "tests".into(),
        r#type: types::EvaluatorType::Command,
        image: None,
        run: Some("./sneaky.sh".into()),
        prompt: None,
        provider: None,
        model: None,
        secrets: vec![],
        workload_identities: vec![],
        required: None,
        stage: 0,
    }];
    let job = rig.handle.create_job(r).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let err = rig
        .handle
        .release_job("acme", "api", job.id)
        .await
        .unwrap_err();
    assert_invariants_of(&rig.invariants);
    assert!(err.to_string().contains("collides"), "{err}");
    assert_invariants_of(&rig.invariants);
}

/// Unexpected wrap-up failure → triage, and the merge queue moves on instead
/// of wedging (design-lifecycle.md). Simulated by deleting the job branch
/// out from under finalization.
#[tokio::test]
async fn finalize_hard_failure_escalates_instead_of_wedging() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    let bare = rig.repo.bare_path();

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        let out = tokio::process::Command::new("git")
            .args([
                "-C",
                bare.to_str().unwrap(),
                "update-ref",
                "-d",
                "refs/heads/job/1",
            ])
            .output()
            .await
            .unwrap();
        assert!(out.status.success(), "{out:?}");
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
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
    match &tasks.last().unwrap().kind {
        types::TaskKind::Human { prompt } => {
            assert!(prompt.contains("wrap-up failed unexpectedly"), "{prompt}");
        }
        other => panic!("expected escalation task, got {other:?}"),
    }
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-escalated".to_string()));
    assert_invariants_of(&rig.invariants);
}

/// The per-job `Job.timeout` override (§1.1) drives the Work agent's own run
/// timeout, while the evaluator keeps the type default — the override is
/// Work-scoped. Asserted at the mechanism: the recorded run configs.
#[tokio::test]
async fn work_timeout_override_applies_to_work_not_eval() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
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
    });

    let mut create = req("impl-agent");
    create.timeout = Some("45m".into());
    let job = rig.handle.create_job(create).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].task_timeout, Duration::from_secs(45 * 60));
    assert_eq!(runs[1].task_timeout, Duration::from_secs(3600));
    assert_invariants_of(&rig.invariants);
}

/// A Work task that outlives the per-job override is killed by the §3.5 timeout
/// scan — the override applies at kill time, escalating the job.
#[tokio::test]
async fn work_timeout_override_times_out_running_work_task() {
    let Some(rig) = rig().await else { return };
    rig.provider
        .on_run(|_| async { tokio::time::sleep(Duration::from_secs(30)).await });

    let mut create = req("impl-agent");
    create.timeout = Some("1s".into());
    let job = rig.handle.create_job(create).await.unwrap();
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
    let work = tasks.iter().find(|t| t.phase == TaskPhase::Work).unwrap();
    assert_eq!(
        work.state,
        TaskState::Failed,
        "the override should have timed out the work task"
    );
    assert_invariants_of(&rig.invariants);
}

/// The evaluator keeps the type default even when the job carries a short work
/// override: a scan after the (short) override elapses must not touch the
/// Evaluation-phase task — the override does not leak past Work.
#[tokio::test]
async fn eval_task_ignores_work_timeout_override() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    rig.provider
        .on_run(|_| async { tokio::time::sleep(Duration::from_secs(30)).await });

    let mut create = req("impl-agent");
    create.timeout = Some("1s".into());
    let job = rig.handle.create_job(create).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;

    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();
    assert_invariants_of(&rig.invariants);

    let job_now = rig
        .store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        job_now.state,
        JobState::Evaluation,
        "eval must survive the work override"
    );
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let eval = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation)
        .unwrap();
    assert_eq!(
        eval.state,
        TaskState::Running,
        "the work override must not time out the eval task"
    );
    assert_invariants_of(&rig.invariants);
}

/// A malformed `Job.timeout` is rejected at release (§1.1: parseability
/// validated at release, not creation) — creation still succeeds.
#[tokio::test]
async fn malformed_timeout_override_rejected_at_release() {
    let Some(rig) = rig().await else { return };
    let mut create = req("impl-agent");
    create.timeout = Some("2 hours".into());
    let job = rig.handle.create_job(create).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let err = rig
        .handle
        .release_job("acme", "api", job.id)
        .await
        .unwrap_err();
    assert_invariants_of(&rig.invariants);
    match err {
        dispatcher::core::CoreError::Validation(errs) => {
            assert!(errs.iter().any(|e| e.field == "timeout"), "{errs:?}");
        }
        other => panic!("expected a validation error on timeout, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

/// Operator-dispatched triage (§1.2) over an Escalated job: creates a Triage
/// agent task, captures the assessment from the CLI's JSON result (no channel
/// MCP), and leaves the job Escalated — purely advisory.
#[tokio::test]
async fn triage_on_escalated_job_records_assessment_and_leaves_escalated() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity)).await else {
        return;
    };

    rig.provider.script_exits([2, 3]);
    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    rig.backend.put_logs(
        br#"{"type":"result","subtype":"success","is_error":false,"result":"Root cause: the work container exited non-zero on both attempts. Recommend Revoke.","session_id":"t","usage":{"input_tokens":10,"output_tokens":20}}"#
            .to_vec(),
    );

    rig.handle.triage_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let triage = test_utils::wait::task_where(
        &rig.store,
        "acme",
        "api",
        job.id,
        format!("triage task for job {} with a result", job.id),
        |t| t.phase == TaskPhase::Triage && t.result.is_some(),
    )
    .await;
    assert_eq!(triage.state, TaskState::Done);
    match triage.result.as_ref().unwrap() {
        types::TaskResult::Triage { assessment, .. } => {
            assert!(assessment.contains("Recommend Revoke"), "{assessment}");
        }
        other => panic!("expected a Triage result, got {other:?}"),
    }

    let job_now = rig
        .store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job_now.state, JobState::Escalated);

    let last = rig.provider.runs().pop().unwrap();
    assert_eq!(last.image, "triage:latest");
    assert!(
        last.mcp_servers.is_empty(),
        "triage runs without the channel MCP"
    );
    assert_invariants_of(&rig.invariants);
}

/// Triage is rejected unless the job is Escalated or Stalled (§1.2).
#[tokio::test]
async fn triage_rejected_on_non_intervention_state() {
    let Some(rig) = rig().await else { return };
    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let err = rig
        .handle
        .triage_job("acme", "api", job.id)
        .await
        .unwrap_err();
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(err, dispatcher::core::CoreError::Conflict(_)),
        "{err:?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Poll the task log until the job's Work-phase task exists, then return it.
/// The record is written at launch with its resolved kind, so it is readable
/// even after the job advances past Work.
async fn wait_for_work_task(store: &NatsStore, seq: u64) -> types::Task {
    test_utils::wait::task_where(
        store,
        "acme",
        "api",
        seq,
        format!("a Work-phase task for job {seq}"),
        |t| t.phase == TaskPhase::Work,
    )
    .await
}

#[tokio::test]
async fn per_job_model_override_reaches_work_task() {
    let Some(rig) = rig().await else { return };
    let mut r = req("impl-cmd");
    r.model = Some("claude-fable-5".into());
    let job = rig.handle.create_job(r).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let work = wait_for_work_task(&rig.store, job.id).await;
    match work.kind {
        types::TaskKind::Agent { model, .. } => {
            assert_eq!(model.as_deref(), Some("claude-fable-5"));
        }
        other => panic!("expected an agent work task, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

#[tokio::test]
async fn work_task_model_none_without_any_default() {
    let Some(rig) = rig().await else { return };
    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let work = wait_for_work_task(&rig.store, job.id).await;
    match work.kind {
        types::TaskKind::Agent { model, .. } => assert_eq!(model, None),
        other => panic!("expected an agent work task, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

/// #71/#72 regression: an agent work container's id lands on the task record
/// while it is still Running — not only in `AgentOutput` after exit — and
/// survives on the finished record.
#[tokio::test]
async fn work_container_id_recorded_while_running_and_kept_after_exit() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity)).await else {
        return;
    };
    let bare = rig.repo.bare_path();
    let store = rig.store.clone();
    rig.provider.on_run(move |cfg| async move {
        let work = test_utils::wait::task_where(
            &store,
            "acme",
            "api",
            1,
            "work task 1/1 to record a container_id while Running",
            |t| t.id == 1 && t.state == TaskState::Running && t.container_id.is_some(),
        )
        .await;
        assert!(
            work.container_id.is_some(),
            "container_id must be set while the work task is Running"
        );
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/x.rs", b"pub fn x() {}", "impl")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let work = wait_for_work_task(&rig.store, job.id).await;
    assert_eq!(work.state, TaskState::Done);
    assert!(
        work.container_id.is_some(),
        "container_id must survive on the completed record"
    );
    assert_invariants_of(&rig.invariants);
}

/// A required agent evaluator's abort verdict escalates immediately, and the
/// job records the reason code + human-readable detail (#69-class diagnosis on
/// the record, not in the logs). Evaluation-phase escalations have no single
/// culprit task, so `failing_task` is None.
#[tokio::test]
async fn abort_verdict_escalates_with_recorded_reason() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: true,
                structured: Some(serde_json::json!({"why": "spec is unbuildable"})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let esc = escalated.escalation.expect("abort records an escalation");
    assert_eq!(esc.reason, "eval_abort");
    assert!(
        esc.detail.contains("not satisfiable by rework"),
        "detail: {}",
        esc.detail
    );
    assert_eq!(esc.failing_task, None);
    assert_invariants_of(&rig.invariants);
}

/// #167 fix 1: a failing command evaluator embeds the captured output tail in
/// `result.output` (was hardcoded empty), and the tail is size-capped and
/// stderr-biased — the LAST bytes survive the cap, the head is dropped. This is
/// the evidence the job page, rework brief, and re-review read inline.
#[tokio::test]
async fn failing_command_eval_embeds_size_capped_output_tail() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let big = format!(
        "HEAD_MARKER_dropped_by_cap\n{}\nTAIL_MARKER_assertion_failed_at_foo_rs_42",
        "x".repeat(20_000)
    );
    rig.backend.put_logs(big.clone().into_bytes());
    rig.backend.script_exits([101]);

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
    let eval = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation)
        .expect("an eval task ran");
    match &eval.result {
        Some(types::TaskResult::Command {
            pass: false,
            output,
            ..
        }) => {
            assert!(
                output.contains("TAIL_MARKER_assertion_failed_at_foo_rs_42"),
                "the failure tail must be embedded: {output:.200?}"
            );
            assert!(
                !output.contains("HEAD_MARKER_dropped_by_cap"),
                "the head must be dropped by the size cap (tail-biased)"
            );
            assert!(
                output.len() < big.len(),
                "the embedded output must be capped below the full stream ({} vs {})",
                output.len(),
                big.len()
            );
        }
        other => panic!("expected a failing command result with a tail, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

/// #167 fix 2 (narrowed #198): a command evaluator whose container dies before it
/// can judge — an ABNORMAL exit (a signal kill `>= 128`) — with a completely
/// empty captured stream is evidence-free: infrastructure loss, not a verdict. It
/// is relaunched automatically WITHOUT burning `eval_retries` (every attempt stays
/// attempt 1, stamped `infra_loss`) and never triggers rework; once the infra
/// relaunch cap is exhausted the job escalates with reason `evaluator_no_output`
/// rather than failing the round on nothing. (A *normal* non-zero exit is a real
/// verdict and reworks — see `normal_nonzero_command_eval_empty_output_reworks`.)
#[tokio::test]
async fn no_output_command_eval_retries_without_burning_retries_then_escalates() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    rig.backend.script_exits([137, 137, 137, 137, 137]);

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    assert_eq!(
        escalated.escalation.expect("escalation recorded").reason,
        "evaluator_no_output",
        "an evidence-free evaluator escalates as no-output, not a code failure"
    );

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let evals: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert!(
        evals.len() >= 2,
        "the evaluator must be relaunched, not resolved on the first empty fail: {evals:?}"
    );
    for t in &evals {
        assert_eq!(
            t.attempt, 1,
            "a no-output relaunch burns no eval_retries (stays attempt 1)"
        );
        assert!(t.infra_loss, "each no-output loss is stamped infra_loss");
    }
    assert_eq!(
        tasks.iter().filter(|t| t.phase == TaskPhase::Work).count(),
        1,
        "an evidence-free eval fail must not trigger rework"
    );
    let no_output_events: Vec<_> = job_events(&rig.store, job.id)
        .await
        .into_iter()
        .filter(|e| e["reason"] == "evaluator_no_output")
        .collect();
    assert!(
        !no_output_events.is_empty(),
        "the evaluator_no_output reason must ride the event stream"
    );
    assert_invariants_of(&rig.invariants);
}

/// #198 ticket test (a): an AGENT evaluator that submits a FAIL verdict with
/// structured findings but whose captured stream is empty follows the NORMAL eval
/// flow — it reworks. A submitted verdict is a verdict regardless of log volume;
/// the #167 no-output invalid path is NOT taken and the failure is never
/// discarded. (This is the prod scenario the #198 hotfix protects: a failing
/// review with empty log capture must rework, not silently retry into a pass.)
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn agent_eval_fail_with_findings_empty_output_reworks_not_invalid() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["missing tests"]})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/rework.rs", b"rework", "rework")
            .await;
        clone.push(&branch).await;
    });
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            4,
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
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.phase == TaskPhase::Work && t.cycle == 2 && t.evaluator.is_none()),
        "an agent FAIL verdict must rework, not retry: {tasks:#?}"
    );
    let eval1 = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation && t.cycle == 1)
        .expect("cycle-1 eval task");
    assert!(
        matches!(
            eval1.result,
            Some(types::TaskResult::Agent { pass: false, .. })
        ),
        "the delivered FAIL verdict is preserved: {:?}",
        eval1.result
    );
    assert!(
        !eval1.infra_loss,
        "a delivered verdict is never stamped infra_loss"
    );
    assert_eq!(
        eval1.attempt, 1,
        "the verdict is taken on attempt 1, no relaunch"
    );
    let no_output = job_events(&rig.store, job.id)
        .await
        .into_iter()
        .filter(|e| e["reason"] == "evaluator_no_output")
        .count();
    assert_eq!(
        no_output, 0,
        "a delivered verdict must not trigger the evidence-free path"
    );
    assert_invariants_of(&rig.invariants);
}

/// #198 ticket test (b): an AGENT evaluator that ends WITHOUT a `submit_eval`
/// verdict and with no captured output is genuinely evidence-free — the #167 case
/// this hotfix preserves. It is relaunched WITHOUT burning `eval_retries` (every
/// attempt stays attempt 1, stamped `infra_loss`) and never reworks; once the
/// infra relaunch cap is exhausted the job escalates `evaluator_no_output`.
#[tokio::test]
async fn agent_eval_no_verdict_no_output_retries_without_burning_eval_retries_then_escalates() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    assert_eq!(
        escalated.escalation.expect("escalation recorded").reason,
        "evaluator_no_output",
        "a verdict-less, output-less agent eval escalates as no-output"
    );

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let evals: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert!(
        evals.len() >= 2,
        "the evaluator must relaunch on the missing verdict: {evals:?}"
    );
    for t in &evals {
        assert_eq!(t.attempt, 1, "a no-verdict relaunch burns no eval_retries");
        assert!(t.infra_loss, "each no-verdict loss is stamped infra_loss");
    }
    assert_eq!(
        tasks.iter().filter(|t| t.phase == TaskPhase::Work).count(),
        1,
        "a verdict-less eval must not trigger rework"
    );
    assert_invariants_of(&rig.invariants);
}

/// #198 ticket test (c counterpart): a COMMAND evaluator that exits with a NORMAL
/// non-zero code (a real fail verdict) and an empty captured stream REWORKS — it
/// is not mislabelled evidence-free. This is the core regression: #167 auto-
/// retried it, converting a real fail into a pass. The evidence-free invalid path
/// is reserved for an abnormal/signal exit (see
/// `no_output_command_eval_retries_without_burning_retries_then_escalates`).
#[tokio::test]
async fn normal_nonzero_command_eval_empty_output_reworks() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/rework.rs", b"rework", "rework")
            .await;
        clone.push(&branch).await;
    });
    rig.backend.script_exits([1, 0]);

    let job = rig.handle.create_job(req("impl-cmd-rework")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert!(
        tasks
            .iter()
            .any(|t| t.phase == TaskPhase::Work && t.cycle == 2 && t.evaluator.is_none()),
        "a normal non-zero command exit must rework, not retry: {tasks:#?}"
    );
    let eval1 = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation && t.cycle == 1)
        .expect("cycle-1 eval task");
    assert!(
        matches!(
            eval1.result,
            Some(types::TaskResult::Command {
                pass: false,
                exit_code: 1,
                ..
            })
        ),
        "a normal non-zero exit is a verdict: {:?}",
        eval1.result
    );
    assert!(
        !eval1.infra_loss,
        "a normal non-zero exit is a verdict, not infra loss"
    );
    let no_output = job_events(&rig.store, job.id)
        .await
        .into_iter()
        .filter(|e| e["reason"] == "evaluator_no_output")
        .count();
    assert_eq!(no_output, 0, "the evidence-free path must not be taken");
    assert_invariants_of(&rig.invariants);
}

/// #167 fix 1, passing path: a passing command evaluator behaves exactly as
/// before — the job reaches Done — and the (small) tail is embedded harmlessly.
#[tokio::test]
async fn passing_command_eval_embeds_tail_and_reaches_done_unchanged() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    rig.backend.put_logs(b"all 42 tests passed".to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let eval = tasks
        .iter()
        .find(|t| t.phase == TaskPhase::Evaluation)
        .expect("an eval task ran");
    match &eval.result {
        Some(types::TaskResult::Command {
            pass: true, output, ..
        }) => assert_eq!(output, "all 42 tests passed", "the small tail is embedded"),
        other => panic!("expected a passing command result, got {other:?}"),
    }
    assert_invariants_of(&rig.invariants);
}

/// #167 fix 1 → rework: a failing command evaluator's embedded tail is threaded
/// into the next work cycle's rework context, clearly fenced and labelled as the
/// evaluator's output (a `command` result carries no structured findings, so the
/// tail is the only evidence the rework agent can fix against).
#[tokio::test]
async fn rework_context_carries_command_eval_output_tail() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    rig.backend
        .put_logs(b"FAILING_CI: assertion failed at src/lib.rs:99".to_vec());
    rig.backend.script_exits([101]);

    let job = rig.handle.create_job(req("impl-cmd-rework")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let prompt = test_utils::wait::poll_default(
        "a rework work cycle (2nd provider run) after the ci failure",
        || {
            let runs = rig.provider.runs();
            (runs.len() >= 2).then(|| runs[1].prompt.clone())
        },
    )
    .await;
    assert!(
        prompt.contains("FAILING_CI: assertion failed at src/lib.rs:99"),
        "the ci output tail must reach the rework prompt: {prompt}"
    );
    assert!(
        prompt.contains("Output (tail) from **ci**"),
        "the tail must be fenced/labelled as the evaluator's output: {prompt}"
    );
    assert_invariants_of(&rig.invariants);
}

/// #168: a work retry (attempt > 1) leads with a predecessor block carrying the
/// prior attempt's captured output tail — size-capped (tail kept, head dropped)
/// and fenced/labelled as the predecessor's output. Attempt 1's prompt is
/// unchanged (nothing precedes it).
#[tokio::test]
async fn work_retry_prepends_predecessor_block_with_capped_tail() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity)).await else {
        return;
    };
    rig.provider.script_exits([2, 3]);
    let log = format!(
        "PRED_HEAD_dropped_by_cap\n{}\nPRED_TAIL_diagnosis_stack_overflow_at_lib_rs_88",
        "y".repeat(20_000)
    );
    rig.backend.put_logs(log.into_bytes());

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 2, "attempt 1 + one retry");
    assert!(
        !runs[0].prompt.contains("Previous Attempt (#168)"),
        "attempt 1 prompt is unchanged: {}",
        runs[0].prompt
    );
    let p = &runs[1].prompt;
    assert!(
        p.contains("## Previous Attempt (#168)"),
        "the retry leads with the predecessor block: {p:.400}"
    );
    assert!(
        p.contains("You are attempt 2"),
        "names the attempt ordinal: {p:.400}"
    );
    assert!(
        p.contains("PRED_TAIL_diagnosis_stack_overflow_at_lib_rs_88"),
        "carries the predecessor's tail"
    );
    assert!(
        !p.contains("PRED_HEAD_dropped_by_cap"),
        "the head is dropped by the size cap (tail-biased)"
    );
    assert!(p.contains("…(truncated)…"), "the cap marks the truncation");
    assert!(p.contains("```"), "the tail is fenced");
    assert_invariants_of(&rig.invariants);
}

/// #168: an agent evaluator relaunched after a #167 no-output invalid fail
/// carries the predecessor block. Its predecessor produced no captured output,
/// so the block says so explicitly (rather than being omitted) — the relaunch
/// still knows a predecessor existed. The first eval attempt has no block.
#[tokio::test]
async fn agent_eval_relaunch_prepends_empty_noted_predecessor_block() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity)).await else {
        return;
    };
    commit_work(&rig);

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let runs = rig.provider.runs();
    let eval_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.prompt.contains("review it"))
        .collect();
    assert!(
        eval_runs.len() >= 2,
        "the evaluator must relaunch, giving a predecessor to describe: {}",
        eval_runs.len()
    );
    assert!(
        !eval_runs[0].prompt.contains("Previous Attempt (#168)"),
        "the first eval attempt has no predecessor: {}",
        eval_runs[0].prompt
    );
    let relaunch = &eval_runs[1].prompt;
    assert!(
        relaunch.contains("## Previous Attempt (#168)"),
        "the eval relaunch leads with the predecessor block: {relaunch}"
    );
    assert!(
        relaunch.contains("produced no captured output"),
        "an empty predecessor is noted, not omitted: {relaunch}"
    );
    assert_invariants_of(&rig.invariants);
}

/// #168: command (ci) evaluator retries are deterministic scripts that read no
/// prompt — the predecessor-block machinery never touches them. A command eval
/// that no-outputs and relaunches produces no agent prompt at all.
#[tokio::test]
async fn command_eval_retry_gets_no_predecessor_prompt() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    rig.backend.script_exits([137, 137, 137, 137, 137]);

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let runs = rig.provider.runs();
    assert_eq!(
        runs.len(),
        1,
        "a command eval retry builds no agent prompt: {runs:?}"
    );
    assert!(
        !runs[0].prompt.contains("Previous Attempt (#168)"),
        "the work prompt is untouched by command-eval retries"
    );
    assert_invariants_of(&rig.invariants);
}

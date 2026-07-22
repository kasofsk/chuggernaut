//! Tier-2 execution tests: the actor core driving work + evaluation with
//! FakeBackend/FakeProvider over real NATS and real bare repos. Covers the
//! happy path (agent commits → eval passes → squash-merge → Done), work retry
//! exhaustion, and the eval-failure rework loop with §4.3 context injection.

use dispatcher::core::{
    Core, CoreConfig, CoreHandle, CreateJobRequest, EvalSubmission, WorkSubmission, spawn,
};
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

// Command work + command eval: both launch through the container backend, so
// each exercises a `backend.launch` failure directly (the agent path already
// surfaces launch failure via the provider as exit -1).
const CMD_WORK: &str = r#"
name: cmd-work
image: img:latest
work:
  type: command
  run: ./build.sh
work_retries: 1
"#;

// Staged evaluation (spec §3.3): a required stage-0 agent review gates a
// stage-1 command evaluator. review runs first; ci only after it passes.
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

// A web-style job with a post-merge wrap-up command (spec §3.2): agent work,
// no evaluators (auto-pass), and a `wrap_up.run` publish that ships the merged
// result. The publish is the only container that launches through the backend.
const WEBPUB: &str = r#"
name: webpub
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
wrap_up:
  run: ./tasks/web-publish.sh
"#;

// Same shape but the stage-0 review is advisory: its failure must not stop the
// stage-1 evaluator from running.
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
    _server: test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
    handle: CoreHandle,
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
    let server = test_utils::nats::NatsTestServer::spawn()?;
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in [
        ("jobs/impl-cmd.yaml", IMPL_CMD_EVAL),
        ("jobs/impl-agent.yaml", IMPL_AGENT_EVAL),
        ("jobs/flaky.yaml", FLAKY),
        ("jobs/cmd-work.yaml", CMD_WORK),
        ("jobs/staged.yaml", STAGED),
        ("jobs/staged-advisory.yaml", STAGED_ADVISORY),
        ("jobs/webpub.yaml", WEBPUB),
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
            // Enables the operator-dispatched triage action (§1.2).
            triage_image: Some("triage:latest".into()),
            launch_queue_max_wait,
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
        backend,
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

/// Registers a work run that commits a stub file to the job branch — the
/// minimal "the agent produced output" so the §3.2 empty-output guard is
/// satisfied and the job advances to Evaluation. Mirrors a real work
/// container's commit+push. First-cycle only: rework cycles inherit the branch.
fn commit_work(rig: &Rig) {
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
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
        clone
            .commit_file("src/new.rs", b"pub fn f() {}", "implement")
            .await;
        clone.push(&branch).await;
    });
    // Command evaluator: exit 0 with structured findings.
    rig.backend.put_file(
        "/workspace/eval-result.json",
        br#"{"coverage": 91}"#.to_vec(),
    );

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
}

#[tokio::test]
async fn work_failure_retries_with_reset_then_escalates() {
    let Some(rig) = rig().await else { return };
    rig.provider.script_exits([2, 3]); // work_retries: 1 → both attempts fail

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    // attempt 1 Failed, attempt 2 Failed, Human escalation task Pending
    assert_eq!(tasks.len(), 3);
    assert_eq!((tasks[0].attempt, tasks[0].state), (1, TaskState::Failed));
    assert_eq!((tasks[1].attempt, tasks[1].state), (2, TaskState::Failed));
    assert!(matches!(tasks[2].kind, types::TaskKind::Human { .. }));
    assert_eq!(tasks[2].state, TaskState::Pending);
    assert_eq!(rig.provider.runs().len(), 2);
}

/// §3.2 empty-output guard: a work container that exits 0 but leaves the branch
/// empty AND submits no summary (the job-79 finish-line signature — a headless
/// agent that ended its turn before committing) is a genuine failure, not a
/// success. Each attempt fails with reason `no_output_produced`, a work retry is
/// burned and the attempt relaunches, and the job NEVER enters Evaluation.
#[tokio::test]
async fn work_exit0_empty_branch_empty_summary_fails_with_no_output() {
    let Some(rig) = rig().await else { return };
    // Default provider: both attempts exit 0, commit nothing, submit nothing.
    // flaky declares work_retries: 1, so attempt 1 fails → relaunch → attempt 2
    // fails → escalate.

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    // Two work attempts ran: the guard burned a retry and relaunched.
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
    // The empty-output failure never advanced the job to Evaluation.
    assert!(
        !tasks.iter().any(|t| t.phase == TaskPhase::Evaluation),
        "no Evaluation task may exist: {tasks:?}"
    );
    // Retries exhausted → the standard work escalation.
    assert_eq!(
        escalated.escalation.expect("escalation recorded").reason,
        "work_retries_exhausted"
    );

    // The machine reason rode the task-failed events, so the UI can show
    // "exited without producing changes" instead of a silent Done cycle.
    let failed: Vec<_> = job_events(&rig.store, job.id)
        .await
        .into_iter()
        .filter(|e| e["event_type"] == "task-failed")
        .collect();
    assert!(
        failed.iter().any(|e| e["reason"] == "no_output_produced"),
        "a task-failed event must carry the machine reason: {failed:?}"
    );
}

/// §3.2 empty-output guard, exception path: an empty branch with a NON-empty
/// summary is a deliberate "no change is the correct outcome", not a finish-line
/// death — so it proceeds to Evaluation (here: no evaluators → straight to Done)
/// and no retry is burned.
#[tokio::test]
async fn work_exit0_empty_branch_with_summary_proceeds() {
    let Some(rig) = rig().await else { return };
    // Work reports a summary over the handle (like submit_result) but commits
    // nothing — the branch stays empty.
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
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
}

/// §3.2 empty-output guard, regression: exit 0 WITH commits is unchanged even
/// when no summary is submitted — a real work run advances to Evaluation.
#[tokio::test]
async fn work_exit0_with_commits_and_no_summary_proceeds() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig); // commits, submits no summary
    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
}

/// Dogfood-#1 regression: an eval container that fails to *launch* must not
/// leave the task `Running` and the job wedged in `Evaluation`. The launch
/// error flows through the task-failure machinery: task Failed with the error
/// in its result → eval_retries → required infra failure → job Escalated.
#[tokio::test]
async fn eval_launch_failure_escalates_instead_of_stuck_running() {
    let Some(rig) = rig().await else { return };
    // The eval command container is refused at launch (e.g. an invalid resolved
    // resource limit). Agent work runs through the provider, so the only
    // `backend.launch` calls in this rig are the eval containers.
    rig.backend
        .fail_launch_if(|_| Some("invalid memory limit \"5g\"".into()));
    commit_work(&rig); // work succeeds (agent path) so the job reaches Evaluation

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    // Never stalls in Evaluation with a Running task: after eval_retries the
    // required evaluator's infra failure escalates.
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    // Work Done; two eval attempts Failed (eval_retries default 1); Human
    // escalation Pending. No task is left Running.
    assert_eq!(tasks[0].phase, TaskPhase::Work);
    assert_eq!(tasks[0].state, TaskState::Done);
    let evals: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Evaluation)
        .collect();
    assert_eq!(evals.len(), 2, "eval attempt + one eval_retries");
    for t in &evals {
        assert_eq!(t.state, TaskState::Failed);
        // The launch error is surfaced in the result — single-wrapped, no
        // spurious `job not found:` / doubled `launch failed:` — so
        // `GET .../tasks` tells the operator what happened.
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
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    // The escalation is self-describing on the job record: reason code, a
    // human-readable detail, and the failing task — no log archaeology (#69).
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
    // The escalation Human task also carries phase Work (escalation::
    // escalation_task), so exclude it — `works` is the container work attempts.
    let works: Vec<_> = tasks
        .iter()
        .filter(|t| t.phase == TaskPhase::Work && !matches!(t.kind, types::TaskKind::Human { .. }))
        .collect();
    // The failing task named on the escalation is the last failed work attempt.
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
}

#[tokio::test]
async fn eval_failure_reworks_with_context_then_passes() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    // Run order: work c1, eval c1 (fail w/ findings), work c2, eval c2 (pass).
    // Task ids are sequential per job: 1=work, 2=eval, 3=work, 4=eval.
    commit_work(&rig); // work cycle 1 commits so the branch is non-empty
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
            },
        )
        .await
        .unwrap();
    });
    rig.provider.on_run(|_| async {}); // work cycle 2
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
            },
        )
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
    // The rework-created Work task self-explains its cause; cycle 1 has none.
    assert_eq!(tasks[0].rework_reason, None);
    assert_eq!(
        tasks[2].rework_reason,
        Some(types::ReworkReason::EvalFailure)
    );
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-rework-started".to_string()));
}

/// §3.2 crash recovery: an attempt that pushes commits then crashes is retried
/// on the SAME branch — the commits survive, the retry's prompt notes the
/// resume, and the recovered work lands on merge instead of being redone.
#[tokio::test]
async fn crashed_work_attempt_recovers_branch_and_notes_resume() {
    let Some(rig) = rig().await else { return };
    let bare = rig.repo.bare_path();

    // Attempt 1 pushes a commit, then "crashes" (the scripted non-zero exit).
    // Attempt 2 (no hook) just succeeds — nothing to push, the recovered commit
    // is the whole product.
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("wip.rs", b"partial work", "wip").await;
        clone.push(&branch).await;
    });
    rig.provider.script_exits([2, 0]); // flaky: work_retries 1 → crash then recover

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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

    // The crashed attempt's commit was recovered — not redone — and merged.
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
}

// ---- §3.5 capacity queue: no free slots defers the launch, never fails it ----

use std::sync::atomic::{AtomicBool, Ordering};

/// Poll the task log for the first task matching `pred`.
async fn wait_for_task(
    store: &NatsStore,
    seq: u64,
    pred: impl Fn(&types::Task) -> bool,
) -> types::Task {
    let tasks = store.tasks().await.unwrap();
    for _ in 0..100 {
        if let Some(t) = tasks
            .list_for_job("acme", "api", seq)
            .await
            .unwrap()
            .into_iter()
            .find(|t| pred(t))
        {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for task");
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

    // Fleet at capacity until we free it. Agent work runs through the provider,
    // so the only `backend.launch` here is the command evaluator.
    let full = Arc::new(AtomicBool::new(true));
    let f = full.clone();
    rig.backend.fail_launch_no_capacity_if(move |_| {
        f.load(Ordering::SeqCst)
            .then(|| "no free slots on any node".to_string())
    });

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();

    // The eval task parks Pending (queued), holding the job in Evaluation.
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

    // Free a slot; the scan message drives the drain and the eval launches.
    full.store(false, Ordering::SeqCst);
    rig.handle.trigger_scan().await.unwrap();
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
    rig.handle.release_job("acme", "api", job.id).await.unwrap();

    let work = wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Work && t.state == TaskState::Pending
    })
    .await;
    assert_eq!(work.attempt, 1, "queueing consumed no work_retries");
    assert!(work.container_id.is_none());
    // The deferred launch is stamped visibly-queued: the UI reads *why* it is
    // Pending and *since when* off the record, not just a bare Pending.
    assert_eq!(
        work.pending_reason,
        Some(types::PendingReason::QueuedForCapacity)
    );
    assert!(work.queued_at.is_some());
    // The queue snapshot the api forwards reflects the same launch: depth 1, and
    // this task at position 1 — the "position 1 of 1" the badge shows.
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
    // Launching cleared the queued markers, and the queue drained empty.
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
}

/// A launch wedged in the queue past the maximum wait escalates with the clear
/// `no_free_slots_timeout` reason — the backstop for a genuinely stuck fleet
/// (§3.5). The rig shrinks the max wait so the scan fires it immediately.
#[tokio::test]
async fn queued_launch_escalates_after_max_wait() {
    let Some(rig) = rig_full(None, Some(Duration::from_millis(1))).await else {
        return;
    };
    // Fleet stays full for the whole test — the launch never gets a slot.
    rig.backend
        .fail_launch_no_capacity_if(|_| Some("no free slots on any node".to_string()));

    let job = rig.handle.create_job(req("cmd-work")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();

    // It queues first (Pending), then the backstop scan escalates it.
    wait_for_task(&rig.store, job.id, |t| {
        t.phase == TaskPhase::Work && t.state == TaskState::Pending
    })
    .await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    rig.handle.trigger_scan().await.unwrap();
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
    // The queued work task is Failed (not left Pending) and a Human escalation
    // task names the wedge.
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
async fn agent_launch_carries_channel_mcp_and_decrypted_secrets() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
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

    // Encrypted write with the public key (the API layer's path)…
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
        // Platform agent credential (reserved global/agents scope): reaches
        // every agent container without any declaration in the job type.
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

    // SSH front enabled: generate a CA so launches carry a job cert (§5.2).
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
        // Work produces a commit so the job clears the §3.2 empty-output guard.
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
    let handle = spawn(core);

    let job = handle.create_job(req("impl-secret")).await.unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&store, job.id, JobState::Done).await;

    let runs = provider.runs();
    assert_eq!(runs.len(), 1);
    // …decrypted read at launch (the dispatcher's path).
    assert_eq!(
        runs[0].env.get("DEPLOY_KEY").map(String::as_str),
        Some("s3cret-value")
    );
    // Platform agent credential injected without being declared anywhere.
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
    // §7.4: per-launch scoped credentials, forwarded to the channel binary.
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
    // Channel binary + §5.2 job SSH credential (key 0600 + cert).
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
}

/// The artifacts a job leaves behind. Before this, an agent's session transcript
/// died with its container: the provider dropped the container id, so nothing
/// could name the file even though the container itself was never removed.
#[tokio::test]
async fn agent_run_captures_transcript_logs_and_measured_usage() {
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity.clone())).await else {
        return;
    };

    // stdout as the real CLI emits it under `--output-format stream-json`: a
    // stream of JSONL events whose final `type:"result"` event carries the
    // authoritative usage. Harvesting scans for the last parseable line.
    rig.backend.put_logs(
        br#"Cloning into '/workspace'...
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}],"usage":{"input_tokens":1200,"output_tokens":10}},"session_id":"s"}
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
        clone
            .commit_file("src/new.rs", b"pub fn f() {}", "implement")
            .await;
        clone.push(&branch).await;
    });
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig.store.tasks().await.unwrap();
    let log = tasks.list_for_job("acme", "api", job.id).await.unwrap();
    let work = log.iter().find(|t| t.phase == TaskPhase::Work).unwrap();

    // The session id is persisted, so the transcript stays addressable after a
    // restart rather than being lost with the process.
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
    // Command evals run no agent, so they have no session.
    assert!(eval.session_id.is_none());
    assert!(!session_id.is_empty());

    // The leak fix (spec §3.1): every container the job ran is removed once its
    // result is recorded. The artifacts above were all read out first, so this
    // proves capture-happens-before-removal — a job leaves nothing on the node.
    let removed = rig.backend.removed();
    assert_eq!(
        removed.len(),
        rig.backend.launches().len(),
        "every launched container should be removed after its task exits"
    );
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
    clone
        .commit_file("jobs/deploy-none.yaml", DEPLOY_NONE.as_bytes(), "type")
        .await;
    clone.push("main").await;

    // The "agent" commits scratch to its branch, like a deploy that jots notes.
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
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // The evaluator still ran; nothing landed on main; the branch is gone.
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
    // The publish container clones main and, in the moment it runs, must see the
    // merged content already on the default branch.
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
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // Task log: work Done, then the WrapUp command task Done.
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

    // The single backend launch is the publish, and it cloned the DEFAULT
    // branch (merged main), not the scratch job branch.
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
    rig.backend.script_exits([7]); // the publish command fails

    let job = rig.handle.create_job(req("webpub")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    // The merge stands — the change is on main despite the failed publish.
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

    // The WrapUp command is Failed and a Human escalation task is open.
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
}

/// A job revoked before it lands never runs its `wrap_up.run` command: the
/// publish only fires off a successful merge, never off a revoke.
#[tokio::test]
async fn revoked_job_never_runs_wrap_up_command() {
    let Some(rig) = rig().await else { return };

    // Hold the work agent open so the revoke lands while the job is in Work —
    // well before any merge or wrap-up.
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (s2, r2) = (started.clone(), release.clone());
    rig.provider.on_run(move |_cfg| async move {
        s2.notify_one();
        r2.notified().await; // block until the test lets go
    });

    let job = rig.handle.create_job(req("webpub")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    started.notified().await; // work is running
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    release.notify_one(); // let the (now-orphaned) work run return

    wait_for_state(&rig.store, job.id, JobState::Revoked).await;
    // Give any erroneous follow-on work a moment to (not) happen.
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
}

/// Abort verdict: a required evaluator declaring the work unsalvageable
/// escalates immediately — the remaining rework budget is not consumed.
#[tokio::test]
async fn eval_abort_escalates_without_consuming_rework_budget() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig); // work cycle 1 produces a commit
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
            },
        )
        .await
        .unwrap();
    });

    // impl-agent has rework_budget: 1 — abort must not spend it.
    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
        // Stage-0 review verdict (task 2).
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
    });

    let job = rig.handle.create_job(req("staged")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    // work(1), review(2, stage 0), ci(3, stage 1) — nothing else.
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[1].evaluator.as_deref(), Some("review"));
    assert_eq!(tasks[1].stage, 0);
    assert_eq!(tasks[2].evaluator.as_deref(), Some("ci"));
    assert_eq!(tasks[2].stage, 1);
    // The gate is ordered: ci is created only after review completes.
    assert!(tasks[2].created_at >= tasks[1].completed_at.unwrap());
    assert_eq!(
        rig.provider.runs().len(),
        2,
        "only work + review run agents"
    );
}

/// Required stage-0 failure short-circuits: the stage-1 `ci` task is never
/// created for that cycle, and the rework cycle restarts from stage 0 (review
/// again). Directly the acceptance case — a review-rejected change spends no CI.
#[tokio::test]
async fn staged_eval_required_review_fail_skips_ci_and_reworks_from_stage0() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig); // work cycle 1 produces a commit
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        // Stage-0 review fails (task 2) → short-circuit; no ci this cycle.
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
            },
        )
        .await
        .unwrap();
    });
    rig.provider.on_run(|_| async {}); // work cycle 2
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        // Rework restarts at stage 0: review again (task 4), now passing.
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
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("staged")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
    // Cycle 1's failed review created no ci task; cycle 2's passing review did.
    assert!(!ci(1), "stage-1 ci must not run when stage-0 review fails");
    assert!(ci(2), "ci runs once review passes on the rework cycle");
    // The rework cycle re-opened at stage 0 with a fresh review task.
    assert!(tasks.iter().any(|t| {
        t.phase == TaskPhase::Evaluation
            && t.cycle == 2
            && t.evaluator.as_deref() == Some("review")
            && t.stage == 0
    }));
}

/// Advisory stage-0 failure does not block: a `required: false` review that
/// fails still lets the stage-1 `ci` evaluator run, and the required ci pass
/// carries the job to Done.
#[tokio::test]
async fn staged_eval_advisory_review_fail_still_runs_ci() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig); // work produces a commit
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            2,
            EvalSubmission {
                pass: false, // advisory fail
                abort: false,
                structured: None,
                token_usage: None,
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("staged-advisory")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    // work(1), advisory review(2, failed), ci(3, passed) → merged.
    assert_eq!(tasks.len(), 3);
    assert!(matches!(
        tasks[1].result,
        Some(types::TaskResult::Agent { pass: false, .. })
    ));
    assert_eq!(tasks[2].evaluator.as_deref(), Some("ci"));
    assert_eq!(tasks[2].state, TaskState::Done);
}

/// An abort from a required stage-0 evaluator escalates immediately: no later
/// stage is created, and (like any abort) the rework budget is not consumed.
#[tokio::test]
async fn staged_eval_stage0_abort_escalates_without_ci() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig); // work produces a commit
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
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("staged")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
    // work(1), review abort(2), Human escalation(3) — no ci ever.
    assert_eq!(tasks.len(), 3);
    assert!(
        !tasks.iter().any(|t| t.evaluator.as_deref() == Some("ci")),
        "the stage-1 ci evaluator must never be created after a stage-0 abort"
    );
    assert!(matches!(tasks[2].kind, types::TaskKind::Human { .. }));
}

/// Additive per-job evaluators: layered on top of the type's list and executed
/// like declared ones.
#[tokio::test]
async fn job_level_evaluators_run_alongside_type_evaluators() {
    let Some(rig) = rig().await else { return };

    commit_work(&rig); // work must produce output to reach Evaluation
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
        stage: 0,
    }];
    let job = rig.handle.create_job(r).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
}

/// §4.4 upfront knowledge injection: the union of the type's `knowledge:`
/// defaults and the job's tags rides the work agent's system prompt, read
/// from tags/{tag}.md at base_ref. Unknown tags are skipped.
#[tokio::test]
async fn knowledge_tags_inject_into_work_system_prompt() {
    let Some(rig) = rig().await else { return };

    commit_work(&rig); // work must produce output to reach Done
    let mut create = req("flaky"); // type declares knowledge: [rust]
    create.knowledge_tags = vec!["style".into(), "no-such-tag".into()];
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
        stage: 0,
    }];
    let job = rig.handle.create_job(r).await.unwrap(); // creation always lands Frozen
    let err = rig
        .handle
        .release_job("acme", "api", job.id)
        .await
        .unwrap_err();
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

    commit_work(&rig); // work commits real content so wrap-up has a squash to run
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        // Sabotage wrap-up: the branch vanishes before the squash-merge.
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
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
}

// ── Issue #31: per-job timeout override + operator-dispatched triage ────────

/// The per-job `Job.timeout` override (§1.1) drives the Work agent's own run
/// timeout, while the evaluator keeps the type default — the override is
/// Work-scoped. Asserted at the mechanism: the recorded run configs.
#[tokio::test]
async fn work_timeout_override_applies_to_work_not_eval() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    commit_work(&rig); // work c1 produces a commit
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
            },
        )
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
    rig.provider
        .on_run(|_| async { tokio::time::sleep(Duration::from_secs(30)).await });

    let mut create = req("impl-agent"); // no work_retries → escalates on first fail
    create.timeout = Some("1s".into());
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Work).await;

    // Age the Running work task past the 1s override, then scan.
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
    let work = tasks.iter().find(|t| t.phase == TaskPhase::Work).unwrap();
    assert_eq!(
        work.state,
        TaskState::Failed,
        "the override should have timed out the work task"
    );
}

/// The evaluator keeps the type default even when the job carries a short work
/// override: a scan after the (short) override elapses must not touch the
/// Evaluation-phase task — the override does not leak past Work.
#[tokio::test]
async fn eval_task_ignores_work_timeout_override() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig); // work c1: produces a commit and exits immediately
    // Eval "container" blocks so it is Running when the scan fires.
    rig.provider
        .on_run(|_| async { tokio::time::sleep(Duration::from_secs(30)).await });

    let mut create = req("impl-agent");
    create.timeout = Some("1s".into());
    let job = rig.handle.create_job(create).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;

    // Age well past the 1s work override; the eval task's 1h default protects it.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.handle.trigger_scan().await.unwrap();

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
}

/// A malformed `Job.timeout` is rejected at release (§1.1: parseability
/// validated at release, not creation) — creation still succeeds.
#[tokio::test]
async fn malformed_timeout_override_rejected_at_release() {
    let Some(rig) = rig().await else { return };
    let mut create = req("impl-agent");
    create.timeout = Some("2 hours".into()); // not a valid duration string
    let job = rig.handle.create_job(create).await.unwrap(); // creation is permissive
    let err = rig
        .handle
        .release_job("acme", "api", job.id)
        .await
        .unwrap_err();
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
    let Some(rig) = rig_with_artifacts(Some(identity)).await else {
        return;
    };

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
        if let Some(t) = log
            .iter()
            .find(|t| t.phase == TaskPhase::Triage && t.result.is_some())
        {
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

    // The run used the platform triage image and carried no channel MCP.
    let last = rig.provider.runs().pop().unwrap();
    assert_eq!(last.image, "triage:latest");
    assert!(
        last.mcp_servers.is_empty(),
        "triage runs without the channel MCP"
    );
}

/// Triage is rejected unless the job is Escalated or Stalled (§1.2).
#[tokio::test]
async fn triage_rejected_on_non_intervention_state() {
    let Some(rig) = rig().await else { return };
    // A freshly created job is Frozen.
    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    let err = rig
        .handle
        .triage_job("acme", "api", job.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, dispatcher::core::CoreError::Conflict(_)),
        "{err:?}"
    );
}

/// Poll the task log until the job's Work-phase task exists, then return it.
/// The record is written at launch with its resolved kind, so it is readable
/// even after the job advances past Work.
async fn wait_for_work_task(store: &NatsStore, seq: u64) -> types::Task {
    let tasks = store.tasks().await.unwrap();
    for _ in 0..100 {
        let log = tasks.list_for_job("acme", "api", seq).await.unwrap();
        if let Some(t) = log.into_iter().find(|t| t.phase == TaskPhase::Work) {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for a work task for #{seq}");
}

// §12.4 model resolution: a per-job `Job.model` override lands on the Work
// agent task. `impl-cmd` declares no `work.model`, this rig has no
// `jobs/_defaults.yaml` and no platform `agent_model_default`, so the override
// is the only source — proving it flows create → launch → the task's kind.
#[tokio::test]
async fn per_job_model_override_reaches_work_task() {
    let Some(rig) = rig().await else { return };
    let mut r = req("impl-cmd");
    r.model = Some("claude-fable-5".into());
    let job = rig.handle.create_job(r).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();

    let work = wait_for_work_task(&rig.store, job.id).await;
    match work.kind {
        types::TaskKind::Agent { model, .. } => {
            assert_eq!(model.as_deref(), Some("claude-fable-5"));
        }
        other => panic!("expected an agent work task, got {other:?}"),
    }
}

// The baseline: with no override, no project default, and no platform default,
// the Work agent's model resolves to None (the provider's built-in default).
#[tokio::test]
async fn work_task_model_none_without_any_default() {
    let Some(rig) = rig().await else { return };
    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();

    let work = wait_for_work_task(&rig.store, job.id).await;
    match work.kind {
        types::TaskKind::Agent { model, .. } => assert_eq!(model, None),
        other => panic!("expected an agent work task, got {other:?}"),
    }
}

/// #71/#72 regression: an agent work container's id lands on the task record
/// while it is still Running — not only in `AgentOutput` after exit — and
/// survives on the finished record.
#[tokio::test]
async fn work_container_id_recorded_while_running_and_kept_after_exit() {
    // Artifacts identity makes the fake provider launch through the backend, so
    // the run reports a container id (as the real ClaudeProvider does).
    let (identity, _public) = store::secrets::generate_age_keypair();
    let Some(rig) = rig_with_artifacts(Some(identity)).await else {
        return;
    };
    let bare = rig.repo.bare_path();
    let store = rig.store.clone();
    rig.provider.on_run(move |cfg| async move {
        // The "container" is alive until this hook returns: the id must already
        // be on the Running task record by now.
        let tasks = store.tasks().await.unwrap();
        let mut recorded = None;
        for _ in 0..100 {
            if let Some(t) = tasks.get("acme", "api", 1, 1).await.unwrap()
                && t.state == TaskState::Running
                && t.container_id.is_some()
            {
                recorded = t.container_id.clone();
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            recorded.is_some(),
            "container_id must be set while the work task is Running"
        );
        // Commit so the job lands real content.
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/x.rs", b"pub fn x() {}", "impl")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // Kept after exit: the completed record still names its container.
    let work = wait_for_work_task(&rig.store, job.id).await;
    assert_eq!(work.state, TaskState::Done);
    assert!(
        work.container_id.is_some(),
        "container_id must survive on the completed record"
    );
}

/// A required agent evaluator's abort verdict escalates immediately, and the
/// job records the reason code + human-readable detail (#69-class diagnosis on
/// the record, not in the logs). Evaluation-phase escalations have no single
/// culprit task, so `failing_task` is None.
#[tokio::test]
async fn abort_verdict_escalates_with_recorded_reason() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();
    commit_work(&rig); // work c1 produces a commit
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
            },
        )
        .await
        .unwrap();
    });

    let job = rig.handle.create_job(req("impl-agent")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    let esc = escalated.escalation.expect("abort records an escalation");
    assert_eq!(esc.reason, "eval_abort");
    assert!(
        esc.detail.contains("not satisfiable by rework"),
        "detail: {}",
        esc.detail
    );
    assert_eq!(esc.failing_task, None);
}

//! Tier-2 tests for the merge gate (§3.3) and human task resolution (§1.2):
//! gate re-run against the candidate commit when HEAD moved, gate failure →
//! rework on the new base, human work/evaluator resolution, and escalation
//! Retry.

use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateJobRequest, spawn};
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{EscalationAction, JobState, TaskPhase, TaskResolution, TaskState};

const IMPL_CMD: &str = r#"
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

const HUMAN_EVAL: &str = r#"
name: human-gated
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: approval
    type: human
    prompt: prompts/approve.md
"#;

const HUMAN_WORK: &str = r#"
name: manual
work:
  type: human
  prompt: prompts/manual.md
eval:
  - name: check
    type: command
    run: ./check.sh
    image: img:latest
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
        ("jobs/impl-cmd.yaml", IMPL_CMD),
        ("jobs/human-gated.yaml", HUMAN_EVAL),
        ("jobs/manual.yaml", HUMAN_WORK),
        ("jobs/flaky.yaml", FLAKY),
        ("prompts/impl.md", "implement it"),
        ("prompts/approve.md", "approve it"),
        ("prompts/manual.md", "do it by hand"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let backend = Arc::new(FakeBackend::new());
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
        deps: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        factory: None,
    }
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
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

/// The work hook commits to the job branch and, before finishing, lands an
/// unrelated commit on main — the concurrent-job scenario the gate exists for.
fn commit_and_move_main(rig: &Rig, file: &'static str) {
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file(file, b"job change", "implement").await;
        clone.push(&branch).await;

        let main = clone_branch_from(&bare, "main").await;
        main.commit_file("docs/other.md", b"landed concurrently", "other job")
            .await;
        main.push("main").await;
    });
}

#[tokio::test]
async fn merge_gate_reruns_ci_against_candidate_and_promotes() {
    let Some(rig) = rig().await else { return };
    commit_and_move_main(&rig, "src/a.rs");
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // Both the concurrent land and the job's change are on main.
    let m = &rig.repo.manager;
    assert!(
        m.read_file_at("acme", "api", "main", "docs/other.md")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        m.read_file_at("acme", "api", "main", "src/a.rs")
            .await
            .unwrap()
            .is_some()
    );

    // Task log: work, eval, then a MergeGate re-run of the command evaluator.
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[2].phase, TaskPhase::MergeGate);
    assert_eq!(tasks[2].state, TaskState::Done);
    // Gate containers clone the candidate ref, not the job branch.
    let launches = rig.backend.launches();
    assert_eq!(launches.len(), 2);
    assert_eq!(launches[0].env["JOB_BRANCH"], format!("job/{}", job.id));
    assert_eq!(
        launches[1].env["JOB_BRANCH"],
        format!("merge-gate/{}", job.id)
    );

    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-merge-gate-started".to_string()));
    // The candidate ref is cleaned up after promotion.
    assert!(
        m.resolve_ref("acme", "api", &format!("merge-gate/{}", job.id))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn merge_gate_failure_reworks_on_new_base_without_budget() {
    let Some(rig) = rig().await else { return };
    // eval c1 pass, gate c1 FAIL, eval c2 pass (no second gate: base caught up).
    rig.backend.script_exits([0, 1, 0]);
    commit_and_move_main(&rig, "src/a.rs");
    // Rework hook: re-commit on the new base.
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        assert!(
            !cfg.eval_context.is_empty(),
            "gate findings must be injected"
        );
        assert!(
            cfg.merge_conflict.is_some(),
            "conflict-style context must be injected"
        );
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change v2", "implement again")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let events = event_types(&rig.store).await;
    let reworks: Vec<&String> = events
        .iter()
        .filter(|e| *e == "job-rework-started")
        .collect();
    assert_eq!(reworks.len(), 1);
    // rework_budget is 0 on this job type — a consumed budget would have
    // escalated instead of reworking. Reaching Done proves it wasn't consumed.
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    // work c1, eval c1, gate c1 (fail), work c2, eval c2
    assert_eq!(tasks.len(), 5);
    assert_eq!(tasks[3].cycle, 2);
    assert!(
        rig.repo
            .manager
            .read_file_at("acme", "api", "main", "src/a.rs")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn human_evaluator_and_human_work_resolve_via_inbox() {
    let Some(rig) = rig().await else { return };

    // Agent work + human evaluator: job parks in Evaluation on a Pending task.
    let gated = rig.handle.create_job(req("human-gated")).await.unwrap();
    rig.handle
        .release_job("acme", "api", gated.id)
        .await
        .unwrap();
    wait_for_state(&rig.store, gated.id, JobState::Evaluation).await;
    tokio::time::sleep(Duration::from_millis(100)).await; // task record settles

    // Wrong kind is rejected.
    let err = rig
        .handle
        .resolve_task(
            "acme",
            "api",
            gated.id,
            2,
            TaskResolution::Escalation {
                action: EscalationAction::Retry,
                structured: None,
            },
            "david",
        )
        .await;
    assert!(err.is_err());

    rig.handle
        .resolve_task(
            "acme",
            "api",
            gated.id,
            2,
            TaskResolution::Pass {
                structured: None,
                summary: None,
            },
            "david",
        )
        .await
        .unwrap();
    wait_for_state(&rig.store, gated.id, JobState::Done).await;

    // Human work: Pending task in Work phase; Pass → command eval → Done.
    let manual = rig.handle.create_job(req("manual")).await.unwrap();
    rig.handle
        .release_job("acme", "api", manual.id)
        .await
        .unwrap();
    wait_for_state(&rig.store, manual.id, JobState::Work).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.handle
        .resolve_task(
            "acme",
            "api",
            manual.id,
            1,
            TaskResolution::Pass {
                structured: None,
                summary: None,
            },
            "david",
        )
        .await
        .unwrap();
    let done = wait_for_state(&rig.store, manual.id, JobState::Done).await;
    assert!(done.ready_at.is_some());

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", manual.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(matches!(
        tasks[0].result,
        Some(types::TaskResult::Human { pass: true, ref operator, .. }) if operator == "david"
    ));
}

#[tokio::test]
async fn escalation_retry_relaunches_work_without_branch_reset() {
    let Some(rig) = rig().await else { return };
    rig.provider.script_exits([1, 1]); // exhaust work_retries: 1

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Task 3 is the escalation task; Retry relaunches in the same cycle.
    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            3,
            TaskResolution::Escalation {
                action: EscalationAction::Retry,
                structured: None,
            },
            "david",
        )
        .await
        .unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await; // third run exits 0

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 4);
    assert_eq!((tasks[3].cycle, tasks[3].attempt), (1, 3)); // same cycle, attempt++
    assert_eq!(tasks[3].state, TaskState::Done);
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-escalation-resolved".to_string()));
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

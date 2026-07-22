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

// Agent work + command eval WITH a rework budget: lets an eval failure drive a
// real eval-failure rework (Change A) rather than escalating on the spot.
const REWORKABLE: &str = r#"
name: reworkable
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
rework_budget: 1
eval:
  - name: tests
    type: command
    run: ./ci.sh
"#;

// Agent work, NO evaluators: auto-squashes at wrap-up with no review to catch
// conflict markers — the case the §3.2 step-12 marker guard exists for.
const NO_EVAL: &str = r#"
name: no-eval
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
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
        ("jobs/reworkable.yaml", REWORKABLE),
        ("jobs/no-eval.yaml", NO_EVAL),
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
    // The operator inbox (`req.tasks.list.pending`) is served off the core actor
    // by the tasks handler; wire it up so tests can hit the real request path.
    dispatcher::handlers::spawn_tasks_handler(&store, handle.clone(), backend.clone())
        .await
        .unwrap();
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

/// Work hook: commit `file` to the job branch, nothing else. `main` stays put,
/// so any later movement is the test's to schedule.
fn commit_branch(rig: &Rig, file: &'static str) {
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file(file, b"job change", "implement").await;
        clone.push(&branch).await;
    });
}

/// Work hook that commits to the job branch AND lands an unrelated commit on
/// main before finishing — the concurrent-job scenario, resolved at Evaluation
/// entry by a rebase rather than a wrap-up gate (§3.2).
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

/// Land an unrelated commit on main while the first evaluation container runs —
/// main moving *during* evaluation is the only case the wrap-up gate still
/// fires for (§3.3).
fn move_main_during_eval(rig: &Rig) {
    let bare = rig.repo.bare_path();
    rig.backend.on_launch(move |_cfg| async move {
        let main = clone_branch_from(&bare, "main").await;
        main.commit_file("docs/other.md", b"landed during eval", "other job")
            .await;
        main.push("main").await;
    });
}

/// Main moves past `base_ref` during WORK → the branch is rebased onto the new
/// HEAD at Evaluation entry, evaluated as it will land, and squashed straight
/// in at wrap-up with NO merge gate (§3.2 pre-eval rebase).
#[tokio::test]
async fn main_moves_during_work_rebases_and_skips_gate() {
    let Some(rig) = rig().await else { return };
    commit_and_move_main(&rig, "src/a.rs");
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    let done = wait_for_state(&rig.store, job.id, JobState::Done).await;

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

    // Only work + eval; the rebase means no MergeGate task ever exists.
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|t| t.phase != TaskPhase::MergeGate));
    // Evaluation ran against the rebased job branch (not a gate candidate).
    let launches = rig.backend.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].env["JOB_BRANCH"], format!("job/{}", job.id));

    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-rebased".to_string()));
    assert!(!events.contains(&"job-merge-gate-started".to_string()));
    // base_ref advanced to the HEAD it was evaluated (and merged) against.
    let head = done.base_ref.unwrap();
    assert_eq!(
        m.read_file_at("acme", "api", &head, "docs/other.md")
            .await
            .unwrap()
            .as_deref(),
        Some("landed concurrently")
    );
}

/// Main moves *during evaluation* → the tested stacking is stale, so the
/// wrap-up merge gate re-runs the required command evaluator against the
/// candidate and promotes it (§3.3), exactly as before the pre-eval rebase.
#[tokio::test]
async fn main_moves_during_eval_fires_merge_gate() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/a.rs");
    move_main_during_eval(&rig);
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

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
    // Main did not move during work, so there was no pre-eval rebase.
    assert!(!events.contains(&"job-rebased".to_string()));
    // The candidate ref is cleaned up after promotion.
    assert!(
        m.resolve_ref("acme", "api", &format!("merge-gate/{}", job.id))
            .await
            .is_err()
    );
}

/// Main untouched throughout → no rebase, no gate: byte-identical to the
/// pre-feature fast path (§3.2 / §3.3).
#[tokio::test]
async fn no_movement_evaluates_and_merges_without_rebase_or_gate() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/a.rs");
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let m = &rig.repo.manager;
    assert!(
        m.read_file_at("acme", "api", "main", "src/a.rs")
            .await
            .unwrap()
            .is_some()
    );

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|t| t.phase != TaskPhase::MergeGate));
    assert_eq!(rig.backend.launches().len(), 1);

    let events = event_types(&rig.store).await;
    assert!(!events.contains(&"job-rebased".to_string()));
    assert!(!events.contains(&"job-rebase-conflict".to_string()));
    assert!(!events.contains(&"job-merge-gate-started".to_string()));
}

/// A concurrent land during WORK that *conflicts* with the job's change → the
/// pre-eval rebase aborts cleanly (commits kept as pushed), evaluation runs on
/// the old base, and the wrap-up conflict machinery reworks it onto the new
/// base — no commits lost (§3.2 rebase-conflict fall-through).
#[tokio::test]
async fn rebase_conflict_falls_through_to_wrapup_conflict_path() {
    let Some(rig) = rig().await else { return };
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());
    let bare = rig.repo.bare_path();
    // Work c1: commit src/a.rs on the branch AND land a conflicting src/a.rs on
    // main, so the rebase at eval entry cannot replay cleanly.
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change", "implement")
            .await;
        clone.push(&branch).await;

        let main = clone_branch_from(&bare, "main").await;
        main.commit_file("src/a.rs", b"conflicting land", "other job")
            .await;
        main.push("main").await;
    });
    // Work c2 (conflict rework): re-commit on the freshly pinned base.
    let bare2 = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        assert!(
            cfg.merge_conflict.is_some(),
            "conflict-style context must be injected on the rework"
        );
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare2, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change v2", "implement again")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let events = event_types(&rig.store).await;
    // The rebase reported a conflict and left the branch as pushed…
    assert!(events.contains(&"job-rebase-conflict".to_string()));
    // …and the wrap-up conflict path (not the gate) drove the single rework.
    let reworks: Vec<&String> = events
        .iter()
        .filter(|e| *e == "job-rework-started")
        .collect();
    assert_eq!(reworks.len(), 1);
    assert!(!events.contains(&"job-merge-gate-started".to_string()));

    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    // work c1, eval c1 (old base), work c2, eval c2 — no gate task.
    assert_eq!(tasks.len(), 4);
    assert_eq!(tasks[2].cycle, 2);
    // The conflict-driven rework Work task records why it exists.
    assert_eq!(
        tasks[2].rework_reason,
        Some(types::ReworkReason::MergeConflict)
    );
    assert!(tasks.iter().all(|t| t.phase != TaskPhase::MergeGate));
    // The reworked change is what landed.
    assert_eq!(
        rig.repo
            .manager
            .read_file_at("acme", "api", "main", "src/a.rs")
            .await
            .unwrap()
            .as_deref(),
        Some("job change v2")
    );
}

#[tokio::test]
async fn merge_gate_failure_reworks_on_new_base_without_budget() {
    let Some(rig) = rig().await else { return };
    // eval c1 pass, gate c1 FAIL, eval c2 pass (no second gate: base caught up).
    rig.backend.script_exits([0, 1, 0]);
    commit_branch(&rig, "src/a.rs");
    move_main_during_eval(&rig);
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
    // The gate-failure rework Work task records its cause.
    assert_eq!(
        tasks[3].rework_reason,
        Some(types::ReworkReason::GateCiFailure)
    );
    assert!(
        rig.repo
            .manager
            .read_file_at("acme", "api", "main", "src/a.rs")
            .await
            .unwrap()
            .is_some()
    );
}

/// Change A: an eval-failure rework PRESERVES the branch. Cycle 1 commits file
/// A; cycle 2 commits only file B — under the old reset-on-re-entry both would
/// need re-doing, so A surviving on the merge proves the commits carry forward
/// (fix-in-place, base_ref unchanged).
#[tokio::test]
async fn eval_failure_rework_preserves_prior_commits() {
    let Some(rig) = rig().await else { return };
    rig.backend.script_exits([1, 0]); // command eval: c1 fail, c2 pass

    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("a.txt", b"from cycle 1", "c1").await;
        clone.push(&branch).await;
    });
    let bare2 = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        // Rework re-entry: commit ONLY file B, never re-creating A.
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare2, &branch).await;
        clone.commit_file("b.txt", b"from cycle 2", "c2").await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("reworkable")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let m = &rig.repo.manager;
    assert_eq!(
        m.read_file_at("acme", "api", "main", "a.txt")
            .await
            .unwrap()
            .as_deref(),
        Some("from cycle 1"),
        "cycle-1 commit must survive the eval-failure rework"
    );
    assert_eq!(
        m.read_file_at("acme", "api", "main", "b.txt")
            .await
            .unwrap()
            .as_deref(),
        Some("from cycle 2")
    );

    let events = event_types(&rig.store).await;
    assert_eq!(
        events.iter().filter(|e| *e == "job-rework-started").count(),
        1
    );
    // work c1, eval c1, work c2, eval c2 — one rework cycle, no extra tasks.
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
}

/// §3.2 step-12 guard: a no-evaluator job auto-squashes with no review to catch
/// markers. A conflict rework leaves a WIP marker commit; if the agent never
/// resolves it, the guard escalates instead of landing `<<<<<<<` on the default
/// branch.
#[tokio::test]
async fn unresolved_markers_on_no_evaluator_job_escalates() {
    let Some(rig) = rig().await else { return };
    let bare = rig.repo.bare_path();
    // Work c1: commit src/x on the branch AND land a conflicting src/x on main.
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("src/x.rs", b"branch side", "c1").await;
        clone.push(&branch).await;
        let main = clone_branch_from(&bare, "main").await;
        main.commit_file("src/x.rs", b"main side", "other").await;
        main.push("main").await;
    });
    // Work c2 (conflict rework): the agent does NOTHING — markers stay unresolved.
    rig.provider.on_run(|_| async {});

    let job = rig.handle.create_job(req("no-eval")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

    // Nothing with markers reached the default branch.
    assert_eq!(
        rig.repo
            .manager
            .read_file_at("acme", "api", "main", "src/x.rs")
            .await
            .unwrap()
            .as_deref(),
        Some("main side"),
        "conflict markers must NOT be squashed onto the default branch"
    );
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-escalated".to_string()));
    assert_eq!(
        events.iter().filter(|e| *e == "job-rework-started").count(),
        1,
        "one conflict rework happened before the guard escalated"
    );
}

/// The happy counterpart to the guard: on a no-evaluator job the agent resolves
/// the WIP markers in place, so the squash is clean and lands the resolved tree
/// as a single commit (spec §3.2 step 12, degenerate 3-way).
#[tokio::test]
async fn resolved_wip_markers_squash_clean_on_no_evaluator_job() {
    let Some(rig) = rig().await else { return };
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("src/x.rs", b"branch side", "c1").await;
        clone.push(&branch).await;
        let main = clone_branch_from(&bare, "main").await;
        main.commit_file("src/x.rs", b"main side", "other").await;
        main.push("main").await;
    });
    let bare2 = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        assert!(
            cfg.merge_conflict.is_some(),
            "resolve-in-place context must be injected on the rework"
        );
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare2, &branch).await;
        clone
            .commit_file("src/x.rs", b"resolved", "resolve markers")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("no-eval")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let m = &rig.repo.manager;
    let landed = m
        .read_file_at("acme", "api", "main", "src/x.rs")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(landed, "resolved");
    assert!(!landed.contains("<<<<<<<"));

    let events = event_types(&rig.store).await;
    assert_eq!(
        events.iter().filter(|e| *e == "job-rework-started").count(),
        1
    );
    assert!(!events.contains(&"job-merge-gate-started".to_string()));
}

#[tokio::test]
async fn human_evaluator_and_human_work_resolve_via_inbox() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/gated.rs"); // agent work produces output (§3.2 guard)

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
                summary: Some("Fixed the config and confirmed the build passes.".into()),
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
    // The operator's resolution summary is persisted on the work task's result,
    // not just used as the squash-commit body.
    assert!(matches!(
        tasks[0].result,
        Some(types::TaskResult::Human { pass: true, ref operator, summary: Some(ref s), .. })
            if operator == "david" && s == "Fixed the config and confirmed the build passes."
    ));
}

#[tokio::test]
async fn escalation_retry_relaunches_work_without_branch_reset() {
    let Some(rig) = rig().await else { return };
    rig.provider.script_exits([1, 1]); // exhaust work_retries: 1
    rig.provider.on_run(|_| async {}); // attempt 1 (exits 1)
    rig.provider.on_run(|_| async {}); // attempt 2 (exits 1)
    commit_branch(&rig, "src/retry.rs"); // Retry attempt commits so it lands (§3.2 guard)

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

/// Hit the real `req.tasks.list.pending` request path — the operator inbox as
/// the API forwards it — and decode the reply into the task list.
async fn pending_inbox(store: &NatsStore) -> Vec<types::Task> {
    let msg = store
        .request_timeout(
            &store::subjects::tasks_list_pending("acme", "api"),
            b"{}",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    serde_json::from_slice(&msg.payload).unwrap()
}

/// Revoking a job with a Pending escalation empties the inbox and leaves the
/// escalation task terminal, recording that the revoke — not an operator —
/// retired it (spec §1.2 revoke-closes-tasks, §2.1 Revoked transition).
#[tokio::test]
async fn revoke_closes_pending_escalation_task() {
    let Some(rig) = rig().await else { return };
    rig.provider.script_exits([1, 1]); // exhaust work_retries: 1 → escalate

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The escalation task sits in the inbox as a Pending Human task.
    let inbox = pending_inbox(&rig.store).await;
    assert_eq!(inbox.len(), 1);
    let esc_id = inbox[0].id;
    assert_eq!(inbox[0].state, TaskState::Pending);

    rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Revoked).await;

    // No zombie remains in the inbox.
    assert!(pending_inbox(&rig.store).await.is_empty());

    // The task record is terminal, closed by a synthetic system revoke.
    let task = rig
        .store
        .tasks()
        .await
        .unwrap()
        .get("acme", "api", job.id, esc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Done);
    match task.result {
        Some(types::TaskResult::Human {
            operator,
            action,
            pass,
            ..
        }) => {
            assert_eq!(operator, "system");
            assert_eq!(action, Some(EscalationAction::Revoke));
            assert!(!pass);
        }
        other => panic!("expected synthetic Human result, got {other:?}"),
    }
}

/// A zombie predating the revoke-closes-tasks fix — a Pending task whose job is
/// already terminal in KV — must vanish from the inbox with no migration
/// (spec §1.2 revoke-closes-tasks, second line of defense).
#[tokio::test]
async fn list_pending_hides_terminal_job_zombie() {
    let Some(rig) = rig().await else { return };
    let jobs = rig.store.jobs().await.unwrap();
    let tasks = rig.store.tasks().await.unwrap();

    // Seed a Revoked job with a leftover Pending Human escalation task.
    let mut job = rig.handle.create_job(req("flaky")).await.unwrap();
    job.state = JobState::Revoked;
    jobs.put(&job).await.unwrap();
    let zombie = types::Task {
        id: 1,
        job_seq: job.id,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: types::TaskKind::Human {
            prompt: "resolve me".into(),
        },
        state: TaskState::Pending,
        attempt: 1,
        evaluator: None,
        stage: 0,
        performed_by: None,
        container_id: None,
        rework_reason: None,
        infra_loss: false,

        pending_reason: None,
        queued_at: None,
        session_id: None,
        result: None,
        created_at: job.created_at,
        started_at: None,
        completed_at: None,
    };
    tasks.put(&zombie).await.unwrap();

    // The pending task exists in KV, but the inbox filters it out.
    assert_eq!(
        tasks
            .list_for_job("acme", "api", job.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(pending_inbox(&rig.store).await.is_empty());
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

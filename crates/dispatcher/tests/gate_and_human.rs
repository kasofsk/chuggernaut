//! Tier-2 tests for the merge gate (§3.3) and human task resolution (§1.2):
//! gate re-run against the candidate commit when HEAD moved, gate failure →
//! rework on the new base, human work/evaluator resolution, and escalation
//! Retry.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateSpec};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{EscalationAction, JobState, TaskPhase, TaskResolution, TaskState};

mod common;
use common::{assert_invariants_of, spawn_checked};

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

const GATEFIX: &str = r#"
name: gatefix
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: build
    type: command
    run: ./build.sh
    stage: 0
  - name: test
    type: command
    run: ./test.sh
    stage: 1
"#;

const HUMAN_EVAL: &str = r#"
name: human-gated
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: sign-off
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

const NO_EVAL: &str = r#"
name: no-eval
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
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
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in [
        ("jobs/impl-cmd.yaml", IMPL_CMD),
        ("jobs/gatefix.yaml", GATEFIX),
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
    let (handle, invariants) = spawn_checked(core);
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

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
    test_utils::wait::job_state(store, "acme", "api", seq, want).await
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
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let done = wait_for_state(&rig.store, job.id, JobState::Done).await;

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
    let launches = rig.backend.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].env["JOB_BRANCH"], format!("job/{}", job.id));

    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-rebased".to_string()));
    assert!(!events.contains(&"job-merge-gate-started".to_string()));
    let head = done.base_ref.unwrap();
    assert_eq!(
        m.read_file_at("acme", "api", &head, "docs/other.md")
            .await
            .unwrap()
            .as_deref(),
        Some("landed concurrently")
    );
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
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
    let launches = rig.backend.launches();
    assert_eq!(launches.len(), 2);
    assert_eq!(launches[0].env["JOB_BRANCH"], format!("job/{}", job.id));
    assert_eq!(
        launches[1].env["JOB_BRANCH"],
        format!("merge-gate/{}", job.id)
    );

    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-merge-gate-started".to_string()));
    assert!(!events.contains(&"job-rebased".to_string()));
    assert!(
        m.resolve_ref("acme", "api", &format!("merge-gate/{}", job.id))
            .await
            .is_err()
    );
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-rebase-conflict".to_string()));
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
    assert_eq!(tasks.len(), 4);
    assert_eq!(tasks[2].cycle, 2);
    assert_eq!(
        tasks[2].rework_reason,
        Some(types::ReworkReason::MergeConflict)
    );
    assert!(tasks.iter().all(|t| t.phase != TaskPhase::MergeGate));
    assert_eq!(
        rig.repo
            .manager
            .read_file_at("acme", "api", "main", "src/a.rs")
            .await
            .unwrap()
            .as_deref(),
        Some("job change v2")
    );
    assert_invariants_of(&rig.invariants);
}

#[tokio::test]
async fn merge_gate_failure_reworks_on_new_base_without_budget() {
    let Some(rig) = rig().await else { return };
    rig.backend.script_exits([0, 1, 0]);
    commit_branch(&rig, "src/a.rs");
    move_main_during_eval(&rig);
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
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let events = event_types(&rig.store).await;
    let reworks: Vec<&String> = events
        .iter()
        .filter(|e| *e == "job-rework-started")
        .collect();
    assert_eq!(reworks.len(), 1);
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 5);
    assert_eq!(tasks[3].cycle, 2);
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
    assert_invariants_of(&rig.invariants);
}

/// Job #154 gate-fix fast path: a compile-only gate failure (stage-0 build) on
/// an approved branch launches a scoped gate-fix task that returns straight to
/// the gate — no re-review, no eval-phase CI — and lands once the re-gate passes.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn gate_compile_failure_takes_fast_path_and_relands() {
    let Some(rig) = rig().await else { return };
    rig.backend.script_exits([0, 0, 1, 0, 0]);
    rig.backend
        .put_logs(b"error[E0433]: failed to resolve: use of undeclared crate `foo`".to_vec());
    commit_branch(&rig, "src/a.rs");
    move_main_during_eval(&rig);
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let brief = cfg
            .merge_conflict
            .clone()
            .expect("gate-fix context must be injected");
        assert!(
            brief.contains("error[E0433]") && brief.contains("Gate build output"),
            "gate-fix brief must include the captured compiler output: {brief}"
        );
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change fixed", "repair compile")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("gatefix")).await.unwrap();
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
    let fixes: Vec<_> = tasks
        .iter()
        .filter(|t| t.rework_reason == Some(types::ReworkReason::GateCompileFix))
        .collect();
    assert_eq!(fixes.len(), 1, "one gate-fix round: {tasks:#?}");
    assert_eq!(fixes[0].phase, TaskPhase::Work);
    assert_eq!(fixes[0].cycle, 2);
    assert_eq!(fixes[0].label.as_deref(), Some("gate-fix"));
    assert!(
        !tasks
            .iter()
            .any(|t| t.phase == TaskPhase::Evaluation && t.cycle == 2),
        "no cycle-2 evaluation tasks: {tasks:#?}"
    );
    assert!(
        tasks
            .iter()
            .any(|t| t.phase == TaskPhase::MergeGate && t.cycle == 2),
        "re-gate must run: {tasks:#?}"
    );
    assert!(
        rig.repo
            .manager
            .read_file_at("acme", "api", "main", "src/a.rs")
            .await
            .unwrap()
            .is_some()
    );
    let body = tokio::process::Command::new("git")
        .args(["log", "-1", "--format=%B", "main"])
        .current_dir(rig.repo.bare_path())
        .output()
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body.stdout).to_string();
    assert!(
        body.contains("gate-fix round"),
        "squash body must note the gate-fix round: {body:?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Job #154: a gate failure at a LATER stage (tests, not build) is not the
/// mechanical compile case — it takes the full rework loop (a Work task the
/// reviewer/eval phase sees again), not the gate-fix fast path.
#[tokio::test]
async fn gate_test_stage_failure_takes_full_rework() {
    let Some(rig) = rig().await else { return };
    rig.backend.script_exits([0, 0, 0, 1, 0, 0]);
    commit_branch(&rig, "src/a.rs");
    move_main_during_eval(&rig);
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change v2", "rework")
            .await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("gatefix")).await.unwrap();
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
            .any(|t| t.rework_reason == Some(types::ReworkReason::GateCiFailure)),
        "test-stage failure must take the full rework loop: {tasks:#?}"
    );
    assert!(
        !tasks
            .iter()
            .any(|t| t.rework_reason == Some(types::ReworkReason::GateCompileFix)),
        "no gate-fix fast path for a test-stage failure: {tasks:#?}"
    );
    assert!(
        tasks
            .iter()
            .any(|t| t.phase == TaskPhase::Evaluation && t.cycle == 2),
        "the eval phase re-runs on the full loop: {tasks:#?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Job #154: the gate-fix budget is bounded. A branch that keeps failing the
/// build across two gate-fix rounds falls back to the full rework loop on the
/// third — the failure wasn't the one-shot mechanical fix the fast path assumes.
#[tokio::test]
async fn gate_fix_budget_exhaustion_falls_back_to_full_rework() {
    let Some(rig) = rig().await else { return };
    rig.backend.script_exits([0, 0, 1, 1, 1, 0, 0]);
    commit_branch(&rig, "src/a.rs");
    move_main_during_eval(&rig);

    let job = rig.handle.create_job(req("gatefix")).await.unwrap();
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
    let gate_fixes = tasks
        .iter()
        .filter(|t| t.rework_reason == Some(types::ReworkReason::GateCompileFix))
        .count();
    assert_eq!(
        gate_fixes, 2,
        "exactly the budget of gate-fix rounds: {tasks:#?}"
    );
    assert!(
        tasks
            .iter()
            .any(|t| t.rework_reason == Some(types::ReworkReason::GateCiFailure)),
        "budget exhaustion falls back to full rework: {tasks:#?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Job #154 safety rail: a SINGLE-stage (opaque) gate cannot be classified —
/// there is no distinct build stage to attribute the failure to — so it takes
/// the full rework loop (GateCiFailure) and never the gate-fix fast path. This
/// is the deterministic-or-full-loop guarantee: never mis-route on ambiguity.
#[tokio::test]
async fn single_stage_gate_failure_is_unclassifiable_full_rework() {
    let Some(rig) = rig().await else { return };
    rig.backend.script_exits([0, 1, 0]);
    commit_branch(&rig, "src/a.rs");
    move_main_during_eval(&rig);
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone
            .commit_file("src/a.rs", b"job change v2", "rework")
            .await;
        clone.push(&branch).await;
    });

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
    assert!(
        tasks
            .iter()
            .any(|t| t.rework_reason == Some(types::ReworkReason::GateCiFailure)),
        "single-stage gate failure must take the full rework loop: {tasks:#?}"
    );
    assert!(
        !tasks
            .iter()
            .any(|t| t.rework_reason == Some(types::ReworkReason::GateCompileFix)),
        "no gate-fix fast path when the gate cannot be classified: {tasks:#?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Change A: an eval-failure rework PRESERVES the branch. Cycle 1 commits file
/// A; cycle 2 commits only file B — under the old reset-on-re-entry both would
/// need re-doing, so A surviving on the merge proves the commits carry forward
/// (fix-in-place, base_ref unchanged).
#[tokio::test]
async fn eval_failure_rework_preserves_prior_commits() {
    let Some(rig) = rig().await else { return };
    rig.backend.script_exits([1, 0]);

    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        clone.commit_file("a.txt", b"from cycle 1", "c1").await;
        clone.push(&branch).await;
    });
    let bare2 = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare2, &branch).await;
        clone.commit_file("b.txt", b"from cycle 2", "c2").await;
        clone.push(&branch).await;
    });

    let job = rig.handle.create_job(req("reworkable")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
}

/// §3.2 step-12 guard: a no-evaluator job auto-squashes with no review to catch
/// markers. A conflict rework leaves a WIP marker commit; if the agent never
/// resolves it, the guard escalates instead of landing `<<<<<<<` on the default
/// branch.
#[tokio::test]
async fn unresolved_markers_on_no_evaluator_job_escalates() {
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
    rig.provider.on_run(|_| async {});

    let job = rig.handle.create_job(req("no-eval")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;

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
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn human_evaluator_and_human_work_resolve_via_inbox() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/gated.rs");

    let gated = rig.handle.create_job(req("human-gated")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .release_job("acme", "api", gated.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, gated.id, JobState::Evaluation).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

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
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, gated.id, JobState::Done).await;

    let manual = rig.handle.create_job(req("manual")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .release_job("acme", "api", manual.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
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
    assert_invariants_of(&rig.invariants);
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
        Some(types::TaskResult::Human { pass: true, ref operator, summary: Some(ref s), .. })
            if operator == "david" && s == "Fixed the config and confirmed the build passes."
    ));
    assert_invariants_of(&rig.invariants);
}

fn req_gated(r#type: &str) -> CreateSpec {
    CreateSpec {
        require_approval: true,
        ..req(r#type)
    }
}

/// The job's pending approval gate, waited for by watcher (#206 principle 3):
/// it is created a whole stage transition after Evaluation is entered, so a
/// fixed sleep would be racy under load.
async fn approval_task(store: &NatsStore, seq: u64) -> types::Task {
    test_utils::wait::task_where(
        store,
        "acme",
        "api",
        seq,
        format!("pending approval gate on #{seq}"),
        |t| t.evaluator.as_deref() == Some("approval") && t.state == TaskState::Pending,
    )
    .await
}

/// A job with `require_approval` gains one required Human evaluator after
/// everything else has passed, and approving it lands the merge (spec §1.1
/// require-approval). The gate is staged past the type's own evaluator, so the
/// operator is asked only once `tests` is Done.
#[tokio::test]
async fn approval_gate_runs_last_and_passing_it_lands_the_merge() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/gated.rs");

    let job = rig
        .handle
        .create_job(req_gated("reworkable"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;

    let gate = approval_task(&rig.store, job.id).await;
    assert_eq!(gate.phase, TaskPhase::Evaluation);
    assert_eq!(gate.stage, 1);
    let inbox = pending_inbox(&rig.store).await;
    assert!(
        inbox.iter().any(|t| t.job_seq == job.id && t.id == gate.id),
        "the gate surfaces in the operator inbox: {inbox:?}"
    );
    let tasks = rig
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    let tests = tasks
        .iter()
        .find(|t| t.evaluator.as_deref() == Some("tests"))
        .expect("the type's own evaluator still runs");
    assert_eq!((tests.stage, tests.state), (0, TaskState::Done));

    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            gate.id,
            TaskResolution::Pass {
                structured: None,
                summary: None,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;
    assert!(pending_inbox(&rig.store).await.is_empty());
    assert_invariants_of(&rig.invariants);
}

/// Rejecting the gate is an ordinary required-evaluator fail: the job reworks
/// and the operator's notes ride into the next work attempt as eval context.
#[tokio::test]
async fn rejecting_the_approval_gate_reworks_with_the_notes() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/gated.rs");
    commit_branch(&rig, "src/gated2.rs");

    let job = rig
        .handle
        .create_job(req_gated("reworkable"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;

    let gate = approval_task(&rig.store, job.id).await;
    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            gate.id,
            TaskResolution::Fail {
                structured: serde_json::json!({ "notes": "rename the flag first" }),
                abort: false,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;

    let rework = &rig.provider.runs()[1];
    assert_eq!(rework.eval_context.len(), 2);
    assert!(
        rework.prompt.contains("rename the flag first"),
        "{}",
        rework.prompt
    );
    assert_invariants_of(&rig.invariants);
}

/// `abort: true` on the rejection means "not satisfiable by rework": the
/// remaining budget is skipped and the job escalates (spec §1.2 Abort verdict).
#[tokio::test]
async fn aborting_the_approval_gate_escalates_instead_of_reworking() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/gated.rs");

    let job = rig
        .handle
        .create_job(req_gated("reworkable"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;

    let gate = approval_task(&rig.store, job.id).await;
    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            gate.id,
            TaskResolution::Fail {
                structured: serde_json::json!({ "notes": "wrong premise entirely" }),
                abort: true,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    let escalated = wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    assert_eq!(
        escalated.escalation.as_ref().map(|e| e.reason.as_str()),
        Some("eval_abort")
    );
    assert_eq!(rig.provider.runs().len(), 1, "abort skips the rework cycle");
    assert_invariants_of(&rig.invariants);
}

/// Revoking a job whose approval is pending empties the inbox — the
/// synthesized gate is closed by the same §1.2 revoke-closes-tasks path every
/// other Human task takes.
#[tokio::test]
async fn revoke_closes_a_pending_approval_task() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/gated.rs");

    let job = rig
        .handle
        .create_job(req_gated("reworkable"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;
    let gate = approval_task(&rig.store, job.id).await;

    rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Revoked).await;
    assert!(pending_inbox(&rig.store).await.is_empty());

    let closed = rig
        .store
        .tasks()
        .await
        .unwrap()
        .get("acme", "api", job.id, gate.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed.state, TaskState::Done);
    assert!(matches!(
        closed.result,
        Some(types::TaskResult::Human { pass: false, ref operator, .. }) if operator == "system"
    ));
    assert_invariants_of(&rig.invariants);
}

/// The gate is editable only while the job could still act on it: a Frozen job
/// takes the flag, a job past Work entry gets a 422 rather than a silent no-op.
#[tokio::test]
async fn the_approval_flag_is_editable_before_work_and_rejected_after() {
    let Some(rig) = rig().await else { return };
    commit_branch(&rig, "src/gated.rs");

    let job = rig.handle.create_job(req("reworkable")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let gated = rig
        .handle
        .set_require_approval("acme", "api", job.id, true)
        .await
        .unwrap();
    assert!(gated.require_approval);

    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Evaluation).await;
    approval_task(&rig.store, job.id).await;

    let err = rig
        .handle
        .set_require_approval("acme", "api", job.id, false)
        .await
        .expect_err("the gate is no longer editable");
    assert!(format!("{err:?}").contains("require_approval"), "{err:?}");
    assert_invariants_of(&rig.invariants);
}

#[tokio::test]
async fn escalation_retry_relaunches_work_without_branch_reset() {
    let Some(rig) = rig().await else { return };
    rig.provider.script_exits([1, 1]);
    rig.provider.on_run(|_| async {});
    rig.provider.on_run(|_| async {});
    commit_branch(&rig, "src/retry.rs");

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

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
    assert_eq!(tasks.len(), 4);
    assert_eq!((tasks[3].cycle, tasks[3].attempt), (1, 3));
    assert_eq!(tasks[3].state, TaskState::Done);
    let events = event_types(&rig.store).await;
    assert!(events.contains(&"job-escalation-resolved".to_string()));
    assert_invariants_of(&rig.invariants);
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
    rig.provider.script_exits([1, 1]);

    let job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Escalated).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let inbox = pending_inbox(&rig.store).await;
    assert_eq!(inbox.len(), 1);
    let esc_id = inbox[0].id;
    assert_eq!(inbox[0].state, TaskState::Pending);

    rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Revoked).await;

    assert!(pending_inbox(&rig.store).await.is_empty());

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
    assert_invariants_of(&rig.invariants);
}

/// A zombie predating the revoke-closes-tasks fix — a Pending task whose job is
/// already terminal in KV — must vanish from the inbox with no migration
/// (spec §1.2 revoke-closes-tasks, second line of defense).
#[tokio::test]
async fn list_pending_hides_terminal_job_zombie() {
    let Some(rig) = rig().await else { return };
    let jobs = rig.store.jobs().await.unwrap();
    let tasks = rig.store.tasks().await.unwrap();

    let mut job = rig.handle.create_job(req("flaky")).await.unwrap();
    assert_invariants_of(&rig.invariants);
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
        created_at: job.created_at,
        started_at: None,
        completed_at: None,
    };
    tasks.put(&zombie).await.unwrap();

    assert_eq!(
        tasks
            .list_for_job("acme", "api", job.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(pending_inbox(&rig.store).await.is_empty());
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

/// Two jobs landing concurrently serialize through the per-project merge
/// queue (§3.3 depth-1), and NEITHER update is lost — the property the queue
/// exists to protect. The interleave itself (who enqueues while who gates) is
/// scheduler-timing and is pinned deterministically at tier 1 by the C2
/// merge-gate decider's queue-behind-gate branch tests; this test pins the
/// end-to-end outcome under real concurrency: both jobs Done, all three
/// commits (two jobs + the concurrent land) present on final main.
#[tokio::test]
async fn two_concurrent_landings_serialize_and_neither_is_lost() {
    let Some(rig) = rig().await else { return };
    rig.backend
        .put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());
    commit_branch(&rig, "src/a.rs");
    commit_branch(&rig, "src/b.rs");
    move_main_during_eval(&rig);

    let a = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let b = rig.handle.create_job(req("impl-cmd")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", a.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, a.id, JobState::Done).await;
    wait_for_state(&rig.store, b.id, JobState::Done).await;

    let m = &rig.repo.manager;
    for path in ["src/a.rs", "src/b.rs", "docs/other.md"] {
        assert!(
            m.read_file_at("acme", "api", "main", path)
                .await
                .unwrap()
                .is_some(),
            "{path} must be on final main — a landing was lost"
        );
    }
    let events = event_types(&rig.store).await;
    assert!(
        events.contains(&"job-merge-gate-started".to_string()),
        "expected at least one gated landing: {events:?}"
    );
    for seq in [a.id, b.id] {
        assert!(
            m.resolve_ref("acme", "api", &format!("merge-gate/{seq}"))
                .await
                .is_err(),
            "merge-gate/{seq} must not survive landing"
        );
    }
    assert_invariants_of(&rig.invariants);
}

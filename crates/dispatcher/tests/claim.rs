//! Tier-2 tests for claimable work attempts (spec §1.2 claims): a human
//! claims a job's next work attempt without changing the task's declared
//! kind. Claim → parked Pending task (no container), Pass → evaluation as
//! usual, Fail → the next attempt launches per the DECLARED kind (the
//! no-conversion property) with the branch PRESERVED and the Fail notes
//! handed off (#121), unclaim → normal launch, in-flight claim → 409.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateSpec};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{JobState, Performer, TaskKind, TaskResolution, TaskState};

mod common;
use common::{assert_invariants_of, spawn_checked};

/// Agent work gated by a command evaluator — the shape a human claims when
/// they want to do an agent-typed ticket locally.
const CODE: &str = r#"
name: code
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

/// Agent work with a retry budget and no evaluators (auto-pass) — isolates
/// the Fail → relaunch-per-declared-kind path.
const RETRYABLE: &str = r#"
name: retryable
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
work_retries: 1
"#;

const MANUAL: &str = r#"
name: manual
work:
  type: human
  prompt: prompts/manual.md
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
        ("jobs/code.yaml", CODE),
        ("jobs/retryable.yaml", RETRYABLE),
        ("jobs/manual.yaml", MANUAL),
        ("prompts/impl.md", "implement it"),
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
        timeout: None,
        model: None,
        factory: None,
        members: vec![],
        inputs: Default::default(),
        draft: false,
    }
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
    // Watch-based wait (#206 principle 3): value-inspecting, hard timeout.
    test_utils::wait::job_state(store, "acme", "api", seq, want).await
}

/// Registers a work run that commits a stub to the job branch, so an agent work
/// run produces output and clears the §3.2 empty-output guard.
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

async fn tasks_for(rig: &Rig, seq: u64) -> Vec<types::Task> {
    rig.store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", seq)
        .await
        .unwrap()
}

/// Claim on Frozen rides through release: the attempt parks as a Pending task
/// with the DECLARED (agent) kind, performed_by human, no container and no
/// session. Pass with a summary submits the branch to evaluation as usual.
#[tokio::test]
async fn claim_parks_declared_kind_and_pass_flows_to_eval_and_merge() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(req("code")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.claim_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    // Idempotent while pending.
    rig.handle.claim_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Work).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Parked, not launched: declared kind preserved, human performer recorded.
    let tasks = tasks_for(&rig, job.id).await;
    assert_eq!(tasks.len(), 1);
    let parked = &tasks[0];
    assert!(matches!(parked.kind, TaskKind::Agent { .. }));
    assert_eq!(parked.performed_by, Some(Performer::Human));
    assert_eq!(parked.state, TaskState::Pending);
    assert!(parked.session_id.is_none());
    assert!(parked.started_at.is_some(), "claim means the human started");
    assert!(rig.provider.runs().is_empty(), "no agent container");
    assert!(rig.backend.launches().is_empty(), "no command container");
    // The claim was consumed at launch.
    let jobs = rig.store.jobs().await.unwrap();
    assert!(
        !jobs
            .get("acme", "api", job.id)
            .await
            .unwrap()
            .unwrap()
            .claim_next
    );

    // The human does the work on the job branch, then submits.
    let clone = clone_branch_from(&rig.repo.bare_path(), &format!("job/{}", job.id)).await;
    clone
        .commit_file("src/human.rs", b"written by hand", "implement locally")
        .await;
    clone.push(&format!("job/{}", job.id)).await;

    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            1,
            TaskResolution::Pass {
                structured: None,
                summary: Some("implemented locally by a human".into()),
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // The ci evaluator ran (one command container) and the work merged.
    assert_eq!(rig.backend.launches().len(), 1);
    assert!(
        rig.repo
            .manager
            .read_file_at("acme", "api", "main", "src/human.rs")
            .await
            .unwrap()
            .is_some()
    );
    assert_invariants_of(&rig.invariants);
}

/// The no-conversion property: a human Fail consumes the attempt through the
/// normal failure path, and the NEXT attempt launches a real agent container
/// per the declared kind — nothing to convert back.
#[tokio::test]
async fn claimed_fail_relaunches_next_attempt_per_declared_kind() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig); // the relaunched agent attempt produces output

    let job = rig.handle.create_job(req("retryable")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.claim_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Work).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rig.provider.runs().is_empty());

    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            1,
            TaskResolution::Fail {
                structured: serde_json::json!({ "notes": "couldn't finish, agent take over" }),
                abort: false,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    // work_retries: 1 → attempt 2 launches per the declared kind (agent),
    // exits 0, and the no-eval job goes to Done.
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    assert_eq!(rig.provider.runs().len(), 1, "agent picked attempt 2 up");
    let tasks = tasks_for(&rig, job.id).await;
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert_eq!(tasks[0].performed_by, Some(Performer::Human));
    assert_eq!(tasks[1].attempt, 2);
    assert!(matches!(tasks[1].kind, TaskKind::Agent { .. }));
    assert_eq!(tasks[1].performed_by, None, "attempt 2 ran normally");
    assert_invariants_of(&rig.invariants);
}

/// #121 regression: a human `Fail` resolution is a deliberate handoff at a
/// clean commit boundary, NOT a crash. The commit the operator pushed to
/// `job/{seq}` before handing off is PRESERVED (the branch is not reset to
/// `base_ref`, unlike a container crash), and the `Fail` `structured` notes
/// ride into the next agent attempt's context like eval findings.
#[tokio::test]
async fn claimed_fail_preserves_branch_and_hands_off_notes() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig); // the relaunched agent attempt produces output

    let job = rig.handle.create_job(req("retryable")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.claim_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Work).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rig.provider.runs().is_empty());

    // The operator pushes a commit (an asset stand-in) on the job branch
    // before handing off — this is the work that must survive the relaunch.
    let branch = format!("job/{}", job.id);
    let clone = clone_branch_from(&rig.repo.bare_path(), &branch).await;
    clone
        .commit_file("assets/logo.svg", b"<svg>operator asset</svg>", "add asset")
        .await;
    clone.push(&branch).await;

    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            1,
            TaskResolution::Fail {
                structured: serde_json::json!({ "notes": "assets committed; agent finish the wiring" }),
                abort: false,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    // The relaunched agent attempt ran, and its prompt carried the handoff
    // notes like eval findings, attributed to the operator (§1.2 / §3.2).
    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 1, "agent picked the handoff up");
    let prompt = &runs[0].prompt;
    assert!(
        prompt.contains("assets committed; agent finish the wiring"),
        "handoff notes injected into next attempt: {prompt}"
    );
    assert!(
        prompt.contains("operator handoff (david)"),
        "handoff attributed to the operator: {prompt}"
    );

    // The operator's pre-handoff commit survived — the branch was PRESERVED,
    // not reset to base_ref — so the asset merges through to main intact.
    let merged = rig
        .repo
        .manager
        .read_file_at("acme", "api", "main", "assets/logo.svg")
        .await
        .unwrap();
    assert_eq!(
        merged.as_deref(),
        Some("<svg>operator asset</svg>"),
        "operator's pre-handoff commit was preserved and merged"
    );
    assert_invariants_of(&rig.invariants);
}

/// No double pickup: while an attempt is in flight — parked for a human or
/// terminal job — claim conflicts.
#[tokio::test]
async fn claim_conflicts_while_attempt_in_flight_or_job_terminal() {
    let Some(rig) = rig().await else { return };

    // Declared-human work parks an attempt at launch; claiming then is a 409.
    let manual = rig.handle.create_job(req("manual")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .release_job("acme", "api", manual.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, manual.id, JobState::Work).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let err = rig.handle.claim_job("acme", "api", manual.id).await;
    assert_invariants_of(&rig.invariants);
    assert!(matches!(err, Err(dispatcher::core::CoreError::Conflict(_))));

    // Terminal jobs cannot be claimed.
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
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, manual.id, JobState::Done).await;
    let err = rig.handle.claim_job("acme", "api", manual.id).await;
    assert_invariants_of(&rig.invariants);
    assert!(matches!(err, Err(dispatcher::core::CoreError::Conflict(_))));
    assert_invariants_of(&rig.invariants);
}

/// Unclaim before launch clears the claim and the job launches normally per
/// its declared kind; unclaiming without a pending claim conflicts.
#[tokio::test]
async fn unclaim_before_launch_restores_normal_execution() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig); // the normal agent launch produces output

    let job = rig.handle.create_job(req("retryable")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let err = rig.handle.unclaim_job("acme", "api", job.id).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(err, Err(dispatcher::core::CoreError::Conflict(_))),
        "no pending claim to clear"
    );
    rig.handle.claim_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.unclaim_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    assert_eq!(rig.provider.runs().len(), 1, "normal agent launch");
    let tasks = tasks_for(&rig, job.id).await;
    assert_eq!(tasks[0].performed_by, None);
    assert_invariants_of(&rig.invariants);
}

/// Rework cycles launch per the declared kind, unclaimed: after a claimed
/// attempt passes work but fails evaluation, the rework work task is a normal
/// agent launch — the claim covered exactly one attempt.
#[tokio::test]
async fn rework_after_claimed_attempt_launches_unclaimed_per_declared_kind() {
    let Some(rig) = rig().await else { return };
    // ci fails the first cycle, passes the second.
    rig.backend.script_exits([1, 0]);
    commit_work(&rig); // the rework (cycle-2) agent work produces output

    let job = rig.handle.create_job(req("code")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.claim_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Work).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    rig.handle
        .resolve_task(
            "acme",
            "api",
            job.id,
            1,
            TaskResolution::Pass {
                structured: None,
                summary: None,
            },
            "david",
        )
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    // cycle 1 eval fails → rework (budget 1) → cycle 2 agent work runs
    // normally → eval passes → Done.
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    assert_eq!(rig.provider.runs().len(), 1, "rework ran as a real agent");
    let tasks = tasks_for(&rig, job.id).await;
    let rework = tasks
        .iter()
        .find(|t| t.cycle == 2 && t.evaluator.is_none() && t.phase == types::TaskPhase::Work)
        .expect("rework work task");
    assert_eq!(rework.performed_by, None);
    assert!(matches!(rework.kind, TaskKind::Agent { .. }));
    assert_invariants_of(&rig.invariants);
}

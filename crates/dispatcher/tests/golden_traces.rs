//! Golden decision traces (refactor-plan B3): the lifecycle and human-escalation
//! scenarios from `lifecycle.rs` / `gate_and_human.rs`, re-driven with a
//! [`TraceSink`] attached so every §2.1 transition and every effect is captured
//! and compared against a committed `tests/traces/*.yaml` fixture. These pin the
//! dispatcher's decisions during Track C decider extraction — a carved-out
//! decider must reproduce the same `(transitions, effects)` the fixture records.
//!
//! Most scenarios drive `Core` **synchronously** (no spawned actor), so the
//! ordering of transitions and effects is fixed by the sequential `.await`s the
//! test makes — a golden trace there cannot flake on scheduler timing. The one
//! actor-driven scenario (`trace_work_eval_merge_no_gate`) instead synchronizes
//! its snapshot on the trace's **terminal effect** (see `wait_trace_effect`),
//! which is emitted strictly after every other write of the step — so it too
//! captures a quiesced, deterministic trace.
//!
//! Regenerate after an intended behavior change:
//!
//! ```sh
//! UPDATE_TRACES=1 cargo test -p dispatcher --test golden_traces
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{assert_invariants, assert_invariants_of, assert_trace, spawn_checked};
use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateSpec};
use dispatcher::invariants::InvariantSink;
use dispatcher::trace::TraceSink;
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::JobState;

const BUILD_YAML: &str = r#"
name: build
image: img:latest
work:
  type: agent
  prompt: prompts/build.md
  secrets: [DEPLOY_KEY]
  provider: claude
  review:
    prompt: prompts/review.md
    iterations: 3
"#;

const DEPLOY_YAML: &str = r#"
name: deploy
image: img:latest
work:
  type: command
  run: ./deploy.sh
"#;

const DEFAULTS_YAML: &str = r#"
eval:
  - name: ci
    type: command
    run: ./scripts/ci.sh
"#;

const IMPL_CMD_YAML: &str = r#"
name: impl-cmd
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
"#;

const IMPL_REWORK_YAML: &str = r#"
name: impl-rework
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
rework_budget: 1
"#;

const GATEFIX_YAML: &str = r#"
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

async fn new_core(store: &NatsStore, repos_root: std::path::PathBuf) -> Core {
    Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: "nats://test".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn seed_repo(repo: &TempRepo) {
    seed_repo_with(repo, true).await
}

/// `with_defaults: false` seeds no `jobs/_defaults.yaml` — the gate-fix
/// scenario needs the staged `gatefix` type's evaluators EXACTLY as declared
/// (the appended `ci` default would share stage 0 with `build`, breaking both
/// the exit-script accounting and the fixture's single-container-per-stage
/// determinism).
async fn seed_repo_with(repo: &TempRepo, with_defaults: bool) {
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/build.yaml", BUILD_YAML.as_bytes(), "add build")
        .await;
    clone
        .commit_file("jobs/deploy.yaml", DEPLOY_YAML.as_bytes(), "add deploy")
        .await;
    if with_defaults {
        clone
            .commit_file("jobs/_defaults.yaml", DEFAULTS_YAML.as_bytes(), "defaults")
            .await;
    }
    clone
        .commit_file(
            "jobs/impl-cmd.yaml",
            IMPL_CMD_YAML.as_bytes(),
            "add impl-cmd",
        )
        .await;
    clone
        .commit_file("jobs/gatefix.yaml", GATEFIX_YAML.as_bytes(), "add gatefix")
        .await;
    clone
        .commit_file(
            "jobs/impl-rework.yaml",
            IMPL_REWORK_YAML.as_bytes(),
            "add impl-rework",
        )
        .await;
    clone
        .commit_file("prompts/build.md", b"build it", "prompt")
        .await;
    clone
        .commit_file("prompts/review.md", b"review it", "prompt")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
}

fn core_repos_root(repo: &TempRepo) -> std::path::PathBuf {
    repo.bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Setup: shared NATS, a seeded bare repo, the declared secret, and a fresh
/// `Core` with a trace sink attached. Returns `None` when NATS is unavailable
/// (the tier-2 self-skip), so the fixture is only asserted where it can run.
async fn setup() -> Option<(NatsStore, TempRepo, Core, TraceSink)> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    seed_repo(&repo).await;
    store
        .raw_bucket(store::buckets::SECRETS)
        .await
        .unwrap()
        .put_json("acme.api.DEPLOY_KEY", &"encrypted-blob")
        .await
        .unwrap();
    let mut core = new_core(&store, core_repos_root(&repo)).await;
    let sink = TraceSink::new();
    core.attach_trace(sink.clone());
    Some((store, repo, core, sink))
}

fn req(r#type: &str, deps: &[u64]) -> CreateSpec {
    CreateSpec {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: deps.to_vec(),
        knowledge_tags: vec![],
        eval: vec![],
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

/// `lifecycle.rs::release_blocking_unblocking_and_events`, distilled to its
/// decision trace: create build + a dependent deploy, release both (deploy
/// Blocks behind build), then complete build so the dependent re-validates and
/// unblocks to Ready.
#[tokio::test]
async fn trace_release_block_unblock() {
    let Some((store, _repo, mut core, sink)) = setup().await else {
        return;
    };

    sink.begin("create build");
    let build = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    sink.begin("create deploy(dep build)");
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    assert_invariants(&core);

    sink.begin("release build");
    assert_eq!(
        core.release_job("acme", "api", build.id).await.unwrap(),
        JobState::Ready
    );
    assert_invariants(&core);
    sink.begin("release deploy");
    assert_eq!(
        core.release_job("acme", "api", deploy.id).await.unwrap(),
        JobState::Blocked
    );
    assert_invariants(&core);

    let jobs = store.jobs().await.unwrap();
    let mut done = jobs.get("acme", "api", build.id).await.unwrap().unwrap();
    done.state = JobState::Done;
    jobs.put(&done).await.unwrap();

    let mut core = new_core(&store, core_repos_root(&_repo)).await;
    core.attach_trace(sink.clone());
    sink.begin("on_job_done build");
    core.on_job_done("acme", "api", build.id).await.unwrap();
    assert_invariants(&core);

    assert_trace(&sink, "release_block_unblock");
}

/// `lifecycle.rs::unblock_revalidation_failure_stalls_with_human_task`: the
/// human-escalation decision. The dependent's job type breaks on main between
/// release and unblock, so completing the upstream re-validates the dependent,
/// fails, and Stalls it behind a Human escalation task (§1.2 pre-Work).
#[tokio::test]
async fn trace_stall_on_revalidation_failure() {
    let Some((store, repo, mut core, sink)) = setup().await else {
        return;
    };

    let build = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    assert_invariants(&core);
    sink.begin("release build");
    core.release_job("acme", "api", build.id).await.unwrap();
    assert_invariants(&core);
    sink.begin("release deploy");
    core.release_job("acme", "api", deploy.id).await.unwrap();
    assert_invariants(&core);

    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/deploy.yaml", b"not: [valid", "break deploy type")
        .await;
    clone.push("main").await;

    let jobs = store.jobs().await.unwrap();
    let mut done = jobs.get("acme", "api", build.id).await.unwrap().unwrap();
    done.state = JobState::Done;
    jobs.put(&done).await.unwrap();

    let mut core = new_core(&store, core_repos_root(&repo)).await;
    core.attach_trace(sink.clone());
    sink.begin("on_job_done build (deploy re-validation fails)");
    core.on_job_done("acme", "api", build.id).await.unwrap();
    assert_invariants(&core);
    assert_eq!(
        jobs.get("acme", "api", deploy.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Stalled
    );

    assert_trace(&sink, "stall_on_revalidation_failure");
}

/// `lifecycle.rs::revoke_cascades_through_pending_dependents`: revoking a Ready
/// job cascades Revoked through its whole pending dependent chain in one step.
#[tokio::test]
async fn trace_revoke_cascade() {
    let Some((_store, _repo, mut core, sink)) = setup().await else {
        return;
    };

    let a = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    let b = core.create_job(req("deploy", &[a.id])).await.unwrap();
    assert_invariants(&core);
    let c = core.create_job(req("deploy", &[b.id])).await.unwrap();
    assert_invariants(&core);
    sink.begin("release a");
    core.release_job("acme", "api", a.id).await.unwrap();
    assert_invariants(&core);

    sink.begin("revoke a (cascades b, c)");
    let cascaded = core.revoke_job("acme", "api", a.id).await.unwrap();
    assert_invariants(&core);
    assert_eq!(cascaded, vec![b.id, c.id]);

    assert_trace(&sink, "revoke_cascade");
}

/// A spawned rig for the actor-driven merge scenario: work + eval run through
/// the single-writer loop against fakes, so the trace still records only the
/// deterministic decision chain (transitions + `job-events`) — fleet/status
/// churn goes through other ports and is not captured.
struct SpawnRig {
    store: NatsStore,
    handle: CoreHandle,
    _repo: TempRepo,
    sink: TraceSink,
    /// Invariant violations the actor logged, drained by `assert_invariants_of`
    /// (refactor-plan B1a). Distinct from `sink`, which records the trace.
    invariants: InvariantSink,
}

/// Gate-scenario knobs for the actor-driven rig. `Default` reproduces the
/// original no-gate setup: one work hook committing `src/a.rs`, main never
/// moves, every container exits 0.
#[derive(Default)]
struct SpawnOpts {
    /// Container exit codes, consumed in backend launch order — the gate
    /// verdicts. Empty means every container exits 0.
    script_exits: Vec<i32>,
    /// Land `docs/other.md` on main while the FIRST eval container runs (the
    /// one-shot `on_launch` hook), so the wrap-up merge gate fires (§3.3).
    move_main_during_eval: bool,
    /// Work hooks queued in cycle order. Empty means one plain `Commit`.
    work_hooks: Vec<WorkHook>,
    /// Captured container logs — the compiler output a gate-fix brief carries.
    logs: Option<&'static [u8]>,
    /// Seed `jobs/_defaults.yaml` (the appended `ci` evaluator). Off for the
    /// staged gate-fix scenario — see [`seed_repo_with`].
    no_defaults: bool,
}

/// One queued `FakeProvider::on_run` hook (hooks are FnOnce, consumed per
/// work/fix cycle in order).
enum WorkHook {
    /// Commit `src/a.rs` on the job branch; main untouched.
    Commit,
    /// Commit `src/a.rs` on the branch AND land a conflicting `src/a.rs` on
    /// main — the rebase at Evaluation entry cannot replay cleanly.
    CommitAndConflictMain,
    /// Rework/fix cycle: re-commit `src/a.rs` v2 on the freshly pinned base.
    Recommit,
}

async fn spawn_setup() -> Option<SpawnRig> {
    spawn_setup_with(SpawnOpts::default()).await
}

#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn spawn_setup_with(opts: SpawnOpts) -> Option<SpawnRig> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    seed_repo_with(&repo, !opts.no_defaults).await;

    let backend = Arc::new(FakeBackend::new());
    let provider = Arc::new(FakeProvider::new());
    if !opts.script_exits.is_empty() {
        backend.script_exits(opts.script_exits);
    }
    if let Some(logs) = opts.logs {
        backend.put_logs(logs.to_vec());
    }
    let hooks = if opts.work_hooks.is_empty() {
        vec![WorkHook::Commit]
    } else {
        opts.work_hooks
    };
    for hook in hooks {
        let bare = repo.bare_path();
        match hook {
            WorkHook::Commit => provider.on_run(move |cfg| async move {
                let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
                let clone = clone_branch_from(&bare, &branch).await;
                clone
                    .commit_file("src/a.rs", b"job change", "implement")
                    .await;
                clone.push(&branch).await;
            }),
            WorkHook::CommitAndConflictMain => provider.on_run(move |cfg| async move {
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
            }),
            WorkHook::Recommit => provider.on_run(move |cfg| async move {
                let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
                let clone = clone_branch_from(&bare, &branch).await;
                clone
                    .commit_file("src/a.rs", b"job change v2", "implement again")
                    .await;
                clone.push(&branch).await;
            }),
        }
    }
    if opts.move_main_during_eval {
        let bare = repo.bare_path();
        backend.on_launch(move |_cfg| async move {
            let main = clone_branch_from(&bare, "main").await;
            main.commit_file("docs/other.md", b"landed during eval", "other job")
                .await;
            main.push("main").await;
        });
    }
    backend.put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());

    let mut core = new_core_with(&store, core_repos_root(&repo), backend.clone(), provider).await;
    let sink = TraceSink::new();
    core.attach_trace(sink.clone());
    let (handle, invariants) = spawn_checked(core);
    Some(SpawnRig {
        store,
        handle,
        _repo: repo,
        sink,
        invariants,
    })
}

async fn new_core_with(
    store: &NatsStore,
    repos_root: std::path::PathBuf,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
) -> Core {
    Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend,
        provider,
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: "nats://test".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

/// `gate_and_human.rs::no_movement_evaluates_and_merges_without_rebase_or_gate`:
/// the full Ready→Work→Evaluation→WrapUp→Done merge chain with no concurrent
/// movement, driven through the spawned actor. Captured as one step — the
/// causal chain is a single deterministic sequence — and asserted once the job
/// reaches Done. Only transitions and `job-events` are recorded (fleet/status
/// publishes go through uninstrumented ports), so no scheduler churn leaks in.
#[tokio::test]
async fn trace_work_eval_merge_no_gate() {
    let Some(rig) = spawn_setup().await else {
        return;
    };

    rig.sink.begin("create+release → work → eval → merge");
    let job = rig.handle.create_job(req("impl-cmd", &[])).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Done).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-done").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "work_eval_merge_no_gate");
    assert_invariants_of(&rig.invariants);
}

/// `gate_and_human.rs::main_moves_during_eval_fires_merge_gate`: main moves
/// while the eval container runs, so landing builds a squash candidate, runs
/// the gate against it, and promotes via the `advance_default` CAS (§3.3).
/// Pins the canonical gate-entry decision chain for the C2 extraction.
#[tokio::test]
async fn trace_gate_entry_and_promote() {
    let Some(rig) = spawn_setup_with(SpawnOpts {
        move_main_during_eval: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };

    rig.sink
        .begin("create+release → eval (main moves) → gate → promote");
    let job = rig.handle.create_job(req("impl-cmd", &[])).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Done).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-done").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "gate_entry_and_promote");
    assert_invariants_of(&rig.invariants);
}

/// `gate_and_human.rs::merge_gate_failure_reworks_on_new_base_without_budget`:
/// the gate fails (CI class), the job reworks on the new base — rebase, base
/// pin, `EnterWork` — and lands on cycle 2 via the fast path (base caught up).
#[tokio::test]
async fn trace_gate_failure_full_rework() {
    let Some(rig) = spawn_setup_with(SpawnOpts {
        script_exits: vec![0, 1, 0],
        move_main_during_eval: true,
        work_hooks: vec![WorkHook::Commit, WorkHook::Recommit],
        ..Default::default()
    })
    .await
    else {
        return;
    };

    rig.sink
        .begin("gate fails → rework on new base → cycle-2 fast path");
    let job = rig.handle.create_job(req("impl-cmd", &[])).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Done).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-done").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "gate_failure_full_rework");
    assert_invariants_of(&rig.invariants);
}

/// `gate_and_human.rs::gate_compile_failure_takes_fast_path_and_relands` (job
/// #154): a stage-0 (build) gate failure classifies as compile and takes the
/// gate-fix fast path — scoped fix task, no cycle-2 eval, straight re-gate,
/// land. Pins the classification + budget decision for C2.
#[tokio::test]
async fn trace_gate_fix_fast_path() {
    let Some(rig) = spawn_setup_with(SpawnOpts {
        script_exits: vec![0, 0, 1, 0, 0],
        move_main_during_eval: true,
        work_hooks: vec![WorkHook::Commit, WorkHook::Recommit],
        logs: Some(b"error[E0433]: failed to resolve: use of undeclared crate `foo`"),
        no_defaults: true,
    })
    .await
    else {
        return;
    };

    rig.sink
        .begin("gate build fails → gate-fix → re-gate → promote");
    let job = rig.handle.create_job(req("gatefix", &[])).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Done).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-done").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "gate_fix_fast_path");
    assert_invariants_of(&rig.invariants);
}

/// `gate_and_human.rs::rebase_conflict_falls_through_to_wrapup_conflict_path`:
/// a conflicting land during work → the pre-eval rebase aborts, eval runs on
/// the old base, and the wrap-up squash conflicts — driving the conflict
/// re-entry (`RebaseOntoWithConflict` + base pin + `EnterWork`), never a gate.
#[tokio::test]
async fn trace_conflict_reentry() {
    let Some(rig) = spawn_setup_with(SpawnOpts {
        work_hooks: vec![WorkHook::CommitAndConflictMain, WorkHook::Recommit],
        ..Default::default()
    })
    .await
    else {
        return;
    };

    rig.sink
        .begin("conflicting land → wrap-up conflict rework → cycle-2 land");
    let job = rig.handle.create_job(req("impl-cmd", &[])).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Done).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-done").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "conflict_reentry");
    assert_invariants_of(&rig.invariants);
}

/// `execution.rs::eval_failure_reworks_with_context_then_passes`, distilled to
/// its decision trace: the required evaluator fails on cycle 1, the §3.3 reduce
/// spends a rework budget (Evaluation→Work, `job-rework-started`), and cycle 2
/// passes and lands. Pins the reduce's rework arm for the C5 extraction.
#[tokio::test]
async fn trace_eval_failure_rework() {
    let Some(rig) = spawn_setup_with(SpawnOpts {
        script_exits: vec![1, 0],
        work_hooks: vec![WorkHook::Commit, WorkHook::Recommit],
        ..Default::default()
    })
    .await
    else {
        return;
    };

    rig.sink
        .begin("eval fails → rework → cycle-2 eval passes → land");
    let job = rig
        .handle
        .create_job(req("impl-rework", &[]))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Done).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-done").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "eval_failure_rework");
    assert_invariants_of(&rig.invariants);
}

/// The reduce's escalate arm: the same required-evaluator failure on a type with
/// no `rework_budget` escalates `rework_budget_exhausted` straight out of
/// Evaluation, behind a Human task (§3.3, §1.2).
#[tokio::test]
async fn trace_eval_failure_no_budget_escalates() {
    let Some(rig) = spawn_setup_with(SpawnOpts {
        script_exits: vec![1],
        ..Default::default()
    })
    .await
    else {
        return;
    };

    rig.sink
        .begin("eval fails with no rework budget → escalate");
    let job = rig.handle.create_job(req("impl-cmd", &[])).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Escalated).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-escalated").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "eval_failure_no_budget_escalates");
    assert_invariants_of(&rig.invariants);
}

/// Staged evaluation (§3.3): the staged `gatefix` type's stage-0 `build`
/// evaluator fails, so stage 1 (`test`) is never created — exactly one
/// `task-created` in the eval fan-out — and the reduce escalates over the
/// stages that actually ran. Pins the staged short-circuit for C5.
#[tokio::test]
async fn trace_staged_eval_short_circuit() {
    let Some(rig) = spawn_setup_with(SpawnOpts {
        script_exits: vec![1],
        no_defaults: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };

    rig.sink
        .begin("staged eval: stage-0 fails → stage 1 never launched → escalate");
    let job = rig.handle.create_job(req("gatefix", &[])).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Escalated).await;
    wait_trace_effect(&rig.sink, "PublishEvent job-escalated").await;
    assert_invariants_of(&rig.invariants);

    assert_trace(&rig.sink, "staged_eval_short_circuit");
    assert_invariants_of(&rig.invariants);
}

/// Block until the captured trace's final step records `effect`. Used to
/// snapshot the actor-driven scenario only after its terminal effect lands — the
/// Done KV write precedes the last `job-done` publish, so waiting on job state
/// would race the trailing effect into or out of the snapshot. Polls the sink
/// (in-memory, no KV watch can see it) under a hard timeout.
async fn wait_trace_effect(sink: &TraceSink, effect: &str) {
    test_utils::wait::poll_default(format!("trace effect {effect:?}"), || {
        sink.snapshot()
            .steps
            .last()
            .filter(|step| step.effects.iter().any(|e| e == effect))
            .map(|_| ())
    })
    .await;
}

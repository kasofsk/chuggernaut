//! Tier-2 lifecycle tests: Core against a real NATS server (Docker) and real
//! bare repos (TempRepo). Covers creation, release validation, blocking,
//! unblock-with-revalidation, escalation, and revoke cascade — everything
//! before Ready→Work.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreError, CreateSpec};
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{EscalationAction, JobState, TaskKind, TaskResolution, TaskState};

mod common;
use common::assert_invariants;

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

/// A job type that declares `min_dispatcher` far above this binary's
/// `CONFIG_SCHEMA_EPOCH` (spec §14.2): a config that would only ever be launched
/// by an older dispatcher than the one that validated it — the N-1 deploy-skew
/// window. Otherwise a valid command type, so the version-skew gate is the sole
/// failure `load_job_type` hits.
const SKEWED_YAML: &str = r#"
name: skewed
image: img:latest
min_dispatcher: 999
work:
  type: command
  run: ./x.sh
"#;

async fn seed_repo(repo: &TempRepo) {
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/build.yaml", BUILD_YAML.as_bytes(), "add build")
        .await;
    clone
        .commit_file("jobs/deploy.yaml", DEPLOY_YAML.as_bytes(), "add deploy")
        .await;
    clone
        .commit_file("jobs/_defaults.yaml", DEFAULTS_YAML.as_bytes(), "defaults")
        .await;
    clone
        .commit_file("prompts/build.md", b"build it", "prompt")
        .await;
    clone
        .commit_file("prompts/review.md", b"review it", "prompt")
        .await;
    clone.push("main").await;
}

async fn setup() -> Option<(
    &'static test_utils::nats::NatsTestServer,
    NatsStore,
    TempRepo,
    Core,
)> {
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
    let core = new_core(&store, core_repos_root(&repo)).await;
    Some((server, store, repo, core))
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
        members: vec![],
        inputs: Default::default(),
        groups: vec![],
        draft: false,
    }
}

#[tokio::test]
async fn release_blocking_unblocking_and_events() {
    let Some((_server, store, _repo, mut core)) = setup().await else {
        return;
    };

    let build = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    assert_invariants(&core);
    assert_eq!(build.state, JobState::Frozen);
    assert_invariants(&core);

    assert_eq!(
        core.release_job("acme", "api", build.id).await.unwrap(),
        JobState::Ready
    );
    assert_invariants(&core);
    assert_eq!(
        core.release_job("acme", "api", deploy.id).await.unwrap(),
        JobState::Blocked
    );
    assert_invariants(&core);
    assert_eq!(core.queue.len(), 1);
    let pinned = core
        .graph("acme", "api")
        .unwrap()
        .get(build.id)
        .unwrap()
        .base_ref
        .clone();
    assert!(pinned.is_some());

    assert!(matches!(
        core.release_job("acme", "api", build.id).await,
        Err(CoreError::Transition(_))
    ));
    assert_invariants(&core);

    let jobs = store.jobs().await.unwrap();
    let mut done = jobs.get("acme", "api", build.id).await.unwrap().unwrap();
    done.state = JobState::Done;
    jobs.put(&done).await.unwrap();

    let mut core = new_core(&store, core_repos_root(&_repo)).await;
    assert_invariants(&core);
    core.on_job_done("acme", "api", build.id).await.unwrap();
    assert_invariants(&core);

    let dep = jobs.get("acme", "api", deploy.id).await.unwrap().unwrap();
    assert_eq!(dep.state, JobState::Ready);
    assert!(dep.base_ref.is_some());
    assert!(dep.ready_at.is_some());
    assert_eq!(core.queue.len(), 1);

    let events = collect_event_types(&store).await;
    for expected in ["job-created", "job-released", "job-unblocked"] {
        assert!(
            events.contains(&expected.to_string()),
            "missing {expected}: {events:?}"
        );
    }
}

#[tokio::test]
async fn release_validation_rejects_bad_wiring_and_missing_secret() {
    let Some((_server, store, _repo, mut core)) = setup().await else {
        return;
    };

    let bad = core.create_job(req("deploy", &[999])).await.unwrap();
    assert_invariants(&core);
    let Err(CoreError::Validation(errs)) = core.release_job("acme", "api", bad.id).await else {
        panic!("expected validation failure");
    };
    assert_invariants(&core);
    let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
    assert!(fields.iter().all(|f| f == &"deps"), "{errs:?}");
    assert_eq!(errs.len(), 1, "{errs:?}");

    store
        .raw_bucket(store::buckets::SECRETS)
        .await
        .unwrap()
        .delete("acme.api.DEPLOY_KEY")
        .await
        .unwrap();
    let b = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    let Err(CoreError::Validation(errs)) = core.release_job("acme", "api", b.id).await else {
        panic!("expected validation failure");
    };
    assert_invariants(&core);
    assert!(
        errs.iter()
            .any(|e| e.field == "secrets" && e.message.contains("DEPLOY_KEY"))
    );
}

#[tokio::test]
async fn unblock_revalidation_failure_stalls_with_human_task() {
    let Some((_server, store, repo, mut core)) = setup().await else {
        return;
    };

    let build = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    assert_invariants(&core);
    core.release_job("acme", "api", build.id).await.unwrap();
    assert_invariants(&core);
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
    core.on_job_done("acme", "api", build.id).await.unwrap();
    assert_invariants(&core);

    let dep = jobs.get("acme", "api", deploy.id).await.unwrap().unwrap();
    assert_eq!(dep.state, JobState::Stalled);
    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", deploy.id)
        .await
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, TaskState::Pending);
    assert!(
        matches!(&tasks[0].kind, TaskKind::Human { prompt } if prompt.contains("re-validation"))
    );
}

/// A Stalled (pre-work) escalation accepts only Retry and Revoke (§1.2, fix
/// #2): Resolve is rejected *without* consuming the task, and a Retry re-runs
/// Ready-transition re-validation — reaching Ready once the type parses again.
/// The dedicated state is what makes Resolve→Evaluation impossible here.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn stalled_job_rejects_resolve_and_retry_revalidates_to_ready() {
    let Some((_server, store, repo, mut core)) = setup().await else {
        return;
    };

    let build = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    assert_invariants(&core);
    core.release_job("acme", "api", build.id).await.unwrap();
    assert_invariants(&core);
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

    let mut core2 = new_core(&store, core_repos_root(&repo)).await;
    core2.on_job_done("acme", "api", build.id).await.unwrap();
    assert_invariants(&core2);
    assert_eq!(
        jobs.get("acme", "api", deploy.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Stalled
    );
    let task_id = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", deploy.id)
        .await
        .unwrap()[0]
        .id;

    let (handle, sink) = common::spawn_checked(core2);

    let err = handle
        .resolve_task(
            "acme",
            "api",
            deploy.id,
            task_id,
            TaskResolution::Escalation {
                action: EscalationAction::Resolve,
                structured: None,
            },
            "david",
        )
        .await;
    common::assert_invariants_of(&sink);
    assert!(
        matches!(err, Err(CoreError::InvalidResolution(_))),
        "got {err:?}"
    );
    common::assert_invariants_of(&sink);
    let task = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", deploy.id)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.id == task_id)
        .unwrap();
    assert_eq!(
        task.state,
        TaskState::Pending,
        "rejected Resolve must not consume the task"
    );

    let clone = repo.clone_branch("main").await;
    clone
        .commit_file(
            "jobs/deploy.yaml",
            DEPLOY_YAML.as_bytes(),
            "fix deploy type",
        )
        .await;
    clone.push("main").await;

    handle
        .resolve_task(
            "acme",
            "api",
            deploy.id,
            task_id,
            TaskResolution::Escalation {
                action: EscalationAction::Retry,
                structured: None,
            },
            "david",
        )
        .await
        .unwrap();
    common::assert_invariants_of(&sink);

    test_utils::wait::job_where(
        &store,
        "acme",
        "api",
        deploy.id,
        format!("job {} to clear the stall after Retry", deploy.id),
        |j| {
            let s = j.state;
            if matches!(s, JobState::Stalled) {
                return false;
            }
            assert!(
                matches!(
                    s,
                    JobState::Ready
                        | JobState::Work
                        | JobState::Evaluation
                        | JobState::WrapUp
                        | JobState::Done
                ),
                "Retry should move the job forward, got {s:?}"
            );
            true
        },
    )
    .await;
    common::assert_invariants_of(&sink);
}

/// spec §14.2 launch park: a job whose pinned config requires a newer dispatcher
/// than the running binary parks **pre-Work (Stalled)** with reason
/// `config_schema_skew` — a single park, Retry/Revoke only — instead of burning
/// the launch into `Escalated` one job at a time (the 2026-07-22 `launch_
/// validation_failed` storm this ticket exists to eliminate). Distinct from an
/// ordinary launch validation failure, which stays `launch_validation_failed`.
#[tokio::test]
async fn version_skewed_config_parks_stalled_not_escalated_at_launch() {
    let Some((_server, store, repo, mut core)) = setup().await else {
        return;
    };

    let clone = repo.clone_branch("main").await;
    clone
        .commit_file(
            "jobs/skewed.yaml",
            SKEWED_YAML.as_bytes(),
            "add skewed type",
        )
        .await;
    clone.push("main").await;
    let skewed_sha = repo.head().await;

    let job = core.create_job(req("skewed", &[])).await.unwrap();
    assert_invariants(&core);
    let jobs = store.jobs().await.unwrap();
    let mut ready = jobs.get("acme", "api", job.id).await.unwrap().unwrap();
    ready.state = JobState::Ready;
    ready.base_ref = Some(skewed_sha);
    jobs.put(&ready).await.unwrap();

    let core2 = new_core(&store, core_repos_root(&repo)).await;
    let (_handle, sink) = common::spawn_checked(core2);

    let rec = test_utils::wait::job_where(
        &store,
        "acme",
        "api",
        job.id,
        format!("skewed job {} to leave Ready (park pre-Work)", job.id),
        |rec| rec.state != JobState::Ready,
    )
    .await;

    common::assert_invariants_of(&sink);

    assert_eq!(
        rec.state,
        JobState::Stalled,
        "version-skewed config must park Stalled, not Escalated"
    );
    let esc = rec.escalation.expect("stall records why on the job");
    assert_eq!(esc.reason, "config_schema_skew");

    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_eq!(
        tasks.len(),
        1,
        "one park, not an escalation storm: {tasks:?}"
    );
    assert!(matches!(tasks[0].kind, TaskKind::Human { .. }));
    assert_eq!(tasks[0].state, TaskState::Pending);
    common::assert_invariants_of(&sink);
}

#[tokio::test]
async fn revoke_cascades_through_pending_dependents() {
    let Some((_server, store, _repo, mut core)) = setup().await else {
        return;
    };

    let a = core.create_job(req("build", &[])).await.unwrap();
    assert_invariants(&core);
    let b = core.create_job(req("deploy", &[a.id])).await.unwrap();
    assert_invariants(&core);
    let c = core.create_job(req("deploy", &[b.id])).await.unwrap();
    assert_invariants(&core);
    core.release_job("acme", "api", a.id).await.unwrap();
    assert_invariants(&core);

    let cascaded = core.revoke_job("acme", "api", a.id).await.unwrap();
    assert_invariants(&core);
    assert_eq!(cascaded, vec![b.id, c.id]);
    assert_invariants(&core);
    assert!(core.queue.is_empty());

    let jobs = store.jobs().await.unwrap();
    for seq in [a.id, b.id, c.id] {
        let rec = jobs.get("acme", "api", seq).await.unwrap().unwrap();
        assert_eq!(rec.state, JobState::Revoked);
        assert!(
            rec.completed_at.is_some(),
            "revoked job #{seq} must carry completed_at"
        );
    }
    assert!(matches!(
        core.revoke_job("acme", "api", a.id).await,
        Err(CoreError::Transition(_))
    ));
    assert_invariants(&core);
}

fn core_repos_root(repo: &TempRepo) -> std::path::PathBuf {
    repo.bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

async fn collect_event_types(store: &NatsStore) -> Vec<String> {
    store
        .read_stream("job-events", 100)
        .await
        .unwrap()
        .iter()
        .map(|payload| {
            let v: serde_json::Value = serde_json::from_slice(payload).unwrap();
            v["event_type"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

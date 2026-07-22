//! Tier-2 lifecycle tests: Core against a real NATS server (Docker) and real
//! bare repos (TempRepo). Covers creation, release validation, blocking,
//! unblock-with-revalidation, escalation, and revoke cascade — everything
//! before Ready→Work.

use dispatcher::core::{Core, CoreConfig, CoreError, CreateJobRequest};
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{EscalationAction, JobState, TaskKind, TaskResolution, TaskState};

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

async fn setup() -> Option<(test_utils::nats::NatsTestServer, NatsStore, TempRepo, Core)> {
    let server = test_utils::nats::NatsTestServer::spawn()?;
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    seed_repo(&repo).await;
    // Declared secret must exist for release validation.
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

fn req(r#type: &str, deps: &[u64]) -> CreateJobRequest {
    CreateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        title: String::new(),
        description: String::new(),
        deps: deps.to_vec(),
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        factory: None,
        draft: false,
    }
}

#[tokio::test]
async fn release_blocking_unblocking_and_events() {
    let Some((_server, store, _repo, mut core)) = setup().await else {
        return;
    };

    let build = core.create_job(req("build", &[])).await.unwrap();
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    assert_eq!(build.state, JobState::Frozen);

    // No deps → Ready, base_ref pinned, queued. Deps not Done → Blocked.
    assert_eq!(
        core.release_job("acme", "api", build.id).await.unwrap(),
        JobState::Ready
    );
    assert_eq!(
        core.release_job("acme", "api", deploy.id).await.unwrap(),
        JobState::Blocked
    );
    assert_eq!(core.queue.len(), 1);
    let pinned = core
        .graph("acme", "api")
        .unwrap()
        .get(build.id)
        .unwrap()
        .base_ref
        .clone();
    assert!(pinned.is_some());

    // Double-release is an invalid transition.
    assert!(matches!(
        core.release_job("acme", "api", build.id).await,
        Err(CoreError::Transition(_))
    ));

    // Simulate build completing (execution slice lands later): Done in KV,
    // then a fresh Core reload — the restart path — and dependent unblocking.
    let jobs = store.jobs().await.unwrap();
    let mut done = jobs.get("acme", "api", build.id).await.unwrap().unwrap();
    done.state = JobState::Done;
    jobs.put(&done).await.unwrap();

    let mut core = new_core(&store, core_repos_root(&_repo)).await;
    core.on_job_done("acme", "api", build.id).await.unwrap();

    let dep = jobs.get("acme", "api", deploy.id).await.unwrap().unwrap();
    assert_eq!(dep.state, JobState::Ready);
    assert!(dep.base_ref.is_some());
    assert!(dep.ready_at.is_some());
    assert_eq!(core.queue.len(), 1);

    // The event stream carries the §6.3 trail.
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

    // Unknown upstream.
    let bad = core.create_job(req("deploy", &[999])).await.unwrap();
    let Err(CoreError::Validation(errs)) = core.release_job("acme", "api", bad.id).await else {
        panic!("expected validation failure");
    };
    let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
    assert!(fields.iter().all(|f| f == &"deps"), "{errs:?}");
    // depends on unknown job #999
    assert_eq!(errs.len(), 1, "{errs:?}");

    // Missing secret fails static validation.
    store
        .raw_bucket(store::buckets::SECRETS)
        .await
        .unwrap()
        .delete("acme.api.DEPLOY_KEY")
        .await
        .unwrap();
    let b = core.create_job(req("build", &[])).await.unwrap();
    let Err(CoreError::Validation(errs)) = core.release_job("acme", "api", b.id).await else {
        panic!("expected validation failure");
    };
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
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    core.release_job("acme", "api", build.id).await.unwrap();
    core.release_job("acme", "api", deploy.id).await.unwrap();

    // The deploy job type breaks on main between release and unblock.
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

    // Pre-work escalation (§1.2): no work task ran, so the job Stalls rather
    // than Escalates — resolvable Retry/Revoke only.
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
async fn stalled_job_rejects_resolve_and_retry_revalidates_to_ready() {
    let Some((_server, store, repo, mut core)) = setup().await else {
        return;
    };

    let build = core.create_job(req("build", &[])).await.unwrap();
    let deploy = core.create_job(req("deploy", &[build.id])).await.unwrap();
    core.release_job("acme", "api", build.id).await.unwrap();
    core.release_job("acme", "api", deploy.id).await.unwrap();

    // Break the deploy type on main between release and unblock.
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/deploy.yaml", b"not: [valid", "break deploy type")
        .await;
    clone.push("main").await;

    let jobs = store.jobs().await.unwrap();
    let mut done = jobs.get("acme", "api", build.id).await.unwrap().unwrap();
    done.state = JobState::Done;
    jobs.put(&done).await.unwrap();

    // A fresh core observes the dep complete and Stalls deploy on re-validation.
    let mut core2 = new_core(&store, core_repos_root(&repo)).await;
    core2.on_job_done("acme", "api", build.id).await.unwrap();
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

    let handle = dispatcher::core::spawn(core2);

    // Resolve is rejected and leaves the escalation task Pending (the reject
    // happens before the task is marked done — fix #2's ordering).
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
    assert!(
        matches!(err, Err(CoreError::InvalidResolution(_))),
        "got {err:?}"
    );
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

    // Fix the type on main; Retry re-validates and clears the stall.
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

    let mut cleared = false;
    for _ in 0..100 {
        let s = jobs
            .get("acme", "api", deploy.id)
            .await
            .unwrap()
            .unwrap()
            .state;
        if !matches!(s, JobState::Stalled) {
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
            cleared = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        cleared,
        "Retry after the fix must clear the stall toward Ready"
    );
}

#[tokio::test]
async fn revoke_cascades_through_pending_dependents() {
    let Some((_server, store, _repo, mut core)) = setup().await else {
        return;
    };

    let a = core.create_job(req("build", &[])).await.unwrap();
    let b = core.create_job(req("deploy", &[a.id])).await.unwrap();
    let c = core.create_job(req("deploy", &[b.id])).await.unwrap();
    core.release_job("acme", "api", a.id).await.unwrap(); // Ready + queued

    let cascaded = core.revoke_job("acme", "api", a.id).await.unwrap();
    assert_eq!(cascaded, vec![b.id, c.id]);
    assert!(core.queue.is_empty());

    let jobs = store.jobs().await.unwrap();
    for seq in [a.id, b.id, c.id] {
        let rec = jobs.get("acme", "api", seq).await.unwrap().unwrap();
        assert_eq!(rec.state, JobState::Revoked);
        // Every terminal transition — including a cascaded revoke — stamps the
        // completion moment so the jobs list can show it without a lookup.
        assert!(
            rec.completed_at.is_some(),
            "revoked job #{seq} must carry completed_at"
        );
    }
    // Terminal: revoking again is rejected.
    assert!(matches!(
        core.revoke_job("acme", "api", a.id).await,
        Err(CoreError::Transition(_))
    ));
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

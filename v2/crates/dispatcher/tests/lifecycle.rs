//! Tier-2 lifecycle tests: Core against a real NATS server (Docker) and real
//! bare repos (TempRepo). Covers creation, release validation, blocking,
//! unblock-with-revalidation, escalation, and revoke cascade — everything
//! before Ready→Work.

use dispatcher::core::{Core, CoreError, CreateJobRequest};
use std::collections::HashMap;
use store::NatsStore;
use test_utils::repo::TempRepo;
use types::{JobState, TaskKind, TaskState};

const BUILD_YAML: &str = r#"
name: build
image: img:latest
work:
  type: agent
  prompt: prompts/build.md
  provider: claude
  review:
    prompt: prompts/review.md
    iterations: 3
secrets: [DEPLOY_KEY]
"#;

const DEPLOY_YAML: &str = r#"
name: deploy
image: img:latest
work:
  type: command
  run: ./deploy.sh
inputs:
  - name: artifact
"#;

const DEFAULTS_YAML: &str = r#"
eval:
  - name: ci
    type: command
    run: ./scripts/ci.sh
"#;

async fn seed_repo(repo: &TempRepo) {
    let clone = repo.clone_branch("main").await;
    clone.commit_file("jobs/build.yaml", BUILD_YAML.as_bytes(), "add build").await;
    clone.commit_file("jobs/deploy.yaml", DEPLOY_YAML.as_bytes(), "add deploy").await;
    clone.commit_file("jobs/_defaults.yaml", DEFAULTS_YAML.as_bytes(), "defaults").await;
    clone.commit_file("prompts/build.md", b"build it", "prompt").await;
    clone.commit_file("prompts/review.md", b"review it", "prompt").await;
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
    let core = Core::new(store.clone(), vcs::RepoManager::new(repo.bare_path().parent().unwrap().parent().unwrap()))
        .await
        .unwrap();
    Some((server, store, repo, core))
}

fn req(r#type: &str, inputs: &[(&str, u64)]) -> CreateJobRequest {
    CreateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        inputs: inputs.iter().map(|(n, s)| (n.to_string(), *s)).collect::<HashMap<_, _>>(),
        knowledge_tags: vec![],
        factory: None,
    }
}

#[tokio::test]
async fn release_blocking_unblocking_and_events() {
    let Some((_server, store, _repo, mut core)) = setup().await else { return };

    let build = core.create_job(req("build", &[])).await.unwrap();
    let deploy = core
        .create_job(req("deploy", &[("artifact", build.id)]))
        .await
        .unwrap();
    assert_eq!(build.state, JobState::Frozen);

    // No deps → Ready, base_ref pinned, queued. Deps not Done → Blocked.
    assert_eq!(core.release_job("acme", "api", build.id).await.unwrap(), JobState::Ready);
    assert_eq!(core.release_job("acme", "api", deploy.id).await.unwrap(), JobState::Blocked);
    assert_eq!(core.queue.len(), 1);
    let pinned = core.graph("acme", "api").unwrap().get(build.id).unwrap().base_ref.clone();
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

    let repos_root = core_repos_root(&_repo);
    let mut core = Core::new(store.clone(), vcs::RepoManager::new(repos_root)).await.unwrap();
    core.on_job_done("acme", "api", build.id).await.unwrap();

    let dep = jobs.get("acme", "api", deploy.id).await.unwrap().unwrap();
    assert_eq!(dep.state, JobState::Ready);
    assert!(dep.base_ref.is_some());
    assert!(dep.ready_at.is_some());
    assert_eq!(core.queue.len(), 1);

    // The event stream carries the §6.3 trail.
    let events = collect_event_types(&store).await;
    for expected in ["job-created", "job-released", "job-unblocked"] {
        assert!(events.contains(&expected.to_string()), "missing {expected}: {events:?}");
    }
}

#[tokio::test]
async fn release_validation_rejects_bad_wiring_and_missing_secret() {
    let Some((_server, store, _repo, mut core)) = setup().await else { return };

    // Unknown upstream + undeclared input name.
    let bad = core.create_job(req("deploy", &[("nope", 999)])).await.unwrap();
    let Err(CoreError::Validation(errs)) = core.release_job("acme", "api", bad.id).await else {
        panic!("expected validation failure");
    };
    let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
    assert!(fields.iter().all(|f| f.starts_with("inputs.")), "{errs:?}");
    // unknown job + undeclared name + declared-but-unwired 'artifact'
    assert_eq!(errs.len(), 3, "{errs:?}");

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
    assert!(errs.iter().any(|e| e.field == "secrets" && e.message.contains("DEPLOY_KEY")));
}

#[tokio::test]
async fn unblock_revalidation_failure_escalates_with_human_task() {
    let Some((_server, store, repo, mut core)) = setup().await else { return };

    let build = core.create_job(req("build", &[])).await.unwrap();
    let deploy = core.create_job(req("deploy", &[("artifact", build.id)])).await.unwrap();
    core.release_job("acme", "api", build.id).await.unwrap();
    core.release_job("acme", "api", deploy.id).await.unwrap();

    // The deploy job type breaks on main between release and unblock.
    let clone = repo.clone_branch("main").await;
    clone.commit_file("jobs/deploy.yaml", b"not: [valid", "break deploy type").await;
    clone.push("main").await;

    let jobs = store.jobs().await.unwrap();
    let mut done = jobs.get("acme", "api", build.id).await.unwrap().unwrap();
    done.state = JobState::Done;
    jobs.put(&done).await.unwrap();

    let mut core = Core::new(store.clone(), vcs::RepoManager::new(core_repos_root(&repo))).await.unwrap();
    core.on_job_done("acme", "api", build.id).await.unwrap();

    let dep = jobs.get("acme", "api", deploy.id).await.unwrap().unwrap();
    assert_eq!(dep.state, JobState::Escalated);
    let tasks = store.tasks().await.unwrap().list_for_job("acme", "api", deploy.id).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].state, TaskState::Pending);
    assert!(matches!(&tasks[0].kind, TaskKind::Human { prompt } if prompt.contains("re-validation")));
}

#[tokio::test]
async fn revoke_cascades_through_pending_dependents() {
    let Some((_server, store, _repo, mut core)) = setup().await else { return };

    let a = core.create_job(req("build", &[])).await.unwrap();
    let b = core.create_job(req("deploy", &[("artifact", a.id)])).await.unwrap();
    let c = core.create_job(req("deploy", &[("artifact", b.id)])).await.unwrap();
    core.release_job("acme", "api", a.id).await.unwrap(); // Ready + queued

    let cascaded = core.revoke_job("acme", "api", a.id).await.unwrap();
    assert_eq!(cascaded, vec![b.id, c.id]);
    assert!(core.queue.is_empty());

    let jobs = store.jobs().await.unwrap();
    for seq in [a.id, b.id, c.id] {
        assert_eq!(jobs.get("acme", "api", seq).await.unwrap().unwrap().state, JobState::Revoked);
    }
    // Terminal: revoking again is rejected.
    assert!(matches!(
        core.revoke_job("acme", "api", a.id).await,
        Err(CoreError::Transition(_))
    ));
}

fn core_repos_root(repo: &TempRepo) -> std::path::PathBuf {
    repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf()
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

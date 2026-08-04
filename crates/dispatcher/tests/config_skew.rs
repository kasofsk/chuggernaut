//! Tier-2 (real git, no NATS): the merge-time version-skew scan (spec §14.3).
//!
//! The authoritative half of the skew gate reads the landing branch's config
//! out of the bare repo and compares it against the running binary's
//! `CONFIG_SCHEMA_EPOCH`. These pin what it scans — job types and schedules, at
//! either config-root layout — and that it reads a file the strict parsers
//! reject, which is the file the gate exists to catch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::release;
use test_utils::repo::TempRepo;
use types::CONFIG_SCHEMA_EPOCH;

fn job_yaml(min_dispatcher: Option<u32>) -> String {
    let declaration = match min_dispatcher {
        Some(epoch) => format!("min_dispatcher: {epoch}\n"),
        None => String::new(),
    };
    format!("name: build\nimage: img:latest\n{declaration}work:\n  type: command\n  run: ./go.sh\n")
}

fn schedule_yaml(min_dispatcher: u32) -> String {
    format!("name: nightly\njob_type: build\ncron: '0 2 * * *'\nmin_dispatcher: {min_dispatcher}\n")
}

/// Commit `files` onto `branch` (created off main) and return its name.
async fn seed_branch(repo: &TempRepo, branch: &str, files: &[(&str, String)]) -> String {
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;
    let clone = repo.clone_branch(branch).await;
    for (path, contents) in files {
        clone.commit_file(path, contents.as_bytes(), "seed").await;
    }
    clone.push(branch).await;
    branch.to_string()
}

async fn skew(repo: &TempRepo, reference: &str) -> Option<types::ConfigSkew> {
    release::branch_config_skew(&repo.manager, "acme", "api", reference)
        .await
        .unwrap()
}

/// A branch whose config declares an epoch above this binary's is named with
/// the file and both epochs — and it is found even though the same file's
/// unknown nested key makes `JobType::parse` refuse it outright (§14.2).
#[tokio::test]
async fn a_config_ahead_of_this_binary_is_named_with_both_epochs() {
    let repo = TempRepo::create("acme", "api").await;
    let ahead = format!(
        "name: build\nimage: img:latest\nmin_dispatcher: {}\n\
         work:\n  type: command\n  run: ./go.sh\n  teleport: true\n",
        CONFIG_SCHEMA_EPOCH + 1
    );
    assert!(types::JobType::parse(&ahead).is_err());
    let branch = seed_branch(
        &repo,
        "job/1",
        &[
            (".chug/jobs/build.yaml", job_yaml(Some(CONFIG_SCHEMA_EPOCH))),
            (".chug/jobs/future.yaml", ahead),
        ],
    )
    .await;

    let found = skew(&repo, &branch).await.expect("skew is detected");
    assert_eq!(found.path, ".chug/jobs/future.yaml");
    assert_eq!(found.needed, CONFIG_SCHEMA_EPOCH + 1);
    assert_eq!(found.running, CONFIG_SCHEMA_EPOCH);
}

/// The ordinary branch: configs at or below the running epoch, and configs
/// declaring nothing, are no skew — the gate must not refuse every landing.
#[tokio::test]
async fn a_branch_within_this_binarys_epoch_carries_no_skew() {
    let repo = TempRepo::create("acme", "api").await;
    let branch = seed_branch(
        &repo,
        "job/1",
        &[
            (".chug/jobs/build.yaml", job_yaml(Some(CONFIG_SCHEMA_EPOCH))),
            (".chug/jobs/old.yaml", job_yaml(Some(1))),
            (".chug/jobs/plain.yaml", job_yaml(None)),
            ("src/work.rs", "pub fn f() {}\n".to_string()),
        ],
    )
    .await;

    assert_eq!(skew(&repo, &branch).await, None);
}

/// A schedule file carries `min_dispatcher` with the same meaning, so it is
/// gated the same way (design #310, §14.2).
#[tokio::test]
async fn a_schedule_file_is_gated_like_a_job_type() {
    let repo = TempRepo::create("acme", "api").await;
    let branch = seed_branch(
        &repo,
        "job/1",
        &[(
            ".chug/schedules/nightly.yaml",
            schedule_yaml(CONFIG_SCHEMA_EPOCH + 2),
        )],
    )
    .await;

    let found = skew(&repo, &branch).await.expect("schedules are scanned");
    assert_eq!(found.path, ".chug/schedules/nightly.yaml");
    assert_eq!(found.needed, CONFIG_SCHEMA_EPOCH + 2);
}

/// Repos that predate the config root keep their config at the repo root, and
/// the scan resolves it there like every other reader (§1.1).
#[tokio::test]
async fn the_repo_root_layout_is_scanned_too() {
    let repo = TempRepo::create("acme", "api").await;
    let branch = seed_branch(
        &repo,
        "job/1",
        &[("jobs/future.yaml", job_yaml(Some(CONFIG_SCHEMA_EPOCH + 1)))],
    )
    .await;

    let found = skew(&repo, &branch)
        .await
        .expect("both layouts are scanned");
    assert_eq!(found.path, "jobs/future.yaml");
}

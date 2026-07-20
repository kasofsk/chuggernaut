//! Tier-2 integration tests for RepoManager: real git, temp bare repos,
//! WorkClone as the simulated agent (testing.md).

use test_utils::repo::TempRepo;
use types::{Job, JobState};
use vcs::{BlobEncoding, MergeOutcome};

fn job(repo: &TempRepo, id: u64, state: JobState, base_ref: Option<String>) -> Job {
    Job {
        id,
        project: format!("{}/{}", repo.owner, repo.project),
        r#type: "implement-endpoint".into(),
        title: String::new(),
        description: String::new(),
        deps: Vec::new(),
        state,
        branch: format!("job/{id}"),
        base_ref,
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        factory: None,
        created_at: chrono::Utc::now(),
        ready_at: None,
    }
}

#[tokio::test]
async fn git_version_is_sufficient() {
    let repo = TempRepo::create("acme", "api").await;
    repo.manager.check_git_version().await.unwrap();
}

/// `container::bootstrap_cmd` clones with `--filter=blob:none`; the server only
/// honours that when `uploadpack.allowFilter` is set on the bare repo (set by
/// `create_project`). Without it git warns and silently falls back to a full
/// clone, so assert the blobs are really omitted.
///
/// Note `remote.origin.promisor` is set by the client either way and proves
/// nothing — the discriminator is whether a historical blob is actually absent.
#[tokio::test]
async fn create_project_allows_partial_clone() {
    let repo = TempRepo::create("acme", "api").await;

    let cfg = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.bare_path())
        .args(["config", "--get", "uploadpack.allowFilter"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "true");

    // Two versions of one file: v1's blob is history, not in the current tree,
    // so a working filtered clone must leave it behind.
    let work = repo.clone_branch("main").await;
    work.commit_file("f.txt", b"old-content-v1", "v1").await;
    work.commit_file("f.txt", b"new-content-v2", "v2").await;
    work.push("main").await;

    let dest = tempfile::tempdir().unwrap();
    let target = dest.path().join("filtered");
    let out = std::process::Command::new("git")
        .args([
            "clone",
            "--single-branch",
            "--filter=blob:none",
            "--branch",
            "main",
        ])
        .arg(format!("file://{}", repo.bare_path().display()))
        .arg(&target)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "clone failed: {stderr}");
    assert!(
        !stderr.contains("filtering not recognized by server"),
        "server rejected the filter: {stderr}"
    );

    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(&target)
        .args(["rev-list", "--objects", "--all", "--missing=print"])
        .output()
        .unwrap();
    let omitted = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|l| l.starts_with('?'))
        .count();
    assert!(omitted > 0, "no blobs omitted — filter did not take effect");

    // History is still walkable; that's what --depth 1 would have cost us.
    let log = std::process::Command::new("git")
        .arg("-C")
        .arg(&target)
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 3);
}

#[tokio::test]
async fn create_project_initializes_head_and_empty_commit() {
    let repo = TempRepo::create("acme", "api").await;
    assert_eq!(
        repo.manager.default_branch("acme", "api").await.unwrap(),
        "main"
    );

    let log = repo.manager.log("acme", "api", None, 50).await.unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].message, "chuggernaut: initialize repository");
    assert_eq!(log[0].author, "chuggernaut");

    let tree = repo.manager.tree("acme", "api", "main").await.unwrap();
    assert!(tree.is_empty());

    // Same owner/project again is an error.
    assert!(
        repo.manager
            .create_project("acme", "api", "main")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn noop_merge_when_branch_has_no_commits() {
    let repo = TempRepo::create("acme", "api").await;
    let base = repo.head().await;
    repo.create_job_branch(1, &base).await;

    assert!(
        !repo
            .manager
            .has_commits_beyond("acme", "api", &base, "job/1")
            .await
            .unwrap()
    );
    let outcome = repo
        .manager
        .squash_merge("acme", "api", 1, &base, "implement-endpoint", None)
        .await
        .unwrap();
    assert_eq!(outcome, MergeOutcome::NoOp);
    assert_eq!(
        repo.head().await,
        base,
        "default branch must not move on no-op"
    );
}

#[tokio::test]
async fn clean_squash_merge_creates_single_commit_with_spec_message() {
    let repo = TempRepo::create("acme", "api").await;
    let base = repo.head().await;
    repo.create_job_branch(2, &base).await;

    // Simulated agent: clone, two commits, push.
    let clone = repo.clone_branch("job/2").await;
    clone
        .commit_file("src/lib.rs", b"pub fn hello() {}\n", "add hello")
        .await;
    clone
        .commit_file(
            "src/lib.rs",
            b"pub fn hello() -> u8 { 42 }\n",
            "return something",
        )
        .await;
    clone.push("job/2").await;

    assert!(
        repo.manager
            .has_commits_beyond("acme", "api", &base, "job/2")
            .await
            .unwrap()
    );
    let outcome = repo
        .manager
        .squash_merge(
            "acme",
            "api",
            2,
            &base,
            "implement-endpoint",
            Some("Added hello."),
        )
        .await
        .unwrap();
    let MergeOutcome::Merged { commit } = outcome else {
        panic!("expected Merged, got {outcome:?}")
    };

    // Exactly one new commit on main (squash), with the §3.2 message format.
    let log = repo.manager.log("acme", "api", None, 50).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].hash, commit);
    assert_eq!(log[0].message, "job/2: implement-endpoint");

    // Content landed on main.
    let content = repo
        .manager
        .read_file_at("acme", "api", "main", "src/lib.rs")
        .await
        .unwrap();
    assert_eq!(content.unwrap(), "pub fn hello() -> u8 { 42 }\n");

    // Branch cleanup (dispatcher does this on Done).
    repo.manager
        .delete_branch("acme", "api", "job/2")
        .await
        .unwrap();
    assert!(
        repo.manager
            .resolve_ref("acme", "api", "job/2")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn conflicting_merge_reports_files_and_builds_context() {
    let repo = TempRepo::create("acme", "api").await;
    let old_base = repo.head().await;
    // Both jobs fork from the same base and touch the same file.
    repo.create_job_branch(3, &old_base).await;
    repo.create_job_branch(4, &old_base).await;

    let c3 = repo.clone_branch("job/3").await;
    c3.commit_file("src/routes.rs", b"fn route_a() {}\n", "job 3 routes")
        .await;
    c3.push("job/3").await;
    let c4 = repo.clone_branch("job/4").await;
    c4.commit_file("src/routes.rs", b"fn route_b() {}\n", "job 4 routes")
        .await;
    c4.push("job/4").await;

    // Job 3 lands first.
    let m3 = repo
        .manager
        .squash_merge("acme", "api", 3, &old_base, "add-routes", None)
        .await
        .unwrap();
    assert!(matches!(m3, MergeOutcome::Merged { .. }));
    let new_base = repo.head().await;

    // Job 4 now conflicts (add/add on the same path).
    let m4 = repo
        .manager
        .squash_merge("acme", "api", 4, &old_base, "add-routes", None)
        .await
        .unwrap();
    let MergeOutcome::Conflict { files } = m4 else {
        panic!("expected Conflict, got {m4:?}")
    };
    assert_eq!(files, vec!["src/routes.rs".to_string()]);
    assert_eq!(
        repo.head().await,
        new_base,
        "default branch must not move on conflict"
    );

    // §4.3 conflict-context block.
    let ctx = repo
        .manager
        .conflict_context("acme", "api", &old_base, &new_base, &files)
        .await
        .unwrap();
    assert!(ctx.starts_with("Conflicting files:\n  src/routes.rs\n"));
    assert!(ctx.contains(&format!(
        "Changes on main since base commit {}:",
        &old_base[..7]
    )));
    assert!(ctx.contains("job/3: add-routes"));
    assert!(ctx.contains("src/routes.rs"));
}

#[tokio::test]
async fn diff_for_job_by_state() {
    let repo = TempRepo::create("acme", "api").await;
    let base = repo.head().await;

    // Frozen: no branch yet → empty.
    let frozen = job(&repo, 7, JobState::Frozen, None);
    let d = repo.manager.diff_for_job(&frozen).await.unwrap();
    assert!(d.files.is_empty() && d.diff.is_empty());

    // Work: diff base_ref..job branch.
    repo.create_job_branch(7, &base).await;
    let clone = repo.clone_branch("job/7").await;
    clone
        .commit_file("README.md", b"# api\n\nhello\n", "docs")
        .await;
    clone.push("job/7").await;
    let work = job(&repo, 7, JobState::Work, Some(base.clone()));
    let d = repo.manager.diff_for_job(&work).await.unwrap();
    assert_eq!(d.files.len(), 1);
    assert_eq!(d.files[0].path, "README.md");
    assert_eq!(d.files[0].additions, 3);
    assert!(d.diff.contains("+# api"));

    // Revoked: empty even though branch existed.
    let revoked = job(&repo, 7, JobState::Revoked, Some(base.clone()));
    assert!(
        repo.manager
            .diff_for_job(&revoked)
            .await
            .unwrap()
            .diff
            .is_empty()
    );
}

#[tokio::test]
async fn done_diff_recovers_squash_commit_without_seq_prefix_collision() {
    let repo = TempRepo::create("acme", "api").await;

    // Merge job/42 and job/421 — the grep for 42 must not match 421.
    for (seq, file) in [(42u64, "forty_two.txt"), (421u64, "four_twenty_one.txt")] {
        let base = repo.head().await;
        repo.create_job_branch(seq, &base).await;
        let clone = repo.clone_branch(&format!("job/{seq}")).await;
        clone.commit_file(file, b"content\n", "work").await;
        clone.push(&format!("job/{seq}")).await;
        let m = repo
            .manager
            .squash_merge("acme", "api", seq, &base, "implement-endpoint", None)
            .await
            .unwrap();
        assert!(matches!(m, MergeOutcome::Merged { .. }));
        repo.manager
            .delete_branch("acme", "api", &format!("job/{seq}"))
            .await
            .unwrap();
    }

    let done42 = job(&repo, 42, JobState::Done, None);
    let d = repo.manager.diff_for_job(&done42).await.unwrap();
    assert_eq!(d.files.len(), 1);
    assert_eq!(d.files[0].path, "forty_two.txt");

    let done421 = job(&repo, 421, JobState::Done, None);
    let d = repo.manager.diff_for_job(&done421).await.unwrap();
    assert_eq!(d.files.len(), 1);
    assert_eq!(d.files[0].path, "four_twenty_one.txt");
}

#[tokio::test]
async fn blob_tree_and_missing_files() {
    let repo = TempRepo::create("acme", "api").await;
    let base = repo.head().await;
    repo.create_job_branch(9, &base).await;
    let clone = repo.clone_branch("job/9").await;
    clone
        .commit_file("docs/guide.md", b"# guide\n", "docs")
        .await;
    clone
        .commit_file("assets/blob.bin", &[0u8, 159, 146, 150, 255], "binary")
        .await;
    clone.push("job/9").await;

    let text = repo
        .manager
        .blob("acme", "api", "job/9", "docs/guide.md")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(text.encoding, BlobEncoding::Utf8);
    assert_eq!(text.content, "# guide\n");

    let bin = repo
        .manager
        .blob("acme", "api", "job/9", "assets/blob.bin")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bin.encoding, BlobEncoding::Base64);

    assert!(
        repo.manager
            .blob("acme", "api", "job/9", "nope.txt")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.manager
            .read_file_at("acme", "api", "main", "docs/guide.md")
            .await
            .unwrap()
            .is_none()
    );

    let tree = repo.manager.tree("acme", "api", "job/9").await.unwrap();
    let blobs: Vec<_> = tree.iter().filter(|e| e.r#type == "blob").collect();
    let trees: Vec<_> = tree.iter().filter(|e| e.r#type == "tree").collect();
    assert_eq!(blobs.len(), 2);
    assert_eq!(trees.len(), 2); // docs/, assets/
    let guide = blobs.iter().find(|e| e.path == "docs/guide.md").unwrap();
    assert_eq!(guide.size, Some(8));
    assert!(trees.iter().all(|e| e.size.is_none()));
}

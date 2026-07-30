//! Tier-2 tests for linked-origin projects (§5.3): link + seed, origin
//! release (guards, hold), sync/reset after PR merge, and the reserved
//! `CHUG_` secret filtering. The "GitHub" origin is a second local bare repo
//! (`FakeOrigin`, `file://` URL); the PR API is a scripted fake.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use dispatcher::core::{Core, CoreConfig, CoreError, CreateSpec};
use dispatcher::forge_ingest::github::{GithubError, PrInfo, PullRequestApi};
use dispatcher::forge_ingest::origin::{SECRET_DEPLOY_KEY, SECRET_PAT};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{FakeOrigin, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{JobState, ProjectRecord, ReleaseStatus};

mod common;
use common::{assert_invariants, assert_invariants_of, spawn_checked};

/// Scripted PR API: `create_pr` mints PR #1 open; `get_pr` returns whatever
/// the test last scripted.
struct FakePr {
    created: Mutex<Vec<(String, String, String)>>, // (repo, head, base)
    current: Mutex<PrInfo>,
}

impl FakePr {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            created: Mutex::new(vec![]),
            current: Mutex::new(PrInfo {
                number: 1,
                url: "https://github.test/pr/1".into(),
                state: "open".into(),
                merged: false,
                merge_commit_sha: None,
            }),
        })
    }

    fn script(&self, state: &str, merged: bool) {
        let mut cur = self.current.lock().unwrap();
        cur.state = state.into();
        cur.merged = merged;
    }
}

#[async_trait]
impl PullRequestApi for FakePr {
    async fn create_pr(
        &self,
        repo: &str,
        _pat: &str,
        head: &str,
        base: &str,
        _title: &str,
        _body: &str,
    ) -> Result<PrInfo, GithubError> {
        self.created
            .lock()
            .unwrap()
            .push((repo.into(), head.into(), base.into()));
        Ok(self.current.lock().unwrap().clone())
    }

    async fn get_pr(&self, _repo: &str, _pat: &str, _number: u64) -> Result<PrInfo, GithubError> {
        Ok(self.current.lock().unwrap().clone())
    }
}

struct Rig {
    _server: &'static test_utils::nats::NatsTestServer,
    store: NatsStore,
    origin: FakeOrigin,
    repos_root: tempfile::TempDir,
    pr: Arc<FakePr>,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
}

impl Rig {
    async fn core(&self) -> Core {
        Core::new(
            self.store.clone(),
            vcs::RepoManager::new(self.repos_root.path()),
            self.backend.clone(),
            self.provider.clone(),
            CoreConfig {
                repo_url_base: format!("file://{}", self.repos_root.path().display()),
                nats_url: "nats://test".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .with_pr_api(self.pr.clone())
    }

    fn repos(&self) -> vcs::RepoManager {
        vcs::RepoManager::new(self.repos_root.path())
    }

    /// Mark the linked project as GitHub-backed so releases exercise the PR
    /// path against the fake API (a `file://` origin parses to no github_repo).
    async fn pretend_github(&self, record: &mut ProjectRecord) {
        record.origin.as_mut().unwrap().github_repo = Some("acme/upstream".into());
        self.store
            .projects()
            .await
            .unwrap()
            .put("acme", "api", record)
            .await
            .unwrap();
        self.store
            .raw_bucket(store::buckets::SECRETS)
            .await
            .unwrap()
            .put_json(&format!("acme.api.{SECRET_PAT}"), &"pat-token")
            .await
            .unwrap();
    }
}

async fn rig() -> Option<Rig> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let origin = FakeOrigin::create().await;
    origin
        .commit_to_main("src/app.py", b"print('existing project')", "existing code")
        .await;
    Some(Rig {
        _server: server,
        store,
        origin,
        repos_root: tempfile::tempdir().unwrap(),
        pr: FakePr::new(),
        backend: Arc::new(FakeBackend::new()),
        provider: Arc::new(FakeProvider::new()),
    })
}

/// Commit files onto the linked project's integration branch — a stand-in for
/// jobs having landed (the pre-receive hook passes local, identity-less pushes).
async fn commit_to_integration(rig: &Rig, files: &[(&str, &str)]) {
    let bare = rig.repos().repo_path("acme", "api");
    let clone = clone_branch_from(&bare, "integration").await;
    for (path, content) in files {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("integration").await;
}

#[tokio::test]
async fn link_seeds_config_without_clobbering_and_leaves_origin_untouched() {
    let Some(rig) = rig().await else { return };
    // The origin already carries its own jobs/code.yaml — seeding must not
    // overwrite it.
    rig.origin
        .commit_to_main("jobs/code.yaml", b"name: custom", "user's own job type")
        .await;
    let origin_main_before = rig.origin.main_sha().await;

    let mut core = rig.core().await;
    let record = core
        .link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);

    let link = record.origin.expect("linked");
    assert_eq!(link.main_branch, "main");
    assert_eq!(link.github_repo, None); // file:// origin
    let repos = rig.repos();
    assert_eq!(
        repos.default_branch("acme", "api").await.unwrap(),
        "integration"
    );
    // Origin untouched by linking.
    assert_eq!(rig.origin.main_sha().await, origin_main_before);
    // Existing file preserved; missing template files seeded.
    let integration_yaml = repos
        .read_file_at("acme", "api", "integration", "jobs/code.yaml")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(integration_yaml, "name: custom");
    assert!(
        repos
            .read_file_at("acme", "api", "integration", "prompts/work/code.md")
            .await
            .unwrap()
            .is_some(),
        "template prompt seeded"
    );
    // Integration is ahead of origin main by exactly the seed commit.
    let origin_sha = repos.origin_main_sha("acme", "api").await.unwrap();
    assert_eq!(
        repos
            .count_commits_beyond("acme", "api", &origin_sha, "integration")
            .await
            .unwrap(),
        1
    );

    // Double-link is a conflict.
    assert!(matches!(
        core.link_project("acme", "api", &rig.origin.url(), None)
            .await,
        Err(CoreError::Conflict(_))
    ));
    assert_invariants(&core);
}

#[tokio::test]
async fn ssh_origin_requires_deploy_key_secret() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    let err = core
        .link_project(
            "acme",
            "api",
            "ssh://git@github.com/acme/upstream.git",
            None,
        )
        .await
        .unwrap_err();
    assert_invariants(&core);
    let CoreError::Validation(errs) = err else {
        panic!("expected validation error, got {err}");
    };
    let all = format!("{errs:?}");
    assert!(all.contains(SECRET_DEPLOY_KEY), "{all}");
    assert!(all.contains(SECRET_PAT), "{all}");
}

#[tokio::test]
async fn release_pushes_branch_opens_pr_and_holds() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    let mut record = core
        .link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    rig.pretend_github(&mut record).await;
    commit_to_integration(&rig, &[("src/feature.py", "print('job 1')")]).await;

    let record = core.origin_release("acme", "api").await.unwrap();
    assert_invariants(&core);
    let release = record.release.expect("release state");
    assert_eq!(release.number, 1);
    assert_eq!(release.pr_number, 1);
    assert_eq!(release.status, ReleaseStatus::Open);
    assert!(
        rig.origin.branch_exists("chug/release-1").await,
        "branch pushed to origin"
    );
    // Local pin at the snapshot.
    let pin = rig
        .repos()
        .resolve_ref("acme", "api", "refs/chug/release-1")
        .await
        .unwrap();
    assert_eq!(pin, release.integration_sha);
    // PR opened head → base.
    let created = rig.pr.created.lock().unwrap().clone();
    assert_eq!(
        created,
        vec![(
            "acme/upstream".into(),
            "chug/release-1".into(),
            "main".into()
        )]
    );
    // Status reflects the hold.
    let status = core.origin_status("acme", "api").await.unwrap();
    assert_invariants(&core);
    assert!(status.held);

    // Second release while open → 409.
    assert!(matches!(
        core.origin_release("acme", "api").await,
        Err(CoreError::Conflict(_))
    ));
    assert_invariants(&core);
}

#[tokio::test]
async fn release_with_nothing_ahead_is_a_conflict() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    core.link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    // Merge the seed commit upstream so integration == origin main.
    let record = core.origin_release("acme", "api").await.unwrap(); // seed commit is releasable
    assert_invariants(&core);
    rig.origin
        .merge_branch_to_main("chug/release-1", false)
        .await;
    core.origin_sync("acme", "api").await.unwrap();
    assert_invariants(&core);
    assert_eq!(
        record.release.unwrap().pr_number,
        0,
        "file:// origin releases without a PR"
    );

    // Now integration has nothing beyond origin main.
    let err = core.origin_release("acme", "api").await.unwrap_err();
    assert_invariants(&core);
    assert!(matches!(err, CoreError::Conflict(_)), "{err}");
}

#[tokio::test]
async fn merged_pr_resets_integration_and_clears_hold() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    let mut record = core
        .link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    rig.pretend_github(&mut record).await;
    commit_to_integration(&rig, &[("src/feature.py", "print('job 1')")]).await;
    core.origin_release("acme", "api").await.unwrap();
    assert_invariants(&core);

    // GitHub squash-merges the PR (worst case: shared trees, no shared commits).
    rig.origin
        .merge_branch_to_main("chug/release-1", true)
        .await;
    rig.pr.script("closed", true);

    let status = core.origin_sync("acme", "api").await.unwrap();
    assert_invariants(&core);
    assert_eq!(
        status.release.as_ref().unwrap().status,
        ReleaseStatus::Merged
    );
    assert!(!status.held);
    assert_eq!(status.ahead_by, 0, "integration reset onto new origin main");
    assert_eq!(
        status.integration_sha.as_deref(),
        Some(rig.origin.main_sha().await.as_str())
    );
    // The pre-reset history stays reachable through the pin.
    assert!(
        rig.repos()
            .resolve_ref("acme", "api", "refs/chug/release-1")
            .await
            .is_ok()
    );

    // A fresh release afterward gets n=2.
    commit_to_integration(&rig, &[("src/more.py", "print('job 2')")]).await;
    let record = core.origin_release("acme", "api").await.unwrap();
    assert_invariants(&core);
    assert_eq!(record.release.unwrap().number, 2);
}

#[tokio::test]
async fn closed_unmerged_pr_clears_hold_without_reset() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    let mut record = core
        .link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    rig.pretend_github(&mut record).await;
    commit_to_integration(&rig, &[("src/feature.py", "print('job 1')")]).await;
    let record = core.origin_release("acme", "api").await.unwrap();
    assert_invariants(&core);
    let integration_before = record.release.as_ref().unwrap().integration_sha.clone();

    rig.pr.script("closed", false); // closed without merging

    let status = core.origin_sync("acme", "api").await.unwrap();
    assert_invariants(&core);
    assert_eq!(
        status.release.as_ref().unwrap().status,
        ReleaseStatus::Closed
    );
    assert!(!status.held);
    // No reset: unreleased work stays on integration.
    assert_eq!(
        status.integration_sha.as_deref(),
        Some(integration_before.as_str())
    );
    assert!(status.ahead_by > 0);
}

#[tokio::test]
async fn sync_fast_forwards_external_commits_when_nothing_unreleased() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    core.link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    // Ship the seed so integration == origin main.
    core.origin_release("acme", "api").await.unwrap();
    assert_invariants(&core);
    rig.origin
        .merge_branch_to_main("chug/release-1", false)
        .await;
    core.origin_sync("acme", "api").await.unwrap();
    assert_invariants(&core);

    // A human pushes to GitHub main directly.
    rig.origin
        .commit_to_main("docs/human.md", b"external", "human commit")
        .await;
    let status = core.origin_sync("acme", "api").await.unwrap();
    assert_invariants(&core);
    assert_eq!(
        status.integration_sha.as_deref(),
        Some(rig.origin.main_sha().await.as_str()),
        "integration fast-forwarded onto the external commit"
    );
}

#[tokio::test]
async fn restart_restores_hold_for_open_release() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    let mut record = core
        .link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    rig.pretend_github(&mut record).await;
    commit_to_integration(&rig, &[("src/feature.py", "print('job 1')")]).await;
    core.origin_release("acme", "api").await.unwrap();
    assert_invariants(&core);
    drop(core);

    // A fresh Core (restart) must come up held.
    let mut core = rig.core().await;
    let status = core.origin_status("acme", "api").await.unwrap();
    assert_invariants(&core);
    assert!(
        status.held,
        "hold restored from the project record at startup"
    );
}

const QUICK_YAML: &str = r#"
name: quick
image: img:latest
work:
  type: agent
  prompt: prompts/quick.md
"#;

/// End-to-end: a job finishing eval during an Open release stays queued
/// (integration unmoved); after the PR squash-merges and sync runs, the held
/// job lands on the reset integration.
#[tokio::test]
// TODO(style): oversized tier-2 test — split when this file is next touched.
#[allow(clippy::too_many_lines)]
async fn held_job_lands_after_merged_release_sync() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    let mut record = core
        .link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    rig.pretend_github(&mut record).await;
    commit_to_integration(
        &rig,
        &[
            ("jobs/quick.yaml", QUICK_YAML),
            ("prompts/quick.md", "do the thing"),
            ("src/feature.py", "print('job 0')"),
        ],
    )
    .await;

    // Fake agent: commit to the job branch.
    let bare = rig.repos().repo_path("acme", "api");
    rig.provider.on_run(move |cfg| {
        let bare = bare.clone();
        async move {
            let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
            let clone = clone_branch_from(&bare, &branch).await;
            clone
                .commit_file("src/job_work.py", b"print('job work')", "implement")
                .await;
            clone.push(&branch).await;
        }
    });

    let (handle, sink) = spawn_checked(core);
    handle.origin_release("acme", "api").await.unwrap();
    assert_invariants_of(&sink);
    let release = handle.origin_status("acme", "api").await.unwrap();
    assert_invariants_of(&sink);
    assert!(release.held);
    let integration_at_release = release.integration_sha.clone().unwrap();

    // Run a job to eval-pass while the release is open.
    let job = handle
        .create_job(CreateSpec {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "quick".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            knowledge_tags: vec![],
            eval: vec![],
            timeout: None,
            model: None,
            factory: None,
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

    // The job passes evaluation and enters WrapUp but cannot land: it parks in
    // the merge queue behind the release hold and integration does not move.
    let jobs = rig.store.jobs().await.unwrap();
    // Watch until WrapUp; the Done guard fires inside the check so a wrong
    // landing panics loudly (#206 principle 3).
    test_utils::wait::job_where(
        &rig.store,
        "acme",
        "api",
        job.id,
        format!("job {} to reach WrapUp (held in merge queue)", job.id),
        |j| {
            assert_ne!(j.state, JobState::Done, "must not land while held");
            j.state == JobState::WrapUp
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await; // let any wrong landing surface
    let j = jobs.get("acme", "api", job.id).await.unwrap().unwrap();
    assert_eq!(j.state, JobState::WrapUp, "held in the merge queue");
    assert_eq!(
        rig.repos()
            .resolve_ref("acme", "api", "integration")
            .await
            .unwrap(),
        integration_at_release,
        "integration unmoved during the hold"
    );

    // GitHub squash-merges; sync resets integration and pumps the queue.
    rig.origin
        .merge_branch_to_main("chug/release-1", true)
        .await;
    rig.pr.script("closed", true);
    handle.origin_sync("acme", "api").await.unwrap();
    assert_invariants_of(&sink);

    test_utils::wait::job_state(&rig.store, "acme", "api", job.id, JobState::Done).await;
    let j = jobs.get("acme", "api", job.id).await.unwrap().unwrap();
    assert_eq!(j.state, JobState::Done, "held job landed after sync");
    // The job's work is on integration, on top of the reset base.
    let repos = rig.repos();
    assert!(
        repos
            .read_file_at("acme", "api", "integration", "src/job_work.py")
            .await
            .unwrap()
            .is_some(),
        "job change landed on integration"
    );
    let origin_sha = repos.origin_main_sha("acme", "api").await.unwrap();
    assert_eq!(
        repos
            .count_commits_beyond("acme", "api", &origin_sha, "integration")
            .await
            .unwrap(),
        1,
        "exactly the job's squash commit sits above the new origin main"
    );
    assert_invariants_of(&sink);
}

/// Declared `CHUG_*` secrets **and vars** fail release validation, and the
/// injection path never surfaces them in a container env. Vars joined the rule
/// with design #311 Decision 4: `CHUG_` is the input namespace's prefix too, and
/// an unchecked var could shadow an origin credential or a §6.3 origin stamp.
#[tokio::test]
async fn reserved_chug_secrets_never_reach_containers() {
    let Some(rig) = rig().await else { return };
    let mut core = rig.core().await;
    core.link_project("acme", "api", &rig.origin.url(), None)
        .await
        .unwrap();
    assert_invariants(&core);
    commit_to_integration(&rig, &[
        (
            "jobs/sneaky.yaml",
            "name: sneaky\nimage: img:latest\nwork:\n  type: agent\n  prompt: prompts/quick.md\n  secrets: [CHUG_ORIGIN_PAT]\nvars: [CHUG_PHASE]\n",
        ),
        ("prompts/quick.md", "do the thing"),
    ])
    .await;
    rig.store
        .raw_bucket(store::buckets::SECRETS)
        .await
        .unwrap()
        .put_json(&format!("acme.api.{SECRET_PAT}"), &"pat-token")
        .await
        .unwrap();

    let (handle, sink) = spawn_checked(core);
    let job = handle
        .create_job(CreateSpec {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "sneaky".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            knowledge_tags: vec![],
            eval: vec![],
            timeout: None,
            model: None,
            factory: None,
            members: vec![],
            inputs: Default::default(),
            groups: vec![],
            draft: false,
        })
        .await
        .unwrap();
    assert_invariants_of(&sink);
    let err = handle.release_job("acme", "api", job.id).await.unwrap_err();
    assert_invariants_of(&sink);
    let CoreError::Validation(errs) = err else {
        panic!("expected validation failure, got {err}");
    };
    let rendered = format!("{errs:?}");
    assert!(rendered.contains("reserved"), "{errs:?}");
    assert!(
        errs.iter().any(|e| e.field == "secrets"),
        "the declared secret is refused: {errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.field == "vars"),
        "the declared var is refused too (#311): {errs:?}"
    );
    assert_invariants_of(&sink);
}

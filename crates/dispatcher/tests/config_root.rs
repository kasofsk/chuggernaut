//! Tier-2 (real git, no NATS): the `.chug/` config root (spec §1.1).
//!
//! Job types, the project defaults overlay and knowledge tags live under
//! `.chug/` in the project repo. Repos — and `base_ref`s — that predate the
//! config root keep their files at the repo root, so every read falls back
//! there; these pin both halves, and that `.chug/` wins when both exist.
//!
//! The other dispatcher suites still seed the repo-root layout, which is what
//! keeps the fallback covered end to end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::release;
use test_utils::repo::TempRepo;

const BUILD_YAML: &str = r"
name: build
image: img:latest
work:
  type: command
  run: ./build.sh
";

/// A second type, so a test can tell WHICH file a load resolved to.
const BUILD_YAML_SHADOWED: &str = r"
name: build
image: stale:latest
work:
  type: command
  run: ./stale.sh
";

const DEFAULTS_YAML: &str = r"
eval:
  - name: ci
    type: command
    run: ./ci.sh
";

/// Commit `files` onto `main` and return the resolved HEAD.
async fn seed(repo: &TempRepo, files: &[(&str, &str)]) -> String {
    let clone = repo.clone_branch("main").await;
    for (path, contents) in files {
        clone.commit_file(path, contents.as_bytes(), "seed").await;
    }
    clone.push("main").await;
    repo.head().await
}

#[tokio::test]
async fn job_type_and_defaults_load_from_the_config_root() {
    let repo = TempRepo::create("acme", "api").await;
    let head = seed(
        &repo,
        &[
            (".chug/jobs/build.yaml", BUILD_YAML),
            (".chug/jobs/_defaults.yaml", DEFAULTS_YAML),
        ],
    )
    .await;

    let jt = release::load_job_type(&repo.manager, "acme", "api", &head, "build", None)
        .await
        .unwrap();
    assert_eq!(jt.image.as_deref(), Some("img:latest"));
    assert_eq!(jt.eval.len(), 1);
    assert_eq!(jt.eval[0].name, "ci");
}

/// A ref that predates the config root still resolves: the whole point of the
/// fallback is that pinned `base_ref`s and unmigrated projects keep working.
#[tokio::test]
async fn job_type_falls_back_to_the_repo_root_layout() {
    let repo = TempRepo::create("acme", "api").await;
    let head = seed(
        &repo,
        &[
            ("jobs/build.yaml", BUILD_YAML),
            ("jobs/_defaults.yaml", DEFAULTS_YAML),
        ],
    )
    .await;

    let jt = release::load_job_type(&repo.manager, "acme", "api", &head, "build", None)
        .await
        .unwrap();
    assert_eq!(jt.image.as_deref(), Some("img:latest"));
    assert_eq!(jt.eval.len(), 1);
}

/// Negative space: a half-migrated repo resolves to exactly one file, and it is
/// the one under `.chug/` — never a merge of the two.
#[tokio::test]
async fn the_config_root_shadows_the_repo_root() {
    let repo = TempRepo::create("acme", "api").await;
    let head = seed(
        &repo,
        &[
            ("jobs/build.yaml", BUILD_YAML_SHADOWED),
            (".chug/jobs/build.yaml", BUILD_YAML),
        ],
    )
    .await;

    let jt = release::load_job_type(&repo.manager, "acme", "api", &head, "build", None)
        .await
        .unwrap();
    assert_eq!(jt.image.as_deref(), Some("img:latest"));
}

/// A type that exists at neither location is a validation error naming the
/// canonical path — the place the operator should create it.
#[tokio::test]
async fn a_missing_job_type_names_the_config_root_path() {
    let repo = TempRepo::create("acme", "api").await;
    let head = seed(&repo, &[(".chug/jobs/build.yaml", BUILD_YAML)]).await;

    let errors = release::load_job_type(&repo.manager, "acme", "api", &head, "absent", None)
        .await
        .unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains(".chug/jobs/absent.yaml")),
        "expected the canonical path in {errors:?}"
    );
}

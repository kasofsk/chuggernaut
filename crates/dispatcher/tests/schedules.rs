//! Tier-2 tests for **scheduled jobs** (spec §1.1 schedules, design #310): the
//! loader against a real repo, and the scan shell against a real `Core`.
//!
//! The anchor rule, coalescing and the skip rule are pinned pure at tier 1
//! (`chuggernaut_domain::decide::schedule`). What needs this tier is everything
//! around the decider: that `.chug/schedules/*.yaml` at default-branch HEAD
//! loads (and that an invalid file is skipped rather than wedging the scan),
//! that a fire really creates and releases a job carrying its provenance, and
//! that the `schedule-*` events land on the jobs the design names.
//!
//! The loader tests need git only; the scan tests need NATS and self-skip
//! without it (`testing.md`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::{Duration, Utc};
use dispatcher::core::{Core, CoreConfig};
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{Job, JobState};

mod common;
use common::{assert_invariants_of, spawn_checked};

const CODE_YAML: &str = r"
name: code
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
";

const COMMAND_YAML: &str = r"
name: sweep
image: img:latest
work:
  type: command
  run: ./sweep.sh
";

/// The scan tests fire *this* type: a human work task parks and waits, so the
/// job the schedule created stays live for the assertions that follow instead
/// of racing a fake agent to Done.
const MANUAL_YAML: &str = r"
name: manual
work:
  type: human
  prompt: prompts/manual.md
";

/// A parameterized target: one required input the schedule supplies, one
/// optional input whose declared default the Ready transition materializes.
const PARAMETERIZED_YAML: &str = r"
name: parameterized
min_dispatcher: 2
work:
  type: human
  prompt: prompts/manual.md
inputs:
  - name: sha
    type: string
    required: true
    pattern: '^[0-9a-f]{7,40}$'
  - name: service
    type: enum
    values: [web, worker]
    default: web
";

const NIGHTLY: &str = r#"
name: nightly
job_type: code
cron: "0 2 * * *"
title: Nightly integration
description: Run the nightly integration suite.
"#;

/// Every minute, so a seeded anchor in the past always has an occurrence behind
/// it — a schedule test must not wait for the clock.
const EVERY_MINUTE: &str = r#"
name: nightly
job_type: manual
cron: "* * * * *"
description: Run it.
"#;

/// The same occurrence cadence against the parameterized target, supplying the
/// required input and leaving the optional one to its declared default.
const EVERY_MINUTE_WITH_INPUTS: &str = r#"
name: nightly
job_type: parameterized
cron: "* * * * *"
description: Run it.
min_dispatcher: 3
inputs:
  sha: 4f9c1ab
"#;

async fn repo_with(files: &[(&str, &str)]) -> TempRepo {
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in [
        (".chug/jobs/code.yaml", CODE_YAML),
        (".chug/jobs/sweep.yaml", COMMAND_YAML),
        (".chug/jobs/manual.yaml", MANUAL_YAML),
        (".chug/jobs/parameterized.yaml", PARAMETERIZED_YAML),
        (".chug/prompts/impl.md", "implement it"),
        (".chug/prompts/manual.md", "do it by hand"),
    ]
    .iter()
    .chain(files.iter())
    {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;
    repo
}

fn repos_root(repo: &TempRepo) -> std::path::PathBuf {
    repo.bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The valid schedules of a repo, by name.
async fn loaded_names(repo: &TempRepo) -> Vec<String> {
    dispatcher::schedules::load(&repo.manager, "acme", "api")
        .await
        .into_iter()
        .map(|s| s.name)
        .collect()
}

#[tokio::test]
async fn a_valid_schedule_loads_from_the_config_root() {
    let repo = repo_with(&[(".chug/schedules/nightly.yaml", NIGHTLY)]).await;
    let loaded = dispatcher::schedules::load(&repo.manager, "acme", "api").await;
    assert_eq!(loaded.len(), 1, "{loaded:?}");
    assert_eq!(loaded[0].name, "nightly");
    assert_eq!(loaded[0].job_title(), "Nightly integration");
    assert!(loaded[0].enabled);
}

/// The pre-`.chug` layout resolves too, exactly like a job type's.
#[tokio::test]
async fn a_schedule_falls_back_to_the_repo_root_layout() {
    let repo = repo_with(&[("schedules/nightly.yaml", NIGHTLY)]).await;
    assert_eq!(loaded_names(&repo).await, vec!["nightly".to_string()]);
}

/// Every reload-time rejection design #310 Decision 2 names, each beside a
/// valid file: an invalid schedule is skipped, and the rest still load.
#[tokio::test]
async fn an_invalid_schedule_is_skipped_and_never_wedges_the_scan() {
    for (name, yaml) in [
        ("unparseable", "name: [\n"),
        ("bad-cron", "name: bad-cron\njob_type: code\ncron: 'nope'\n"),
        (
            "stem-mismatch",
            "name: other\njob_type: code\ncron: '0 2 * * *'\ndescription: x\n",
        ),
        (
            "missing-type",
            "name: missing-type\njob_type: absent\ncron: '0 2 * * *'\ndescription: x\n",
        ),
        (
            "agent-no-description",
            "name: agent-no-description\njob_type: code\ncron: '0 2 * * *'\n",
        ),
        (
            "skewed",
            "name: skewed\njob_type: code\ncron: '0 2 * * *'\ndescription: x\nmin_dispatcher: 999\n",
        ),
        (
            "undeclared-input",
            "name: undeclared-input\njob_type: parameterized\ncron: '0 2 * * *'\n\
             min_dispatcher: 3\ninputs:\n  region: eu\n",
        ),
        (
            "ungated-inputs",
            "name: ungated-inputs\njob_type: parameterized\ncron: '0 2 * * *'\n\
             inputs:\n  sha: 4f9c1ab\n",
        ),
    ] {
        let repo = repo_with(&[
            (".chug/schedules/nightly.yaml", NIGHTLY),
            (&format!(".chug/schedules/{name}.yaml"), yaml),
        ])
        .await;
        assert_eq!(
            loaded_names(&repo).await,
            vec!["nightly".to_string()],
            "'{name}' should be skipped, and 'nightly' should still load"
        );
    }
}

/// A command target needs no `description` — the rule is agent-only — and a
/// disabled schedule still loads (the decider is what declines to fire it).
#[tokio::test]
async fn a_command_target_needs_no_description_and_disabled_still_loads() {
    let repo = repo_with(&[
        (
            ".chug/schedules/sweep.yaml",
            "name: sweep\njob_type: sweep\ncron: '0 5 * * *'\n",
        ),
        (
            ".chug/schedules/off.yaml",
            "name: off\njob_type: sweep\ncron: '0 5 * * *'\nenabled: false\n",
        ),
    ])
    .await;
    assert_eq!(
        loaded_names(&repo).await,
        vec!["off".to_string(), "sweep".to_string()]
    );
}

struct Rig {
    _server: &'static test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
}

async fn rig(schedule: &str) -> Option<Rig> {
    let server = test_utils::nats::NatsTestServer::shared().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = repo_with(&[(".chug/schedules/nightly.yaml", schedule)]).await;
    Some(Rig {
        _server: server,
        store,
        repo,
    })
}

/// Seed the job a schedule's provenance points at, as if a prior dispatcher had
/// created it — the anchor every scan test starts from.
async fn seed_prior_job(rig: &Rig, state: JobState, age: Duration, completed: bool) -> u64 {
    let seq = rig
        .store
        .counters()
        .await
        .unwrap()
        .next("acme", "api")
        .await
        .unwrap();
    let created_at = Utc::now() - age;
    let job = Job {
        r#type: "code".into(),
        state,
        schedule: Some("nightly".into()),
        created_at,
        completed_at: completed.then_some(created_at + Duration::minutes(20)),
        ..test_utils::fixture::job("acme/api", seq)
    };
    rig.store.jobs().await.unwrap().put(&job).await.unwrap();
    seq
}

async fn core_of(rig: &Rig) -> Core {
    Core::new(
        rig.store.clone(),
        vcs::RepoManager::new(repos_root(&rig.repo)),
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

async fn all_jobs(rig: &Rig) -> Vec<Job> {
    let mut jobs = rig.store.jobs().await.unwrap().list_all().await.unwrap();
    jobs.sort_by_key(|j| j.id);
    jobs
}

/// Every `job-events` payload of one job, in stream order.
async fn job_events(rig: &Rig, seq: u64) -> Vec<serde_json::Value> {
    rig.store
        .read_stream("job-events", 200)
        .await
        .unwrap()
        .iter()
        .map(|payload| serde_json::from_slice::<serde_json::Value>(payload).unwrap())
        .filter(|v| v["job_seq"] == serde_json::json!(seq))
        .collect()
}

fn event_types(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .map(|v| v["event_type"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The acceptance case, and the restart half of design #310 Decision 5: a
/// dispatcher that was down across many occurrences comes up, fires **once**,
/// and the job it creates carries `schedule` provenance, is released, and is
/// announced by `schedule-fired` plus a `job-created` naming the schedule.
#[tokio::test]
async fn a_due_schedule_fires_once_with_provenance_and_its_event() {
    let Some(rig) = rig(EVERY_MINUTE).await else {
        return;
    };
    let prior = seed_prior_job(&rig, JobState::Done, Duration::hours(3), true).await;

    let (handle, sink) = spawn_checked(core_of(&rig).await);
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);

    let jobs = all_jobs(&rig).await;
    assert_eq!(jobs.len(), 2, "one job, not 180: {jobs:?}");
    let fired = jobs.last().unwrap();
    assert_ne!(fired.id, prior);
    assert_eq!(fired.schedule.as_deref(), Some("nightly"));
    assert_eq!(fired.r#type, "manual");
    assert_eq!(fired.description, "Run it.");
    assert_ne!(fired.state, JobState::Frozen, "auto-released");
    assert!(!fired.state.is_terminal());
    assert!(fired.base_ref.is_some(), "the release pinned a base_ref");

    let events = job_events(&rig, fired.id).await;
    let types = event_types(&events);
    for expected in ["job-created", "schedule-fired", "job-released"] {
        assert!(types.contains(&expected.to_string()), "{types:?}");
    }
    let created = events
        .iter()
        .find(|v| v["event_type"] == serde_json::json!("job-created"))
        .unwrap();
    assert_eq!(created["schedule"], serde_json::json!("nightly"));
    let fired_event = events
        .iter()
        .find(|v| v["event_type"] == serde_json::json!("schedule-fired"))
        .unwrap();
    assert_eq!(fired_event["schedule"], serde_json::json!("nightly"));
    assert!(fired_event["occurrence_at"].is_string(), "{fired_event}");

    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    assert_eq!(
        all_jobs(&rig).await.len(),
        2,
        "the job it just created blocks the next occurrence"
    );
}

/// Design #311 slice C: the schedule's `inputs:` reach the job record as its
/// supplied set, the target's declared default materializes **once** at the
/// Ready transition, and the two events carry the two sets §10.3 names.
#[tokio::test]
async fn a_fired_schedule_carries_its_inputs_onto_the_job_record() {
    let Some(rig) = rig(EVERY_MINUTE_WITH_INPUTS).await else {
        return;
    };
    seed_prior_job(&rig, JobState::Done, Duration::hours(3), true).await;

    let (handle, sink) = spawn_checked(core_of(&rig).await);
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);

    let jobs = all_jobs(&rig).await;
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    let fired = jobs.last().unwrap();
    assert_eq!(fired.r#type, "parameterized");
    assert_eq!(
        fired.inputs,
        std::collections::BTreeMap::from([
            ("service".to_string(), "web".to_string()),
            ("sha".to_string(), "4f9c1ab".to_string()),
        ]),
        "the schedule's value plus the declared default, materialized once"
    );
    assert!(fired.base_ref.is_some(), "the release pinned a base_ref");

    let events = job_events(&rig, fired.id).await;
    let of = |name: &str| {
        events
            .iter()
            .find(|v| v["event_type"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("no {name} in {:?}", event_types(&events)))
            .clone()
    };
    assert_eq!(
        of("job-created")["inputs"],
        serde_json::json!({ "sha": "4f9c1ab" }),
        "job-created carries what the schedule supplied"
    );
    assert_eq!(
        of("job-released")["inputs"],
        serde_json::json!({ "service": "web", "sha": "4f9c1ab" }),
        "the release event carries the effective set"
    );
}

/// The golden backstop: a schedule with no `inputs:` produces the job it
/// produced before the field existed — no map on the record, and no `inputs`
/// key on any of its events.
#[tokio::test]
async fn a_schedule_without_inputs_fires_a_job_identical_to_today_s() {
    let Some(rig) = rig(EVERY_MINUTE).await else {
        return;
    };
    seed_prior_job(&rig, JobState::Done, Duration::hours(3), true).await;

    let (handle, sink) = spawn_checked(core_of(&rig).await);
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);

    let jobs = all_jobs(&rig).await;
    let fired = jobs.last().unwrap();
    assert!(fired.inputs.is_empty(), "{:?}", fired.inputs);
    for event in job_events(&rig, fired.id).await {
        assert!(
            event.get("inputs").is_none(),
            "an input-free job stamps nothing: {event}"
        );
    }
}

/// Design #310 Decision 4: an occurrence that comes due while a prior run is
/// non-terminal is skipped — no job, one `schedule-skipped` on the **blocking**
/// job — and reported once rather than once per tick.
#[tokio::test]
async fn an_overlapping_occurrence_is_skipped_once_on_the_blocking_job() {
    let Some(rig) = rig(EVERY_MINUTE).await else {
        return;
    };
    let blocking = seed_prior_job(&rig, JobState::Frozen, Duration::hours(3), false).await;

    let (handle, sink) = spawn_checked(core_of(&rig).await);
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);

    assert_eq!(all_jobs(&rig).await.len(), 1, "nothing new is created");
    let skips: Vec<serde_json::Value> = job_events(&rig, blocking)
        .await
        .into_iter()
        .filter(|v| v["event_type"] == serde_json::json!("schedule-skipped"))
        .collect();
    assert_eq!(skips.len(), 1, "{skips:?}");
    assert_eq!(skips[0]["schedule"], serde_json::json!("nightly"));
    assert!(skips[0]["occurrence_at"].is_string());

    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    let skips: Vec<serde_json::Value> = job_events(&rig, blocking)
        .await
        .into_iter()
        .filter(|v| v["event_type"] == serde_json::json!("schedule-skipped"))
        .collect();
    assert!(
        skips.len() <= 1,
        "one event per occurrence, not one per tick: {skips:?}"
    );
    assert_eq!(all_jobs(&rig).await.len(), 1);
}

/// A schedule whose file is invalid at HEAD never fires, and the scan that read
/// it still completes — the "logged, not fatal" half of Decision 2.
#[tokio::test]
async fn an_invalid_schedule_file_fires_nothing() {
    let Some(rig) = rig("name: nightly\njob_type: code\ncron: 'every minute'\n").await else {
        return;
    };
    seed_prior_job(&rig, JobState::Done, Duration::hours(3), true).await;

    let (handle, sink) = spawn_checked(core_of(&rig).await);
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    assert_eq!(all_jobs(&rig).await.len(), 1);
}

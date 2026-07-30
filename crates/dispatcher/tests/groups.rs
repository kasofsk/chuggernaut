//! Tier-2 tests for job **groups** (spec §1.1 `groups:`, §6.2; design #321
//! slice A): the three write paths into `Job.groups`, the mutate-anywhere verb
//! that is accepted on a terminal job, and — the load-bearing one — the assert
//! that a group changes **nothing** about what a job runs.
//!
//! The shape rules themselves are pinned pure in `types::groups`, and the job
//! brief's half of the inertness property in `dispatcher::exec`
//! (`groups_never_reach_the_job_brief`). What needs this tier is everything the
//! single-writer actor decides: that the verb has no state guard, that the
//! record and the `job-updated` event agree, and that two jobs differing only in
//! their groups launch containers with the same environment. The last one needs
//! a real launch, which is what makes it tier 2 rather than tier 1
//! (`testing.md`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreError, CreateSpec, UpdateJobRequest};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{Job, JobState};

mod common;
use common::{assert_invariants, assert_invariants_of, spawn_checked};

/// An ordinary agent job with a command evaluator, so one run exercises both
/// container kinds a group would have to stay out of.
const CODE: &str = r"
name: code
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: ci
    type: command
    run: ./ci.sh
";

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
        ("jobs/code.yaml", CODE),
        ("prompts/impl.md", "implement it"),
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

fn req(groups: &[&str]) -> CreateSpec {
    CreateSpec {
        owner: "acme".into(),
        project: "api".into(),
        r#type: "code".into(),
        title: "Ship the thing".into(),
        description: "Do the work described here.".into(),
        cover_html: None,
        deps: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        factory: None,
        members: vec![],
        inputs: BTreeMap::new(),
        groups: names(groups),
        draft: false,
    }
}

fn names(groups: &[&str]) -> Vec<String> {
    groups.iter().map(|g| (*g).to_string()).collect()
}

async fn stored(store: &NatsStore, seq: u64) -> Job {
    store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", seq)
        .await
        .unwrap()
        .unwrap()
}

/// Every `job-events` payload for one job, in stream order.
async fn job_events(store: &NatsStore, seq: u64) -> Vec<serde_json::Value> {
    store
        .read_stream("job-events", 200)
        .await
        .unwrap()
        .iter()
        .map(|payload| serde_json::from_slice::<serde_json::Value>(payload).unwrap())
        .filter(|v| v["job_seq"] == serde_json::json!(seq))
        .collect()
}

/// Just the job's `job-updated` events, which is what every group edit
/// announces (`{"fields": ["groups"]}` — the existing payload shape).
async fn update_events(store: &NatsStore, seq: u64) -> Vec<serde_json::Value> {
    job_events(store, seq)
        .await
        .into_iter()
        .filter(|v| v["event_type"] == serde_json::json!("job-updated"))
        .collect()
}

/// The work agent commits on the job branch, so the job can actually land.
fn commit_on_work(rig: &Rig) {
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let work = clone_branch_from(&bare, &branch).await;
        // Branch-derived content so the commit always diffs: this test runs two
        // jobs in sequence and the first one's merge puts the stub on the base.
        let body = format!("// work produced on {branch}\n");
        work.commit_file("src/work.rs", body.as_bytes(), "work")
            .await;
        work.push(&branch).await;
    });
}

/// The env keys that legitimately differ between two *different* jobs: the
/// job's own identity, the task's, and the per-launch credentials minted from
/// them. Everything else is the environment the job type composes, and that is
/// what must not move.
const PER_JOB_ENV_KEYS: &[&str] = &[
    "JOB_ID",
    "JOB_BRANCH",
    "CHUG_TASK_ID",
    "JOB_TASK_ID",
    "NATS_CREDS",
];

fn comparable_env(env: &HashMap<String, String>) -> BTreeMap<&str, &str> {
    env.iter()
        .filter(|(k, _)| !PER_JOB_ENV_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// **The golden trace** (#321's contracts table): a job created ungrouped is
/// annotated after it is **Done**, the record shows it, and the event says
/// exactly which field moved.
///
/// This is the acceptance case the whole design turns on (Decision 5): every job
/// of the group that motivated #321 is already finished, so a model that can
/// only group at creation cannot express it. There is no state guard on the
/// verb, and that is deliberate — what "terminal jobs are immutable" protects is
/// the execution record, which `groups` is inert to.
#[tokio::test]
async fn a_done_job_can_be_grouped_and_the_record_shows_it() {
    let Some(rig) = rig().await else { return };
    let store = rig.store.clone();
    commit_on_work(&rig);

    let (handle, sink) = spawn_checked(rig.core);
    let job = handle.create_job(req(&[])).await.unwrap();
    assert!(job.groups.is_empty(), "created ungrouped");
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);
    test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Done).await;

    // The mutation the rest of the record forbids: a write to a terminal job.
    let updated = handle
        .edit_groups(
            "acme",
            "api",
            job.id,
            names(&["design/321-job-groups"]),
            vec![],
        )
        .await
        .unwrap();
    assert_invariants_of(&sink);
    assert_eq!(updated.groups, names(&["design/321-job-groups"]));
    assert_eq!(updated.state, JobState::Done, "the state did not move");
    let record = stored(&store, job.id).await;
    assert_eq!(record.groups, names(&["design/321-job-groups"]));
    assert_eq!(record.state, JobState::Done);

    // …announced as `job-updated {fields:["groups"]}` — the existing shape.
    let updates = update_events(&store, job.id).await;
    assert_eq!(
        updates.len(),
        1,
        "exactly one update announced: {updates:?}"
    );
    assert_eq!(updates[0]["fields"], serde_json::json!(["groups"]));
    assert_invariants_of(&sink);
}

/// The verb is **add/remove, not replace** (Decision 5), and it is idempotent:
/// two operators grouping one job from two tabs both succeed, where a whole-list
/// `PUT` would lose one. A request that changes nothing writes nothing and
/// announces nothing, so a retry cannot produce an event nobody can act on.
#[tokio::test]
async fn group_edits_are_add_remove_and_idempotent() {
    let Some(mut rig) = rig().await else { return };
    let job = rig.core.create_job(req(&[])).await.unwrap();

    let first = rig
        .core
        .edit_groups(
            "acme",
            "api",
            job.id,
            names(&["design/321-job-groups"]),
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(first.groups, names(&["design/321-job-groups"]));

    // The second operator's label lands beside the first, not over it.
    let both = rig
        .core
        .edit_groups("acme", "api", job.id, names(&["beacon-import"]), vec![])
        .await
        .unwrap();
    assert_eq!(
        both.groups,
        names(&["design/321-job-groups", "beacon-import"])
    );
    assert_invariants(&rig.core);
    assert_eq!(update_events(&rig.store, job.id).await.len(), 2);

    // Re-adding a label the job already carries changes nothing, and so does
    // removing one it never had.
    for (add, remove) in [
        (names(&["beacon-import"]), vec![]),
        (vec![], names(&["never-set"])),
    ] {
        let noop = rig
            .core
            .edit_groups("acme", "api", job.id, add, remove)
            .await
            .unwrap();
        assert_eq!(noop.groups, both.groups);
    }
    assert_eq!(
        update_events(&rig.store, job.id).await.len(),
        2,
        "a no-op published nothing"
    );

    // Removal is the same verb, and a name on both sides survives: removes
    // apply first, so an add always wins.
    let pruned = rig
        .core
        .edit_groups(
            "acme",
            "api",
            job.id,
            names(&["beacon-import"]),
            names(&["design/321-job-groups", "beacon-import"]),
        )
        .await
        .unwrap();
    assert_eq!(pruned.groups, names(&["beacon-import"]));
    assert_eq!(
        stored(&rig.store, job.id).await.groups,
        names(&["beacon-import"])
    );
    assert_invariants(&rig.core);
}

/// A revoked job is annotatable too — the other terminal state, and the one an
/// operator most wants to file under "what happened to that design".
#[tokio::test]
async fn a_revoked_job_can_be_grouped() {
    let Some(mut rig) = rig().await else { return };
    let job = rig.core.create_job(req(&[])).await.unwrap();
    rig.core.revoke_job("acme", "api", job.id).await.unwrap();
    assert_invariants(&rig.core);
    assert_eq!(
        stored(&rig.store, job.id).await.state,
        JobState::Revoked,
        "the precondition of this test"
    );

    let updated = rig
        .core
        .edit_groups("acme", "api", job.id, names(&["beacon-import"]), vec![])
        .await
        .unwrap();
    assert_invariants(&rig.core);
    assert_eq!(updated.groups, names(&["beacon-import"]));
    assert_eq!(updated.state, JobState::Revoked);
}

/// **The inertness assert**, container half (design #321 Decision 3, STYLE.md
/// Tier 2 #2 — negative space): two jobs of one type, identical but for their
/// groups, launch a work agent and an evaluator whose environment and prompt are
/// the same. No code path that composes a container's environment, prompt or
/// resolved config may read `Job.groups`, and this is what makes that a property
/// rather than an intention — it is also the whole reason mutating a terminal
/// job's record is safe.
///
/// The job-identity keys ([`PER_JOB_ENV_KEYS`]) are excluded because these are
/// two different jobs; every other key is compared by value, and the group names
/// are searched for across the whole env in both directions.
#[tokio::test]
async fn groups_never_reach_the_container_env() {
    let Some(rig) = rig().await else { return };
    let (store, backend, provider) = (rig.store.clone(), rig.backend.clone(), rig.provider.clone());
    commit_on_work(&rig);

    let (handle, sink) = spawn_checked(rig.core);
    let plain = handle.create_job(req(&[])).await.unwrap();
    handle.release_job("acme", "api", plain.id).await.unwrap();
    test_utils::wait::job_state(&store, "acme", "api", plain.id, JobState::Done).await;

    let grouped = handle
        .create_job(req(&["design/321-job-groups", "beacon-import"]))
        .await
        .unwrap();
    assert_eq!(
        grouped.groups,
        names(&["design/321-job-groups", "beacon-import"]),
        "the create path persisted them, or this test proves nothing"
    );
    handle.release_job("acme", "api", grouped.id).await.unwrap();
    test_utils::wait::job_state(&store, "acme", "api", grouped.id, JobState::Done).await;
    assert_invariants_of(&sink);

    // 1. The work agent: same env, and the same prompt — the §4.3 brief is
    //    composed from the record, so a leak would land there first.
    let runs = provider.runs();
    assert_eq!(runs.len(), 2, "both jobs ran a work agent");
    assert_eq!(
        comparable_env(&runs[0].env),
        comparable_env(&runs[1].env),
        "a group changed the work container's environment"
    );
    assert_eq!(
        runs[0].prompt, runs[1].prompt,
        "a group changed the work agent's prompt"
    );
    assert_eq!(
        runs[0].system_prompt, runs[1].system_prompt,
        "a group changed the work agent's system prompt"
    );

    // 2. The evaluator, which is the same choke point (`Exec::container_env`).
    let evals: Vec<_> = backend
        .launches()
        .into_iter()
        .filter(|c| c.cmd.iter().any(|arg| arg.contains("./ci.sh")))
        .collect();
    assert_eq!(evals.len(), 2, "both jobs ran the ci evaluator");
    assert_eq!(
        comparable_env(&evals[0].env),
        comparable_env(&evals[1].env),
        "a group changed the eval container's environment"
    );

    // 3. …and no group name appears anywhere in either, key or value. The
    //    comparison above already implies it; this is what a reader checks.
    for env in [&runs[1].env, &evals[1].env] {
        assert!(
            !env.iter()
                .any(|(k, v)| k.contains("321-job-groups") || v.contains("321-job-groups")),
            "a group name reached a container: {env:?}"
        );
    }
    assert_invariants_of(&sink);
}

/// The Draft edit is a full-field replace, like every other field it carries —
/// and it names `groups` in the `job-updated` event when it moves one.
#[tokio::test]
async fn the_draft_edit_replaces_groups_and_names_the_change() {
    let Some(mut rig) = rig().await else { return };
    let draft = rig
        .core
        .create_job(CreateSpec {
            draft: true,
            ..req(&["beacon-import"])
        })
        .await
        .unwrap();
    assert_eq!(draft.state, JobState::Draft);
    assert_invariants(&rig.core);

    let update = |groups: &[&str]| UpdateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        seq: draft.id,
        r#type: "code".into(),
        title: draft.title.clone(),
        description: draft.description.clone(),
        cover_html: None,
        deps: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        inputs: BTreeMap::new(),
        groups: names(groups),
    };

    // A replace, not a merge: the old label is gone.
    let edited = rig
        .core
        .update_job(update(&["design/321-job-groups"]))
        .await;
    assert_eq!(
        edited.unwrap().groups,
        names(&["design/321-job-groups"]),
        "the Draft edit replaces the list wholesale"
    );
    assert_invariants(&rig.core);
    let updates = update_events(&rig.store, draft.id).await;
    assert_eq!(
        updates.first().expect("the edit announced itself")["fields"],
        serde_json::json!(["groups"])
    );

    // An edit that leaves groups alone does not name the field.
    rig.core
        .update_job(update(&["design/321-job-groups"]))
        .await
        .unwrap();
    let updates = update_events(&rig.store, draft.id).await;
    assert_eq!(
        updates.last().expect("both edits announced")["fields"],
        serde_json::json!([])
    );
}

/// Every §1.1 bound is decided by the actor as a 422 (`CoreError::Validation`)
/// naming the `groups` field and the rule that broke — count, shape, length and
/// duplicates alike — and the record is left exactly as it was.
#[tokio::test]
async fn a_bad_group_is_refused_by_the_rule_it_broke() {
    let Some(mut rig) = rig().await else { return };
    let job = rig.core.create_job(req(&["beacon-import"])).await.unwrap();
    assert_invariants(&rig.core);

    let cases = [
        (names(&["NOT A GROUP"]), "malformed"),
        (
            names(&["x".repeat(types::GROUP_NAME_LEN_MAX + 1).as_str()]),
            "character",
        ),
        (
            (0..types::GROUPS_COUNT_MAX)
                .map(|i| format!("g-{i}"))
                .collect::<Vec<_>>(),
            "exceeds the limit",
        ),
    ];
    for (add, expected) in cases {
        let Err(CoreError::Validation(errs)) = rig
            .core
            .edit_groups("acme", "api", job.id, add.clone(), vec![])
            .await
        else {
            panic!("{add:?} must be refused");
        };
        assert_invariants(&rig.core);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].field, "groups");
        assert_eq!(errs[0].job_seq, Some(job.id));
        assert!(
            errs[0].message.contains(expected),
            "{:?} should name the rule it broke",
            errs[0].message
        );
        // A refused edit writes nothing.
        assert_eq!(
            stored(&rig.store, job.id).await.groups,
            names(&["beacon-import"])
        );
    }

    // The count bound is on the RESULT, not the delta: seven more labels on a
    // job that already carries one is exactly eight and is accepted; one more
    // is nine, and nine is over.
    let at_limit: Vec<String> = (0..types::GROUPS_COUNT_MAX - 1)
        .map(|i| format!("g-{i}"))
        .collect();
    assert!(
        rig.core
            .edit_groups("acme", "api", job.id, at_limit, vec![])
            .await
            .is_ok(),
        "exactly at the bound is accepted"
    );
    assert_invariants(&rig.core);
    assert!(
        rig.core
            .edit_groups("acme", "api", job.id, names(&["one-too-many"]), vec![])
            .await
            .is_err(),
        "one over the bound is an error, not a truncation"
    );
    assert_eq!(
        stored(&rig.store, job.id).await.groups.len(),
        types::GROUPS_COUNT_MAX
    );
}

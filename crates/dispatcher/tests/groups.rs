//! Tier-2 tests for job **groups** (spec §1.1 `groups:`, §6.2; design #321
//! slices A and B): the three write paths into `Job.groups`, the mutate-anywhere
//! verb that is accepted on a terminal job, the assert that a group changes
//! **nothing** about what a job runs, and — at the end of the file — the two
//! derived reads that roll those labels back up.
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
        assert_eq!(
            stored(&rig.store, job.id).await.groups,
            names(&["beacon-import"])
        );
    }

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

/// The four design documents the registry read enumerates: one with jobs still
/// in flight, one nobody has ticketed, one whose members are all terminal, and
/// one with neither a `Status:` line nor a seq in its name.
async fn seed_design_docs(rig: &Rig) {
    let clone = rig.repo.clone_branch("main").await;
    for (path, body) in [
        (
            "docs/design/311-job-inputs.md",
            "# Design #311 — Job inputs\n\nStatus: PROPOSED. Written against the tree.\n",
        ),
        (
            "docs/design/313-workload-identity.md",
            "# Design #313 — Workload identity\n\nStatus: PROPOSED\n",
        ),
        (
            "docs/design/238-finding.md",
            "# Design #238 — A finding\n\nStatus: FINDING\n",
        ),
        ("docs/design/scratch.md", "# Scratch\n\nJust notes.\n"),
    ] {
        clone.commit_file(path, body.as_bytes(), "design doc").await;
    }
    clone.push("main").await;
}

/// Two jobs in `design/311-job-inputs` (one of them also in `beacon-import`), a
/// revoked member of `beacon-import`, a fully terminal design group, and a
/// group whose name resolves to no document. Returns the seqs in filing order.
async fn seed_grouped_jobs(rig: &mut Rig) -> Vec<u64> {
    let mut seqs = Vec::new();
    for groups in [
        &["design/311-job-inputs"][..],
        &["design/311-job-inputs", "beacon-import"][..],
        &["beacon-import"][..],
        &["design/238-finding"][..],
        &["ops/fleet-refresh"][..],
    ] {
        seqs.push(rig.core.create_job(req(groups)).await.unwrap().id);
    }
    for seq in &seqs[2..] {
        rig.core.revoke_job("acme", "api", *seq).await.unwrap();
    }
    assert_invariants(&rig.core);
    seqs
}

/// The design that wrote itself: a job whose seq **is** its document's number,
/// grouped under its own design exactly as #333's backfill grouped every design
/// job (Decision 4). Seeded in that order — job, then doc, then label — because
/// the group name is only knowable once the seq is. Returns the seq.
///
/// The group ends fully terminal, and the document still carries a status: the
/// exact shape #337 was, where every design in the repo read stale the moment
/// the job that wrote it landed.
async fn seed_self_authored_design(rig: &mut Rig) -> u64 {
    let seq = rig.core.create_job(req(&[])).await.unwrap().id;
    let slug = format!("{seq}-self-authored");
    let clone = rig.repo.clone_branch("main").await;
    clone
        .commit_file(
            &format!("docs/design/{slug}.md"),
            b"# A design that wrote itself\n\nStatus: PROPOSED\n",
            "design doc",
        )
        .await;
    clone.push("main").await;
    rig.core
        .edit_groups("acme", "api", seq, vec![format!("design/{slug}")], vec![])
        .await
        .unwrap();
    rig.core.revoke_job("acme", "api", seq).await.unwrap();
    assert_invariants(&rig.core);
    seq
}

/// Request one derived read and parse its rows. Both subjects answer a JSON
/// array with no payload, so the two calls differ only in the subject.
async fn derived_read(store: &NatsStore, subject: &str) -> Vec<serde_json::Value> {
    let reply = store
        .request_timeout(subject, b"{}", std::time::Duration::from_secs(5))
        .await
        .unwrap_or_else(|e| panic!("{subject}: {e}"));
    serde_json::from_slice(&reply.payload).unwrap_or_else(|e| {
        panic!(
            "{subject}: {e}: {}",
            String::from_utf8_lossy(&reply.payload)
        )
    })
}

/// The one row whose `key` is `value`.
fn row<'a>(rows: &'a [serde_json::Value], key: &str, value: &str) -> &'a serde_json::Value {
    rows.iter()
        .find(|r| r[key] == serde_json::json!(value))
        .unwrap_or_else(|| panic!("no row with {key} = {value}: {rows:?}"))
}

/// `GET .../groups`: the group set is `distinct(job.groups)` and nothing else,
/// each group carrying its members, its histogram, its open count and — for a
/// `design/` name only — the document it resolves to at default HEAD.
fn assert_groups_rollup(groups: &[serde_json::Value], seqs: &[u64], self_seq: u64) {
    let mut expected = names(&[
        "beacon-import",
        "design/238-finding",
        "design/311-job-inputs",
        "ops/fleet-refresh",
    ]);
    expected.push(format!("design/{self_seq}-self-authored"));
    expected.sort();
    assert_eq!(
        groups
            .iter()
            .map(|g| g["name"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>(),
        expected,
        "distinct(job.groups), and no group nobody is in: {groups:?}"
    );

    let inputs = row(groups, "name", "design/311-job-inputs");
    assert_eq!(
        inputs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["id"].as_u64().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec![seqs[0], seqs[1]],
        "the two-group job is a full member here too"
    );
    assert_eq!(inputs["counts"]["Frozen"], serde_json::json!(2));
    assert_eq!(inputs["open"], serde_json::json!(2));
    assert_eq!(
        inputs["doc_path"],
        serde_json::json!("docs/design/311-job-inputs.md")
    );
    assert_eq!(
        inputs["doc_status"],
        serde_json::json!("PROPOSED. Written against the tree."),
        "the status line is surfaced verbatim and unparsed"
    );

    let beacon = row(groups, "name", "beacon-import");
    assert_eq!(beacon["jobs"].as_array().unwrap().len(), 2);
    assert_eq!(beacon["counts"]["Revoked"], serde_json::json!(1));
    assert_eq!(beacon["counts"]["Frozen"], serde_json::json!(1));
    assert_eq!(beacon["open"], serde_json::json!(1));

    let ops = row(groups, "name", "ops/fleet-refresh");
    assert_eq!(ops["open"], serde_json::json!(0), "its member is Revoked");
    assert!(
        ops.get("doc_path").is_none(),
        "no document, no path: {ops:?}"
    );
    assert!(ops.get("doc_status").is_none());
    assert!(
        !groups
            .iter()
            .any(|g| g["name"] == serde_json::json!("design/313-workload-identity")),
        "an empty group is unrepresentable: {groups:?}"
    );
}

/// `GET .../designs`: every document under `docs/design/` in path order,
/// including the one with no jobs, each with its verbatim status line, its
/// staleness flag and the same roll-up shape `/groups` serves.
fn assert_designs_registry(designs: &[serde_json::Value], self_seq: u64) {
    let mut expected = names(&[
        "docs/design/238-finding.md",
        "docs/design/311-job-inputs.md",
        "docs/design/313-workload-identity.md",
        "docs/design/scratch.md",
    ]);
    expected.push(format!("docs/design/{self_seq}-self-authored.md"));
    expected.sort();
    assert_eq!(
        designs
            .iter()
            .map(|d| d["path"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>(),
        expected,
        "every doc at default HEAD, jobs or no jobs: {designs:?}"
    );

    let unticketed = row(designs, "slug", "313-workload-identity");
    assert_eq!(unticketed["seq"], serde_json::json!(313));
    assert_eq!(
        unticketed["title"],
        serde_json::json!("Design #313 — Workload identity")
    );
    assert_eq!(
        unticketed["name"],
        serde_json::json!("design/313-workload-identity")
    );
    assert_eq!(
        unticketed["jobs"],
        serde_json::json!([]),
        "no jobs, still a row"
    );
    assert_eq!(unticketed["counts"], serde_json::json!({}));
    assert_eq!(unticketed["open"], serde_json::json!(0));
    assert_eq!(
        unticketed["status_stale"],
        serde_json::json!(false),
        "with no members, 'every member is terminal' is vacuous"
    );

    let scratch = row(designs, "slug", "scratch");
    assert!(scratch.get("status").is_none(), "{scratch:?}");
    assert!(scratch.get("seq").is_none(), "{scratch:?}");
    assert_eq!(scratch["title"], serde_json::json!("Scratch"));
    assert_eq!(scratch["status_stale"], serde_json::json!(false));
}

/// `status_stale` over the records: which member closed decides it, not just how
/// many closed (#337). The rule is pinned pure in `types::rollup`; what this
/// tier adds is that the seq the flag compares against is the seq the dispatcher
/// actually allocated, joined to the document the repo actually holds.
fn assert_designs_staleness(designs: &[serde_json::Value], self_seq: u64) {
    let stale = row(designs, "slug", "238-finding");
    assert_eq!(stale["jobs"].as_array().unwrap().len(), 1);
    assert_ne!(stale["jobs"][0]["id"], serde_json::json!(238));
    assert_eq!(stale["open"], serde_json::json!(0));
    assert_eq!(stale["status"], serde_json::json!("FINDING"));
    assert_eq!(stale["status_stale"], serde_json::json!(true));

    let authored = row(designs, "slug", &format!("{self_seq}-self-authored"));
    assert_eq!(
        authored["jobs"],
        serde_json::json!([{
            "id": self_seq, "type": "code", "title": "Ship the thing", "state": "Revoked"
        }]),
        "grouped under its own design, exactly as #333 backfilled"
    );
    assert_eq!(authored["open"], serde_json::json!(0));
    assert_eq!(authored["status"], serde_json::json!("PROPOSED"));
    assert_eq!(
        authored["status_stale"],
        serde_json::json!(false),
        "the job that wrote the doc is not implementation of it"
    );

    let live = row(designs, "slug", "311-job-inputs");
    assert_eq!(live["status_stale"], serde_json::json!(false));
    assert_eq!(live["jobs"].as_array().unwrap().len(), 2);
}

/// **Slice B end to end** (design #321 Decisions 4, 7 and 8): both derived reads
/// over real NATS, against real job records and a real repo.
///
/// The derivation matrix itself is pinned pure in `types::rollup`; what needs
/// this tier is the wiring — that the two subjects answer, that the roll-up is
/// computed from the records the dispatcher actually wrote (there being no
/// stored aggregate to compute it from), and that the `docs/design/` join
/// resolves at default-branch HEAD. It carries the three cases that exist only
/// where the repo and the records meet: a design with **no** jobs, which
/// `/groups` cannot represent, a document with no `Status:` line, and a design
/// whose one member is the job whose seq the document is named after.
#[tokio::test]
async fn the_derived_reads_roll_up_groups_and_the_design_registry() {
    let Some(mut rig) = rig().await else { return };
    seed_design_docs(&rig).await;
    let seqs = seed_grouped_jobs(&mut rig).await;
    let self_seq = seed_self_authored_design(&mut rig).await;

    let store = rig.store.clone();
    let repos = Arc::new(vcs::RepoManager::new(
        rig.repo
            .bare_path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf(),
    ));
    let backend = rig.backend.clone();
    let (handle, sink) = spawn_checked(rig.core);
    dispatcher::handlers::spawn_api_handlers(&store, handle, repos, None, None, backend)
        .await
        .unwrap();

    let groups = derived_read(&store, &store::subjects::groups_list("acme", "api")).await;
    assert_groups_rollup(&groups, &seqs, self_seq);
    let designs = derived_read(&store, &store::subjects::designs_list("acme", "api")).await;
    assert_designs_registry(&designs, self_seq);
    assert_designs_staleness(&designs, self_seq);
    assert_invariants_of(&sink);
}

//! Tier-2 tests for job **inputs** (spec §1.1 `inputs:`, §2.2; design #311
//! slice A): the create → release → Ready sequence a defaulted input travels,
//! where a bad input fails, and the immutability of `Job::inputs` across a later
//! `base_ref` update.
//!
//! Everything here needs a real repo at a ref (the declaration is a file-derived
//! fact) plus the single-writer actor, which is what makes it tier 2 rather than
//! tier 1 (`testing.md`). The rules themselves are pinned pure in
//! `chuggernaut_domain::inputs` and `chuggernaut_domain::decide::ready`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreError, CreateJobRequest, spawn};
use std::collections::BTreeMap;
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{Job, JobState};

/// The worked example from design #311: a required SHA narrowed to hex (the value
/// that reaches an argv position) and an optional `service` enum with a
/// materializable default. `min_dispatcher` is the structural skew gate a
/// non-empty `inputs:` requires (spec §14, `types::INPUTS_SCHEMA_EPOCH`).
const PARAMETERIZED: &str = r#"
name: parameterized
image: img:latest
min_dispatcher: 2
inputs:
  - name: sha
    type: string
    required: true
    pattern: '^[0-9a-f]{7,40}$'
    description: The commit to act on.
  - name: service
    type: enum
    values: [web, worker, bot]
    default: web
work:
  type: agent
  prompt: prompts/impl.md
"#;

/// The same type with the `service` default moved and a *new* defaulted input —
/// committed to main mid-flight, so any re-resolution of defaults after the first
/// pin shows up as a `region` key that should not exist.
const PARAMETERIZED_MOVED: &str = r#"
name: parameterized
image: img:latest
min_dispatcher: 2
inputs:
  - name: sha
    type: string
    required: true
    pattern: '^[0-9a-f]{7,40}$'
  - name: service
    type: enum
    values: [web, worker, bot]
    default: bot
  - name: region
    type: string
    default: eu
work:
  type: agent
  prompt: prompts/impl.md
"#;

/// A type declaring nothing, so a job of it must behave byte-identically to
/// every job that existed before inputs.
const PLAIN: &str = r#"
name: plain
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
"#;

struct Rig {
    _server: &'static test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
    _backend: Arc<FakeBackend>,
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
        ("jobs/parameterized.yaml", PARAMETERIZED),
        ("jobs/plain.yaml", PLAIN),
        ("prompts/impl.md", "implement it"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let backend = Arc::new(FakeBackend::new());
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
        _backend: backend,
        provider,
        core,
    })
}

fn req(r#type: &str, deps: &[u64], inputs: &[(&str, &str)]) -> CreateJobRequest {
    CreateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: deps.to_vec(),
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        factory: None,
        members: vec![],
        inputs: pairs(inputs),
        draft: false,
    }
}

fn pairs(inputs: &[(&str, &str)]) -> BTreeMap<String, String> {
    inputs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
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

/// The headline sequence: a creator supplies `sha`, the Ready transition that
/// pins `base_ref` fills `service` from the declaration at that ref, and
/// `Job::inputs` becomes the effective set — supplied plus resolved defaults,
/// with the supplied value untouched (#311 Decision 3).
#[tokio::test]
async fn release_to_ready_materializes_declared_defaults() {
    let Some(mut rig) = rig().await else { return };

    let job = rig
        .core
        .create_job(req("parameterized", &[], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    // At creation the record holds exactly what was supplied — no defaults yet,
    // because no `base_ref` has been recorded to resolve them against.
    assert_eq!(job.inputs, pairs(&[("sha", "4f9c1ab")]));
    assert_eq!(stored(&rig.store, job.id).await.inputs, job.inputs);

    assert_eq!(
        rig.core.release_job("acme", "api", job.id).await.unwrap(),
        JobState::Ready
    );
    let ready = stored(&rig.store, job.id).await;
    assert!(ready.base_ref.is_some(), "Ready pins base_ref");
    assert_eq!(
        ready.inputs,
        pairs(&[("service", "web"), ("sha", "4f9c1ab")]),
        "the fill added 'service' and left the supplied 'sha' alone"
    );
}

/// A supplied value for an input that also declares a default wins: the fill is
/// add-only, and this is the case that would break if it were a merge.
#[tokio::test]
async fn a_supplied_value_survives_the_default_fill() {
    let Some(mut rig) = rig().await else { return };

    let job = rig
        .core
        .create_job(req(
            "parameterized",
            &[],
            &[("sha", "4f9c1ab"), ("service", "worker")],
        ))
        .await
        .unwrap();
    rig.core.release_job("acme", "api", job.id).await.unwrap();

    assert_eq!(
        stored(&rig.store, job.id).await.inputs,
        pairs(&[("service", "worker"), ("sha", "4f9c1ab")]),
        "'worker' is what the operator asked for; 'web' is only a default"
    );
}

/// The release-time semantic pass (§2.2 pass 1), reported per input under
/// `inputs.{name}` so the create form can highlight the offending field.
#[tokio::test]
async fn release_rejects_a_missing_required_input_by_field() {
    let Some(mut rig) = rig().await else { return };

    let bare = rig
        .core
        .create_job(req("parameterized", &[], &[]))
        .await
        .unwrap();
    let Err(CoreError::Validation(errs)) = rig.core.release_job("acme", "api", bare.id).await
    else {
        panic!("a missing required input must fail release validation");
    };
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "inputs.sha");
    assert_eq!(errs[0].job_seq, Some(bare.id));
    assert!(errs[0].message.contains("required"), "{errs:?}");
    // Rejected release leaves the job Frozen — nothing pinned, nothing filled.
    let after = stored(&rig.store, bare.id).await;
    assert_eq!(after.state, JobState::Frozen);
    assert!(after.base_ref.is_none() && after.inputs.is_empty());
}

/// The other three semantic rules, each naming its own input: a value outside a
/// declared `enum`, a value outside a declared `pattern`, and a name the type
/// does not declare at all (which the creation pass deliberately let through —
/// it needs the type file).
#[tokio::test]
async fn release_rejects_enum_pattern_and_undeclared_inputs_by_field() {
    let Some(mut rig) = rig().await else { return };

    let job = rig
        .core
        .create_job(req(
            "parameterized",
            &[],
            &[("sha", "nothex"), ("service", "database"), ("region", "eu")],
        ))
        .await
        .unwrap();
    let Err(CoreError::Validation(errs)) = rig.core.release_job("acme", "api", job.id).await else {
        panic!("bad input values must fail release validation");
    };
    let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
    assert_eq!(
        fields,
        vec!["inputs.region", "inputs.service", "inputs.sha"],
        "{errs:?}"
    );
}

/// A job type declaring no inputs, released by a creator supplying none: the
/// record carries an empty map and the wire bytes carry no `inputs` key at all —
/// the feature is off, not merely unused.
#[tokio::test]
async fn a_job_with_no_inputs_is_byte_identical_to_today() {
    let Some(mut rig) = rig().await else { return };

    let job = rig.core.create_job(req("plain", &[], &[])).await.unwrap();
    assert_eq!(
        rig.core.release_job("acme", "api", job.id).await.unwrap(),
        JobState::Ready
    );
    let ready = stored(&rig.store, job.id).await;
    assert!(ready.inputs.is_empty());
    assert!(
        !serde_json::to_string(&ready).unwrap().contains("inputs"),
        "an input-free job record must not grow an 'inputs' key"
    );
}

/// A release into `Blocked` pins nothing, so it resolves nothing: the defaults
/// land at the **unblock**, against the type as it stands at the `base_ref` that
/// write records — which is the ref the run will actually use (§2.2 pass 2).
/// Here the declaration moves between the two, and the unblock's value wins.
#[tokio::test]
async fn a_blocked_job_resolves_its_defaults_at_the_unblock_ref() {
    let Some(mut rig) = rig().await else { return };

    let upstream = rig.core.create_job(req("plain", &[], &[])).await.unwrap();
    let job = rig
        .core
        .create_job(req("parameterized", &[upstream.id], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    rig.core
        .release_job("acme", "api", upstream.id)
        .await
        .unwrap();
    assert_eq!(
        rig.core.release_job("acme", "api", job.id).await.unwrap(),
        JobState::Blocked
    );
    let blocked = stored(&rig.store, job.id).await;
    assert!(blocked.base_ref.is_none());
    assert_eq!(
        blocked.inputs,
        pairs(&[("sha", "4f9c1ab")]),
        "no fill without a pin"
    );

    // The type's declaration moves on main before the dependency completes.
    let clone = rig.repo.clone_branch("main").await;
    clone
        .commit_file(
            "jobs/parameterized.yaml",
            PARAMETERIZED_MOVED.as_bytes(),
            "move the service default",
        )
        .await;
    clone.push("main").await;

    let jobs = rig.store.jobs().await.unwrap();
    let mut done = jobs.get("acme", "api", upstream.id).await.unwrap().unwrap();
    done.state = JobState::Done;
    jobs.put(&done).await.unwrap();
    rig.core
        .on_job_done("acme", "api", upstream.id)
        .await
        .unwrap();

    let unblocked = stored(&rig.store, job.id).await;
    assert_eq!(unblocked.state, JobState::Ready);
    assert!(unblocked.base_ref.is_some());
    assert_eq!(
        unblocked.inputs,
        pairs(&[("region", "eu"), ("service", "bot"), ("sha", "4f9c1ab")]),
        "the defaults resolve from the type at the pinned base_ref, not at release"
    );
}

/// **Immutability across a later `base_ref` update** (#311 Decision 6): main
/// moves while the job works, so the §3.2 pre-eval rebase advances `base_ref` —
/// and the declaration at that new ref carries a moved default and an extra
/// defaulted input. `Job::inputs` must not move with it. Defaults resolve exactly
/// once; a target that changed mid-flight would make the record a lie about at
/// least one cycle.
#[tokio::test]
async fn inputs_are_immutable_across_the_pre_eval_rebase() {
    let Some(rig) = rig().await else { return };
    let store = rig.store.clone();

    // The work agent commits to the job branch and lands both an unrelated commit
    // AND the moved job-type declaration on main before finishing.
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let work = clone_branch_from(&bare, &branch).await;
        work.commit_file("src/a.rs", b"job change", "implement")
            .await;
        work.push(&branch).await;

        let main = clone_branch_from(&bare, "main").await;
        main.commit_file(
            "jobs/parameterized.yaml",
            PARAMETERIZED_MOVED.as_bytes(),
            "move the declaration under the running job",
        )
        .await;
        main.push("main").await;
    });

    let handle = spawn(rig.core);
    let job = handle
        .create_job(req("parameterized", &[], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();
    let pinned = test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Ready)
        .await
        .base_ref
        .expect("Ready pins base_ref");

    let done = test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Done).await;
    assert_ne!(
        done.base_ref.as_deref(),
        Some(pinned.as_str()),
        "the rebase must have advanced base_ref, or this proves nothing"
    );
    assert_eq!(
        done.inputs,
        pairs(&[("service", "web"), ("sha", "4f9c1ab")]),
        "the effective set is the one resolved at the FIRST pin: no 'region', and \
         'service' is still the default that was declared then"
    );
}

/// Batch × inputs is excluded in v1 (#311 Decision 3): values do not union the
/// way `deps` and `eval` do, so a batch whose members carry inputs is rejected
/// with a field error rather than silently dropping a member's target.
#[tokio::test]
async fn a_batch_refuses_members_that_carry_inputs() {
    let Some(mut rig) = rig().await else { return };

    let a = rig
        .core
        .create_job(req("parameterized", &[], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    let b = rig
        .core
        .create_job(req("parameterized", &[], &[("sha", "a91f22c")]))
        .await
        .unwrap();

    let mut batch = req("parameterized", &[], &[("sha", "4f9c1ab")]);
    batch.members = vec![a.id, b.id];
    let Err(CoreError::Validation(errs)) = rig.core.create_job(batch).await else {
        panic!("a batch over members carrying inputs must be rejected");
    };
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(errs.iter().all(|e| e.field == "members"), "{errs:?}");
    assert!(
        errs.iter().all(|e| e.message.contains("carries inputs")),
        "{errs:?}"
    );

    // Members that carry none batch exactly as before.
    let c = rig.core.create_job(req("plain", &[], &[])).await.unwrap();
    let d = rig.core.create_job(req("plain", &[], &[])).await.unwrap();
    let mut plain_batch = req("plain", &[], &[]);
    plain_batch.members = vec![c.id, d.id];
    assert!(rig.core.create_job(plain_batch).await.is_ok());
}

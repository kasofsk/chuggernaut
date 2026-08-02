//! Tier-2 tests for job **inputs** (spec §1.1 `inputs:`, §2.2, §4.1; design #311
//! slice A): the create → release → Ready sequence a defaulted input travels,
//! where a bad input fails, the immutability of `Job::inputs` across a later
//! `base_ref` update, and the launch round trip — the golden trace from
//! `job-created` to the `CHUG_INPUT_*` keys a container actually sees.
//!
//! Everything here needs a real repo at a ref (the declaration is a file-derived
//! fact) plus the single-writer actor, which is what makes it tier 2 rather than
//! tier 1 (`testing.md`). The rules themselves are pinned pure in
//! `chuggernaut_domain::inputs` and `chuggernaut_domain::decide::ready`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreError, CreateSpec};
use std::collections::BTreeMap;
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{Job, JobState};

mod common;
use common::{assert_invariants, assert_invariants_of, spawn_checked};

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
  - name: note
    type: string
    description: Optional, and declares no default — so it never resolves.
work:
  type: agent
  prompt: prompts/impl.md
"#;

/// The same declaration plus a command evaluator, so one run exercises both
/// container kinds inputs are delivered to (#311 Decision 4: work, wrap-up and
/// eval alike).
const PARAMETERIZED_CI: &str = r#"
name: parameterized-ci
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
    default: web
  - name: note
    type: string
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: ci
    type: command
    run: ./ci.sh
"#;

/// A type declaring no inputs but running the same command evaluator — the
/// byte-identical-env regression guard for every job type in the repo today.
const PLAIN_CI: &str = r#"
name: plain-ci
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: ci
    type: command
    run: ./ci.sh
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
        ("jobs/parameterized.yaml", PARAMETERIZED),
        ("jobs/parameterized-ci.yaml", PARAMETERIZED_CI),
        ("jobs/plain.yaml", PLAIN),
        ("jobs/plain-ci.yaml", PLAIN_CI),
        ("prompts/impl.md", "implement it"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let backend = Arc::new(FakeBackend::new());
    backend.put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());
    let provider = Arc::new(FakeProvider::new());
    let core = core_over(&store, &repo, &backend, &provider, server.url()).await;
    Some(Rig {
        _server: server,
        store,
        repo,
        backend,
        provider,
        core,
    })
}

/// A `Core` over an already-seeded store and repo. Shared by [`rig`] and by the
/// two tests that rewrite a job record behind a live `Core`: only a fresh one
/// re-reads `jobs.*` KV, re-enqueuing every `Ready` job and re-indexing the
/// dependency graph the in-memory one still remembers as it was.
async fn core_over(
    store: &NatsStore,
    repo: &TempRepo,
    backend: &Arc<FakeBackend>,
    provider: &Arc<FakeProvider>,
    nats_url: &str,
) -> Core {
    Core::new(
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
            nats_url: nats_url.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

fn req(r#type: &str, deps: &[u64], inputs: &[(&str, &str)]) -> CreateSpec {
    CreateSpec {
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
        schedule: None,
        members: vec![],
        inputs: pairs(inputs),
        groups: vec![],
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
    assert_invariants(&rig.core);
    assert_eq!(job.inputs, pairs(&[("sha", "4f9c1ab")]));
    assert_eq!(stored(&rig.store, job.id).await.inputs, job.inputs);

    assert_eq!(
        rig.core.release_job("acme", "api", job.id).await.unwrap(),
        JobState::Ready
    );
    assert_invariants(&rig.core);
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
    assert_invariants(&rig.core);
    rig.core.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants(&rig.core);

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
    assert_invariants(&rig.core);
    let Err(CoreError::Validation(errs)) = rig.core.release_job("acme", "api", bare.id).await
    else {
        panic!("a missing required input must fail release validation");
    };
    assert_invariants(&rig.core);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "inputs.sha");
    assert_eq!(errs[0].job_seq, Some(bare.id));
    assert!(errs[0].message.contains("required"), "{errs:?}");
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
    assert_invariants(&rig.core);
    let Err(CoreError::Validation(errs)) = rig.core.release_job("acme", "api", job.id).await else {
        panic!("bad input values must fail release validation");
    };
    assert_invariants(&rig.core);
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
    assert_invariants(&rig.core);
    assert_eq!(
        rig.core.release_job("acme", "api", job.id).await.unwrap(),
        JobState::Ready
    );
    assert_invariants(&rig.core);
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
    assert_invariants(&rig.core);
    let job = rig
        .core
        .create_job(req("parameterized", &[upstream.id], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    assert_invariants(&rig.core);
    rig.core
        .release_job("acme", "api", upstream.id)
        .await
        .unwrap();
    assert_invariants(&rig.core);
    assert_eq!(
        rig.core.release_job("acme", "api", job.id).await.unwrap(),
        JobState::Blocked
    );
    assert_invariants(&rig.core);
    let blocked = stored(&rig.store, job.id).await;
    assert!(blocked.base_ref.is_none());
    assert_eq!(
        blocked.inputs,
        pairs(&[("sha", "4f9c1ab")]),
        "no fill without a pin"
    );

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
    let mut core = core_over(
        &rig.store,
        &rig.repo,
        &rig.backend,
        &rig.provider,
        rig._server.url(),
    )
    .await;
    core.on_job_done("acme", "api", upstream.id).await.unwrap();
    assert_invariants(&core);

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

    let (handle, sink) = spawn_checked(rig.core);
    let job = handle
        .create_job(req("parameterized", &[], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);
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
    assert_invariants_of(&sink);
}

/// **The golden trace** (#311's contracts table): `job-created` carries the
/// *supplied* inputs → `job-released` the *effective* ones, after the default
/// fill on the write that pins `base_ref` → `job-started` → and the containers
/// see exactly one `CHUG_INPUT_*` key per input with a resolved value.
///
/// The declared-but-unresolved `note` (optional, no `default`, nothing supplied)
/// is the case that must produce **no key at all** — absent, never blank, which
/// is what lets a `set -eu` script fail loudly. Both the work agent's env and the
/// command evaluator's are checked: evaluators receive inputs too (#311 Decision
/// 4 — an input supplies a value down a path a repo author already opened; it
/// cannot open one).
#[tokio::test]
async fn golden_trace_inputs_reach_work_and_eval_container_envs() {
    let Some(rig) = rig().await else { return };
    let (store, backend, provider) = (rig.store.clone(), rig.backend.clone(), rig.provider.clone());
    commit_on_work(&rig);

    let (handle, sink) = spawn_checked(rig.core);
    let job = handle
        .create_job(req("parameterized-ci", &[], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);
    test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Done).await;

    let events = job_events(&store, job.id).await;
    assert_eq!(
        event(&events, "job-created")["inputs"],
        serde_json::json!({ "sha": "4f9c1ab" }),
        "job-created carries the SUPPLIED set"
    );
    assert_eq!(
        event(&events, "job-released")["inputs"],
        serde_json::json!({ "sha": "4f9c1ab", "service": "web" }),
        "job-released carries the EFFECTIVE set — the default is the difference"
    );
    assert!(
        !event(&events, "job-started").is_null(),
        "the job started: {events:?}"
    );

    let expected = vec![
        ("CHUG_INPUT_SERVICE".to_string(), "web".to_string()),
        ("CHUG_INPUT_SHA".to_string(), "4f9c1ab".to_string()),
    ];
    let runs = provider.runs();
    assert_eq!(injected(&runs[0].env), expected, "work container env");
    assert!(
        runs[0].prompt.contains(
            "## Job Brief\n\n### Inputs\n<untrusted_input>\nservice: web\nsha: 4f9c1ab\n\
             </untrusted_input>\n"
        ),
        "the work agent reads the resolved set in its §4.3 brief, nested under \
         the brief heading, and never learns of the unresolved 'note': {}",
        runs[0].prompt
    );

    let eval = backend
        .launches()
        .into_iter()
        .find(|c| c.cmd.iter().any(|arg| arg.contains("./ci.sh")))
        .expect("the ci evaluator ran in a container");
    assert_eq!(injected(&eval.env), expected, "eval container env");
    assert_invariants_of(&sink);
}

/// The regression guard for **every job type in this repo today** (#311's
/// contracts table): a job whose type declares no inputs launches an eval
/// container whose env is byte-identical to what it was before inputs existed —
/// the feature is off, not merely unused. The key list is pinned rather than
/// merely filtered for `CHUG_INPUT_*`, so *any* new env key has to be a
/// deliberate edit here.
#[tokio::test]
async fn an_input_free_job_launches_a_byte_identical_eval_env() {
    let Some(rig) = rig().await else { return };
    let (store, backend, provider) = (rig.store.clone(), rig.backend.clone(), rig.provider.clone());
    commit_on_work(&rig);

    let (handle, sink) = spawn_checked(rig.core);
    let job = handle.create_job(req("plain-ci", &[], &[])).await.unwrap();
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&sink);
    test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Done).await;

    let eval = backend
        .launches()
        .into_iter()
        .find(|c| c.cmd.iter().any(|arg| arg.contains("./ci.sh")))
        .expect("the ci evaluator ran in a container");
    let mut keys: Vec<&str> = eval.env.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "BASE_BRANCH",
            "CHANNEL_ROLE",
            "CHUG_EVALUATOR",
            "CHUG_PHASE",
            "CHUG_TASK_ID",
            "JOB_BRANCH",
            "JOB_ID",
            "JOB_PROJECT",
            "JOB_TASK_ID",
            "NATS_URL",
            "REPO_URL",
        ],
        "an input-free job's eval env must not grow a key"
    );
    let work = &provider.runs()[0];
    assert!(
        !work.prompt.contains("### Inputs") && !work.prompt.contains("<untrusted_input>"),
        "the same guarantee on the prompt side (#311 slice B): an input-free job's \
         §4.3 brief is the one it read before inputs existed: {}",
        work.prompt
    );
    let released = event(&job_events(&store, job.id).await, "job-released");
    assert_eq!(released["state"], serde_json::json!("Ready"));
    assert!(released["inputs"].is_null(), "{released}");
    assert_invariants_of(&sink);
}

/// §2.2's **third and last pass** (#311 Decision 3): a value that no longer
/// clears the charset is caught immediately before injection and parks the job
/// like a missing secret — no container, no `CHUG_INPUT_*`. Reaching it means an
/// earlier pass was bypassed, which is exactly what this test stages: the record
/// is rewritten after the Ready transition, the way a record written before the
/// rule existed would look.
#[tokio::test]
async fn a_value_outside_the_charset_parks_the_job_at_launch() {
    let Some(mut rig) = rig().await else { return };

    let job = rig
        .core
        .create_job(req("parameterized", &[], &[("sha", "4f9c1ab")]))
        .await
        .unwrap();
    assert_invariants(&rig.core);
    assert_eq!(
        rig.core.release_job("acme", "api", job.id).await.unwrap(),
        JobState::Ready
    );
    assert_invariants(&rig.core);

    let jobs = rig.store.jobs().await.unwrap();
    let mut ready = jobs.get("acme", "api", job.id).await.unwrap().unwrap();
    ready.inputs.insert("sha".into(), "4f9c1ab;rm -rf /".into());
    jobs.put(&ready).await.unwrap();

    let core = core_over(
        &rig.store,
        &rig.repo,
        &rig.backend,
        &rig.provider,
        rig._server.url(),
    )
    .await;
    let (_handle, sink) = spawn_checked(core);

    let parked = test_utils::wait::job_where(
        &rig.store,
        "acme",
        "api",
        job.id,
        format!("job {} to park at launch on its bad input", job.id),
        |rec| rec.state != JobState::Ready,
    )
    .await;
    assert_invariants_of(&sink);
    assert_eq!(
        parked.state,
        JobState::Stalled,
        "parks like a missing KV, and a pre-work park is Stalled (spec §575)"
    );
    let escalation = parked.escalation.expect("the park records why");
    assert_eq!(escalation.reason, "launch_validation_failed");
    assert!(
        escalation.detail.contains("input 'sha'"),
        "the park names the offending input: {}",
        escalation.detail
    );
    assert!(
        rig.provider.runs().is_empty() && rig.backend.launches().is_empty(),
        "a parked launch launches nothing"
    );
    assert_invariants_of(&sink);
}

/// The work-agent hook every launch-reaching test needs: commit on the job branch
/// so the §3.2 finish-line guard sees output and the job proceeds to Done.
fn commit_on_work(rig: &Rig) {
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let work = clone_branch_from(&bare, &branch).await;
        work.commit_file("src/a.rs", b"job change", "implement")
            .await;
        work.push(&branch).await;
    });
}

/// The `CHUG_INPUT_*` slice of a container env, sorted — what a script can read.
fn injected(env: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = env
        .iter()
        .filter(|(k, _)| k.starts_with(types::INPUT_ENV_PREFIX))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    keys.sort();
    keys
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

/// The first event of a type, or `Value::Null` when the stream carries none.
fn event(events: &[serde_json::Value], event_type: &str) -> serde_json::Value {
    events
        .iter()
        .find(|v| v["event_type"] == serde_json::json!(event_type))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
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
    assert_invariants(&rig.core);
    let b = rig
        .core
        .create_job(req("parameterized", &[], &[("sha", "a91f22c")]))
        .await
        .unwrap();
    assert_invariants(&rig.core);

    let mut batch = req("parameterized", &[], &[("sha", "4f9c1ab")]);
    batch.members = vec![a.id, b.id];
    let Err(CoreError::Validation(errs)) = rig.core.create_job(batch).await else {
        panic!("a batch over members carrying inputs must be rejected");
    };
    assert_invariants(&rig.core);
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(errs.iter().all(|e| e.field == "members"), "{errs:?}");
    assert!(
        errs.iter().all(|e| e.message.contains("carries inputs")),
        "{errs:?}"
    );

    let c = rig.core.create_job(req("plain", &[], &[])).await.unwrap();
    assert_invariants(&rig.core);
    let d = rig.core.create_job(req("plain", &[], &[])).await.unwrap();
    assert_invariants(&rig.core);
    let mut plain_batch = req("plain", &[], &[]);
    plain_batch.members = vec![c.id, d.id];
    assert!(rig.core.create_job(plain_batch).await.is_ok());
    assert_invariants(&rig.core);
}

//! Tier-2 tests for the Draft state (spec §2.1): a job's definition can be
//! iterated on before it enters the DAG for real. Covers create-as-draft →
//! edit → release running the EDITED definition, edit rejected outside Draft,
//! Frozen → Draft → edit → release, revoke from Draft, a dependent on a Draft
//! staying Blocked until the draft releases and completes, and claim rejected
//! on a Draft.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreError, CoreHandle, CreateSpec, UpdateJobRequest};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{Evaluator, EvaluatorType, JobState};

mod common;
use common::{assert_invariants_of, spawn_checked};

/// A plain agent job, no evaluators: a clean exit takes it straight to Done,
/// so the harness can assert lifecycle without scripting eval side effects.
const SIMPLE: &str = r#"
name: simple
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
"#;

struct Rig {
    _server: &'static test_utils::nats::NatsTestServer,
    _repo: TempRepo,
    store: NatsStore,
    provider: Arc<FakeProvider>,
    handle: CoreHandle,
    /// Invariant violations the actor logged, drained by
    /// `assert_invariants_of` (refactor-plan B1a).
    invariants: InvariantSink,
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
        ("jobs/simple.yaml", SIMPLE),
        ("prompts/impl.md", "IMPLEMENT THE TICKET"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    let backend = Arc::new(FakeBackend::new());
    let provider = Arc::new(FakeProvider::new());
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend,
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (handle, invariants) = spawn_checked(core);
    Some(Rig {
        _server: server,
        _repo: repo,
        store,
        provider,
        handle,
        invariants,
    })
}

fn create(draft: bool, deps: &[u64], description: &str) -> CreateSpec {
    CreateSpec {
        owner: "acme".into(),
        project: "api".into(),
        r#type: "simple".into(),
        title: String::new(),
        description: description.into(),
        cover_html: None,
        deps: deps.to_vec(),
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        factory: None,
        members: vec![],
        inputs: Default::default(),
        groups: vec![],
        draft,
    }
}

fn update(seq: u64, deps: &[u64], description: &str) -> UpdateJobRequest {
    UpdateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        seq,
        r#type: "simple".into(),
        title: String::new(),
        description: description.into(),
        cover_html: None,
        deps: deps.to_vec(),
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        inputs: Default::default(),
        groups: vec![],
    }
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
    test_utils::wait::job_state(store, "acme", "api", seq, want).await
}

/// Registers a work run that commits a stub file to the job branch, so an
/// agent work run produces output and clears the §3.2 empty-output guard.
fn commit_work(rig: &Rig) {
    let bare = rig._repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let clone = clone_branch_from(&bare, &branch).await;
        let body = format!("// work produced on {branch}\n");
        clone
            .commit_file("src/work.rs", body.as_bytes(), "work")
            .await;
        clone.push(&branch).await;
    });
}

async fn event_types(store: &NatsStore) -> Vec<String> {
    store
        .read_stream("job-events", 200)
        .await
        .unwrap()
        .iter()
        .map(|payload| {
            let v: serde_json::Value = serde_json::from_slice(payload).unwrap();
            v["event_type"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

async fn state_of(store: &NatsStore, seq: u64) -> JobState {
    store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", seq)
        .await
        .unwrap()
        .unwrap()
        .state
}

/// Create-as-draft → edit → release: the job lands Draft, the edited
/// description reaches the work prompt (the EDITED definition is what runs),
/// and the original is gone.
#[tokio::test]
async fn draft_edit_release_runs_edited_description() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let job = rig
        .handle
        .create_job(create(true, &[], "ORIGINAL BODY"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(job.state, JobState::Draft, "draft:true lands Draft");

    rig.handle
        .update_job(update(job.id, &[], "EDITED BODY REACHES PROMPT"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);

    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let prompt = &rig.provider.runs()[0].prompt;
    assert!(
        prompt.contains("EDITED BODY REACHES PROMPT"),
        "edited description must reach the work prompt: {prompt}"
    );
    assert!(
        !prompt.contains("ORIGINAL BODY"),
        "the pre-edit description must not survive: {prompt}"
    );
    assert_invariants_of(&rig.invariants);
}

/// `cover_html` round-trips through create and the Draft PATCH, and is
/// presentational only: the edited cover is stored but never reaches the work
/// prompt (the description does). Proves the field is Draft-mutable and prompt-
/// clean end to end.
#[tokio::test]
async fn draft_cover_html_round_trips_and_stays_out_of_prompt() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let mut req = create(true, &[], "THE DESCRIPTION");
    req.cover_html = Some("<h1>SPLASHY COVER</h1>".into());
    let job = rig.handle.create_job(req).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(job.cover_html.as_deref(), Some("<h1>SPLASHY COVER</h1>"));

    let mut edit = update(job.id, &[], "THE DESCRIPTION");
    edit.cover_html = Some("<h1>EDITED COVER</h1>".into());
    let edited = rig.handle.update_job(edit).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(edited.cover_html.as_deref(), Some("<h1>EDITED COVER</h1>"));

    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let done = wait_for_state(&rig.store, job.id, JobState::Done).await;
    assert_eq!(done.cover_html.as_deref(), Some("<h1>EDITED COVER</h1>"));

    let prompt = &rig.provider.runs()[0].prompt;
    assert!(
        prompt.contains("THE DESCRIPTION"),
        "description reaches prompt"
    );
    assert!(
        !prompt.contains("COVER"),
        "cover_html must never reach the work prompt: {prompt}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Edited deps are enforced at release: a draft edited to depend on an
/// incomplete job releases to Blocked, exactly as if it had been created with
/// that dependency.
#[tokio::test]
async fn draft_edited_deps_are_enforced_at_release() {
    let Some(rig) = rig().await else { return };

    let upstream = rig.handle.create_job(create(false, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let draft = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .update_job(update(draft.id, &[upstream.id], ""))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);

    let state = rig
        .handle
        .release_job("acme", "api", draft.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(state, JobState::Blocked, "edited dep (not Done) → Blocked");
    assert_invariants_of(&rig.invariants);
}

/// The edit endpoint is Draft-only: once a job is Ready/Work or terminal, an
/// update is rejected (409 Conflict).
#[tokio::test]
async fn edit_rejected_outside_draft() {
    let Some(rig) = rig().await else { return };

    commit_work(&rig);
    let job = rig.handle.create_job(create(false, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let err = rig.handle.update_job(update(job.id, &[], "too late")).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(err, Err(CoreError::Conflict(_))),
        "edit of a non-Draft job must conflict, got {err:?}"
    );

    wait_for_state(&rig.store, job.id, JobState::Done).await;
    let err = rig
        .handle
        .update_job(update(job.id, &[], "still too late"))
        .await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(err, Err(CoreError::Conflict(_))),
        "edit of a terminal job must conflict, got {err:?}"
    );
    assert_invariants_of(&rig.invariants);
}

/// Frozen → Draft reopens a never-released job for editing; the edit then
/// release runs the edited definition.
#[tokio::test]
async fn frozen_to_draft_edit_release() {
    let Some(rig) = rig().await else { return };

    commit_work(&rig);
    let job = rig
        .handle
        .create_job(create(false, &[], "FROZEN BODY"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(job.state, JobState::Frozen);

    rig.handle.draft_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(state_of(&rig.store, job.id).await, JobState::Draft);

    let err = rig.handle.draft_job("acme", "api", job.id).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(err, Err(CoreError::Transition(_))),
        "Draft → Draft must be an invalid transition, got {err:?}"
    );

    rig.handle
        .update_job(update(job.id, &[], "REOPENED AND EDITED"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let prompt = &rig.provider.runs()[0].prompt;
    assert!(prompt.contains("REOPENED AND EDITED"), "{prompt}");
    assert!(!prompt.contains("FROZEN BODY"), "{prompt}");
    assert_invariants_of(&rig.invariants);
}

/// Revoke from Draft is allowed and terminal — the job never ran, so nothing
/// cascades or lingers.
#[tokio::test]
async fn revoke_from_draft() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let cascaded = rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert!(cascaded.is_empty(), "a Draft never ran; no cascade");
    assert_eq!(state_of(&rig.store, job.id).await, JobState::Revoked);
    assert_invariants_of(&rig.invariants);
}

/// A dependent on a Draft job stays Blocked until the draft is released AND
/// completes — a Draft is invisible to scheduling but a valid dependency.
#[tokio::test]
async fn dep_on_draft_stays_blocked_until_released_and_done() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    commit_work(&rig);

    let draft = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let dependent = rig
        .handle
        .create_job(create(false, &[draft.id], ""))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);

    let state = rig
        .handle
        .release_job("acme", "api", dependent.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(state, JobState::Blocked);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        state_of(&rig.store, dependent.id).await,
        JobState::Blocked,
        "still Blocked while upstream is only a Draft"
    );

    rig.handle
        .release_job("acme", "api", draft.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, draft.id, JobState::Done).await;
    wait_for_state(&rig.store, dependent.id, JobState::Done).await;
    assert_invariants_of(&rig.invariants);
}

/// Editing a Draft's deps prunes the stale reverse edge: after a draft is
/// re-pointed from upstream U to upstream V and released, revoking U must NOT
/// cascade to the released job — it no longer depends on U. Guards the
/// edit-then-revoke window that immutable deps used to make impossible.
#[tokio::test]
async fn edit_dropping_upstream_prunes_revoke_cascade() {
    let Some(rig) = rig().await else { return };

    let u = rig.handle.create_job(create(false, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let v = rig.handle.create_job(create(false, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let draft = rig
        .handle
        .create_job(create(true, &[u.id], ""))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .update_job(update(draft.id, &[v.id], ""))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);

    let state = rig
        .handle
        .release_job("acme", "api", draft.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(state, JobState::Blocked);

    let cascaded = rig.handle.revoke_job("acme", "api", u.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert!(
        !cascaded.contains(&draft.id),
        "released job no longer depends on U; must not cascade: {cascaded:?}"
    );
    assert_eq!(
        state_of(&rig.store, draft.id).await,
        JobState::Blocked,
        "job re-pointed away from U must survive U's revoke"
    );
    assert_invariants_of(&rig.invariants);
}

/// #166: finalize an edited Draft → Frozen. The edits are preserved, the job
/// parks unscheduled (no queue entry), `job-finalized` is emitted, and the
/// resulting Frozen job can then be batched — the gap that stranded edited jobs
/// outside batching (release was Draft's only exit).
#[tokio::test]
async fn draft_finalize_parks_frozen_preserves_edits_and_is_batchable() {
    let Some(rig) = rig().await else { return };

    let a = rig
        .handle
        .create_job(create(true, &[], "ORIGINAL A"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle
        .update_job(update(a.id, &[], "EDITED A"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.finalize_job("acme", "api", a.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let frozen_a = rig
        .store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", a.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frozen_a.state, JobState::Frozen, "finalize parks Frozen");
    assert_eq!(
        frozen_a.description, "EDITED A",
        "the edited definition is preserved through finalize"
    );

    assert!(
        event_types(&rig.store)
            .await
            .contains(&"job-finalized".into()),
        "finalize emits job-finalized"
    );

    let b = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    rig.handle.finalize_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let mut batch = create(false, &[], "");
    batch.members = vec![a.id, b.id];
    rig.handle
        .create_job(batch)
        .await
        .expect("two finalized Frozen jobs are batchable");
    assert_invariants_of(&rig.invariants);
}

/// #166: finalize is Draft-only. On any non-Draft state (here a Frozen job that
/// was never drafted) it is an invalid transition (409).
#[tokio::test]
async fn finalize_on_non_draft_conflicts() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(create(false, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(job.state, JobState::Frozen);
    let err = rig.handle.finalize_job("acme", "api", job.id).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(err, Err(CoreError::Transition(_))),
        "finalize of a non-Draft job must be an invalid transition, got {err:?}"
    );
    assert_eq!(
        state_of(&rig.store, job.id).await,
        JobState::Frozen,
        "a rejected finalize leaves the state untouched"
    );
    assert_invariants_of(&rig.invariants);
}

/// #166: a definition that fails validation cannot be finalized — the field
/// errors surface and the job stays Draft (mirrors release's validation, which
/// would reject the same edit).
#[tokio::test]
async fn finalize_validation_failure_stays_draft() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let mut bad = update(job.id, &[], "");
    bad.eval = vec![Evaluator {
        name: "broken".into(),
        r#type: EvaluatorType::Command,
        image: None,
        run: None,
        prompt: None,
        provider: None,
        model: None,
        secrets: vec![],
        required: None,
        stage: 0,
    }];
    rig.handle.update_job(bad).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let err = rig.handle.finalize_job("acme", "api", job.id).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(&err, Err(CoreError::Validation(errs)) if errs.iter().any(|e| e.message.contains("run"))),
        "finalize must reject the malformed evaluator with a field error, got {err:?}"
    );
    assert_eq!(
        state_of(&rig.store, job.id).await,
        JobState::Draft,
        "a failed finalize leaves the job editable in Draft"
    );
    assert_invariants_of(&rig.invariants);
}

/// #166: Frozen → Draft → finalize → Frozen round-trips idempotently — a
/// never-released job can be reopened, finalized untouched, and remain Frozen.
#[tokio::test]
async fn frozen_draft_finalize_round_trip() {
    let Some(rig) = rig().await else { return };

    let job = rig
        .handle
        .create_job(create(false, &[], "BODY"))
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(job.state, JobState::Frozen);

    rig.handle.draft_job("acme", "api", job.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(state_of(&rig.store, job.id).await, JobState::Draft);

    rig.handle
        .finalize_job("acme", "api", job.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    let back = rig
        .store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", job.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back.state, JobState::Frozen, "round-trips back to Frozen");
    assert_eq!(
        back.description, "BODY",
        "an untouched finalize preserves the record"
    );
    assert_invariants_of(&rig.invariants);
}

/// A Draft holds no work attempt to claim — claiming is rejected until it is
/// released.
#[tokio::test]
async fn claim_on_draft_rejected() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    assert_invariants_of(&rig.invariants);
    let err = rig.handle.claim_job("acme", "api", job.id).await;
    assert_invariants_of(&rig.invariants);
    assert!(
        matches!(err, Err(CoreError::Conflict(_))),
        "claim on a Draft must conflict, got {err:?}"
    );
    assert_invariants_of(&rig.invariants);
}

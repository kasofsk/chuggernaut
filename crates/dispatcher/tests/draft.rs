//! Tier-2 tests for the Draft state (spec §2.1): a job's definition can be
//! iterated on before it enters the DAG for real. Covers create-as-draft →
//! edit → release running the EDITED definition, edit rejected outside Draft,
//! Frozen → Draft → edit → release, revoke from Draft, a dependent on a Draft
//! staying Blocked until the draft releases and completes, and claim rejected
//! on a Draft.

use dispatcher::core::{
    Core, CoreConfig, CoreError, CoreHandle, CreateJobRequest, UpdateJobRequest, spawn,
};
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::JobState;

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
    _server: test_utils::nats::NatsTestServer,
    store: NatsStore,
    provider: Arc<FakeProvider>,
    handle: CoreHandle,
}

async fn rig() -> Option<Rig> {
    let server = test_utils::nats::NatsTestServer::spawn()?;
    let store = NatsStore::connect(server.url()).await.unwrap();
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
    let handle = spawn(core);
    Some(Rig {
        _server: server,
        store,
        provider,
        handle,
    })
}

fn create(draft: bool, deps: &[u64], description: &str) -> CreateJobRequest {
    CreateJobRequest {
        owner: "acme".into(),
        project: "api".into(),
        r#type: "simple".into(),
        title: String::new(),
        description: description.into(),
        deps: deps.to_vec(),
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        factory: None,
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
        deps: deps.to_vec(),
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
    }
}

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
    let jobs = store.jobs().await.unwrap();
    for _ in 0..100 {
        if let Some(job) = jobs.get("acme", "api", seq).await.unwrap()
            && job.state == want
        {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {want:?}");
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

    let job = rig
        .handle
        .create_job(create(true, &[], "ORIGINAL BODY"))
        .await
        .unwrap();
    assert_eq!(job.state, JobState::Draft, "draft:true lands Draft");

    rig.handle
        .update_job(update(job.id, &[], "EDITED BODY REACHES PROMPT"))
        .await
        .unwrap();

    rig.handle.release_job("acme", "api", job.id).await.unwrap();
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
}

/// Edited deps are enforced at release: a draft edited to depend on an
/// incomplete job releases to Blocked, exactly as if it had been created with
/// that dependency.
#[tokio::test]
async fn draft_edited_deps_are_enforced_at_release() {
    let Some(rig) = rig().await else { return };

    // An upstream that stays Frozen (never released) — a valid, non-terminal
    // dependency that is not Done.
    let upstream = rig.handle.create_job(create(false, &[], "")).await.unwrap();

    let draft = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    // Edit adds the dependency the draft was created without.
    rig.handle
        .update_job(update(draft.id, &[upstream.id], ""))
        .await
        .unwrap();

    let state = rig
        .handle
        .release_job("acme", "api", draft.id)
        .await
        .unwrap();
    assert_eq!(state, JobState::Blocked, "edited dep (not Done) → Blocked");
}

/// The edit endpoint is Draft-only: once a job is Ready/Work or terminal, an
/// update is rejected (409 Conflict).
#[tokio::test]
async fn edit_rejected_outside_draft() {
    let Some(rig) = rig().await else { return };

    // A released job is running/queued, not editable.
    let job = rig.handle.create_job(create(false, &[], "")).await.unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    // Frozen → Ready happens on release; either way it is no longer Draft.
    let err = rig.handle.update_job(update(job.id, &[], "too late")).await;
    assert!(
        matches!(err, Err(CoreError::Conflict(_))),
        "edit of a non-Draft job must conflict, got {err:?}"
    );

    // And once terminal it is likewise not editable.
    wait_for_state(&rig.store, job.id, JobState::Done).await;
    let err = rig
        .handle
        .update_job(update(job.id, &[], "still too late"))
        .await;
    assert!(
        matches!(err, Err(CoreError::Conflict(_))),
        "edit of a terminal job must conflict, got {err:?}"
    );
}

/// Frozen → Draft reopens a never-released job for editing; the edit then
/// release runs the edited definition.
#[tokio::test]
async fn frozen_to_draft_edit_release() {
    let Some(rig) = rig().await else { return };

    // A normally-created (Frozen) job.
    let job = rig
        .handle
        .create_job(create(false, &[], "FROZEN BODY"))
        .await
        .unwrap();
    assert_eq!(job.state, JobState::Frozen);

    // Reopen for editing.
    rig.handle.draft_job("acme", "api", job.id).await.unwrap();
    assert_eq!(state_of(&rig.store, job.id).await, JobState::Draft);

    // Only Frozen → Draft: a Draft cannot be re-drafted.
    let err = rig.handle.draft_job("acme", "api", job.id).await;
    assert!(
        matches!(err, Err(CoreError::Transition(_))),
        "Draft → Draft must be an invalid transition, got {err:?}"
    );

    rig.handle
        .update_job(update(job.id, &[], "REOPENED AND EDITED"))
        .await
        .unwrap();
    rig.handle.release_job("acme", "api", job.id).await.unwrap();
    wait_for_state(&rig.store, job.id, JobState::Done).await;

    let prompt = &rig.provider.runs()[0].prompt;
    assert!(prompt.contains("REOPENED AND EDITED"), "{prompt}");
    assert!(!prompt.contains("FROZEN BODY"), "{prompt}");
}

/// Revoke from Draft is allowed and terminal — the job never ran, so nothing
/// cascades or lingers.
#[tokio::test]
async fn revoke_from_draft() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    let cascaded = rig.handle.revoke_job("acme", "api", job.id).await.unwrap();
    assert!(cascaded.is_empty(), "a Draft never ran; no cascade");
    assert_eq!(state_of(&rig.store, job.id).await, JobState::Revoked);
}

/// A dependent on a Draft job stays Blocked until the draft is released AND
/// completes — a Draft is invisible to scheduling but a valid dependency.
#[tokio::test]
async fn dep_on_draft_stays_blocked_until_released_and_done() {
    let Some(rig) = rig().await else { return };

    let draft = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    let dependent = rig
        .handle
        .create_job(create(false, &[draft.id], ""))
        .await
        .unwrap();

    // Releasing the dependent while its upstream is a Draft → Blocked; the
    // Draft is not Done (nor even released), so the dep is unsatisfied.
    let state = rig
        .handle
        .release_job("acme", "api", dependent.id)
        .await
        .unwrap();
    assert_eq!(state, JobState::Blocked);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        state_of(&rig.store, dependent.id).await,
        JobState::Blocked,
        "still Blocked while upstream is only a Draft"
    );

    // Release the draft; it runs to Done, which unblocks the dependent, which
    // then runs to Done itself.
    rig.handle
        .release_job("acme", "api", draft.id)
        .await
        .unwrap();
    wait_for_state(&rig.store, draft.id, JobState::Done).await;
    wait_for_state(&rig.store, dependent.id, JobState::Done).await;
}

/// Editing a Draft's deps prunes the stale reverse edge: after a draft is
/// re-pointed from upstream U to upstream V and released, revoking U must NOT
/// cascade to the released job — it no longer depends on U. Guards the
/// edit-then-revoke window that immutable deps used to make impossible.
#[tokio::test]
async fn edit_dropping_upstream_prunes_revoke_cascade() {
    let Some(rig) = rig().await else { return };

    // Two Frozen upstreams; neither is Done, so a dependent on either releases
    // to Blocked (a stable, cascade-eligible state to observe).
    let u = rig.handle.create_job(create(false, &[], "")).await.unwrap();
    let v = rig.handle.create_job(create(false, &[], "")).await.unwrap();

    // Draft created depending on U, then edited to depend on V instead.
    let draft = rig
        .handle
        .create_job(create(true, &[u.id], ""))
        .await
        .unwrap();
    rig.handle
        .update_job(update(draft.id, &[v.id], ""))
        .await
        .unwrap();

    // Release the draft: its (edited) dep V is not Done → Blocked.
    let state = rig
        .handle
        .release_job("acme", "api", draft.id)
        .await
        .unwrap();
    assert_eq!(state, JobState::Blocked);

    // Revoke U. The draft no longer depends on U, so the stale U→draft edge
    // must not drag it into the cascade.
    let cascaded = rig.handle.revoke_job("acme", "api", u.id).await.unwrap();
    assert!(
        !cascaded.contains(&draft.id),
        "released job no longer depends on U; must not cascade: {cascaded:?}"
    );
    assert_eq!(
        state_of(&rig.store, draft.id).await,
        JobState::Blocked,
        "job re-pointed away from U must survive U's revoke"
    );
}

/// A Draft holds no work attempt to claim — claiming is rejected until it is
/// released.
#[tokio::test]
async fn claim_on_draft_rejected() {
    let Some(rig) = rig().await else { return };

    let job = rig.handle.create_job(create(true, &[], "")).await.unwrap();
    let err = rig.handle.claim_job("acme", "api", job.id).await;
    assert!(
        matches!(err, Err(CoreError::Conflict(_))),
        "claim on a Draft must conflict, got {err:?}"
    );
}

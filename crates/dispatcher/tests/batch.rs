//! Tier-2 tests for job batches (spec §2.1 batches): a batch is a job that
//! absorbs N same-type Frozen members, produces one branch, is evaluated under
//! the union of their criteria, and whose single completion fans out to every
//! member. Covers the creation-validation matrix, the batch-aware prompt, the
//! completion fan-out (members Done + a dependent unblocks), revoke → members
//! back to Frozen (re-batchable), the eval-union collision error, and the
//! single rework budget shared across the whole batch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CoreError, CoreHandle, CreateSpec, EvalSubmission};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{Evaluator, EvaluatorType, JobState};

mod common;
use common::{assert_invariants_of, spawn_checked};

const WEB: &str = r#"
name: web
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
"#;

const REVIEW: &str = r#"
name: review
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
rework_budget: 1
eval:
  - name: reviewer
    type: agent
    prompt: prompts/eval.md
"#;

const CODE: &str = r#"
name: code
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
        ("jobs/web.yaml", WEB),
        ("jobs/review.yaml", REVIEW),
        ("jobs/code.yaml", CODE),
        ("prompts/impl.md", "IMPLEMENT THE TICKET"),
        ("prompts/eval.md", "REVIEW THE TICKET"),
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

/// A member/ordinary job creation request (no `members` payload).
/// `create_job` plus the invariant check the message it drives owes
/// (refactor-plan B1a). Batch tests create four to six jobs of setup apiece, so
/// the check lives in the wrapper rather than in a line after every one of them.
async fn create_checked(rig: &Rig, req: CreateSpec) -> types::Job {
    let job = rig.handle.create_job(req).await.unwrap();
    assert_invariants_of(&rig.invariants);
    job
}

fn member(r#type: &str, deps: &[u64], title: &str, description: &str) -> CreateSpec {
    CreateSpec {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        title: title.into(),
        description: description.into(),
        cover_html: None,
        deps: deps.to_vec(),
        members: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        require_approval: false,
        timeout: None,
        model: None,
        factory: None,
        schedule: None,
        inputs: Default::default(),
        groups: vec![],
        draft: false,
    }
}

/// A batch creation request over the given member seqs.
fn batch(r#type: &str, members: &[u64], title: &str, description: &str) -> CreateSpec {
    let mut req = member(r#type, &[], title, description);
    req.members = members.to_vec();
    req
}

/// A **draft** batch creation request: stages the member list without absorbing
/// it (spec §2.1 draft batches).
fn draft_batch(r#type: &str, members: &[u64], title: &str, description: &str) -> CreateSpec {
    let mut req = batch(r#type, members, title, description);
    req.draft = true;
    req
}

/// A command evaluator with the given name and run script — used to build
/// additive per-member evaluators for the eval-union tests.
fn cmd_eval(name: &str, run: &str) -> Evaluator {
    Evaluator {
        name: name.into(),
        r#type: EvaluatorType::Command,
        image: None,
        run: Some(run.into()),
        prompt: None,
        provider: None,
        model: None,
        secrets: vec![],
        workload_identities: vec![],
        required: None,
        stage: 0,
    }
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

async fn get_job(store: &NatsStore, seq: u64) -> types::Job {
    store
        .jobs()
        .await
        .unwrap()
        .get("acme", "api", seq)
        .await
        .unwrap()
        .unwrap()
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

async fn wait_for_state(store: &NatsStore, seq: u64, want: JobState) -> types::Job {
    test_utils::wait::job_state(store, "acme", "api", seq, want).await
}

/// Registers a work run that commits a stub file to the job branch, so the
/// batch's single work agent produces output and clears the §3.2 empty-output
/// guard.
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

/// A batch absorbs its members: each goes Frozen→Batched with `batch_id` set,
/// the batch lands Frozen carrying the member list, and its description defaults
/// to an auto-index naming every member.
#[tokio::test]
async fn batch_creation_absorbs_members() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;

    let b = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;

    assert!(b.is_batch());
    assert_eq!(b.members, vec![m1.id, m2.id]);
    assert_eq!(b.state, JobState::Frozen);
    assert!(
        b.description.contains(&format!("#{}", m1.id))
            && b.description.contains(&format!("#{}", m2.id)),
        "auto-index description names members: {}",
        b.description
    );

    for m in [&m1, &m2] {
        let mj = get_job(&rig.store, m.id).await;
        assert_eq!(mj.state, JobState::Batched);
        assert_eq!(mj.batch_id, Some(b.id));
    }
    assert_invariants_of(&rig.invariants);
}

/// Every creation-rule violation is rejected: fewer than 2 members, a
/// non-Frozen member, a wrong-type member, an already-batched member, and a
/// batch-of-a-batch.
#[tokio::test]
async fn batch_creation_validation_rejects() {
    let Some(rig) = rig().await else { return };

    let a = create_checked(&rig, member("web", &[], "", "")).await;
    let b = create_checked(&rig, member("web", &[], "", "")).await;

    assert!(matches!(
        rig.handle.create_job(batch("web", &[a.id], "", "")).await,
        Err(CoreError::Validation(_))
    ));
    assert_invariants_of(&rig.invariants);

    let c = create_checked(&rig, member("code", &[], "", "")).await;
    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[a.id, c.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));
    assert_invariants_of(&rig.invariants);

    rig.handle.release_job("acme", "api", a.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_ne!(state_of(&rig.store, a.id).await, JobState::Frozen);
    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[a.id, b.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));
    assert_invariants_of(&rig.invariants);

    let d = create_checked(&rig, member("web", &[], "", "")).await;
    let e = create_checked(&rig, member("web", &[], "", "")).await;
    let real = create_checked(&rig, batch("web", &[b.id, d.id], "", "")).await;
    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[b.id, e.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));
    assert_invariants_of(&rig.invariants);

    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[real.id, e.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));
    assert_invariants_of(&rig.invariants);
}

/// Member-on-member deps are allowed (satisfied jointly, so they drop out of
/// the batch's deps) and the members' external deps are unioned onto the batch.
#[tokio::test]
async fn batch_unions_external_deps_and_drops_internal() {
    let Some(rig) = rig().await else { return };

    let ext = create_checked(&rig, member("web", &[], "", "")).await;
    let m1 = create_checked(&rig, member("web", &[ext.id], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[m1.id], "", "")).await;

    let b = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;

    assert_eq!(
        b.deps,
        vec![ext.id],
        "external dep unioned, internal dropped"
    );
    assert_invariants_of(&rig.invariants);
}

/// Unioning the members' additive evaluators is by name: two members carrying a
/// same-named evaluator with DIFFERENT definitions is a creation error (the
/// existing name-collision primitive), while identical duplicates dedup.
#[tokio::test]
async fn batch_eval_union_collision_is_error() {
    let Some(rig) = rig().await else { return };

    let mut m1 = member("web", &[], "", "");
    m1.eval = vec![cmd_eval("ci", "./a.sh")];
    let m1 = create_checked(&rig, m1).await;

    let mut m2 = member("web", &[], "", "");
    m2.eval = vec![cmd_eval("ci", "./b.sh")];
    let m2 = create_checked(&rig, m2).await;

    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[m1.id, m2.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));
    assert_invariants_of(&rig.invariants);

    let mut m3 = member("web", &[], "", "");
    m3.eval = vec![cmd_eval("ci", "./a.sh")];
    let m3 = create_checked(&rig, m3).await;
    let b = create_checked(&rig, batch("web", &[m1.id, m3.id], "", "")).await;
    assert_eq!(b.eval.len(), 1, "identical evaluators dedup: {:?}", b.eval);
    assert_eq!(b.eval[0].name, "ci");
    assert_invariants_of(&rig.invariants);
}

/// The batch work agent sees the batch-aware brief: the preamble plus every
/// member's ticket under its own numbered heading.
#[tokio::test]
async fn batch_prompt_has_preamble_and_all_briefs() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let m1 = create_checked(
        &rig,
        member("web", &[], "Add login", "Build the login form."),
    )
    .await;
    let m2 = create_checked(
        &rig,
        member("web", &[], "Fix header", "The header wraps on mobile."),
    )
    .await;
    let b = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;

    rig.handle.release_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, b.id, JobState::Done).await;

    let prompt = &rig.provider.runs()[0].prompt;
    assert!(
        prompt.contains("This is a job batch"),
        "preamble missing: {prompt}"
    );
    assert!(prompt.contains(&format!("Ticket #{}", m1.id)), "{prompt}");
    assert!(prompt.contains(&format!("Ticket #{}", m2.id)), "{prompt}");
    assert!(prompt.contains("Add login") && prompt.contains("Build the login form."));
    assert!(prompt.contains("Fix header") && prompt.contains("The header wraps on mobile."));
    assert_invariants_of(&rig.invariants);
}

/// The batch's single completion lands every member Done and unblocks a
/// dependent that waited on one of them — exactly as if the member had run
/// individually.
#[tokio::test]
async fn batch_completion_fans_out_and_unblocks_dependent() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);
    commit_work(&rig);

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;

    let dep = create_checked(&rig, member("web", &[m1.id], "", "")).await;

    let b = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;

    let dep_state = rig.handle.release_job("acme", "api", dep.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(
        dep_state,
        JobState::Blocked,
        "dep on a Batched member waits"
    );

    rig.handle.release_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, b.id, JobState::Done).await;

    let m1j = wait_for_state(&rig.store, m1.id, JobState::Done).await;
    let m2j = wait_for_state(&rig.store, m2.id, JobState::Done).await;
    assert!(m1j.completed_at.is_some() && m2j.completed_at.is_some());
    assert_eq!(m1j.batch_id, Some(b.id), "provenance kept after completion");

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(rig._repo.bare_path())
        .args(["log", "-1", "--format=%b", "main"])
        .output()
        .unwrap();
    let body = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        body.starts_with(&format!("Batch of 2 web jobs: #{} #{}", m1.id, m2.id)),
        "squash body must open with the member list: {body:?}"
    );

    wait_for_state(&rig.store, dep.id, JobState::Done).await;
    assert_invariants_of(&rig.invariants);
}

/// Revoking a batch returns its members to Frozen with `batch_id` cleared, so
/// they can be re-batched; the batch itself is Revoked.
#[tokio::test]
async fn revoke_batch_returns_members_frozen_and_rebatchable() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;
    let b = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;

    let cascaded = rig.handle.revoke_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert!(
        cascaded.is_empty(),
        "members are not graph dependents; no cascade: {cascaded:?}"
    );
    assert_eq!(state_of(&rig.store, b.id).await, JobState::Revoked);

    for m in [&m1, &m2] {
        let mj = get_job(&rig.store, m.id).await;
        assert_eq!(mj.state, JobState::Frozen, "member returns to Frozen");
        assert_eq!(mj.batch_id, None, "batch_id cleared");
    }

    let b2 = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;
    assert!(b2.is_batch());
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Batched);
    assert_eq!(get_job(&rig.store, m1.id).await.batch_id, Some(b2.id));
    assert_invariants_of(&rig.invariants);
}

/// The batch is an ordinary job downstream of creation: it gets ONE rework
/// budget for the whole thing. A first eval fail reworks (budget 1), the rework
/// passes, and the batch reaches Done — exactly two work runs, not one per
/// member.
#[tokio::test]
async fn batch_has_one_rework_budget() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    let m1 = create_checked(&rig, member("review", &[], "A", "a")).await;
    let m2 = create_checked(&rig, member("review", &[], "B", "b")).await;
    let b = create_checked(&rig, batch("review", &[m1.id, m2.id], "", "")).await;
    let batch_seq = b.id;

    commit_work(&rig);
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            batch_seq,
            2,
            EvalSubmission {
                pass: false,
                abort: false,
                structured: Some(serde_json::json!({"issues": ["ticket 2 unaddressed"]})),
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    rig.provider.on_run(|_| async {});
    let h = handle.clone();
    rig.provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            batch_seq,
            4,
            EvalSubmission {
                pass: true,
                abort: false,
                structured: None,
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });

    rig.handle.release_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, b.id, JobState::Done).await;

    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 4, "work c1, eval c1, work c2, eval c2");
    assert!(
        runs[2].prompt.contains("This is a job batch"),
        "{}",
        runs[2].prompt
    );
    assert_eq!(runs[2].eval_context.len(), 1);
    assert!(!runs[2].eval_context[0].pass);

    wait_for_state(&rig.store, m1.id, JobState::Done).await;
    wait_for_state(&rig.store, m2.id, JobState::Done).await;
    assert_invariants_of(&rig.invariants);
}

/// A `draft:true` batch stages its member list WITHOUT absorbing it: the batch
/// lands Draft carrying the members, but each member stays Frozen (claimable /
/// batchable elsewhere) and no `job-batched` is emitted. The dep/eval unions
/// are deferred, so the draft record carries none yet.
#[tokio::test]
async fn draft_batch_stages_without_absorbing() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;

    let d = create_checked(&rig, draft_batch("web", &[m1.id, m2.id], "", "")).await;

    assert_eq!(d.state, JobState::Draft, "a draft batch lands Draft");
    assert_eq!(
        d.members,
        vec![m1.id, m2.id],
        "members visible on the draft"
    );
    assert!(d.deps.is_empty(), "unions deferred to finalize/release");

    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Frozen);
    assert_eq!(state_of(&rig.store, m2.id).await, JobState::Frozen);
    assert!(get_job(&rig.store, m1.id).await.batch_id.is_none());
    assert!(
        !event_types(&rig.store)
            .await
            .contains(&"job-batched".into()),
        "a draft batch absorbs nothing at create"
    );
    assert_invariants_of(&rig.invariants);
}

/// While Draft, `edit_members` adds and removes members freely, re-validating
/// each add per-candidate (wrong type / non-Frozen / already-batched /
/// batch-of-batch all reject and leave the list unchanged) and emitting
/// `job-updated` for the changes that stick.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn draft_batch_edit_members_validates_adds() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;
    let d = create_checked(&rig, draft_batch("web", &[m1.id], "", "")).await;

    rig.handle
        .edit_members("acme", "api", d.id, vec![m2.id], vec![])
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    let after_add = rig
        .handle
        .edit_members("acme", "api", d.id, vec![], vec![m1.id])
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(after_add.members, vec![m2.id], "add+remove both applied");
    assert!(
        event_types(&rig.store)
            .await
            .contains(&"job-updated".into()),
        "membership edits emit job-updated"
    );

    let wrong_type = create_checked(&rig, member("code", &[], "", "")).await;
    let released = create_checked(&rig, member("web", &[], "", "")).await;
    rig.handle
        .release_job("acme", "api", released.id)
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);
    let bm1 = create_checked(&rig, member("web", &[], "", "")).await;
    let bm2 = create_checked(&rig, member("web", &[], "", "")).await;
    let other = create_checked(&rig, batch("web", &[bm1.id, bm2.id], "", "")).await;

    for bad in [wrong_type.id, released.id, bm1.id, other.id] {
        assert!(
            matches!(
                rig.handle
                    .edit_members("acme", "api", d.id, vec![bad], vec![])
                    .await,
                Err(CoreError::Validation(_))
            ),
            "adding #{bad} must be a field error"
        );
        assert_invariants_of(&rig.invariants);
        assert_eq!(
            get_job(&rig.store, d.id).await.members,
            vec![m2.id],
            "a rejected add leaves membership unchanged"
        );
    }

    assert!(matches!(
        rig.handle
            .edit_members("acme", "api", d.id, vec![], vec![m2.id])
            .await,
        Err(CoreError::Conflict(_))
    ));
    assert_invariants_of(&rig.invariants);
}

/// `edit_members` is Draft-only: a committed (Frozen) batch's membership is
/// never mutated in place.
#[tokio::test]
async fn edit_members_on_non_draft_conflicts() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;
    let extra = create_checked(&rig, member("web", &[], "", "")).await;
    let b = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;

    assert!(matches!(
        rig.handle
            .edit_members("acme", "api", b.id, vec![extra.id], vec![])
            .await,
        Err(CoreError::Conflict(_))
    ));
    assert_invariants_of(&rig.invariants);
}

/// Finalizing a Draft batch absorbs its members (Frozen→Batched), recomputes
/// the external-dep union and the auto-index description, and parks the batch
/// Frozen (re-batchable) — exactly what an atomic create would have written.
#[tokio::test]
async fn draft_batch_finalize_absorbs_and_computes_unions() {
    let Some(rig) = rig().await else { return };

    let ext = create_checked(&rig, member("web", &[], "", "")).await;
    let m1 = create_checked(&rig, member("web", &[ext.id], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[m1.id], "", "")).await;
    let d = create_checked(&rig, draft_batch("web", &[m1.id, m2.id], "", "")).await;

    rig.handle.finalize_job("acme", "api", d.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let batch = get_job(&rig.store, d.id).await;
    assert_eq!(batch.state, JobState::Frozen, "finalize parks Frozen");
    assert_eq!(
        batch.deps,
        vec![ext.id],
        "external dep unioned, internal dropped"
    );
    assert_eq!(
        batch.description,
        format!("Batch of 2 web jobs: #{} #{}", m1.id, m2.id),
        "auto-index description computed at finalize"
    );
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Batched);
    assert_eq!(state_of(&rig.store, m2.id).await, JobState::Batched);
    assert_eq!(get_job(&rig.store, m1.id).await.batch_id, Some(d.id));
    assert_invariants_of(&rig.invariants);
}

/// If a member is no longer batchable when a Draft batch is finalized (here one
/// was released meanwhile), finalize fails with a field error, the batch stays
/// Draft, and NOTHING is absorbed.
#[tokio::test]
async fn draft_batch_finalize_stale_member_stays_draft() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;
    let d = create_checked(&rig, draft_batch("web", &[m1.id, m2.id], "", "")).await;

    rig.handle.release_job("acme", "api", m1.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    assert!(matches!(
        rig.handle.finalize_job("acme", "api", d.id).await,
        Err(CoreError::Validation(_))
    ));
    assert_invariants_of(&rig.invariants);
    assert_eq!(
        state_of(&rig.store, d.id).await,
        JobState::Draft,
        "a failed finalize leaves the batch Draft"
    );
    assert_eq!(
        state_of(&rig.store, m2.id).await,
        JobState::Frozen,
        "nothing is absorbed on a failed finalize"
    );
    assert_invariants_of(&rig.invariants);
}

/// Releasing a Draft batch absorbs its members and schedules the batch: the
/// members land Batched, and (deps all Done) the batch runs to Done, fanning
/// completion out to every member.
#[tokio::test]
async fn draft_batch_release_absorbs_and_runs() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let m1 = create_checked(&rig, member("web", &[], "A", "first")).await;
    let m2 = create_checked(&rig, member("web", &[], "B", "second")).await;
    let d = create_checked(&rig, draft_batch("web", &[m1.id, m2.id], "", "")).await;

    rig.handle.release_job("acme", "api", d.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    wait_for_state(&rig.store, d.id, JobState::Done).await;
    wait_for_state(&rig.store, m1.id, JobState::Done).await;
    wait_for_state(&rig.store, m2.id, JobState::Done).await;
    assert_invariants_of(&rig.invariants);
}

/// Full round trip: a committed batch is reopened (Frozen→Draft un-absorbs its
/// members), its membership is edited, and re-finalizing re-absorbs the NEW set
/// with the unions recomputed against it.
#[tokio::test]
async fn frozen_draft_edit_finalize_round_trip() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;
    let ext = create_checked(&rig, member("web", &[], "", "")).await;
    let m3 = create_checked(&rig, member("web", &[ext.id], "", "")).await;

    let b = create_checked(&rig, batch("web", &[m1.id, m2.id], "", "")).await;
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Batched);

    rig.handle.draft_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);
    assert_eq!(state_of(&rig.store, b.id).await, JobState::Draft);
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Frozen);
    assert_eq!(state_of(&rig.store, m2.id).await, JobState::Frozen);
    assert_eq!(get_job(&rig.store, m1.id).await.batch_id, None);

    rig.handle
        .edit_members("acme", "api", b.id, vec![m3.id], vec![m2.id])
        .await
        .unwrap();
    assert_invariants_of(&rig.invariants);

    rig.handle.finalize_job("acme", "api", b.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    let batch = get_job(&rig.store, b.id).await;
    assert_eq!(batch.members, vec![m1.id, m3.id], "membership change stuck");
    assert_eq!(
        batch.deps,
        vec![ext.id],
        "unions recomputed for the new set"
    );
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Batched);
    assert_eq!(state_of(&rig.store, m3.id).await, JobState::Batched);
    assert_eq!(
        state_of(&rig.store, m2.id).await,
        JobState::Frozen,
        "the removed member is no longer batched"
    );
    assert_invariants_of(&rig.invariants);
}

/// Revoking a Draft batch is trivial — nothing was absorbed, so its would-be
/// members are left exactly as they were (Frozen, unbatched).
#[tokio::test]
async fn draft_batch_revoke_leaves_members_untouched() {
    let Some(rig) = rig().await else { return };

    let m1 = create_checked(&rig, member("web", &[], "", "")).await;
    let m2 = create_checked(&rig, member("web", &[], "", "")).await;
    let d = create_checked(&rig, draft_batch("web", &[m1.id, m2.id], "", "")).await;

    rig.handle.revoke_job("acme", "api", d.id).await.unwrap();
    assert_invariants_of(&rig.invariants);

    assert_eq!(state_of(&rig.store, d.id).await, JobState::Revoked);
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Frozen);
    assert_eq!(state_of(&rig.store, m2.id).await, JobState::Frozen);
    assert!(get_job(&rig.store, m1.id).await.batch_id.is_none());
    assert!(
        !event_types(&rig.store)
            .await
            .contains(&"job-unbatched".into()),
        "a draft batch never absorbed, so revoke un-absorbs nothing"
    );
    assert_invariants_of(&rig.invariants);
}

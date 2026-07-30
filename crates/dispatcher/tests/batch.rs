//! Tier-2 tests for job batches (spec §2.1 batches): a batch is a job that
//! absorbs N same-type Frozen members, produces one branch, is evaluated under
//! the union of their criteria, and whose single completion fans out to every
//! member. Covers the creation-validation matrix, the batch-aware prompt, the
//! completion fan-out (members Done + a dependent unblocks), revoke → members
//! back to Frozen (re-batchable), the eval-union collision error, and the
//! single rework budget shared across the whole batch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{
    Core, CoreConfig, CoreError, CoreHandle, CreateJobRequest, EvalSubmission, spawn,
};
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::{Evaluator, EvaluatorType, JobState};

// A plain agent job with no evaluators: a clean work exit takes the job (or
// batch) straight to Done, so the harness can assert lifecycle without
// scripting eval verdicts.
const WEB: &str = r#"
name: web
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
"#;

// An agent job with an agent evaluator and a single rework cycle: used to show
// the batch gets ONE rework budget for the whole thing, like any other job.
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

// A second type, distinct from `web`, so the "type must match" rule can be
// exercised with a real member of the wrong type.
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
    let handle = spawn(core);
    Some(Rig {
        _server: server,
        _repo: repo,
        store,
        provider,
        handle,
    })
}

/// A member/ordinary job creation request (no `members` payload).
fn member(r#type: &str, deps: &[u64], title: &str, description: &str) -> CreateJobRequest {
    CreateJobRequest {
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
        timeout: None,
        model: None,
        factory: None,
        inputs: Default::default(),
        draft: false,
    }
}

/// A batch creation request over the given member seqs.
fn batch(r#type: &str, members: &[u64], title: &str, description: &str) -> CreateJobRequest {
    let mut req = member(r#type, &[], title, description);
    req.members = members.to_vec();
    req
}

/// A **draft** batch creation request: stages the member list without absorbing
/// it (spec §2.1 draft batches).
fn draft_batch(r#type: &str, members: &[u64], title: &str, description: &str) -> CreateJobRequest {
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
    // Watch-based wait (#206 principle 3): value-inspecting, hard timeout.
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

// ── Creation validation matrix ───────────────────────────────────────────

/// A batch absorbs its members: each goes Frozen→Batched with `batch_id` set,
/// the batch lands Frozen carrying the member list, and its description defaults
/// to an auto-index naming every member.
#[tokio::test]
async fn batch_creation_absorbs_members() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();

    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

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
}

/// Every creation-rule violation is rejected: fewer than 2 members, a
/// non-Frozen member, a wrong-type member, an already-batched member, and a
/// batch-of-a-batch.
#[tokio::test]
async fn batch_creation_validation_rejects() {
    let Some(rig) = rig().await else { return };

    let a = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let b = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();

    // Fewer than 2 members.
    assert!(matches!(
        rig.handle.create_job(batch("web", &[a.id], "", "")).await,
        Err(CoreError::Validation(_))
    ));

    // Wrong type: a `code` member cannot join a `web` batch.
    let c = rig
        .handle
        .create_job(member("code", &[], "", ""))
        .await
        .unwrap();
    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[a.id, c.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));

    // Non-Frozen member: release `a` (→ Ready, no deps), then it cannot batch.
    rig.handle.release_job("acme", "api", a.id).await.unwrap();
    assert_ne!(state_of(&rig.store, a.id).await, JobState::Frozen);
    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[a.id, b.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));

    // Already-batched member: batch [b, d], then try to reuse b.
    let d = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let e = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let real = rig
        .handle
        .create_job(batch("web", &[b.id, d.id], "", ""))
        .await
        .unwrap();
    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[b.id, e.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));

    // Batch-of-a-batch: the batch job itself (Frozen, type web) cannot be a
    // member of another batch.
    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[real.id, e.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));
}

/// Member-on-member deps are allowed (satisfied jointly, so they drop out of
/// the batch's deps) and the members' external deps are unioned onto the batch.
#[tokio::test]
async fn batch_unions_external_deps_and_drops_internal() {
    let Some(rig) = rig().await else { return };

    // An external upstream both members transitively depend on.
    let ext = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    // m1 depends on the external job; m2 depends on m1 (member-on-member).
    let m1 = rig
        .handle
        .create_job(member("web", &[ext.id], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[m1.id], "", ""))
        .await
        .unwrap();

    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    // Only the external dep survives; the intra-batch m1→m2 edge is dropped.
    assert_eq!(
        b.deps,
        vec![ext.id],
        "external dep unioned, internal dropped"
    );
}

/// Unioning the members' additive evaluators is by name: two members carrying a
/// same-named evaluator with DIFFERENT definitions is a creation error (the
/// existing name-collision primitive), while identical duplicates dedup.
#[tokio::test]
async fn batch_eval_union_collision_is_error() {
    let Some(rig) = rig().await else { return };

    let mut m1 = member("web", &[], "", "");
    m1.eval = vec![cmd_eval("ci", "./a.sh")];
    let m1 = rig.handle.create_job(m1).await.unwrap();

    // Same-name, different definition → collision.
    let mut m2 = member("web", &[], "", "");
    m2.eval = vec![cmd_eval("ci", "./b.sh")];
    let m2 = rig.handle.create_job(m2).await.unwrap();

    assert!(matches!(
        rig.handle
            .create_job(batch("web", &[m1.id, m2.id], "", ""))
            .await,
        Err(CoreError::Validation(_))
    ));

    // Same-name, IDENTICAL definition → deduped to one, batch created clean.
    let mut m3 = member("web", &[], "", "");
    m3.eval = vec![cmd_eval("ci", "./a.sh")];
    let m3 = rig.handle.create_job(m3).await.unwrap();
    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m3.id], "", ""))
        .await
        .unwrap();
    assert_eq!(b.eval.len(), 1, "identical evaluators dedup: {:?}", b.eval);
    assert_eq!(b.eval[0].name, "ci");
}

// ── Prompt ────────────────────────────────────────────────────────────────

/// The batch work agent sees the batch-aware brief: the preamble plus every
/// member's ticket under its own numbered heading.
#[tokio::test]
async fn batch_prompt_has_preamble_and_all_briefs() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let m1 = rig
        .handle
        .create_job(member("web", &[], "Add login", "Build the login form."))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member(
            "web",
            &[],
            "Fix header",
            "The header wraps on mobile.",
        ))
        .await
        .unwrap();
    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    rig.handle.release_job("acme", "api", b.id).await.unwrap();
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
}

// ── Completion fan-out ──────────────────────────────────────────────────────

/// The batch's single completion lands every member Done and unblocks a
/// dependent that waited on one of them — exactly as if the member had run
/// individually.
#[tokio::test]
async fn batch_completion_fans_out_and_unblocks_dependent() {
    let Some(rig) = rig().await else { return };
    // Two work runs: the batch's own agent, then the unblocked dependent's.
    commit_work(&rig);
    commit_work(&rig);

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();

    // An external dependent on m1 (a single member), released → Blocked.
    let dep = rig
        .handle
        .create_job(member("web", &[m1.id], "", ""))
        .await
        .unwrap();

    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    let dep_state = rig.handle.release_job("acme", "api", dep.id).await.unwrap();
    assert_eq!(
        dep_state,
        JobState::Blocked,
        "dep on a Batched member waits"
    );

    // Run the batch to completion; its merge completes both members.
    rig.handle.release_job("acme", "api", b.id).await.unwrap();
    wait_for_state(&rig.store, b.id, JobState::Done).await;

    let m1j = wait_for_state(&rig.store, m1.id, JobState::Done).await;
    let m2j = wait_for_state(&rig.store, m2.id, JobState::Done).await;
    assert!(m1j.completed_at.is_some() && m2j.completed_at.is_some());
    assert_eq!(m1j.batch_id, Some(b.id), "provenance kept after completion");

    // The single squash's commit body opens with the member list, so git
    // history records which tickets this one merge closed (spec §2.1 batches).
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

    // The dependent unblocks off the individual member's Done and runs itself.
    wait_for_state(&rig.store, dep.id, JobState::Done).await;
}

// ── Revoke ──────────────────────────────────────────────────────────────────

/// Revoking a batch returns its members to Frozen with `batch_id` cleared, so
/// they can be re-batched; the batch itself is Revoked.
#[tokio::test]
async fn revoke_batch_returns_members_frozen_and_rebatchable() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    let cascaded = rig.handle.revoke_job("acme", "api", b.id).await.unwrap();
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

    // Re-batchable: the freed members form a fresh batch.
    let b2 = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();
    assert!(b2.is_batch());
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Batched);
    assert_eq!(get_job(&rig.store, m1.id).await.batch_id, Some(b2.id));
}

// ── Rework budget ───────────────────────────────────────────────────────────

/// The batch is an ordinary job downstream of creation: it gets ONE rework
/// budget for the whole thing. A first eval fail reworks (budget 1), the rework
/// passes, and the batch reaches Done — exactly two work runs, not one per
/// member.
#[tokio::test]
async fn batch_has_one_rework_budget() {
    let Some(rig) = rig().await else { return };
    let handle = rig.handle.clone();

    let m1 = rig
        .handle
        .create_job(member("review", &[], "A", "a"))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("review", &[], "B", "b"))
        .await
        .unwrap();
    let b = rig
        .handle
        .create_job(batch("review", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();
    let batch_seq = b.id;

    // Run order (task ids sequential within the batch job): work c1 (task 1),
    // eval c1 fails (task 2), work c2 (task 3), eval c2 passes (task 4).
    commit_work(&rig); // work cycle 1 commits
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
    rig.provider.on_run(|_| async {}); // work cycle 2 (rework)
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
    wait_for_state(&rig.store, b.id, JobState::Done).await;

    // One work agent per cycle (2 total) plus one evaluator per cycle (2) — the
    // budget is the batch's, not per-member.
    let runs = rig.provider.runs();
    assert_eq!(runs.len(), 4, "work c1, eval c1, work c2, eval c2");
    // The rework work run carried the batch brief AND the prior eval findings.
    assert!(
        runs[2].prompt.contains("This is a job batch"),
        "{}",
        runs[2].prompt
    );
    assert_eq!(runs[2].eval_context.len(), 1);
    assert!(!runs[2].eval_context[0].pass);

    // Both members completed via the batch.
    wait_for_state(&rig.store, m1.id, JobState::Done).await;
    wait_for_state(&rig.store, m2.id, JobState::Done).await;
}

// ── Draft batches (spec §2.1 draft batches) ─────────────────────────────────

/// A `draft:true` batch stages its member list WITHOUT absorbing it: the batch
/// lands Draft carrying the members, but each member stays Frozen (claimable /
/// batchable elsewhere) and no `job-batched` is emitted. The dep/eval unions
/// are deferred, so the draft record carries none yet.
#[tokio::test]
async fn draft_batch_stages_without_absorbing() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();

    let d = rig
        .handle
        .create_job(draft_batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    assert_eq!(d.state, JobState::Draft, "a draft batch lands Draft");
    assert_eq!(
        d.members,
        vec![m1.id, m2.id],
        "members visible on the draft"
    );
    assert!(d.deps.is_empty(), "unions deferred to finalize/release");

    // Members are NOT absorbed: still Frozen, no batch_id, no job-batched event.
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Frozen);
    assert_eq!(state_of(&rig.store, m2.id).await, JobState::Frozen);
    assert!(get_job(&rig.store, m1.id).await.batch_id.is_none());
    assert!(
        !event_types(&rig.store)
            .await
            .contains(&"job-batched".into()),
        "a draft batch absorbs nothing at create"
    );
}

/// While Draft, `edit_members` adds and removes members freely, re-validating
/// each add per-candidate (wrong type / non-Frozen / already-batched /
/// batch-of-batch all reject and leave the list unchanged) and emitting
/// `job-updated` for the changes that stick.
#[tokio::test]
// TODO(style): oversized tier-2 test — split when this file is next touched.
#[allow(clippy::too_many_lines)]
async fn draft_batch_edit_members_validates_adds() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let d = rig
        .handle
        .create_job(draft_batch("web", &[m1.id], "", ""))
        .await
        .unwrap();

    // Add m2, then remove m1: the membership edits stick.
    rig.handle
        .edit_members("acme", "api", d.id, vec![m2.id], vec![])
        .await
        .unwrap();
    let after_add = rig
        .handle
        .edit_members("acme", "api", d.id, vec![], vec![m1.id])
        .await
        .unwrap();
    assert_eq!(after_add.members, vec![m2.id], "add+remove both applied");
    assert!(
        event_types(&rig.store)
            .await
            .contains(&"job-updated".into()),
        "membership edits emit job-updated"
    );

    // Validation matrix on adds — each rejects and leaves membership == [m2].
    let wrong_type = rig
        .handle
        .create_job(member("code", &[], "", ""))
        .await
        .unwrap();
    let released = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    rig.handle
        .release_job("acme", "api", released.id)
        .await
        .unwrap();
    // An already-batched member: absorbed by a separate real batch.
    let bm1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let bm2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let other = rig
        .handle
        .create_job(batch("web", &[bm1.id, bm2.id], "", ""))
        .await
        .unwrap();

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
        assert_eq!(
            get_job(&rig.store, d.id).await.members,
            vec![m2.id],
            "a rejected add leaves membership unchanged"
        );
    }

    // Emptying the batch is refused — it would erase batch identity (§2.1).
    assert!(matches!(
        rig.handle
            .edit_members("acme", "api", d.id, vec![], vec![m2.id])
            .await,
        Err(CoreError::Conflict(_))
    ));
}

/// `edit_members` is Draft-only: a committed (Frozen) batch's membership is
/// never mutated in place.
#[tokio::test]
async fn edit_members_on_non_draft_conflicts() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let extra = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    // A non-draft (Frozen) batch.
    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    assert!(matches!(
        rig.handle
            .edit_members("acme", "api", b.id, vec![extra.id], vec![])
            .await,
        Err(CoreError::Conflict(_))
    ));
}

/// Finalizing a Draft batch absorbs its members (Frozen→Batched), recomputes
/// the external-dep union and the auto-index description, and parks the batch
/// Frozen (re-batchable) — exactly what an atomic create would have written.
#[tokio::test]
async fn draft_batch_finalize_absorbs_and_computes_unions() {
    let Some(rig) = rig().await else { return };

    let ext = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m1 = rig
        .handle
        .create_job(member("web", &[ext.id], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[m1.id], "", ""))
        .await
        .unwrap();
    let d = rig
        .handle
        .create_job(draft_batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    rig.handle.finalize_job("acme", "api", d.id).await.unwrap();

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
}

/// If a member is no longer batchable when a Draft batch is finalized (here one
/// was released meanwhile), finalize fails with a field error, the batch stays
/// Draft, and NOTHING is absorbed.
#[tokio::test]
async fn draft_batch_finalize_stale_member_stays_draft() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let d = rig
        .handle
        .create_job(draft_batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    // m1 is released (→ Ready) while the draft composes: it can no longer batch.
    rig.handle.release_job("acme", "api", m1.id).await.unwrap();

    assert!(matches!(
        rig.handle.finalize_job("acme", "api", d.id).await,
        Err(CoreError::Validation(_))
    ));
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
}

/// Releasing a Draft batch absorbs its members and schedules the batch: the
/// members land Batched, and (deps all Done) the batch runs to Done, fanning
/// completion out to every member.
#[tokio::test]
async fn draft_batch_release_absorbs_and_runs() {
    let Some(rig) = rig().await else { return };
    commit_work(&rig);

    let m1 = rig
        .handle
        .create_job(member("web", &[], "A", "first"))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "B", "second"))
        .await
        .unwrap();
    let d = rig
        .handle
        .create_job(draft_batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    rig.handle.release_job("acme", "api", d.id).await.unwrap();
    wait_for_state(&rig.store, d.id, JobState::Done).await;
    wait_for_state(&rig.store, m1.id, JobState::Done).await;
    wait_for_state(&rig.store, m2.id, JobState::Done).await;
}

/// Full round trip: a committed batch is reopened (Frozen→Draft un-absorbs its
/// members), its membership is edited, and re-finalizing re-absorbs the NEW set
/// with the unions recomputed against it.
#[tokio::test]
async fn frozen_draft_edit_finalize_round_trip() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let ext = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m3 = rig
        .handle
        .create_job(member("web", &[ext.id], "", ""))
        .await
        .unwrap();

    // A committed batch [m1, m2] — both absorbed.
    let b = rig
        .handle
        .create_job(batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Batched);

    // Reopen for editing: members return to Frozen (un-absorbed).
    rig.handle.draft_job("acme", "api", b.id).await.unwrap();
    assert_eq!(state_of(&rig.store, b.id).await, JobState::Draft);
    assert_eq!(state_of(&rig.store, m1.id).await, JobState::Frozen);
    assert_eq!(state_of(&rig.store, m2.id).await, JobState::Frozen);
    assert_eq!(get_job(&rig.store, m1.id).await.batch_id, None);

    // Swap m2 for m3 (which carries an external dep).
    rig.handle
        .edit_members("acme", "api", b.id, vec![m3.id], vec![m2.id])
        .await
        .unwrap();

    rig.handle.finalize_job("acme", "api", b.id).await.unwrap();

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
}

/// Revoking a Draft batch is trivial — nothing was absorbed, so its would-be
/// members are left exactly as they were (Frozen, unbatched).
#[tokio::test]
async fn draft_batch_revoke_leaves_members_untouched() {
    let Some(rig) = rig().await else { return };

    let m1 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let m2 = rig
        .handle
        .create_job(member("web", &[], "", ""))
        .await
        .unwrap();
    let d = rig
        .handle
        .create_job(draft_batch("web", &[m1.id, m2.id], "", ""))
        .await
        .unwrap();

    rig.handle.revoke_job("acme", "api", d.id).await.unwrap();

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
}

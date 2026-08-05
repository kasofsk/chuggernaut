//! The `req.jobs.*` family (spec §6.2): the operator's whole job surface —
//! create, the Draft edit verbs, release/revoke, claims, advisory triage, and
//! the reads behind the job page. Mutations go through the core actor because
//! it is the single writer of job records; reads go straight to the store.
//!
//! The wire bodies live here rather than in `types` because they are the HTTP
//! bridge's shapes, not domain data: every field defaults so an older client
//! keeps working, and the semantic bounds (the `cover_html` cap) are checked
//! after parsing, which is what separates a 400 from a 422.
//!
//! - **Accepts:** `req.jobs.{create,update,draft,finalize,get,list,release,
//!   revoke,members,groups,approval,claim,unclaim,triage,criteria}.{owner}.{project}[.{seq}]`.
//! - **Emits:** the matching `CoreHandle` call, and a reply built by
//!   [`super::jobs_reply`] (the derived job view, the jobs list with its
//!   channel-progress join, or the resolved criteria).
//! - **Guarantees:** no state decision of its own — every verb is a `CoreHandle`
//!   call whose result is re-read from KV, so the reply is the committed record
//!   rather than an optimistic echo.
//! - **Spec:** §6.1, §6.2, §2.1, §1.2.

use super::jobs_reply::{fetch_job, job_criteria, latest_channel_updates};
use super::reply::{bad_request, error_reply, ok_reply, unprocessable};
use crate::core::{CoreHandle, CreateSpec, UpdateJobRequest};
use std::collections::BTreeMap;
use std::sync::Arc;
use store::NatsStore;
use vcs::RepoManager;

/// The ports every `req.jobs.*` verb reaches for: KV reads, the single-writer
/// actor, and the repo (criteria resolution). Bundled so each verb handler
/// takes one context argument instead of three.
struct JobsCtx {
    store: NatsStore,
    handle: CoreHandle,
    repos: Arc<RepoManager>,
}

/// Byte cap on a job's optional `cover_html` (§1.1). The one job field with a
/// size bound today: it can carry a whole rendered page, so a runaway value
/// would bloat every job-list reply. ~256 KiB is generous for a self-contained
/// styled page yet keeps records sane. Over → 422.
pub(super) const COVER_HTML_MAX_BYTES: usize = 256 * 1024;

/// True when a supplied `cover_html` exceeds [`COVER_HTML_MAX_BYTES`]. None
/// (no cover) is always fine.
fn cover_too_large(cover_html: &Option<String>) -> bool {
    cover_html
        .as_ref()
        .is_some_and(|h| h.len() > COVER_HTML_MAX_BYTES)
}

/// The 422 body for a supplied `inputs` map that fails the §2.2 creation pass —
/// count, name shape, charset, length (`types::inputs::check_supplied`). `None`
/// when the map is well-shaped.
///
/// These are exactly the rules that need no job type file, which is why they are
/// answered here: the operator gets them back on the form immediately, and they
/// are the injection-relevant ones (design #311 Decision 3). Whether a name is
/// *declared*, a `required` input is present, or a value matches its
/// `pattern`/`values` is release-time — the type is read at a ref, not at create.
fn inputs_shape_error(inputs: &BTreeMap<String, String>) -> Option<String> {
    types::check_supplied(inputs)
        .err()
        .map(|e| format!("inputs: {e}"))
}

/// The 422 body for a supplied `groups` list that fails the §1.1 bounds — count,
/// name shape, name length, duplicates (`types::groups::check_groups`). `None`
/// when the list is well-shaped.
///
/// Unlike `inputs`, this is the **only** pass there is: a group has no
/// declaration to be checked against and no registry to exist in (design #321
/// Decision 2), so nothing is deferred to release. The add/remove endpoint runs
/// the same check over its *result*, which is why the rule lives in `types`.
fn groups_shape_error(groups: &[String]) -> Option<String> {
    types::check_groups(groups)
        .err()
        .map(|e| format!("groups: {e}"))
}

/// Wire body for `req.jobs.create` (spec §6.2 POST .../jobs).
#[derive(serde::Deserialize)]
struct CreateJobBody {
    r#type: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    /// Optional rich cover page for the UI (§1.1, §4.3). Presentational only —
    /// never enters an agent prompt. Size-capped ([`COVER_HTML_MAX_BYTES`], 422).
    #[serde(default)]
    cover_html: Option<String>,
    /// Upstream job ids this job depends on (must be Done before it starts).
    #[serde(default)]
    deps: Vec<u64>,
    /// Member job seqs to absorb into a batch (spec §2.1 batches). Non-empty →
    /// this request creates a batch instead of an ordinary job.
    #[serde(default)]
    members: Vec<u64>,
    #[serde(default)]
    knowledge_tags: Vec<String>,
    /// Additive per-job evaluators (docs/reference/design-lifecycle.md); validated at release.
    #[serde(default)]
    eval: Vec<types::Evaluator>,
    /// Gate the job on an explicit operator sign-off (§1.1 require-approval):
    /// criteria resolution adds one required Human evaluator, staged last.
    #[serde(default)]
    require_approval: bool,
    /// Optional per-job work-task timeout override (duration string, §1.1);
    /// layers over the type's `resources.task_timeout` for Work tasks only.
    /// Parseability validated at release. Absent → the type default applies.
    #[serde(default)]
    timeout: Option<String>,
    /// Optional per-job Work agent model override (§12.4); wins over the job
    /// type, project, and platform defaults. Absent → the resolution chain applies.
    #[serde(default)]
    model: Option<String>,
    /// The values this job supplies for its type's declared `inputs:` (§1.1).
    /// Shape-checked here (422 via [`inputs_shape_error`]); the semantic check
    /// against the declaration happens at release. Absent → none supplied, which
    /// is what every job of a type declaring no inputs sends.
    #[serde(default)]
    inputs: BTreeMap<String, String>,
    /// What this job is part of (§1.1 `groups`, design #321). Shape-checked here
    /// (422 via [`groups_shape_error`]) and nowhere else — there is no
    /// release-time pass. Absent → ungrouped, and a label can be added later in
    /// any state via `req.jobs.groups.*`.
    #[serde(default)]
    groups: Vec<String>,
    /// Land the job in Draft instead of Frozen (§2.1) so it can be edited
    /// before release. Absent/false preserves today's create-lands-Frozen path.
    #[serde(default)]
    draft: bool,
}

/// Wire body for `req.jobs.update` (spec §6.2 PATCH .../jobs/{seq}): the same
/// shape as create, minus the `draft` flag (an update never changes the state).
#[derive(serde::Deserialize)]
struct UpdateJobBody {
    r#type: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    cover_html: Option<String>,
    #[serde(default)]
    deps: Vec<u64>,
    #[serde(default)]
    knowledge_tags: Vec<String>,
    #[serde(default)]
    eval: Vec<types::Evaluator>,
    #[serde(default)]
    require_approval: bool,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// Full-field replace like every other field, and Draft-only: after release
    /// `Job::inputs` is immutable (§2.1, design #311 Decision 6).
    #[serde(default)]
    inputs: BTreeMap<String, String>,
    /// Full-field replace like every other field here. Unlike the rest, the
    /// field stays writable after release — through `req.jobs.groups.*`, which
    /// is add/remove rather than a replace (design #321 Decision 5).
    #[serde(default)]
    groups: Vec<String>,
}

/// Wire body for `req.jobs.groups` (design #321 Decision 5): the group labels to
/// add to / remove from a job, in any state. Both default empty so a caller can
/// add-only or remove-only.
#[derive(serde::Deserialize)]
struct GroupsBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

/// Wire body for `req.jobs.approval` (spec §1.1 require-approval): whether the
/// job needs an operator sign-off before it can pass evaluation.
#[derive(serde::Deserialize)]
struct ApprovalBody {
    require: bool,
}

/// Wire body for `req.jobs.members` (spec §2.1 draft batches): the seqs to
/// add to / remove from a Draft batch's member list. Both default empty so a
/// caller can add-only or remove-only.
#[derive(serde::Deserialize)]
struct MembersBody {
    #[serde(default)]
    add: Vec<u64>,
    #[serde(default)]
    remove: Vec<u64>,
}

/// Subscribe `req.jobs.>` — one subscription owns the whole family, so a verb
/// can never be answered twice.
pub(super) async fn spawn_jobs_handler(
    store: &NatsStore,
    handle: CoreHandle,
    repos: Arc<RepoManager>,
) -> store::Result<()> {
    let mut jobs_sub = store.subscribe_requests("req.jobs.>").await?;
    let ctx = JobsCtx {
        store: store.clone(),
        handle,
        repos,
    };
    tokio::spawn(async move {
        while let Some(req) = jobs_sub.next().await {
            let Some((verb, owner, project)) = super::subject_target(&req.subject) else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            let seq = req.subject.split('.').nth(5).and_then(|s| s.parse().ok());
            let body = jobs_dispatch(&ctx, verb, owner, project, seq, &req.payload).await;
            req.respond(body).await;
        }
    });
    Ok(())
}

/// Route one `req.jobs.*` request by verb. A verb that does not match its
/// addressing shape (a seq where none belongs, or none where one is required)
/// falls through to the same 400 an unknown verb gets.
async fn jobs_dispatch(
    ctx: &JobsCtx,
    verb: &str,
    owner: &str,
    project: &str,
    seq: Option<u64>,
    payload: &[u8],
) -> Vec<u8> {
    match (verb, seq) {
        ("create", None) => jobs_create(ctx, owner, project, payload).await,
        ("update", Some(seq)) => jobs_update(ctx, owner, project, seq, payload).await,
        ("get", Some(seq)) => fetch_job(&ctx.store, owner, project, seq).await,
        ("list", None) => jobs_list(&ctx.store, owner, project).await,
        ("members", Some(seq)) => jobs_members(ctx, owner, project, seq, payload).await,
        ("groups", Some(seq)) => jobs_groups(ctx, owner, project, seq, payload).await,
        ("approval", Some(seq)) => jobs_approval(ctx, owner, project, seq, payload).await,
        ("criteria", Some(seq)) => job_criteria(&ctx.store, &ctx.repos, owner, project, seq).await,
        (verb, Some(seq)) => jobs_transition(ctx, verb, owner, project, seq)
            .await
            .unwrap_or_else(|| bad_request("malformed subject")),
        _ => bad_request("malformed subject"),
    }
}

/// The verbs whose whole reply is "ask the actor, then re-read the record":
/// the §2.1 lifecycle edits (draft/finalize/release/revoke), the §1.2 claims,
/// and advisory triage (which changes nothing, so the re-read is the job
/// as-is). `None` when `verb` is not one of them.
async fn jobs_transition(
    ctx: &JobsCtx,
    verb: &str,
    owner: &str,
    project: &str,
    seq: u64,
) -> Option<Vec<u8>> {
    let handle = &ctx.handle;
    let outcome = match verb {
        "draft" => handle.draft_job(owner, project, seq).await,
        "finalize" => handle.finalize_job(owner, project, seq).await,
        "release" => handle.release_job(owner, project, seq).await.map(|_| ()),
        "revoke" => handle.revoke_job(owner, project, seq).await.map(|_| ()),
        "claim" => handle.claim_job(owner, project, seq).await,
        "unclaim" => handle.unclaim_job(owner, project, seq).await,
        "triage" => handle.triage_job(owner, project, seq).await,
        _ => return None,
    };
    Some(match outcome {
        Err(e) => error_reply(&e),
        Ok(()) => fetch_job(&ctx.store, owner, project, seq).await,
    })
}

/// `req.jobs.create` — the §6.2 POST body, cover cap, input shape, then the actor.
async fn jobs_create(ctx: &JobsCtx, owner: &str, project: &str, payload: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<CreateJobBody>(payload) {
        Err(e) => bad_request(&e.to_string()),
        Ok(b) if cover_too_large(&b.cover_html) => unprocessable(&format!(
            "cover_html exceeds the {COVER_HTML_MAX_BYTES}-byte limit"
        )),
        Ok(b) => match inputs_shape_error(&b.inputs).or_else(|| groups_shape_error(&b.groups)) {
            Some(message) => unprocessable(&message),
            None => {
                let create = CreateSpec {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    r#type: b.r#type,
                    title: b.title,
                    description: b.description,
                    cover_html: b.cover_html,
                    deps: b.deps,
                    members: b.members,
                    knowledge_tags: b.knowledge_tags,
                    eval: b.eval,
                    require_approval: b.require_approval,
                    timeout: b.timeout,
                    model: b.model,
                    inputs: b.inputs,
                    groups: b.groups,
                    factory: None,
                    schedule: None,
                    draft: b.draft,
                };
                match ctx.handle.create_job(create).await {
                    Ok(job) => ok_reply(&job),
                    Err(e) => error_reply(&e),
                }
            }
        },
    }
}

/// `req.jobs.update` — the §2.1 draft edit: full-field replace of a Draft job
/// (409 in any other state). Same body shape as create.
async fn jobs_update(
    ctx: &JobsCtx,
    owner: &str,
    project: &str,
    seq: u64,
    payload: &[u8],
) -> Vec<u8> {
    match serde_json::from_slice::<UpdateJobBody>(payload) {
        Err(e) => bad_request(&e.to_string()),
        Ok(b) if cover_too_large(&b.cover_html) => unprocessable(&format!(
            "cover_html exceeds the {COVER_HTML_MAX_BYTES}-byte limit"
        )),
        Ok(b) => match inputs_shape_error(&b.inputs).or_else(|| groups_shape_error(&b.groups)) {
            Some(message) => unprocessable(&message),
            None => {
                let update = UpdateJobRequest {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    seq,
                    r#type: b.r#type,
                    title: b.title,
                    description: b.description,
                    cover_html: b.cover_html,
                    deps: b.deps,
                    knowledge_tags: b.knowledge_tags,
                    eval: b.eval,
                    require_approval: b.require_approval,
                    timeout: b.timeout,
                    model: b.model,
                    inputs: b.inputs,
                    groups: b.groups,
                };
                match ctx.handle.update_job(update).await {
                    Err(e) => error_reply(&e),
                    Ok(_) => fetch_job(&ctx.store, owner, project, seq).await,
                }
            }
        },
    }
}

/// `req.jobs.members` — §2.1 draft batches: add/remove a Draft batch's members
/// while composing it (409 in any other state). Reply is the updated job.
async fn jobs_members(
    ctx: &JobsCtx,
    owner: &str,
    project: &str,
    seq: u64,
    payload: &[u8],
) -> Vec<u8> {
    match serde_json::from_slice::<MembersBody>(payload) {
        Err(e) => bad_request(&e.to_string()),
        Ok(b) => match ctx
            .handle
            .edit_members(owner, project, seq, b.add, b.remove)
            .await
        {
            Err(e) => error_reply(&e),
            Ok(_) => fetch_job(&ctx.store, owner, project, seq).await,
        },
    }
}

/// `req.jobs.groups` — design #321 Decision 5: add/remove a job's group labels.
/// Accepted in **every** state, terminal included; the actor re-checks the
/// resulting list and answers 422 with the rule it broke. Reply is the updated
/// job.
async fn jobs_groups(
    ctx: &JobsCtx,
    owner: &str,
    project: &str,
    seq: u64,
    payload: &[u8],
) -> Vec<u8> {
    match serde_json::from_slice::<GroupsBody>(payload) {
        Err(e) => bad_request(&e.to_string()),
        Ok(b) => match ctx
            .handle
            .edit_groups(owner, project, seq, b.add, b.remove)
            .await
        {
            Err(e) => error_reply(&e),
            Ok(_) => fetch_job(&ctx.store, owner, project, seq).await,
        },
    }
}

/// `req.jobs.approval` — spec §1.1 require-approval: set/clear the operator
/// sign-off gate, pre-Work states only. The actor answers 422 with the state it
/// refused; the reply is the updated job.
async fn jobs_approval(
    ctx: &JobsCtx,
    owner: &str,
    project: &str,
    seq: u64,
    payload: &[u8],
) -> Vec<u8> {
    match serde_json::from_slice::<ApprovalBody>(payload) {
        Err(e) => bad_request(&e.to_string()),
        Ok(b) => match ctx
            .handle
            .set_require_approval(owner, project, seq, b.require)
            .await
        {
            Err(e) => error_reply(&e),
            Ok(_) => fetch_job(&ctx.store, owner, project, seq).await,
        },
    }
}

/// `req.jobs.list` — the project's jobs as summaries, each carrying its latest
/// channel post so the UI can show a live progress line without an SSE replay.
async fn jobs_list(store: &NatsStore, owner: &str, project: &str) -> Vec<u8> {
    match store.jobs().await {
        Ok(jobs) => match jobs.list(owner, project).await {
            Ok(list) => {
                let channels = latest_channel_updates(store, owner, project).await;
                let summaries: Vec<types::JobSummary<'_>> = list
                    .iter()
                    .map(|job| types::JobSummary::from(job).with_channel(channels.get(&job.id)))
                    .collect();
                ok_reply(&summaries)
            }
            Err(e) => error_reply(&e.into()),
        },
        Err(e) => error_reply(&e.into()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::reply::unprocessable;
    use super::{
        COVER_HTML_MAX_BYTES, CreateJobBody, GroupsBody, UpdateJobBody, cover_too_large,
        groups_shape_error, inputs_shape_error,
    };

    fn status(body: &[u8]) -> i64 {
        serde_json::from_slice::<serde_json::Value>(body).unwrap()["error"]["status"]
            .as_i64()
            .unwrap()
    }

    /// The §1.1 cover cap: a body at/under the limit is accepted, one over it is
    /// rejected with a 422 (not a 400 — the body parsed fine). Exercised on both
    /// create and the Draft PATCH bodies, since both carry `cover_html`.
    #[test]
    fn cover_html_size_cap_rejects_over_limit_with_422() {
        assert!(!cover_too_large(&None));
        assert!(!cover_too_large(&Some("x".repeat(COVER_HTML_MAX_BYTES))));
        assert!(cover_too_large(&Some("x".repeat(COVER_HTML_MAX_BYTES + 1))));
        assert_eq!(status(&unprocessable("too big")), 422);

        let over = format!(
            r#"{{"type":"code","cover_html":{}}}"#,
            serde_json::to_string(&"x".repeat(COVER_HTML_MAX_BYTES + 1)).unwrap()
        );
        let create: CreateJobBody = serde_json::from_str(&over).unwrap();
        assert!(cover_too_large(&create.cover_html));
        let update: UpdateJobBody = serde_json::from_str(&over).unwrap();
        assert!(cover_too_large(&update.cover_html));

        let ok = r#"{"type":"code","cover_html":"<h1>hi</h1>"}"#;
        let create: CreateJobBody = serde_json::from_str(ok).unwrap();
        assert_eq!(create.cover_html.as_deref(), Some("<h1>hi</h1>"));
        assert!(!cover_too_large(&create.cover_html));

        let bare: CreateJobBody = serde_json::from_str(r#"{"type":"code"}"#).unwrap();
        assert!(bare.cover_html.is_none());
    }

    /// The §2.2 creation pass over `inputs` (design #311 Decision 3): a
    /// well-shaped map is accepted, a malformed name or an out-of-charset value is
    /// a **422** (the body parsed fine — this is a semantic bound), and a
    /// *semantic* violation is deliberately NOT decided here.
    #[test]
    fn supplied_inputs_shape_is_checked_at_create_with_422() {
        let bare: CreateJobBody = serde_json::from_str(r#"{"type":"code"}"#).unwrap();
        assert!(bare.inputs.is_empty());
        assert_eq!(inputs_shape_error(&bare.inputs), None);
        let bare: UpdateJobBody = serde_json::from_str(r#"{"type":"code"}"#).unwrap();
        assert!(bare.inputs.is_empty());

        let ok: CreateJobBody =
            serde_json::from_str(r#"{"type":"rollback","inputs":{"sha":"4f9c1ab"}}"#).unwrap();
        assert_eq!(ok.inputs.get("sha").map(String::as_str), Some("4f9c1ab"));
        assert_eq!(inputs_shape_error(&ok.inputs), None);

        for body in [
            r#"{"type":"rollback","inputs":{"sha":"4f9c1ab; rm -rf /"}}"#,
            r#"{"type":"rollback","inputs":{"SHA":"4f9c1ab"}}"#,
        ] {
            let parsed: CreateJobBody = serde_json::from_str(body).unwrap();
            let message = inputs_shape_error(&parsed.inputs).expect("must be refused");
            assert!(message.starts_with("inputs: "), "{message}");
            assert_eq!(status(&unprocessable(&message)), 422);
        }

        let update: UpdateJobBody =
            serde_json::from_str(r#"{"type":"rollback","inputs":{"sha":"a b"}}"#).unwrap();
        assert!(inputs_shape_error(&update.inputs).is_some());

        let undeclared: CreateJobBody =
            serde_json::from_str(r#"{"type":"code","inputs":{"nobody_declared_me":"x"}}"#).unwrap();
        assert_eq!(inputs_shape_error(&undeclared.inputs), None);
    }

    /// The §1.1 bounds over a supplied `groups` list (design #321): a well-shaped
    /// list is accepted, and every violation is a **422** naming the rule it
    /// broke — the body parsed fine, so this is a semantic bound, not a 400.
    /// Unlike `inputs`, nothing is deferred: there is no release-time pass.
    #[test]
    fn supplied_groups_shape_is_checked_at_the_wire_edge_with_422() {
        let bare: CreateJobBody = serde_json::from_str(r#"{"type":"code"}"#).unwrap();
        assert!(bare.groups.is_empty());
        assert_eq!(groups_shape_error(&bare.groups), None);
        let bare: UpdateJobBody = serde_json::from_str(r#"{"type":"code"}"#).unwrap();
        assert!(bare.groups.is_empty());
        assert_eq!(groups_shape_error(&bare.groups), None);

        let ok: CreateJobBody = serde_json::from_str(
            r#"{"type":"code","groups":["design/321-job-groups","beacon-import"]}"#,
        )
        .unwrap();
        assert_eq!(ok.groups.len(), 2);
        assert_eq!(groups_shape_error(&ok.groups), None);

        let long = "x".repeat(types::GROUP_NAME_LEN_MAX + 1);
        let many: Vec<String> = (0..=types::GROUPS_COUNT_MAX)
            .map(|i| format!("\"g-{i}\""))
            .collect();
        for body in [
            r#"{"type":"code","groups":["NOT A GROUP"]}"#.to_string(),
            format!(r#"{{"type":"code","groups":["{long}"]}}"#),
            r#"{"type":"code","groups":["dup","dup"]}"#.to_string(),
            format!(r#"{{"type":"code","groups":[{}]}}"#, many.join(",")),
        ] {
            let parsed: CreateJobBody = serde_json::from_str(&body).unwrap();
            let message = groups_shape_error(&parsed.groups).expect("must be refused");
            assert!(message.starts_with("groups: "), "{message}");
            assert_eq!(status(&unprocessable(&message)), 422);
            let update: UpdateJobBody = serde_json::from_str(&body).unwrap();
            assert!(groups_shape_error(&update.groups).is_some());
        }
    }

    /// The add/remove body (design #321 Decision 5): both sides default empty, so
    /// a caller can add-only or remove-only, and an empty body is a legal no-op.
    #[test]
    fn groups_body_defaults_both_sides() {
        let empty: GroupsBody = serde_json::from_str("{}").unwrap();
        assert!(empty.add.is_empty() && empty.remove.is_empty());
        let add_only: GroupsBody = serde_json::from_str(r#"{"add":["beacon-import"]}"#).unwrap();
        assert_eq!(add_only.add, vec!["beacon-import".to_string()]);
        assert!(add_only.remove.is_empty());
        let remove_only: GroupsBody = serde_json::from_str(r#"{"remove":["x"]}"#).unwrap();
        assert!(remove_only.add.is_empty());
        assert_eq!(remove_only.remove, vec!["x".to_string()]);
    }
}

//! Job-authoring primitives (spec §2.1 batches) — refactor-plan F1a.
//!
//! The batch-composition rules every authoring path shares: `POST jobs
//! {members}` at create, the Draft batch's member edits, and the
//! finalize/release re-validation that commits a Draft batch's membership
//! against *current* state. All three ask the same two questions — is this
//! candidate admissible, and what deps/evaluators does the batch inherit — so
//! the answers live here once, as pure functions over the project's
//! [`JobGraph`], rather than as `&mut Core` methods each path reimplements.
//!
//! This module is deliberately a decider **without a `decide`** yet: F1a moves
//! the primitives, F1b builds `decide(view, event) -> AuthoringOutcome` on top
//! of them. Until then the dispatcher's `core` keeps one-line delegates, so
//! every call site is unchanged and no behavior moved with the code.
//!
//! The graph is borrowed as `Option<&JobGraph>` — `None` is a project with no
//! in-memory graph yet, in which case every candidate reads as nonexistent,
//! exactly as the dispatcher's per-project graph map answers today. It is also
//! the shape F1b's `AuthoringView` carries.
//!
//! - **Accepts:** the project's [`JobGraph`] (or `None`), the batch's type, and
//!   a candidate member list.
//! - **Emits:** [`ValidationError`]s per violated membership rule, and on a
//!   clean pass the [`BatchComposition`] to commit — values only, never a write.
//! - **Guarantees:** pure and synchronous; no mutation of the graph or of any
//!   member record (the caller absorbs); an error list and a composition are
//!   never both returned.
//! - **Spec:** §2.1 (batches), §6.5 (the validation-error shape);
//!   refactor-plan F1a.

use crate::graph::JobGraph;
use crate::release::ValidationError;
use std::collections::HashSet;
use types::{BatchComposition, Evaluator, Job, JobState};

/// Validate one candidate against the batch membership rules (spec §2.1) at
/// *current* state: exists, Frozen, same type, not already batched, and not
/// itself a batch. Pushes a field error per violation and returns the record
/// (or `None` if the candidate does not exist). Shared by every path that
/// admits a member — atomic create, draft-member edits, and the
/// finalize/release re-validation.
pub fn validate_member(
    graph: Option<&JobGraph>,
    ty: &str,
    m: u64,
    errs: &mut Vec<ValidationError>,
) -> Option<Job> {
    let Some(job) = graph.and_then(|g| g.get(m)).cloned() else {
        errs.push(ValidationError::new(
            Some(m),
            "members",
            format!("member #{m} does not exist"),
        ));
        return None;
    };
    if job.state != JobState::Frozen {
        errs.push(ValidationError::new(
            Some(m),
            "members",
            format!(
                "member #{m} is {:?}; only a Frozen job can be batched",
                job.state
            ),
        ));
    }
    if job.r#type != ty {
        errs.push(ValidationError::new(
            Some(m),
            "members",
            format!(
                "member #{m} is type '{}'; a batch absorbs one type ('{}')",
                job.r#type, ty
            ),
        ));
    }
    if job.batch_id.is_some() {
        errs.push(ValidationError::new(
            Some(m),
            "members",
            format!("member #{m} is already batched"),
        ));
    }
    if job.is_batch() {
        errs.push(ValidationError::new(
            Some(m),
            "members",
            format!("member #{m} is itself a batch; batches do not nest"),
        ));
    }
    if !job.inputs.is_empty() {
        errs.push(ValidationError::new(
            Some(m),
            "members",
            format!(
                "member #{m} carries inputs ({}); a batch cannot union input values — \
                 release it on its own",
                job.inputs
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    Some(job)
}

/// Validate `member_seqs` against the batch rules at *current* state and, on a
/// clean pass, compute the composition the batch commits (spec §2.1): external
/// deps union minus the members, evaluators union by name, the approval gate
/// unions as an OR. Pure — the caller absorbs — and `min_members` is the
/// committable floor (2), which a Draft batch composing passes as 1.
pub fn plan_batch(
    graph: Option<&JobGraph>,
    ty: &str,
    member_seqs: &[u64],
    min_members: usize,
) -> Result<BatchComposition, Vec<ValidationError>> {
    let mut errs: Vec<ValidationError> = Vec::new();
    if member_seqs.len() < min_members {
        errs.push(ValidationError::new(
            None,
            "members",
            format!("a batch needs at least {min_members} members"),
        ));
    }
    let mut seen = HashSet::new();
    for &m in member_seqs {
        if !seen.insert(m) {
            errs.push(ValidationError::new(
                None,
                "members",
                format!("member #{m} listed more than once"),
            ));
        }
    }
    let member_set: HashSet<u64> = member_seqs.iter().copied().collect();

    let mut members: Vec<Job> = Vec::new();
    for &m in member_seqs {
        if let Some(job) = validate_member(graph, ty, m, &mut errs) {
            members.push(job);
        }
    }

    let mut deps: Vec<u64> = Vec::new();
    let mut eval: Vec<Evaluator> = Vec::new();
    for job in &members {
        for &d in &job.deps {
            if !member_set.contains(&d) && !deps.contains(&d) {
                deps.push(d);
            }
        }
        for e in &job.eval {
            match eval.iter().find(|x| x.name == e.name) {
                Some(existing) if *existing != *e => errs.push(ValidationError::new(
                    None,
                    "eval.name",
                    format!(
                        "evaluator '{}' is defined differently across batch members",
                        e.name
                    ),
                )),
                Some(_) => {}
                None => eval.push(e.clone()),
            }
        }
    }

    if !errs.is_empty() {
        return Err(errs);
    }
    let require_approval = members.iter().any(|j| j.require_approval);
    Ok(BatchComposition {
        deps,
        eval,
        require_approval,
    })
}

/// The auto-index description a batch defaults to (spec §2.1):
/// `Batch of N {type} jobs: #a #b …`.
pub fn batch_auto_description(ty: &str, member_seqs: &[u64]) -> String {
    format!(
        "Batch of {} {} jobs: {}",
        member_seqs.len(),
        ty,
        member_seqs
            .iter()
            .map(|m| format!("#{m}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use chrono::Utc;
    use types::EvaluatorType;

    fn job(seq: u64, deps: &[u64], state: JobState) -> Job {
        Job {
            id: seq,
            project: "acme/api".into(),
            r#type: "code".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: deps.to_vec(),
            members: vec![],
            batch_id: None,
            state,
            branch: format!("job/{seq}"),
            base_ref: None,
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            inputs: Default::default(),
            groups: vec![],
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
            task_time_ms: None,
        }
    }

    fn evaluator(name: &str, prompt: &str) -> Evaluator {
        Evaluator {
            name: name.into(),
            r#type: EvaluatorType::Agent,
            image: None,
            run: None,
            prompt: Some(prompt.into()),
            provider: None,
            model: None,
            secrets: vec![],
            required: None,
            stage: 0,
        }
    }

    /// The composition a batch commits: member-on-member deps drop out,
    /// external deps union, identical evaluators dedup by name.
    #[test]
    fn plan_batch_unions_external_deps_and_dedups_evaluators() {
        let mut g = JobGraph::default();
        g.insert(job(1, &[], JobState::Done));
        let mut a = job(2, &[1], JobState::Frozen);
        a.eval = vec![evaluator("ci", "run ci")];
        let mut b = job(3, &[2], JobState::Frozen);
        b.eval = vec![evaluator("ci", "run ci")];
        g.insert(a);
        g.insert(b);

        let comp = plan_batch(Some(&g), "code", &[2, 3], 2).unwrap();
        assert_eq!(comp.deps, vec![1]);
        assert_eq!(comp.eval.len(), 1);
        assert!(!comp.require_approval);
    }

    /// One merge completes every member, so one member's approval gate governs
    /// the whole batch — the union is an OR, not an AND.
    #[test]
    fn plan_batch_inherits_approval_from_any_member() {
        let mut g = JobGraph::default();
        let plain = job(1, &[], JobState::Frozen);
        let mut gated = job(2, &[], JobState::Frozen);
        gated.require_approval = true;
        g.insert(plain);
        g.insert(gated);

        assert!(
            plan_batch(Some(&g), "code", &[1, 2], 2)
                .unwrap()
                .require_approval
        );
        assert!(
            !plan_batch(Some(&g), "code", &[1], 1)
                .unwrap()
                .require_approval
        );
    }

    /// A same-name-different-definition evaluator has no defensible union.
    #[test]
    fn plan_batch_rejects_clashing_evaluator_definitions() {
        let mut g = JobGraph::default();
        let mut a = job(1, &[], JobState::Frozen);
        a.eval = vec![evaluator("ci", "run ci")];
        let mut b = job(2, &[], JobState::Frozen);
        b.eval = vec![evaluator("ci", "run something else")];
        g.insert(a);
        g.insert(b);

        let errs = plan_batch(Some(&g), "code", &[1, 2], 2).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "eval.name"));
    }

    /// Every membership rule reports through `members`, including the floor and
    /// the duplicate check that `validate_member` never sees.
    #[test]
    fn plan_batch_reports_floor_duplicates_and_stale_members() {
        let mut g = JobGraph::default();
        g.insert(job(1, &[], JobState::Work));
        let errs = plan_batch(Some(&g), "code", &[1, 1], 3).unwrap_err();
        let messages: Vec<&str> = errs.iter().map(|e| e.message.as_str()).collect();
        assert!(errs.iter().all(|e| e.field == "members"));
        assert!(messages.iter().any(|m| m.contains("at least 3 members")));
        assert!(messages.iter().any(|m| m.contains("listed more than once")));
        assert!(messages.iter().any(|m| m.contains("only a Frozen job")));
    }

    /// A project with no graph yet answers every candidate the same way a graph
    /// missing that seq does — the record does not exist.
    #[test]
    fn validate_member_without_graph_reports_nonexistent() {
        let mut errs = Vec::new();
        assert!(validate_member(None, "code", 7, &mut errs).is_none());
        assert_eq!(errs.len(), 1);
        assert!(errs[0].message.contains("#7 does not exist"));
    }

    /// Batches do not nest, and an already-batched member is not admissible.
    #[test]
    fn validate_member_rejects_nested_and_absorbed_members() {
        let mut g = JobGraph::default();
        let mut nested = job(1, &[], JobState::Frozen);
        nested.members = vec![4, 5];
        let mut absorbed = job(2, &[], JobState::Frozen);
        absorbed.batch_id = Some(9);
        g.insert(nested);
        g.insert(absorbed);

        let mut errs = Vec::new();
        assert!(validate_member(Some(&g), "code", 1, &mut errs).is_some());
        assert!(validate_member(Some(&g), "code", 2, &mut errs).is_some());
        let messages: Vec<&str> = errs.iter().map(|e| e.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("batches do not nest")));
        assert!(messages.iter().any(|m| m.contains("already batched")));
    }

    #[test]
    fn auto_description_indexes_members() {
        assert_eq!(
            batch_auto_description("code", &[7, 8, 9]),
            "Batch of 3 code jobs: #7 #8 #9"
        );
    }
}

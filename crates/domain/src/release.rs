//! Release validation, the pure half (spec §2.2): the validation-error
//! vocabulary, graph wiring rules, and the additive-evaluator merge. Used at
//! release time (against current HEAD) and at the Blocked→Ready re-validation
//! (against the freshly pinned `base_ref`). The ref-reading half — loading
//! `.chug/jobs/*.yaml` and checking prompt paths at a git ref — needs the `vcs`
//! port, so it lives in the dispatcher's `release` module, which re-exports
//! this one to keep one `release::*` surface.
//!
//! - **Accepts:** a job and its graph, or a job type to merge and validate.
//! - **Emits:** a validation verdict — wiring violations and config errors,
//!   or a clean pass.
//! - **Guarantees:** pure checks, no state writes, no I/O; the same rules run
//!   at release and at Blocked→Ready re-validation.
//! - **Spec:** §2.2, §2.3.

use crate::graph::JobGraph;
use serde::Serialize;
use std::collections::HashSet;
use types::{Evaluator, EvaluatorType, Job, JobType};

/// §6.5 validation error shape: `field` uses dot notation matching the job
/// type YAML structure; `job_seq` is omitted for errors not tied to a job.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_seq: Option<u64>,
    pub field: String,
    pub message: String,
}

/// The `field` value the dispatcher's `load_job_type` stamps on a version-skew
/// error — the config declares a `min_dispatcher` newer than this binary's
/// `CONFIG_SCHEMA_EPOCH` (spec §14.2). It is the one launch-validation failure
/// class that means "config ahead of binary" rather than "config is wrong", so
/// the launch path routes it to a pre-Work park (Stalled) instead of an
/// Escalated storm. Kept as a named constant so the producer and the
/// [`ValidationError::is_schema_skew`] consumer can never drift.
pub const SCHEMA_SKEW_FIELD: &str = "min_dispatcher";

impl ValidationError {
    pub fn new(job_seq: Option<u64>, field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            job_seq,
            field: field.into(),
            message: message.into(),
        }
    }

    /// True when this error is the config-ahead-of-binary version-skew gate
    /// (spec §14.2). The launch path uses this to distinguish a skewed config
    /// (park pre-Work as Stalled) from an ordinary launch validation failure
    /// (Escalated).
    pub fn is_schema_skew(&self) -> bool {
        self.field == SCHEMA_SKEW_FIELD
    }
}

/// Everything static validation needs besides the repo: which secret, var and
/// cloud-identity names exist in KV. The core fetches these once per validation
/// pass.
pub struct KvNames {
    pub secrets: HashSet<String>,
    pub vars: HashSet<String>,
    /// Names in the `cloud-identities.*` bucket (design #313 A5). A separate
    /// namespace from `secrets`, and deliberately never merged with it: a cloud
    /// identity is not expressible as a secret.
    pub cloud_identities: HashSet<String>,
}

/// Graph wiring rules (§2.2): dependencies exist, no self-edges, no cycles,
/// no duplicates, nothing Revoked. `graph` must already contain the job.
pub fn wiring_errors(job: &Job, graph: &JobGraph) -> Vec<ValidationError> {
    let mut errs = Vec::new();
    let seq = Some(job.id);

    for &upstream in &job.deps {
        match graph.get(upstream) {
            None => errs.push(ValidationError::new(
                seq,
                "deps",
                format!("depends on unknown job #{upstream}"),
            )),
            Some(dep) if dep.state == types::JobState::Revoked => {
                errs.push(ValidationError::new(
                    seq,
                    "deps",
                    format!("depends on revoked job #{upstream}"),
                ));
            }
            Some(_) => {}
        }
        if upstream == job.id {
            errs.push(ValidationError::new(seq, "deps", "job depends on itself"));
        }
    }
    let mut seen = HashSet::new();
    for &upstream in &job.deps {
        if !seen.insert(upstream) {
            errs.push(ValidationError::new(
                seq,
                "deps",
                format!("duplicate dependency #{upstream}"),
            ));
        }
    }
    if graph.creates_cycle(job.id, &job.deps) {
        errs.push(ValidationError::new(
            seq,
            "deps",
            "dependency cycle detected",
        ));
    }
    errs
}

/// The evaluator name reserved for the per-job approval gate
/// ([`Job::require_approval`], spec §1.1). Reserved unconditionally, so a job
/// type or a per-job evaluator claiming it is a release-time error rather than a
/// silent overwrite in either direction.
pub const APPROVAL_EVALUATOR_NAME: &str = "approval";

/// The sign-off instructions the synthesized approval task carries into the
/// operator inbox. Inline text rather than a repo path, so requiring approval
/// adds nothing to a project's config tree.
const APPROVAL_PROMPT: &str = "## Operator approval required\n\n\
    This job cannot pass evaluation without your explicit sign-off, and every \
    other evaluator has already passed. Review the branch and the results \
    below, then **pass** to approve the merge, or **fail** with notes — the \
    notes become the rework context the next work cycle reads. Fail with \
    **abort** when the work is not satisfiable by rework at all.\n";

/// The operator sign-off evaluator synthesized for a job that requires approval
/// (spec §1.1). Staged one past every evaluator in `resolved` so an operator is
/// never asked to sign off on work a later stage is about to reject; `None` when
/// an evaluator already sits at the `u32` stage ceiling and nothing can follow it.
pub fn approval_evaluator(resolved: &[Evaluator]) -> Option<Evaluator> {
    let stage = resolved
        .iter()
        .map(|e| e.stage)
        .max()
        .map_or(Some(0), |highest| highest.checked_add(1))?;
    assert!(
        resolved.iter().all(|e| e.stage < stage),
        "the approval gate must stage after every other evaluator"
    );
    Some(Evaluator {
        name: APPROVAL_EVALUATOR_NAME.to_string(),
        r#type: EvaluatorType::Human,
        image: None,
        run: None,
        prompt: Some(APPROVAL_PROMPT.to_string()),
        provider: None,
        model: None,
        secrets: Vec::new(),
        workload_identities: Vec::new(),
        tools: vec![],
        required: Some(true),
        stage,
    })
}

/// Layer the job's additive evaluators (docs/reference/design-lifecycle.md) and its approval
/// gate ([`Job::require_approval`]) on top of the type's list — the type's
/// evaluators are a floor, so a name collision is an error and the merged list
/// must still pass the §1.1 field rules. The base type already validated clean
/// in `load_job_type`, so any error here is the extras'.
pub fn with_job_evaluators(job_type: JobType, job: &Job) -> Result<JobType, Vec<ValidationError>> {
    let claims_reserved =
        |evals: &[Evaluator]| evals.iter().any(|e| e.name == APPROVAL_EVALUATOR_NAME);
    if job.eval.is_empty() && !job.require_approval && !claims_reserved(&job_type.eval) {
        return Ok(job_type);
    }
    let mut merged = job_type;
    let mut errs = Vec::new();
    for e in &job.eval {
        if merged.eval.iter().any(|x| x.name == e.name) {
            errs.push(ValidationError::new(
                Some(job.id),
                "eval.name",
                format!(
                    "job evaluator '{}' collides with a declared evaluator",
                    e.name
                ),
            ));
            continue;
        }
        merged.eval.push(e.clone());
    }
    if claims_reserved(&merged.eval) {
        errs.push(ValidationError::new(
            Some(job.id),
            "eval.name",
            format!(
                "evaluator '{APPROVAL_EVALUATOR_NAME}' is reserved for the per-job approval gate"
            ),
        ));
    } else if job.require_approval {
        match approval_evaluator(&merged.eval) {
            Some(gate) => merged.eval.push(gate),
            None => errs.push(ValidationError::new(
                Some(job.id),
                "eval.stage",
                "the approval gate cannot be staged past an evaluator at the u32 stage ceiling",
            )),
        }
    }
    errs.extend(
        merged
            .validate()
            .into_iter()
            .map(|e| ValidationError::new(Some(job.id), "eval", e.to_string())),
    );
    if errs.is_empty() {
        Ok(merged)
    } else {
        Err(errs)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn agent_type_with_memory(mem: &str) -> JobType {
        JobType::parse(&format!(
            r#"
name: code
image: img:latest
work:
  type: agent
  prompt: p.md
resources:
  memory: "{mem}"
"#
        ))
        .unwrap()
    }

    fn job_with_extra_eval() -> Job {
        Job {
            id: 1,
            project: "acme/api".into(),
            r#type: "code".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state: types::JobState::Ready,
            branch: "job/1".into(),
            base_ref: None,
            knowledge_tags: vec![],
            eval: vec![Evaluator {
                name: "extra-ci".into(),
                r#type: EvaluatorType::Command,
                image: None,
                run: Some("./ci.sh".into()),
                prompt: None,
                provider: None,
                model: None,
                secrets: vec![],
                workload_identities: vec![],
                tools: vec![],
                required: None,
                stage: 0,
            }],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        }
    }

    /// Release validation reuses `types::JobType::validate`, so a malformed
    /// `resources.memory` (the dogfood `5g` bug) surfaces as a
    /// `ValidationError` at release time instead of wedging at container
    /// launch. Good limits pass clean.
    #[test]
    fn release_validation_rejects_bad_memory() {
        let errs = with_job_evaluators(agent_type_with_memory("5g"), &job_with_extra_eval())
            .expect_err("5g must fail release validation");
        assert!(
            errs.iter().any(|e| e.message.contains("resources.memory")),
            "expected a resources.memory error, got {errs:?}"
        );

        assert!(
            with_job_evaluators(agent_type_with_memory("5Gi"), &job_with_extra_eval()).is_ok(),
            "5Gi must pass release validation"
        );
    }

    /// **Inputs never influence config resolution** (design #311 Decision 1, the
    /// invariant that keeps docs/reference/design-lifecycle.md's eval floor intact): for any job
    /// type, any job, and any two input maps, the `JobType` the release path
    /// resolves is *equal*. An input is a value delivered to a container, never a
    /// substitution into the file that decides which gates run — so this fails the
    /// moment anyone threads `Job::inputs` into `with_job_evaluators` or the
    /// merge below it, which is the closest thing to unrepresentable available
    /// without a newtype ceremony `types` should not carry.
    #[test]
    fn resolved_job_type_is_equal_for_any_two_input_maps() {
        let cases = [
            BTreeMap::new(),
            BTreeMap::from([("service".to_string(), "web".to_string())]),
            BTreeMap::from([
                ("service".to_string(), "worker".to_string()),
                ("sha".to_string(), "4f9c1ab".to_string()),
            ]),
            BTreeMap::from([
                ("image".to_string(), "attacker/img:latest".to_string()),
                ("eval".to_string(), "ci".to_string()),
                ("secrets".to_string(), "DEPLOY_KEY".to_string()),
                ("prompt".to_string(), "prompts/other.md".to_string()),
            ]),
        ];
        for base in [job_with_extra_eval(), {
            let mut plain = job_with_extra_eval();
            plain.eval.clear();
            plain
        }] {
            let resolutions: Vec<_> = cases
                .iter()
                .map(|inputs| {
                    let mut job = base.clone();
                    job.inputs = inputs.clone();
                    with_job_evaluators(agent_type_with_memory("5Gi"), &job)
                })
                .collect();
            for (i, resolved) in resolutions.iter().enumerate() {
                assert_eq!(
                    resolved, &resolutions[0],
                    "input map #{i} changed the resolved job type — inputs must never \
                     reach config resolution (#311 Decision 1)"
                );
            }
        }
    }

    fn staged_type(stages: &[u32]) -> JobType {
        let evals: String = stages
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!("  - name: e{i}\n    type: command\n    run: ./e.sh\n    stage: {s}\n")
            })
            .collect();
        JobType::parse(&format!(
            "name: code\nimage: img:latest\nwork:\n  type: agent\n  prompt: p.md\neval:\n{evals}"
        ))
        .unwrap()
    }

    fn approving_job() -> Job {
        let mut job = job_with_extra_eval();
        job.eval.clear();
        job.require_approval = true;
        job
    }

    /// The flag adds exactly one required Human evaluator, staged past every
    /// other one, and removes nothing the type declared.
    #[test]
    fn approval_gate_is_additive_and_stages_last() {
        let jt = staged_type(&[0, 1, 1]);
        let resolved = with_job_evaluators(jt.clone(), &approving_job()).unwrap();
        assert_eq!(resolved.eval.len(), jt.eval.len() + 1);
        for declared in &jt.eval {
            assert!(resolved.eval.contains(declared), "{resolved:?}");
        }
        let gate: Vec<_> = resolved
            .eval
            .iter()
            .filter(|e| e.name == APPROVAL_EVALUATOR_NAME)
            .collect();
        assert_eq!(gate.len(), 1, "{resolved:?}");
        assert_eq!(gate[0].r#type, EvaluatorType::Human);
        assert_eq!(gate[0].required, Some(true));
        assert_eq!(gate[0].stage, 2);
    }

    /// A type with no evaluators leaves the gate at stage 0, and the per-job
    /// additions count toward the maximum the gate stages past.
    #[test]
    fn approval_stage_is_computed_from_the_resolved_set() {
        let bare =
            JobType::parse("name: code\nimage: img:latest\nwork:\n  type: agent\n  prompt: p.md\n")
                .unwrap();
        let resolved = with_job_evaluators(bare, &approving_job()).unwrap();
        assert_eq!(resolved.eval.len(), 1);
        assert_eq!(resolved.eval[0].stage, 0);

        let mut job = approving_job();
        job.eval = vec![Evaluator {
            name: "extra-ci".into(),
            r#type: EvaluatorType::Command,
            image: None,
            run: Some("./ci.sh".into()),
            prompt: None,
            provider: None,
            model: None,
            secrets: vec![],
            workload_identities: vec![],
            tools: vec![],
            required: None,
            stage: 7,
        }];
        let resolved = with_job_evaluators(staged_type(&[0]), &job).unwrap();
        let gate = resolved
            .eval
            .iter()
            .find(|e| e.name == APPROVAL_EVALUATOR_NAME)
            .expect("gate");
        assert_eq!(gate.stage, 8);
    }

    /// Flag unset ⇒ resolution is byte-for-byte what it was before the gate
    /// existed, including the untouched-type fast path.
    #[test]
    fn without_the_flag_resolution_is_unchanged() {
        let jt = staged_type(&[0, 3]);
        let mut job = approving_job();
        job.require_approval = false;
        assert_eq!(with_job_evaluators(jt.clone(), &job).unwrap(), jt);
    }

    /// A config evaluator at the `u32` stage ceiling leaves nowhere to stage the
    /// gate, which is a release-time error like every other bad-config case —
    /// never a panic inside the single-writer actor.
    #[test]
    fn approval_past_the_stage_ceiling_is_a_release_error() {
        assert!(
            approval_evaluator(&staged_type(&[u32::MAX]).eval).is_none(),
            "nothing can stage past the ceiling"
        );
        let errs = with_job_evaluators(staged_type(&[0, u32::MAX]), &approving_job())
            .expect_err("no stage left for the gate");
        assert!(errs.iter().any(|e| e.field == "eval.stage"), "{errs:?}");
    }

    /// The reserved name is reserved unconditionally — from the type's side and
    /// from the job's — so neither can silently overwrite the gate.
    #[test]
    fn reserved_approval_name_is_a_release_error() {
        let jt = JobType::parse(
            "name: code\nimage: img:latest\nwork:\n  type: agent\n  prompt: p.md\n\
             eval:\n  - name: approval\n    type: command\n    run: ./a.sh\n",
        )
        .unwrap();
        for flag in [false, true] {
            let mut job = approving_job();
            job.require_approval = flag;
            let errs = with_job_evaluators(jt.clone(), &job).expect_err("reserved name");
            assert!(errs.iter().any(|e| e.field == "eval.name"), "{errs:?}");
        }

        let mut job = approving_job();
        job.require_approval = false;
        job.eval = vec![approval_evaluator(&[]).expect("gate")];
        let errs = with_job_evaluators(staged_type(&[0]), &job).expect_err("reserved name");
        assert!(errs.iter().any(|e| e.field == "eval.name"), "{errs:?}");
    }
}

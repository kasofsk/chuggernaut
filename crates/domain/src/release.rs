//! Release validation, the pure half (spec §2.2): the validation-error
//! vocabulary, graph wiring rules, and the additive-evaluator merge. Used at
//! release time (against current HEAD) and at the Blocked→Ready re-validation
//! (against the freshly pinned `base_ref`). The ref-reading half — loading
//! `jobs/*.yaml` and checking prompt paths at a git ref — needs the `vcs`
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
use types::{Job, JobType};

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

/// Everything static validation needs besides the repo: which secret and var
/// names exist in KV. The core fetches these once per validation pass.
pub struct KvNames {
    pub secrets: HashSet<String>,
    pub vars: HashSet<String>,
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

/// Layer the job's additive evaluators (design-lifecycle.md) on top of the
/// type's list. The type's evaluators are a floor: a name collision is an
/// error, and the merged list must still pass the §1.1 field rules (which
/// also enforces the image fallback for the extras). The base type already
/// validated clean in `load_job_type`, so any error here is the extras'.
pub fn with_job_evaluators(job_type: JobType, job: &Job) -> Result<JobType, Vec<ValidationError>> {
    if job.eval.is_empty() {
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
    use super::*;
    use chrono::Utc;
    use types::{Evaluator, EvaluatorType};

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
                required: None,
                stage: 0,
            }],
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
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
}

//! Declarative job type YAML schema (spec §1.1).
//!
//! Durations (`task_timeout`, `job_deadline`, `batch_window`) are kept as strings
//! in the schema ("2h", "30m"); [`crate::duration::parse_duration`] is the shared
//! parser, and `validate()` checks parseability.

use crate::duration::parse_duration;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct JobType {
    pub name: String,
    /// Human-facing name for the library and the create-form type picker;
    /// falls back to `name`.
    pub display_name: Option<String>,
    /// One-line summary shown alongside the display name in the type picker.
    pub description: Option<String>,
    /// Required for agent/command work; disallowed at top level for human work.
    pub image: Option<String>,
    pub work: WorkSpec,
    /// The third step of the job (work → evaluation → wrap-up): what happens
    /// after eval-pass (design-lifecycle.md).
    #[serde(default)]
    pub wrap_up: WrapUp,
    pub resources: Option<Resources>,
    pub job_deadline: Option<String>,
    pub work_retries: Option<u32>,
    pub eval_retries: Option<u32>,
    pub rework_budget: Option<u32>,
    #[serde(default)]
    pub eval: Vec<Evaluator>,
    #[serde(default)]
    pub knowledge: Vec<String>,
    #[serde(default)]
    pub vars: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkSpec {
    pub r#type: WorkType,
    /// agent/human: required. command: disallowed.
    pub prompt: Option<String>,
    /// agent only.
    pub provider: Option<Provider>,
    /// agent only.
    pub model: Option<String>,
    /// agent only; enables the inline review loop (spec §4.5).
    pub review: Option<ReviewSpec>,
    /// command only.
    pub run: Option<String>,
    /// Secrets injected into the work container (agent/command). Scoped here
    /// because that is the only container they reach — evaluators declare
    /// their own (§4.1). Disallowed for human work (no container).
    #[serde(default)]
    pub secrets: Vec<String>,
}

pub const DEFAULT_REVIEW_ITERATIONS: u32 = 5;

/// Inline review loop declaration (spec §1.1, §4.5). The reviewer runs inside
/// the work container; its acceptance gates `submit_result`, not the merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReviewSpec {
    /// Path to reviewer prompt file in repo (resolved from base_ref).
    pub prompt: String,
    /// Defaults to the work provider. v1 supports claude only (release-time
    /// validation).
    pub provider: Option<Provider>,
    pub model: Option<String>,
    /// Max author↔reviewer rounds before submitting anyway. Default 5.
    pub iterations: Option<u32>,
}

impl ReviewSpec {
    pub fn iteration_budget(&self) -> u32 {
        self.iterations.unwrap_or(DEFAULT_REVIEW_ITERATIONS)
    }
}

/// Wrap-up declaration (design-lifecycle.md): the job's third step. A block
/// rather than a bare scalar so future wrap-up behavior (e.g. a
/// `deployed/{env}` tag ref) extends it without reshaping the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WrapUp {
    /// `merge` (default) squash-merges the job branch through the merge
    /// queue/gate; `none` goes straight to Done — for jobs whose effect is
    /// external (deploys, reports) and whose branch is scratch.
    #[serde(default)]
    pub r#type: Finalize,
}

/// Wrap-up mode after eval-pass (design-lifecycle.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Finalize {
    /// Squash-merge the job branch to the default branch (merge queue, merge
    /// gate, conflict rework) — the code-change wrap-up.
    #[default]
    Merge,
    /// Nothing to land: eval-pass goes straight to Done. The job branch is
    /// scratch and is deleted unmerged.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WorkType {
    Agent,
    Command,
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Resources {
    pub cpu: Option<f64>,
    pub memory: Option<String>,
    pub task_timeout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Evaluator {
    pub name: String,
    pub r#type: EvaluatorType,
    /// command/agent: optional, falls back to top-level image; one of the two required.
    pub image: Option<String>,
    pub run: Option<String>,
    pub prompt: Option<String>,
    pub provider: Option<Provider>,
    pub model: Option<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Default true; false = advisory.
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EvaluatorType {
    Command,
    Agent,
    Human,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum FieldRuleError {
    #[error("field '{field}' is required for {context}")]
    Required {
        field: &'static str,
        context: String,
    },
    #[error("field '{field}' is disallowed for {context}")]
    Disallowed {
        field: &'static str,
        context: String,
    },
    #[error("field '{field}' is invalid for {context}: {reason}")]
    Invalid {
        field: &'static str,
        context: String,
        reason: String,
    },
}

impl JobType {
    pub fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Enforce the §1.1 field-rules matrices. Returns all violations, not just
    /// the first.
    pub fn validate(&self) -> Vec<FieldRuleError> {
        let mut errs = Vec::new();
        let ctx = |w: WorkType| format!("work.type: {w:?}").to_lowercase();

        match self.work.r#type {
            WorkType::Agent => {
                if self.image.is_none() {
                    errs.push(FieldRuleError::Required {
                        field: "image",
                        context: ctx(WorkType::Agent),
                    });
                }
                if self.work.prompt.is_none() {
                    errs.push(FieldRuleError::Required {
                        field: "work.prompt",
                        context: ctx(WorkType::Agent),
                    });
                }
                if self.work.run.is_some() {
                    errs.push(FieldRuleError::Disallowed {
                        field: "work.run",
                        context: ctx(WorkType::Agent),
                    });
                }
                if let Some(review) = &self.work.review {
                    // v1: the resolved review provider must be claude. Only
                    // statically resolvable here; a None-None chain falls back
                    // to platform config, checked by the dispatcher at release.
                    if review.provider.or(self.work.provider) == Some(Provider::Codex) {
                        errs.push(FieldRuleError::Invalid {
                            field: "work.review.provider",
                            context: ctx(WorkType::Agent),
                            reason: "inline review supports claude only in v1".into(),
                        });
                    }
                    if review.iterations == Some(0) {
                        errs.push(FieldRuleError::Invalid {
                            field: "work.review.iterations",
                            context: ctx(WorkType::Agent),
                            reason: "must be at least 1".into(),
                        });
                    }
                }
            }
            WorkType::Command => {
                if self.image.is_none() {
                    errs.push(FieldRuleError::Required {
                        field: "image",
                        context: ctx(WorkType::Command),
                    });
                }
                if self.work.run.is_none() {
                    errs.push(FieldRuleError::Required {
                        field: "work.run",
                        context: ctx(WorkType::Command),
                    });
                }
                for (present, field) in [
                    (self.work.prompt.is_some(), "work.prompt"),
                    (self.work.provider.is_some(), "work.provider"),
                    (self.work.model.is_some(), "work.model"),
                    (self.work.review.is_some(), "work.review"),
                    (self.rework_budget.is_some(), "rework_budget"),
                ] {
                    if present {
                        errs.push(FieldRuleError::Disallowed {
                            field,
                            context: ctx(WorkType::Command),
                        });
                    }
                }
            }
            WorkType::Human => {
                if self.work.prompt.is_none() {
                    errs.push(FieldRuleError::Required {
                        field: "work.prompt",
                        context: ctx(WorkType::Human),
                    });
                }
                for (present, field) in [
                    (self.image.is_some(), "image"),
                    (self.resources.is_some(), "resources"),
                    (self.work_retries.is_some(), "work_retries"),
                    (!self.work.secrets.is_empty(), "work.secrets"),
                    (self.work.run.is_some(), "work.run"),
                    (self.work.provider.is_some(), "work.provider"),
                    (self.work.model.is_some(), "work.model"),
                    (self.work.review.is_some(), "work.review"),
                ] {
                    if present {
                        errs.push(FieldRuleError::Disallowed {
                            field,
                            context: ctx(WorkType::Human),
                        });
                    }
                }
            }
        }

        for e in &self.eval {
            let ectx = format!("evaluator '{}' (type {:?})", e.name, e.r#type).to_lowercase();
            match e.r#type {
                EvaluatorType::Command => {
                    if e.run.is_none() {
                        errs.push(FieldRuleError::Required {
                            field: "run",
                            context: ectx.clone(),
                        });
                    }
                    for (present, field) in [
                        (e.prompt.is_some(), "prompt"),
                        (e.provider.is_some(), "provider"),
                        (e.model.is_some(), "model"),
                    ] {
                        if present {
                            errs.push(FieldRuleError::Disallowed {
                                field,
                                context: ectx.clone(),
                            });
                        }
                    }
                }
                EvaluatorType::Agent => {
                    if e.prompt.is_none() {
                        errs.push(FieldRuleError::Required {
                            field: "prompt",
                            context: ectx.clone(),
                        });
                    }
                    if e.run.is_some() {
                        errs.push(FieldRuleError::Disallowed {
                            field: "run",
                            context: ectx.clone(),
                        });
                    }
                }
                EvaluatorType::Human => {
                    if e.prompt.is_none() {
                        errs.push(FieldRuleError::Required {
                            field: "prompt",
                            context: ectx.clone(),
                        });
                    }
                    for (present, field) in [
                        (e.run.is_some(), "run"),
                        (e.image.is_some(), "image"),
                        (e.provider.is_some(), "provider"),
                        (e.model.is_some(), "model"),
                        (!e.secrets.is_empty(), "secrets"),
                    ] {
                        if present {
                            errs.push(FieldRuleError::Disallowed {
                                field,
                                context: ectx.clone(),
                            });
                        }
                    }
                }
            }
            // Container evaluators need an image from somewhere (per-evaluator or top-level).
            if matches!(e.r#type, EvaluatorType::Command | EvaluatorType::Agent)
                && e.image.is_none()
                && self.image.is_none()
            {
                errs.push(FieldRuleError::Required {
                    field: "image",
                    context: ectx,
                });
            }
        }

        let durations = [
            (
                self.resources.as_ref().and_then(|r| r.task_timeout.as_deref()),
                "resources.task_timeout",
            ),
            (self.job_deadline.as_deref(), "job_deadline"),
        ];
        for (value, field) in durations {
            if let Some(v) = value
                && let Err(e) = parse_duration(v)
            {
                errs.push(FieldRuleError::Invalid {
                    field,
                    context: format!("job type '{}'", self.name),
                    reason: e.to_string(),
                });
            }
        }

        errs
    }

    /// Append project default evaluators (`jobs/_defaults.yaml`, spec §1.1).
    /// An evaluator name collision between the defaults and this job type is an
    /// error. Validate the merged result with [`JobType::validate`] — image
    /// fallback rules apply against this job type's top-level image.
    pub fn with_defaults(&self, defaults: &ProjectDefaults) -> Result<JobType, FieldRuleError> {
        let mut merged = self.clone();
        for d in &defaults.eval {
            if self.eval.iter().any(|e| e.name == d.name) {
                return Err(FieldRuleError::Invalid {
                    field: "eval.name",
                    context: format!("job type '{}'", self.name),
                    reason: format!(
                        "evaluator '{}' collides with a project default evaluator",
                        d.name
                    ),
                });
            }
            merged.eval.push(d.clone());
        }
        Ok(merged)
    }
}

/// Project-wide default evaluators, `jobs/_defaults.yaml` (spec §1.1). Appended
/// to every job type's eval list at load; this is how a project gates all
/// changes on an evergreen test suite without per-job-type declarations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectDefaults {
    #[serde(default)]
    pub eval: Vec<Evaluator>,
}

impl ProjectDefaults {
    pub fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
name: implement-endpoint
image: registry.acme.com/agents/impl:latest
work:
  type: agent
  prompt: prompts/work/implement-endpoint.md
  provider: claude
  model: claude-sonnet-4-6
  secrets: [GITHUB_TOKEN]
  review:
    prompt: prompts/review/implement-endpoint.md
    model: claude-sonnet-4-6
    iterations: 5
resources:
  cpu: 2
  memory: 4Gi
  task_timeout: 2h
job_deadline: 24h
work_retries: 3
eval_retries: 1
rework_budget: 2
eval:
  - name: unit-tests
    type: command
    run: cargo test --no-fail-fast
  - name: security-review
    type: agent
    prompt: prompts/eval/security-review.md
    provider: claude
    model: claude-opus-4-6
    secrets: [GITHUB_TOKEN]
  - name: architecture-review
    type: agent
    prompt: prompts/eval/architecture-review.md
    required: false
  - name: human-approval
    type: human
    prompt: prompts/eval/human-approval.md
knowledge:
  - rust
  - rest-api
vars: [RUST_EDITION]
"#;

    #[test]
    fn spec_example_parses_and_validates() {
        let jt = JobType::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(jt.name, "implement-endpoint");
        assert_eq!(jt.eval.len(), 4);
        assert_eq!(jt.work.review.as_ref().unwrap().iteration_budget(), 5);
        assert_eq!(jt.validate(), vec![]);
    }

    #[test]
    fn review_defaults_iterations_and_rejects_codex() {
        let yaml = r#"
name: impl
image: img:latest
work:
  type: agent
  prompt: p.md
  provider: codex
  review:
    prompt: r.md
"#;
        let jt = JobType::parse(yaml).unwrap();
        // review.provider unset → defaults to work provider (codex) → rejected in v1
        assert!(jt.validate().iter().any(|e| matches!(
            e,
            FieldRuleError::Invalid {
                field: "work.review.provider",
                ..
            }
        )));
        assert_eq!(jt.work.review.as_ref().unwrap().iteration_budget(), 5);
    }

    #[test]
    fn review_disallowed_for_command_work() {
        let yaml = r#"
name: deploy
image: img:latest
work:
  type: command
  run: ./deploy.sh
  review:
    prompt: r.md
"#;
        let jt = JobType::parse(yaml).unwrap();
        assert!(jt.validate().iter().any(|e| matches!(
            e,
            FieldRuleError::Disallowed {
                field: "work.review",
                ..
            }
        )));
    }

    #[test]
    fn durations_validated() {
        let yaml = r#"
name: impl
image: img:latest
work:
  type: agent
  prompt: p.md
resources:
  task_timeout: 2 hours
job_deadline: 24h
"#;
        let jt = JobType::parse(yaml).unwrap();
        let errs = jt.validate();
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            &errs[0],
            FieldRuleError::Invalid {
                field: "resources.task_timeout",
                ..
            }
        ));
    }

    #[test]
    fn project_defaults_append_and_collide() {
        let jt = JobType::parse(SPEC_EXAMPLE).unwrap();
        let defaults = ProjectDefaults::parse(
            r#"
eval:
  - name: ci
    type: command
    run: ./scripts/ci.sh
"#,
        )
        .unwrap();
        let merged = jt.with_defaults(&defaults).unwrap();
        assert_eq!(merged.eval.len(), 5);
        assert_eq!(merged.eval.last().unwrap().name, "ci");
        assert_eq!(merged.validate(), vec![]);

        let colliding = ProjectDefaults::parse(
            r#"
eval:
  - name: unit-tests
    type: command
    run: ./scripts/ci.sh
"#,
        )
        .unwrap();
        assert!(jt.with_defaults(&colliding).is_err());
    }

    #[test]
    fn human_work_with_container_eval_requires_evaluator_image() {
        let yaml = r#"
name: review-docs
work:
  type: human
  prompt: prompts/work/review-docs.md
eval:
  - name: linkcheck
    type: command
    run: lychee docs/
"#;
        let jt = JobType::parse(yaml).unwrap();
        let errs = jt.validate();
        assert!(
            errs.iter()
                .any(|e| matches!(e, FieldRuleError::Required { field: "image", .. }))
        );
    }

    #[test]
    fn wrap_up_defaults_to_merge_and_parses_none() {
        let jt = JobType::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(jt.wrap_up.r#type, Finalize::Merge);

        let yaml = r#"
name: deploy
image: img:latest
work:
  type: command
  run: ./deploy.sh
wrap_up:
  type: none
"#;
        let jt = JobType::parse(yaml).unwrap();
        assert_eq!(jt.wrap_up.r#type, Finalize::None);
        assert_eq!(jt.validate(), vec![]);
    }

    #[test]
    fn command_work_disallows_rework_budget() {
        let yaml = r#"
name: deploy-staging
image: registry.acme.com/runners/deploy:latest
work:
  type: command
  run: scripts/deploy.sh staging
rework_budget: 1
"#;
        let jt = JobType::parse(yaml).unwrap();
        let errs = jt.validate();
        assert!(errs.iter().any(|e| matches!(
            e,
            FieldRuleError::Disallowed {
                field: "rework_budget",
                ..
            }
        )));
    }
}

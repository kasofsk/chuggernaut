//! Declarative job type YAML schema (spec §1.1).
//!
//! Durations (`task_timeout`, `job_deadline`, `batch_window`) are kept as strings
//! at this layer ("2h", "30m"); parsing to `std::time::Duration` is TODO and will
//! live here so every consumer shares it.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobType {
    pub name: String,
    /// Required for agent/command work; disallowed at top level for human work.
    pub image: Option<String>,
    pub work: WorkSpec,
    pub resources: Option<Resources>,
    pub job_deadline: Option<String>,
    pub work_retries: Option<u32>,
    pub eval_retries: Option<u32>,
    pub rework_budget: Option<u32>,
    #[serde(default)]
    pub inputs: Vec<InputDecl>,
    #[serde(default)]
    pub eval: Vec<Evaluator>,
    #[serde(default)]
    pub knowledge: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
    #[serde(default)]
    pub vars: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkSpec {
    pub r#type: WorkType,
    /// agent/human: required. command: disallowed.
    pub prompt: Option<String>,
    /// agent only.
    pub provider: Option<Provider>,
    /// agent only.
    pub model: Option<String>,
    /// command only.
    pub run: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkType {
    Agent,
    Command,
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    pub cpu: Option<f64>,
    pub memory: Option<String>,
    pub task_timeout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputDecl {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
                    (self.work.run.is_some(), "work.run"),
                    (self.work.provider.is_some(), "work.provider"),
                    (self.work.model.is_some(), "work.model"),
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

        errs
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
resources:
  cpu: 2
  memory: 4Gi
  task_timeout: 2h
job_deadline: 24h
work_retries: 3
eval_retries: 1
rework_budget: 2
inputs:
  - name: spec
  - name: codebase
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
secrets: [GITHUB_TOKEN]
vars: [RUST_EDITION]
"#;

    #[test]
    fn spec_example_parses_and_validates() {
        let jt = JobType::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(jt.name, "implement-endpoint");
        assert_eq!(jt.eval.len(), 4);
        assert_eq!(jt.validate(), vec![]);
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

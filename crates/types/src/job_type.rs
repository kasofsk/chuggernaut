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
    pub wrap_up: WrapUpSpec,
    pub resources: Option<Resources>,
    /// Optional placement pin (spec §3.1). When set, every container this job
    /// type launches is placed on the named fleet node instead of the
    /// most-free one. A single pin — no labels, no anti-affinity, no spillover.
    pub placement: Option<Placement>,
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WrapUpSpec {
    /// `merge` (default) squash-merges the job branch through the merge
    /// queue/gate; `none` goes straight to Done — for jobs whose effect is
    /// external (deploys, reports) and whose branch is scratch.
    #[serde(default)]
    pub r#type: WrapUpMode,
    /// Optional post-merge command (spec §3.2, design-lifecycle.md wrap-up
    /// hook): a shell command run in the WrapUp phase *after* the squash lands
    /// on the default branch, against the merged main content. It ships the
    /// merged result — a web job publishing its built UI, say — so it only runs
    /// once the merge is final, and never at all if the job is revoked or
    /// escalated before landing. Valid with `type: merge` only. A non-zero exit
    /// escalates the job (the merge is not undone). The command clones the
    /// default branch, so it must be idempotent (a restart may re-launch it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// Image for the `run` container; falls back to the job's top-level image
    /// (like an evaluator, §1.1). Required when `run` is set and the job type
    /// declares no top-level image (`work.type: human`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Secrets injected into the `run` container. Scoped here because that is
    /// the only container they reach; not inherited from `work.secrets`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
}

/// Wrap-up mode after eval-pass (design-lifecycle.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WrapUpMode {
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
    /// Memory limit: a positive integer, optionally suffixed with a binary
    /// unit (`Ki`/`Mi`/`Gi`), or plain bytes — e.g. `512Mi`, `4Gi`, `1048576`.
    /// No other suffixes (`5g`, `4GB` are rejected). Validated at parse time so
    /// a bad value fails offline instead of at container launch.
    #[cfg_attr(feature = "schema", schemars(extend("pattern" = crate::resources::MEMORY_PATTERN)))]
    pub memory: Option<String>,
    pub task_timeout: Option<String>,
}

/// Placement pin (spec §3.1). Shape-only here: `node` names a fleet node, but
/// whether that node is actually configured cannot be checked offline (the
/// fleet lives in the dispatcher's env), so release/`validate` only enforce the
/// name is a well-formed node token — the launch honors or errors on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Placement {
    /// Fleet node name to pin onto (`[A-Za-z0-9_-]+`, the same subject-safe
    /// token the fleet config uses).
    pub node: Option<String>,
}

impl Placement {
    /// True when `node` is a well-formed fleet node token (non-empty,
    /// `[A-Za-z0-9_-]+`). Kept in sync with the fleet config's `is_subject_safe`
    /// without depending on it (`types` is pure data).
    fn node_is_well_formed(node: &str) -> bool {
        !node.is_empty()
            && node
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }
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
    /// Staged evaluation ordering (spec §3.3): evaluators run in ascending
    /// `stage` order; within a stage they fan out in parallel. A later stage's
    /// tasks are created only after every *required* evaluator in the prior
    /// stage passes. Default 0 — a single-stage job is byte-for-byte the
    /// unstaged behavior. Non-negative (`u32`, enforced at parse).
    #[serde(default)]
    pub stage: u32,
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

        // `resources.memory` is an opaque string until the container backend
        // parses it at launch; validate the format here so `chuggernaut
        // validate` and release validation reject a bad limit (e.g. "5g")
        // offline instead of wedging the job when the eval container fails to
        // launch. `resources.cpu` needs no such check — serde already enforces
        // it is a float, and it is never re-parsed from a string downstream.
        if let Some(mem) = self.resources.as_ref().and_then(|r| r.memory.as_deref())
            && let Err(e) = crate::resources::parse_memory(mem)
        {
            errs.push(FieldRuleError::Invalid {
                field: "resources.memory",
                context: format!("job type '{}'", self.name),
                reason: e.to_string(),
            });
        }

        let durations = [
            (
                self.resources
                    .as_ref()
                    .and_then(|r| r.task_timeout.as_deref()),
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

        // Wrap-up command hook (spec §3.2): the post-merge `run` only makes
        // sense for a merge job (there is no merge to follow otherwise), needs an
        // image from somewhere, and its `image`/`secrets` are meaningless without
        // it.
        let wrap = &self.wrap_up;
        let wctx = format!("wrap_up.run in job type '{}'", self.name);
        if wrap.run.is_some() {
            if wrap.r#type == WrapUpMode::None {
                errs.push(FieldRuleError::Disallowed {
                    field: "wrap_up.run",
                    context: "wrap_up.type: none".into(),
                });
            }
            if wrap.image.is_none() && self.image.is_none() {
                errs.push(FieldRuleError::Required {
                    field: "wrap_up.image",
                    context: wctx,
                });
            }
        } else {
            for (present, field) in [
                (wrap.image.is_some(), "wrap_up.image"),
                (!wrap.secrets.is_empty(), "wrap_up.secrets"),
            ] {
                if present {
                    errs.push(FieldRuleError::Disallowed {
                        field,
                        context: format!("wrap_up without run in job type '{}'", self.name),
                    });
                }
            }
        }

        // Placement is shape-validated only (spec §3.1): the fleet node list
        // lives in the dispatcher's env and is not knowable offline, so a bad
        // node token is the only thing catchable here. An unknown-but-valid
        // name surfaces at launch as a placement error.
        if let Some(node) = self.placement.as_ref().and_then(|p| p.node.as_deref())
            && !Placement::node_is_well_formed(node)
        {
            errs.push(FieldRuleError::Invalid {
                field: "placement.node",
                context: format!("job type '{}'", self.name),
                reason: format!("{node:?} is not a valid node name ([A-Za-z0-9_-]+)"),
            });
        }

        errs
    }

    /// The fleet node this job type pins onto, if any (spec §3.1 placement).
    pub fn placement_node(&self) -> Option<&str> {
        self.placement.as_ref().and_then(|p| p.node.as_deref())
    }

    /// Append project default evaluators (`jobs/_defaults.yaml`, spec §1.1) and
    /// apply the project-level default `model` (spec §12.4). An evaluator name
    /// collision between the defaults and this job type is an error. Validate
    /// the merged result with [`JobType::validate`] — image fallback rules apply
    /// against this job type's top-level image.
    ///
    /// The project `model` is folded in as a fallback for every agent that does
    /// not already declare one — the work agent and each agent evaluator (the
    /// same reach as the platform default, spec §12.4). It sits *below* the job
    /// type's own `model` (a type that names a model keeps it) and *above* the
    /// platform default, giving the resolution chain `job type → project default
    /// → platform default`. Command/human work and command/human evaluators take
    /// no model, so they are left untouched.
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
        if let Some(model) = &defaults.model {
            if merged.work.r#type == WorkType::Agent && merged.work.model.is_none() {
                merged.work.model = Some(model.clone());
            }
            for e in merged.eval.iter_mut() {
                if e.r#type == EvaluatorType::Agent && e.model.is_none() {
                    e.model = Some(model.clone());
                }
            }
        }
        Ok(merged)
    }
}

/// Project-wide defaults, `jobs/_defaults.yaml` (spec §1.1, §12.4). The `eval`
/// list is appended to every job type's evaluators — how a project gates all
/// changes on an evergreen suite without per-job-type declarations. `model`
/// sets a project-level default agent model layered between the platform
/// default and job-type declarations (spec §12.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProjectDefaults {
    #[serde(default)]
    pub eval: Vec<Evaluator>,
    /// Project-level default agent model (spec §12.4). Applied to the work
    /// agent and agent evaluators that do not declare their own `model`.
    #[serde(default)]
    pub model: Option<String>,
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
    fn memory_format_validated() {
        let jt_with_mem = |mem: &str| {
            JobType::parse(&format!(
                r#"
name: impl
image: img:latest
work:
  type: agent
  prompt: p.md
resources:
  memory: "{mem}"
"#
            ))
            .unwrap()
        };

        // The dogfood bug: "5g" passed validate but failed at launch.
        let errs = jt_with_mem("5g").validate();
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            &errs[0],
            FieldRuleError::Invalid {
                field: "resources.memory",
                ..
            }
        ));
        // The accepted form validates cleanly.
        assert_eq!(jt_with_mem("5Gi").validate(), vec![]);
        assert_eq!(jt_with_mem("512Mi").validate(), vec![]);
        assert_eq!(jt_with_mem("1048576").validate(), vec![]);

        // Other malformed forms are all rejected offline.
        for bad in ["4GB", "-5", "0", "1.5Gi"] {
            assert!(
                jt_with_mem(bad).validate().iter().any(|e| matches!(
                    e,
                    FieldRuleError::Invalid {
                        field: "resources.memory",
                        ..
                    }
                )),
                "should reject memory: {bad}"
            );
        }
    }

    #[test]
    fn placement_shape_validated() {
        let jt_with_node = |node: &str| {
            JobType::parse(&format!(
                r#"
name: impl
image: img:latest
work:
  type: agent
  prompt: p.md
placement:
  node: "{node}"
"#
            ))
            .unwrap()
        };

        // Well-formed node tokens validate and thread through `placement_node`.
        let jt = jt_with_node("gumbo-nuc-0");
        assert_eq!(jt.validate(), vec![]);
        assert_eq!(jt.placement_node(), Some("gumbo-nuc-0"));

        // A bad token is caught offline — the node existing is not checkable
        // here, but its shape is.
        for bad in ["nuc.0", "has space", "nuc>"] {
            let errs = jt_with_node(bad).validate();
            assert!(
                errs.iter().any(|e| matches!(
                    e,
                    FieldRuleError::Invalid {
                        field: "placement.node",
                        ..
                    }
                )),
                "expected placement.node error for {bad:?}, got {errs:?}"
            );
        }

        // Omitted placement (and an empty block) is fine: no pin.
        let none = JobType::parse(
            r#"
name: impl
image: img:latest
work:
  type: agent
  prompt: p.md
"#,
        )
        .unwrap();
        assert_eq!(none.validate(), vec![]);
        assert_eq!(none.placement_node(), None);
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
    fn project_default_model_fills_undeclared_agents_only() {
        // A job type whose work agent and one evaluator declare no model, plus
        // an evaluator and a command evaluator that do.
        let yaml = r#"
name: code
image: img:latest
work:
  type: agent
  prompt: p.md
eval:
  - name: review
    type: agent
    prompt: r.md
  - name: security
    type: agent
    prompt: s.md
    model: claude-opus-4-8
  - name: ci
    type: command
    run: ./ci.sh
"#;
        let jt = JobType::parse(yaml).unwrap();
        let defaults = ProjectDefaults::parse("model: claude-sonnet-5\n").unwrap();
        let merged = jt.with_defaults(&defaults).unwrap();
        // Work agent: undeclared → gets the project default.
        assert_eq!(merged.work.model.as_deref(), Some("claude-sonnet-5"));
        // Agent evaluator without a model → gets the project default.
        assert_eq!(merged.eval[0].model.as_deref(), Some("claude-sonnet-5"));
        // Agent evaluator that declared one → keeps it (type wins over project).
        assert_eq!(merged.eval[1].model.as_deref(), Some("claude-opus-4-8"));
        // Command evaluator → left untouched (takes no model).
        assert_eq!(merged.eval[2].model, None);
        assert_eq!(merged.validate(), vec![]);
    }

    #[test]
    fn project_default_model_does_not_override_declared_work_model() {
        let jt = JobType::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(jt.work.model.as_deref(), Some("claude-sonnet-4-6"));
        let defaults = ProjectDefaults::parse("model: claude-sonnet-5\n").unwrap();
        let merged = jt.with_defaults(&defaults).unwrap();
        // The type's own work.model wins over the project default.
        assert_eq!(merged.work.model.as_deref(), Some("claude-sonnet-4-6"));
        // The type's undeclared-model agent evaluators pick up the default;
        // the ones that declared a model keep it.
        let arch = merged.eval.iter().find(|e| e.name == "architecture-review");
        assert_eq!(arch.unwrap().model.as_deref(), Some("claude-sonnet-5"));
        let sec = merged.eval.iter().find(|e| e.name == "security-review");
        assert_eq!(sec.unwrap().model.as_deref(), Some("claude-opus-4-6"));
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
        assert_eq!(jt.wrap_up.r#type, WrapUpMode::Merge);

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
        assert_eq!(jt.wrap_up.r#type, WrapUpMode::None);
        assert_eq!(jt.validate(), vec![]);
    }

    #[test]
    fn wrap_up_run_parses_and_validates_against_top_level_image() {
        // The web-job shape: default `merge` wrap-up plus a post-merge publish
        // command that inherits the top-level image and declares its own secret.
        let yaml = r#"
name: web
image: img:latest
work:
  type: agent
  prompt: p.md
wrap_up:
  run: ./tasks/web-publish.sh
  secrets: [MINI_DEPLOY_KEY]
"#;
        let jt = JobType::parse(yaml).unwrap();
        assert_eq!(jt.wrap_up.r#type, WrapUpMode::Merge);
        assert_eq!(jt.wrap_up.run.as_deref(), Some("./tasks/web-publish.sh"));
        assert_eq!(jt.wrap_up.secrets, vec!["MINI_DEPLOY_KEY".to_string()]);
        assert_eq!(jt.validate(), vec![]);
    }

    #[test]
    fn wrap_up_run_takes_its_own_image() {
        // A human-work job has no top-level image, so the wrap-up command must
        // carry one; when it does, it validates.
        let yaml = r#"
name: manual
work:
  type: human
  prompt: p.md
wrap_up:
  run: ./publish.sh
  image: publisher:latest
"#;
        let jt = JobType::parse(yaml).unwrap();
        assert_eq!(jt.wrap_up.image.as_deref(), Some("publisher:latest"));
        assert_eq!(jt.validate(), vec![]);
    }

    #[test]
    fn wrap_up_run_requires_an_image_and_forbids_type_none() {
        // No top-level image and no wrap_up.image → the command cannot launch;
        // and `run` on a `type: none` job has no merge to follow.
        let yaml = r#"
name: manual
work:
  type: human
  prompt: p.md
wrap_up:
  type: none
  run: ./publish.sh
"#;
        let jt = JobType::parse(yaml).unwrap();
        let errs = jt.validate();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                FieldRuleError::Required {
                    field: "wrap_up.image",
                    ..
                }
            )),
            "missing image should be Required: {errs:?}"
        );
        assert!(
            errs.iter().any(|e| matches!(
                e,
                FieldRuleError::Disallowed {
                    field: "wrap_up.run",
                    ..
                }
            )),
            "run on type: none should be Disallowed: {errs:?}"
        );
    }

    #[test]
    fn wrap_up_image_and_secrets_disallowed_without_run() {
        let yaml = r#"
name: web
image: img:latest
work:
  type: agent
  prompt: p.md
wrap_up:
  image: publisher:latest
  secrets: [TOKEN]
"#;
        let jt = JobType::parse(yaml).unwrap();
        let errs = jt.validate();
        assert!(errs.iter().any(|e| matches!(
            e,
            FieldRuleError::Disallowed {
                field: "wrap_up.image",
                ..
            }
        )));
        assert!(errs.iter().any(|e| matches!(
            e,
            FieldRuleError::Disallowed {
                field: "wrap_up.secrets",
                ..
            }
        )));
    }

    #[test]
    fn evaluator_stage_defaults_zero_parses_and_validates() {
        // Omitted `stage` defaults to 0 across all evaluator kinds; an explicit
        // non-negative value parses. A negative value is rejected at parse
        // (serde into u32), so it can never reach validate().
        let jt = JobType::parse(SPEC_EXAMPLE).unwrap();
        assert!(jt.eval.iter().all(|e| e.stage == 0));

        let yaml = r#"
name: impl
image: img:latest
work:
  type: agent
  prompt: p.md
eval:
  - name: review
    type: agent
    prompt: r.md
    stage: 0
  - name: ci
    type: command
    run: ./ci.sh
    stage: 2
"#;
        let jt = JobType::parse(yaml).unwrap();
        assert_eq!(jt.eval[0].stage, 0);
        assert_eq!(jt.eval[1].stage, 2);
        assert_eq!(jt.validate(), vec![]);

        let negative = r#"
name: impl
image: img:latest
work:
  type: agent
  prompt: p.md
eval:
  - name: review
    type: agent
    prompt: r.md
    stage: -1
"#;
        assert!(JobType::parse(negative).is_err());
    }

    #[test]
    fn project_defaults_preserve_declared_stage_after_append() {
        // The append does not reorder; the default keeps whatever `stage` it
        // declares, so a job type's stage-0 review sits ahead of a stage-1 CI
        // default in the merged list.
        let yaml = r#"
name: code
image: img:latest
work:
  type: agent
  prompt: p.md
eval:
  - name: review
    type: agent
    prompt: r.md
    stage: 0
"#;
        let jt = JobType::parse(yaml).unwrap();
        let defaults = ProjectDefaults::parse(
            r#"
eval:
  - name: ci
    type: command
    run: ./tasks/ci.sh
    stage: 1
"#,
        )
        .unwrap();
        let merged = jt.with_defaults(&defaults).unwrap();
        assert_eq!(merged.eval.len(), 2);
        assert_eq!(merged.eval[0].name, "review");
        assert_eq!(merged.eval[0].stage, 0);
        assert_eq!(merged.eval[1].name, "ci");
        assert_eq!(merged.eval[1].stage, 1);
        assert_eq!(merged.validate(), vec![]);
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

    #[test]
    fn repo_doc_producing_job_types_parse_and_validate() {
        // The shipped `design`/`docs` job types (spec §9.4) must parse and pass
        // the §1.1 field rules once the project `_defaults.yaml` is merged in —
        // exactly as the dispatcher loads them at release. Embedded from the
        // repo so an edit to either file that breaks the schema fails here, at
        // the lowest tier, instead of at a live job launch.
        let defaults =
            ProjectDefaults::parse(include_str!("../../../jobs/_defaults.yaml")).unwrap();
        for (name, yaml) in [
            ("design", include_str!("../../../jobs/design.yaml")),
            ("docs", include_str!("../../../jobs/docs.yaml")),
        ] {
            let jt =
                JobType::parse(yaml).unwrap_or_else(|e| panic!("{name}.yaml parse error: {e}"));
            assert_eq!(jt.name, name);
            let merged = jt
                .with_defaults(&defaults)
                .unwrap_or_else(|e| panic!("{name}.yaml default merge error: {e}"));
            assert_eq!(merged.validate(), vec![], "{name}.yaml field rules");
            // The shared doc lint is a stage-1 command evaluator, the type's own
            // reviewer is stage 0, and the appended `ci` default rounds out
            // stage 1 (it self-skips a doc-only diff).
            assert!(
                merged
                    .eval
                    .iter()
                    .any(|e| e.name == "doc-lint" && e.stage == 1),
                "{name}.yaml should wire doc-lint at stage 1"
            );
            assert!(
                merged
                    .eval
                    .iter()
                    .any(|e| e.name.starts_with("review-") && e.stage == 0),
                "{name}.yaml should wire its agent reviewer at stage 0"
            );
            assert!(
                merged.eval.iter().any(|e| e.name == "ci"),
                "{name}.yaml should inherit the project ci default"
            );
        }
    }
}

//! Declarative job type YAML schema (spec §1.1).
//!
//! Durations (`task_timeout`, `job_deadline`, `batch_window`) are kept as strings
//! in the schema ("2h", "30m"); [`crate::duration::parse_duration`] is the shared
//! parser, and `validate()` checks parseability.

use crate::duration::parse_duration;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// The top-level job-type struct deliberately does **not** carry
/// `deny_unknown_fields` (unlike every nested block below): an unknown
/// top-level key is captured into [`JobType::unknown`] and surfaced as a
/// *warning*, not a parse error. This is the schema-tolerance half of the
/// config/binary version-skew contract (spec §14): job-type config is read
/// live from the default branch, so a config change can land ahead of the
/// running dispatcher. A newly-added top-level section (a future `wrap_up`,
/// `deploy`, …) an older binary doesn't know about means "a feature is quietly
/// off" — acceptable when flagged loudly, and vastly preferable to the
/// 2026-07-22 incident where the strict parser rejected the whole config and
/// escalated every job.
///
/// Laxity is safe *only* at the top level. The nested blocks keep
/// `deny_unknown_fields` because an unknown field inside a security-relevant
/// section is not benign: an ignored key inside an [`Evaluator`] could silently
/// skip a *gate* (a typo'd `required: flase`, a mis-nested check), turning
/// "config ahead of binary" into "a merge gate quietly disabled". Those stay
/// hard errors; see [`JobType::validate`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// The values a job of this type accepts (spec §1.1, design #311). Empty for
    /// every job type that declares none, which is every job type that predates
    /// the feature.
    ///
    /// An input is a **value delivered to a running container**, never a
    /// substitution into this file: nothing here can select an image, an
    /// evaluator, a secret or a `run:` string, so the job type resolves without
    /// reading a job's inputs at all (#311 Decision 1). Parameterization happens
    /// inside the work, where `deploy.sh` reads `$CHUG_INPUT_SERVICE`.
    ///
    /// A non-empty list requires [`JobType::min_dispatcher`] — see
    /// [`JobType::validate`]. To an N-1 dispatcher `inputs:` is just an unknown
    /// top-level field it tolerates (captured into [`JobType::unknown`]), so the
    /// declaration would be silently ignored and the container would launch with
    /// no value at all; `min_dispatcher` is a field that dispatcher *does* parse,
    /// which is why the skew gate is structural rather than left to authorship.
    #[serde(default)]
    pub inputs: Vec<Input>,
    /// Minimum dispatcher schema epoch this config requires (spec §14). When
    /// set and greater than the running dispatcher's
    /// [`crate::version::CONFIG_SCHEMA_EPOCH`], the config is ahead of the
    /// binary: the dispatcher parks the job with a platform-level diagnostic
    /// ("config requires dispatcher >= X") instead of launching it, and the
    /// merge-time CI check fails the config's own build if it can reach a
    /// deployed dispatcher advertising an older epoch. Author it in the same
    /// commit that relies on a schema feature the previous generation lacks, so
    /// "merging config" can never silently become "deploying config".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_dispatcher: Option<u32>,
    /// Unknown top-level fields, captured rather than rejected (see the type
    /// doc). Populated by serde's flatten catch-all; each key surfaces as a
    /// [`ConfigWarning`] from [`JobType::config_warnings`]. Skipped from the
    /// generated JSON Schema (`serde_yaml::Value` has no `JsonSchema` impl, and
    /// the schema advertises tolerance via an absent top-level
    /// `additionalProperties: false`).
    #[serde(flatten, default)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub unknown: BTreeMap<String, serde_yaml::Value>,
}

/// A non-fatal job-type config warning (spec §14): the config is accepted and
/// will run, but something in it the running dispatcher does not understand was
/// ignored. Emitted as a loud platform-level event so an operator sees the
/// silently-off feature rather than discovering it job-by-job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    /// The unknown top-level field name that was ignored.
    pub field: String,
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown top-level field '{}' ignored (config is ahead of this dispatcher; \
             the feature it configures is off until the dispatcher is deployed)",
            self.field
        )
    }
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
    /// Human-facing label for the wrap-up task, so it is as self-describing as
    /// an evaluator (`Command · publish` instead of a bare `Command`, job #146).
    /// Validated like an evaluator name. Unset → derived from the mode (see
    /// [`WrapUpSpec::label`]): a command wrap-up takes its script's basename
    /// (`.chug/tasks/web-publish.sh` → `web-publish`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
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

impl WrapUpSpec {
    /// The wrap-up task's display label (job #146): the explicit `name`, else a
    /// value derived from the mode. A command wrap-up (`run` set) derives from
    /// the script's basename with any extension stripped
    /// (`./tasks/web-publish.sh staging` → `web-publish`); anything else falls
    /// back to a generic marker. Only consulted when a wrap-up task actually
    /// launches, so the fallback is a safety net, not a common path.
    pub fn label(&self) -> String {
        if let Some(name) = self.name.as_ref().filter(|n| !n.is_empty()) {
            return name.clone();
        }
        if let Some(run) = &self.run
            && let Some(derived) = derive_command_label(run)
        {
            return derived;
        }
        "wrap-up-agent".to_string()
    }
}

/// Derive a task label from a shell `run` command: the first token's path
/// basename with a single trailing extension stripped
/// (`./tasks/web-publish.sh staging` → `web-publish`). None when the command is
/// empty or reduces to nothing usable.
fn derive_command_label(run: &str) -> Option<String> {
    let first = run.split_whitespace().next()?;
    let base = first.rsplit('/').next().unwrap_or(first);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    (!stem.is_empty()).then(|| stem.to_string())
}

/// Whether a label/evaluator-style name is well-formed (job #146): non-empty and
/// limited to `[A-Za-z0-9._-]` — no whitespace or shell-hostile characters, so
/// it renders cleanly in a task row and travels safely through events.
pub fn is_valid_task_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
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

/// One declared job input (spec §1.1, design #311 Decision 2): a name, a kind,
/// and the narrowing that makes a supplied value safe to hand to a script.
///
/// A nested block, so it keeps `deny_unknown_fields` like every other
/// gate-relevant block (§14.2): an ignored key here could silently drop a
/// `pattern`, and `pattern` is a validation control, not decoration.
///
/// The kind set is deliberately two. `bool` is an `enum` over `["true",
/// "false"]`, `int` is a `string` with `pattern: '^[0-9]+$'`, and lists have no
/// env representation that is not an encoding decision — the env value is a
/// string either way, so a richer type system here would be a second config
/// language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Input {
    /// `[a-z][a-z0-9_]*` ([`crate::inputs::INPUT_NAME_PATTERN`]) — lowercase so
    /// the mapping onto one reserved env name is injective.
    #[cfg_attr(feature = "schema", schemars(extend("pattern" = crate::inputs::INPUT_NAME_PATTERN)))]
    pub name: String,
    pub r#type: InputKind,
    /// Default false. An optional input with no supplied value and no `default`
    /// is *absent*, never an empty string: `set -u` catches an unset
    /// `$CHUG_INPUT_SHA` loudly, where an empty string would silently run
    /// `update.sh ` with no argument.
    #[serde(default)]
    pub required: bool,
    /// A value the platform materializes onto the job record when the creator
    /// supplies none — not a create-form pre-fill, so what actually ran is on
    /// the audit surfaces. Disallowed with `required: true`, and validated here
    /// against the charset and this declaration's own `pattern`/`values`: a
    /// default no supply path could have produced would otherwise arrive by the
    /// back door and be caught only at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(extend("pattern" = crate::inputs::INPUT_VALUE_PATTERN)))]
    pub default: Option<String>,
    /// The closed list for `type: enum`; disallowed for `type: string`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// A regex the **whole** value must match; `type: string` only. It may only
    /// narrow the default charset, never widen it (the effective check is
    /// `charset AND pattern` — [`crate::inputs::check_value`]). An input whose
    /// value reaches an argv position wants one: the charset stops metacharacter
    /// injection but not a value that begins with `-` or `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Shown in the create form and in the agent's job brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// What kind of value an [`Input`] accepts (spec §1.1, design #311 Decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InputKind {
    /// Any value inside the default charset, optionally narrowed by `pattern`.
    String,
    /// One of a closed list of `values`.
    Enum,
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
    #[allow(
        clippy::too_many_lines,
        reason = "TODO(style): pre-existing violation (refactor-plan A4) — fix when this function is next touched."
    )]
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

        let wrap = &self.wrap_up;
        if let Some(name) = wrap.name.as_deref()
            && !is_valid_task_name(name)
        {
            errs.push(FieldRuleError::Invalid {
                field: "wrap_up.name",
                context: format!("job type '{}'", self.name),
                reason: format!("{name:?} is not a valid name ([A-Za-z0-9._-]+)"),
            });
        }
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

        if let Some(node) = self.placement.as_ref().and_then(|p| p.node.as_deref())
            && !Placement::node_is_well_formed(node)
        {
            errs.push(FieldRuleError::Invalid {
                field: "placement.node",
                context: format!("job type '{}'", self.name),
                reason: format!("{node:?} is not a valid node name ([A-Za-z0-9_-]+)"),
            });
        }

        errs.extend(self.validate_inputs());
        errs
    }

    /// The `inputs:` block's declaration rules (spec §1.1, design #311
    /// Decision 2). Split out of [`JobType::validate`] so the declaration rules
    /// and the per-input rules each fit inside the 70-line bound; dissolving
    /// `validate`'s own pre-existing violation is refactor-plan A4, not this
    /// slice. Everything here is about the *declaration*; what a
    /// supplied value must satisfy lives in [`crate::inputs`], shared with the
    /// release, Ready-transition and launch passes.
    ///
    /// A type declaring no inputs produces no errors from here, so it validates
    /// exactly as it did before the feature existed.
    fn validate_inputs(&self) -> Vec<FieldRuleError> {
        let mut errs = Vec::new();
        if self.inputs.is_empty() {
            return errs;
        }
        let ctx = format!("job type '{}'", self.name);
        if self.min_dispatcher.unwrap_or(0) < crate::version::INPUTS_SCHEMA_EPOCH {
            errs.push(FieldRuleError::Required {
                field: "min_dispatcher",
                context: format!(
                    "a job type declaring 'inputs:' (needs min_dispatcher >= {})",
                    crate::version::INPUTS_SCHEMA_EPOCH
                ),
            });
        }
        if self.inputs.len() > crate::inputs::INPUTS_COUNT_MAX {
            errs.push(FieldRuleError::Invalid {
                field: "inputs",
                context: ctx.clone(),
                reason: format!(
                    "{} declared inputs exceeds the limit of {}",
                    self.inputs.len(),
                    crate::inputs::INPUTS_COUNT_MAX
                ),
            });
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for input in &self.inputs {
            if !crate::inputs::name_is_well_formed(&input.name) {
                errs.push(FieldRuleError::Invalid {
                    field: "inputs.name",
                    context: ctx.clone(),
                    reason: format!(
                        "{:?} is not a valid input name ({})",
                        input.name,
                        crate::inputs::INPUT_NAME_PATTERN
                    ),
                });
            } else if !seen.insert(input.name.as_str()) {
                errs.push(FieldRuleError::Invalid {
                    field: "inputs.name",
                    context: ctx.clone(),
                    reason: format!("input '{}' is declared twice", input.name),
                });
            }
            errs.extend(self.validate_inputs_declaration(input));
        }
        errs
    }

    /// One [`Input`]'s kind-specific rules and its `default` (design #311
    /// Decision 2). Kept separate from [`JobType::validate_inputs`], which owns
    /// the block-level rules (the skew gate, the count bound, name shape).
    fn validate_inputs_declaration(&self, input: &Input) -> Vec<FieldRuleError> {
        let mut errs = Vec::new();
        let ctx = format!("input '{}' in job type '{}'", input.name, self.name);
        match input.r#type {
            InputKind::Enum => {
                if input.values.is_empty() {
                    errs.push(FieldRuleError::Required {
                        field: "inputs.values",
                        context: ctx.clone(),
                    });
                }
                if input.pattern.is_some() {
                    errs.push(FieldRuleError::Disallowed {
                        field: "inputs.pattern",
                        context: ctx.clone(),
                    });
                }
                for value in &input.values {
                    if let Err(e) = crate::inputs::check_value_charset(value) {
                        errs.push(FieldRuleError::Invalid {
                            field: "inputs.values",
                            context: ctx.clone(),
                            reason: e.to_string(),
                        });
                    }
                }
            }
            InputKind::String => {
                if !input.values.is_empty() {
                    errs.push(FieldRuleError::Disallowed {
                        field: "inputs.values",
                        context: ctx.clone(),
                    });
                }
                if let Some(pattern) = &input.pattern
                    && let Err(e) = crate::inputs::check_pattern(pattern)
                {
                    errs.push(FieldRuleError::Invalid {
                        field: "inputs.pattern",
                        context: ctx.clone(),
                        reason: e.to_string(),
                    });
                }
            }
        }
        if let Some(default) = &input.default {
            if input.required {
                errs.push(FieldRuleError::Disallowed {
                    field: "inputs.default",
                    context: format!("{ctx} with required: true"),
                });
            } else if errs.is_empty()
                && let Err(e) = crate::inputs::check_value(input, default)
            {
                errs.push(FieldRuleError::Invalid {
                    field: "inputs.default",
                    context: ctx,
                    reason: e.to_string(),
                });
            }
        }
        errs
    }

    /// The fleet node this job type pins onto, if any (spec §3.1 placement).
    pub fn placement_node(&self) -> Option<&str> {
        self.placement.as_ref().and_then(|p| p.node.as_deref())
    }

    /// Non-fatal config warnings (spec §14): unknown top-level fields that were
    /// tolerated (config accepted, field ignored) rather than rejected. The
    /// dispatcher emits these as a loud platform event; they never block a
    /// launch. Deterministically ordered (the backing map is a `BTreeMap`).
    pub fn config_warnings(&self) -> Vec<ConfigWarning> {
        self.unknown
            .keys()
            .map(|field| ConfigWarning {
                field: field.clone(),
            })
            .collect()
    }

    /// If this config requires a newer dispatcher than `dispatcher_epoch`
    /// (usually [`crate::version::CONFIG_SCHEMA_EPOCH`]), the epoch it needs.
    /// `Some(needed)` means the config is ahead of the binary: the dispatcher
    /// must park the job with a diagnostic naming the needed version rather than
    /// launch it, and the merge-time CI check must fail the config's build.
    pub fn requires_dispatcher(&self, dispatcher_epoch: u32) -> Option<u32> {
        self.min_dispatcher.filter(|&need| need > dispatcher_epoch)
    }

    /// Append project default evaluators (`.chug/jobs/_defaults.yaml`, spec §1.1) and
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

/// Project-wide defaults, `.chug/jobs/_defaults.yaml` (spec §1.1, §12.4). The `eval`
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
name: implement-endpoint
image: registry.acme.com/agents/impl:latest
work:
  type: agent
  prompt: .chug/prompts/work/implement-endpoint.md
  provider: claude
  model: claude-sonnet-4-6
  secrets: [GITHUB_TOKEN]
  review:
    prompt: .chug/prompts/review/implement-endpoint.md
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
    prompt: .chug/prompts/eval/security-review.md
    provider: claude
    model: claude-opus-4-6
    secrets: [GITHUB_TOKEN]
  - name: architecture-review
    type: agent
    prompt: .chug/prompts/eval/architecture-review.md
    required: false
  - name: human-approval
    type: human
    prompt: .chug/prompts/eval/human-approval.md
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

        let errs = jt_with_mem("5g").validate();
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            &errs[0],
            FieldRuleError::Invalid {
                field: "resources.memory",
                ..
            }
        ));
        assert_eq!(jt_with_mem("5Gi").validate(), vec![]);
        assert_eq!(jt_with_mem("512Mi").validate(), vec![]);
        assert_eq!(jt_with_mem("1048576").validate(), vec![]);

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

        let jt = jt_with_node("gumbo-nuc-0");
        assert_eq!(jt.validate(), vec![]);
        assert_eq!(jt.placement_node(), Some("gumbo-nuc-0"));

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
        assert_eq!(merged.work.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(merged.eval[0].model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(merged.eval[1].model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(merged.eval[2].model, None);
        assert_eq!(merged.validate(), vec![]);
    }

    #[test]
    fn project_default_model_does_not_override_declared_work_model() {
        let jt = JobType::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(jt.work.model.as_deref(), Some("claude-sonnet-4-6"));
        let defaults = ProjectDefaults::parse("model: claude-sonnet-5\n").unwrap();
        let merged = jt.with_defaults(&defaults).unwrap();
        assert_eq!(merged.work.model.as_deref(), Some("claude-sonnet-4-6"));
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
  prompt: .chug/prompts/work/review-docs.md
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
    fn wrap_up_name_round_trips_and_derives_label() {
        let yaml = r#"
name: web
image: img:latest
work:
  type: agent
  prompt: p.md
wrap_up:
  run: ./tasks/web-publish.sh
  name: publish
"#;
        let jt = JobType::parse(yaml).unwrap();
        assert_eq!(jt.wrap_up.name.as_deref(), Some("publish"));
        assert_eq!(jt.wrap_up.label(), "publish");
        assert_eq!(jt.validate(), vec![]);
        let no_name = JobType::parse(
            r#"
name: web
image: img:latest
work:
  type: agent
  prompt: p.md
wrap_up:
  run: ./tasks/web-publish.sh
"#,
        )
        .unwrap();
        assert_eq!(no_name.wrap_up.name, None);
        assert!(
            !serde_yaml::to_string(&no_name.wrap_up)
                .unwrap()
                .contains("name")
        );
        assert_eq!(no_name.wrap_up.label(), "web-publish");

        let derived = |run: &str| {
            WrapUpSpec {
                run: Some(run.into()),
                ..Default::default()
            }
            .label()
        };
        assert_eq!(derived("./tasks/web-publish.sh staging"), "web-publish");
        assert_eq!(derived("deploy"), "deploy");
        assert_eq!(WrapUpSpec::default().label(), "wrap-up-agent");
    }

    #[test]
    fn wrap_up_name_rejects_bad_tokens() {
        let jt_with_name = |name: &str| {
            JobType::parse(&format!(
                r#"
name: web
image: img:latest
work:
  type: agent
  prompt: p.md
wrap_up:
  run: ./tasks/web-publish.sh
  name: "{name}"
"#
            ))
            .unwrap()
        };
        assert_eq!(jt_with_name("publish").validate(), vec![]);
        assert_eq!(jt_with_name("web-publish").validate(), vec![]);
        for bad in ["has space", "bad;rm", "", "na/me"] {
            let errs = jt_with_name(bad).validate();
            assert!(
                errs.iter().any(|e| matches!(
                    e,
                    FieldRuleError::Invalid {
                        field: "wrap_up.name",
                        ..
                    }
                )),
                "expected wrap_up.name error for {bad:?}, got {errs:?}"
            );
        }
    }

    #[test]
    fn evaluator_stage_defaults_zero_parses_and_validates() {
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
    fn unknown_top_level_field_is_tolerated_with_a_warning() {
        let yaml = r#"
name: web
image: img:latest
work:
  type: agent
  prompt: p.md
future_section:
  some_new_knob: true
another_unknown: 42
"#;
        let jt = JobType::parse(yaml).expect("unknown top-level fields must not fail parsing");
        assert_eq!(
            jt.validate(),
            vec![],
            "unknown fields are not field-rule errors"
        );
        let warnings = jt.config_warnings();
        let fields: Vec<&str> = warnings.iter().map(|w| w.field.as_str()).collect();
        assert_eq!(fields, vec!["another_unknown", "future_section"]);
        assert!(warnings[0].to_string().contains("ignored"));
        assert!(
            JobType::parse(SPEC_EXAMPLE)
                .unwrap()
                .config_warnings()
                .is_empty()
        );
    }

    #[test]
    fn unknown_field_inside_evaluator_is_still_a_hard_error() {
        let yaml = r#"
name: code
image: img:latest
work:
  type: agent
  prompt: p.md
eval:
  - name: ci
    type: command
    run: ./ci.sh
    requird: false
"#;
        let err = JobType::parse(yaml).expect_err("unknown evaluator field must fail parsing");
        assert!(err.to_string().contains("requird"), "{err}");
    }

    #[test]
    fn min_dispatcher_gates_config_ahead_of_binary() {
        let yaml = r#"
name: web
image: img:latest
work:
  type: agent
  prompt: p.md
min_dispatcher: 5
"#;
        let jt = JobType::parse(yaml).unwrap();
        assert_eq!(jt.min_dispatcher, Some(5));
        assert_eq!(jt.validate(), vec![]);
        assert_eq!(jt.requires_dispatcher(1), Some(5));
        assert_eq!(jt.requires_dispatcher(5), None);
        assert_eq!(jt.requires_dispatcher(6), None);
        assert_eq!(
            JobType::parse(SPEC_EXAMPLE).unwrap().requires_dispatcher(0),
            None
        );
    }

    /// A command job type carrying `block` as its `inputs:` list, with the
    /// `min_dispatcher` a non-empty block requires already declared — so each
    /// test below asserts about its own rule and nothing else.
    fn jt_with_inputs(block: &str) -> JobType {
        JobType::parse(&format!(
            "name: rollback\nimage: img:latest\nmin_dispatcher: {}\n\
             work:\n  type: command\n  run: ./.chug/tasks/rollback.sh\ninputs:\n{block}",
            crate::version::INPUTS_SCHEMA_EPOCH
        ))
        .expect("inputs block should parse")
    }

    fn input_errors(block: &str) -> Vec<FieldRuleError> {
        jt_with_inputs(block).validate()
    }

    #[test]
    fn inputs_block_parses_and_validates() {
        let jt = jt_with_inputs(
            "  - name: sha\n    type: string\n    required: true\n    \
             pattern: '^[0-9a-f]{7,40}$'\n    description: The commit SHA.\n\
             \x20 - name: service\n    type: enum\n    values: [web, worker, bot]\n    \
             default: web\n",
        );
        assert_eq!(jt.validate(), vec![]);
        assert_eq!(jt.inputs.len(), 2);
        assert_eq!(jt.inputs[0].name, "sha");
        assert_eq!(jt.inputs[0].r#type, InputKind::String);
        assert!(jt.inputs[0].required);
        assert_eq!(jt.inputs[1].r#type, InputKind::Enum);
        assert!(!jt.inputs[1].required);
        assert_eq!(jt.inputs[1].default.as_deref(), Some("web"));
        let none = JobType::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(none.inputs, vec![]);
        assert_eq!(none.validate(), vec![]);
    }

    #[test]
    fn unknown_field_inside_an_input_is_a_hard_error() {
        let yaml = format!(
            "name: rollback\nimage: img:latest\nmin_dispatcher: {}\n\
             work:\n  type: command\n  run: ./r.sh\n\
             inputs:\n  - name: sha\n    type: string\n    patern: '^[0-9a-f]+$'\n",
            crate::version::INPUTS_SCHEMA_EPOCH
        );
        let err = JobType::parse(&yaml).expect_err("unknown input field must fail parsing");
        assert!(err.to_string().contains("patern"), "{err}");
    }

    #[test]
    fn non_empty_inputs_require_the_min_dispatcher_declaration() {
        let yaml = |line: &str| {
            format!(
                "name: rollback\nimage: img:latest\n{line}work:\n  type: command\n  run: ./r.sh\n\
                 inputs:\n  - name: sha\n    type: string\n"
            )
        };
        let missing = JobType::parse(&yaml("")).unwrap();
        assert_eq!(
            missing.validate(),
            vec![FieldRuleError::Required {
                field: "min_dispatcher",
                context: format!(
                    "a job type declaring 'inputs:' (needs min_dispatcher >= {})",
                    crate::version::INPUTS_SCHEMA_EPOCH
                ),
            }]
        );
        let stale = JobType::parse(&yaml(&format!(
            "min_dispatcher: {}\n",
            crate::version::INPUTS_SCHEMA_EPOCH - 1
        )))
        .unwrap();
        assert!(stale.validate().iter().any(|e| matches!(
            e,
            FieldRuleError::Required {
                field: "min_dispatcher",
                ..
            }
        )));
        let newer = JobType::parse(&yaml(&format!(
            "min_dispatcher: {}\n",
            crate::version::INPUTS_SCHEMA_EPOCH + 1
        )))
        .unwrap();
        assert_eq!(newer.validate(), vec![]);
        assert_eq!(
            JobType::parse("name: x\nimage: i:l\nwork:\n  type: command\n  run: ./r.sh\n")
                .unwrap()
                .validate(),
            vec![]
        );
    }

    #[test]
    fn input_names_are_lowercase_tokens_and_unique() {
        for bad in ["IMAGE_TAG", "1sha", "_sha", "image-tag", "image.tag"] {
            let errs = input_errors(&format!("  - name: {bad}\n    type: string\n"));
            assert!(
                errs.iter().any(|e| matches!(
                    e,
                    FieldRuleError::Invalid {
                        field: "inputs.name",
                        ..
                    }
                )),
                "expected inputs.name error for {bad:?}, got {errs:?}"
            );
        }
        assert_eq!(
            input_errors("  - name: image_tag\n    type: string\n"),
            vec![]
        );
        let dupes = input_errors(
            "  - name: sha\n    type: string\n  - name: sha\n    type: string\n    required: true\n",
        );
        assert!(
            dupes.iter().any(|e| matches!(
                e,
                FieldRuleError::Invalid {
                    field: "inputs.name",
                    reason,
                    ..
                } if reason.contains("declared twice")
            )),
            "{dupes:?}"
        );
    }

    #[test]
    fn enum_requires_values_and_string_disallows_them() {
        let no_values = input_errors("  - name: service\n    type: enum\n");
        assert!(no_values.iter().any(|e| matches!(
            e,
            FieldRuleError::Required {
                field: "inputs.values",
                ..
            }
        )));
        let bad_value = input_errors("  - name: service\n    type: enum\n    values: ['a b']\n");
        assert!(bad_value.iter().any(|e| matches!(
            e,
            FieldRuleError::Invalid {
                field: "inputs.values",
                ..
            }
        )));
        let string_values = input_errors("  - name: sha\n    type: string\n    values: [a, b]\n");
        assert!(string_values.iter().any(|e| matches!(
            e,
            FieldRuleError::Disallowed {
                field: "inputs.values",
                ..
            }
        )));
        assert_eq!(
            input_errors("  - name: service\n    type: enum\n    values: [web, worker]\n"),
            vec![]
        );
    }

    #[test]
    fn pattern_is_string_only_and_must_be_a_usable_regex() {
        let on_enum = input_errors(
            "  - name: service\n    type: enum\n    values: [web]\n    pattern: '^web$'\n",
        );
        assert!(on_enum.iter().any(|e| matches!(
            e,
            FieldRuleError::Disallowed {
                field: "inputs.pattern",
                ..
            }
        )));
        let broken = input_errors("  - name: sha\n    type: string\n    pattern: '[unclosed'\n");
        assert!(
            broken.iter().any(|e| matches!(
                e,
                FieldRuleError::Invalid {
                    field: "inputs.pattern",
                    ..
                }
            )),
            "{broken:?}"
        );
        assert_eq!(
            input_errors("  - name: sha\n    type: string\n    pattern: '^[0-9a-f]{7,40}$'\n"),
            vec![]
        );
    }

    #[test]
    fn default_is_disallowed_when_required_and_must_satisfy_its_declaration() {
        let with_required = input_errors(
            "  - name: sha\n    type: string\n    required: true\n    default: 4f9c1ab\n",
        );
        assert!(with_required.iter().any(|e| matches!(
            e,
            FieldRuleError::Disallowed {
                field: "inputs.default",
                ..
            }
        )));
        for bad in [
            "  - name: sha\n    type: string\n    default: 'a;rm -rf'\n",
            "  - name: sha\n    type: string\n    pattern: '^[0-9a-f]{7,40}$'\n    default: nope\n",
            "  - name: service\n    type: enum\n    values: [web]\n    default: worker\n",
        ] {
            let errs = input_errors(bad);
            assert!(
                errs.iter().any(|e| matches!(
                    e,
                    FieldRuleError::Invalid {
                        field: "inputs.default",
                        ..
                    }
                )),
                "expected inputs.default error for {bad:?}, got {errs:?}"
            );
        }
        assert_eq!(
            input_errors(
                "  - name: sha\n    type: string\n    pattern: '^[0-9a-f]{7,40}$'\n    \
                 default: 4f9c1ab\n"
            ),
            vec![]
        );
    }

    #[test]
    fn input_bounds_are_hard_errors_at_the_boundary() {
        let block = |count: usize| {
            (0..count)
                .map(|i| format!("  - name: in_{i}\n    type: string\n"))
                .collect::<String>()
        };
        assert_eq!(
            input_errors(&block(crate::inputs::INPUTS_COUNT_MAX)),
            vec![]
        );
        let over = input_errors(&block(crate::inputs::INPUTS_COUNT_MAX + 1));
        assert!(
            over.iter().any(|e| matches!(
                e,
                FieldRuleError::Invalid {
                    field: "inputs",
                    ..
                }
            )),
            "{over:?}"
        );
        let with_default = |len: usize| {
            input_errors(&format!(
                "  - name: sha\n    type: string\n    default: {}\n",
                "a".repeat(len)
            ))
        };
        assert_eq!(with_default(crate::inputs::INPUT_VALUE_LEN_MAX), vec![]);
        let too_long = with_default(crate::inputs::INPUT_VALUE_LEN_MAX + 1);
        assert!(
            too_long.iter().any(|e| matches!(
                e,
                FieldRuleError::Invalid {
                    field: "inputs.default",
                    ..
                }
            )),
            "{too_long:?}"
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

    #[test]
    fn repo_doc_producing_job_types_parse_and_validate() {
        let defaults =
            ProjectDefaults::parse(include_str!("../../../.chug/jobs/_defaults.yaml")).unwrap();
        for (name, yaml) in [
            ("design", include_str!("../../../.chug/jobs/design.yaml")),
            ("docs", include_str!("../../../.chug/jobs/docs.yaml")),
        ] {
            let jt =
                JobType::parse(yaml).unwrap_or_else(|e| panic!("{name}.yaml parse error: {e}"));
            assert_eq!(jt.name, name);
            let merged = jt
                .with_defaults(&defaults)
                .unwrap_or_else(|e| panic!("{name}.yaml default merge error: {e}"));
            assert_eq!(merged.validate(), vec![], "{name}.yaml field rules");
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

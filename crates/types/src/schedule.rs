//! Scheduled job config — `.chug/schedules/{name}.yaml` (spec §1.1, design
//! #310 Decision 2).
//!
//! A schedule says "create a job of this type on this cron". It is
//! repo-versioned config like a job type, read from default-branch HEAD through
//! [`crate::config_paths`], so a schedule change ships in the same commit as
//! the job type it fires and passes the same gates.
//!
//! Schema tolerance follows spec §14 exactly, and for the same reason a job
//! type carries it: a file read live from HEAD can merge ahead of the binary
//! that parses it. Unknown *top-level* fields are therefore tolerated as
//! warnings, and `min_dispatcher` gates a file that genuinely needs a newer
//! dispatcher.
//!
//! - **Accepts:** the YAML text of one schedule file, plus the file's stem and
//!   the target [`JobType`] for the rules that need them.
//! - **Emits:** [`Schedule`], [`crate::job_type::FieldRuleError`] naming the
//!   rule a file broke, and [`crate::job_type::ConfigWarning`] for a tolerated
//!   unknown field.
//! - **Guarantees:** pure and total — no I/O, no async; every violation is
//!   reported, never the first only.
//! - **Spec:** §1.1 (field rules), design #310 (Decisions 2, 3 and 6).

use crate::cron::{CronExpr, CronParseError};
use crate::job_type::{ConfigWarning, FieldRuleError, Input, JobType, WorkType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The config-root-relative directory a project keeps its schedules in, so
/// `.chug/schedules/` and the pre-config-root `schedules/` both resolve through
/// [`crate::config_paths`].
pub const SCHEDULES_DIR: &str = "schedules";

/// Most schedule files one project loads (design #310 Decision 2). Files beyond
/// the cap are refused and reported, never silently truncated, which keeps the
/// per-tick work bounded by a constant per project.
pub const SCHEDULES_MAX: usize = 64;

/// One `.chug/schedules/{name}.yaml` file.
///
/// Unlike a job type this has no nested blocks, so `deny_unknown_fields` has
/// nowhere to apply; the first nested block to land keeps it, per spec §14.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Schedule {
    /// Unique within the repo, and equal to the file stem.
    pub name: String,
    /// The `.chug/jobs/{job_type}.yaml` this schedule creates a job of.
    pub job_type: String,
    /// A five-field UTC cron expression ([`crate::cron`]).
    pub cron: String,
    /// A disabled schedule is loaded and validated but never fires.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// The created job's title; defaults to [`Schedule::job_title`]'s fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The created job's ticket body — the §4.3 job brief the run receives, so
    /// it is required when the target job type declares `work.type: agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Minimum dispatcher schema epoch this file requires (spec §14), with the
    /// same meaning it carries on a job type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_dispatcher: Option<u32>,
    /// The values every occurrence supplies to the job it creates (design #311
    /// slice C), in the shape [`crate::Job::inputs`] uses. A non-empty map
    /// requires `min_dispatcher >= SCHEDULE_INPUTS_SCHEMA_EPOCH`, because a
    /// dispatcher that cannot see this field would fire the occurrence with the
    /// values dropped.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    /// Unknown top-level fields, captured rather than rejected; each surfaces
    /// as a [`ConfigWarning`].
    #[serde(flatten, default)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub unknown: BTreeMap<String, serde_yaml::Value>,
}

fn enabled_default() -> bool {
    true
}

impl Schedule {
    pub fn parse(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// The parsed [`CronExpr`] this schedule fires on.
    pub fn cron_expr(&self) -> Result<CronExpr, CronParseError> {
        CronExpr::parse(&self.cron)
    }

    /// The title of the job an occurrence creates: `title` when set, else the
    /// schedule's own name.
    #[must_use]
    pub fn job_title(&self) -> &str {
        match &self.title {
            Some(title) if !title.trim().is_empty() => title,
            _ => &self.name,
        }
    }

    /// The §1.1 field rules a schedule file clears on its own, given the
    /// `{name}` stem of the file it was read from. Returns every violation, not
    /// just the first.
    pub fn validate(&self, file_stem: &str) -> Vec<FieldRuleError> {
        assert!(!file_stem.is_empty(), "a schedule file has a stem");
        let context = format!("schedule '{file_stem}'");
        let mut errs = Vec::new();
        if self.name.trim().is_empty() {
            errs.push(FieldRuleError::Required {
                field: "name",
                context: context.clone(),
            });
        } else if self.name != file_stem {
            errs.push(FieldRuleError::Invalid {
                field: "name",
                context: context.clone(),
                reason: format!("must equal the file stem '{file_stem}'"),
            });
        }
        if self.job_type.trim().is_empty() {
            errs.push(FieldRuleError::Required {
                field: "job_type",
                context: context.clone(),
            });
        }
        if let Err(e) = self.cron_expr() {
            errs.push(FieldRuleError::Invalid {
                field: "cron",
                context: context.clone(),
                reason: e.to_string(),
            });
        }
        for (field, value) in [("title", &self.title), ("description", &self.description)] {
            if value.as_ref().is_some_and(|v| v.trim().is_empty()) {
                errs.push(FieldRuleError::Invalid {
                    field,
                    context: context.clone(),
                    reason: "must not be blank when set".to_string(),
                });
            }
        }
        errs.extend(self.validate_inputs(&context));
        debug_assert!(
            !errs.is_empty() || self.cron_expr().is_ok(),
            "a schedule with no violations has a parseable cron expression"
        );
        errs
    }

    /// The supplied `inputs:` map's own rules (design #311 slice C): the §14
    /// skew declaration, and the shape every input value clears whatever
    /// declares it — count, name form, charset, length — reused wholesale from
    /// [`crate::inputs`] rather than restated here.
    fn validate_inputs(&self, context: &str) -> Vec<FieldRuleError> {
        let mut errs = Vec::new();
        if self.inputs.is_empty() {
            return errs;
        }
        if self.min_dispatcher.unwrap_or(0) < crate::version::SCHEDULE_INPUTS_SCHEMA_EPOCH {
            errs.push(FieldRuleError::Required {
                field: "min_dispatcher",
                context: format!(
                    "a schedule supplying 'inputs:' (needs min_dispatcher >= {})",
                    crate::version::SCHEDULE_INPUTS_SCHEMA_EPOCH
                ),
            });
        }
        if let Err(e) = crate::inputs::check_supplied(&self.inputs) {
            errs.push(FieldRuleError::Invalid {
                field: "inputs",
                context: context.to_string(),
                reason: e.to_string(),
            });
        }
        errs
    }

    /// The rules that need the target job type (design #310 Decision 6, design
    /// #311 slice C): an agent run's job brief comes from the schedule, so
    /// `description` is required when the target declares `work.type: agent`;
    /// and the supplied `inputs:` must satisfy the target's declaration.
    pub fn validate_against_target(&self, target: &JobType) -> Vec<FieldRuleError> {
        let missing = self
            .description
            .as_ref()
            .is_none_or(|d| d.trim().is_empty());
        let mut errs = Vec::new();
        if target.work.r#type == WorkType::Agent && missing {
            errs.push(FieldRuleError::Required {
                field: "description",
                context: format!("schedule '{}' targeting work.type: agent", self.name),
            });
        }
        errs.extend(self.declared_input_errors(&target.inputs));
        errs
    }

    /// Every occurrence supplies the same map, so the values the target
    /// declares are decidable the moment both files are readable — which is why
    /// this runs at config load and at `chuggernaut validate`, not at 3am.
    fn declared_input_errors(&self, declared: &[Input]) -> Vec<FieldRuleError> {
        let context = format!(
            "schedule '{}' targeting job type '{}'",
            self.name, self.job_type
        );
        let mut errs = Vec::new();
        let invalid = |reason: String| FieldRuleError::Invalid {
            field: "inputs",
            context: context.clone(),
            reason,
        };
        for (name, value) in &self.inputs {
            match declared.iter().find(|d| &d.name == name) {
                None => errs.push(invalid(format!(
                    "input '{name}' is not declared by this job type"
                ))),
                Some(input) => {
                    if let Err(e) = crate::inputs::check_value(input, value) {
                        errs.push(invalid(format!("input '{name}': {e}")));
                    }
                }
            }
        }
        for input in declared {
            if input.required && !self.inputs.contains_key(&input.name) {
                errs.push(invalid(format!(
                    "input '{}' is required but the schedule supplies no value",
                    input.name
                )));
            }
        }
        errs
    }

    /// Non-fatal config warnings (spec §14): unknown top-level fields that were
    /// tolerated rather than rejected. Deterministically ordered.
    pub fn config_warnings(&self) -> Vec<ConfigWarning> {
        self.unknown
            .keys()
            .map(|field| ConfigWarning {
                field: field.clone(),
            })
            .collect()
    }

    /// If this file requires a newer dispatcher than `dispatcher_epoch`, the
    /// epoch it needs (spec §14).
    pub fn requires_dispatcher(&self, dispatcher_epoch: u32) -> Option<u32> {
        self.min_dispatcher.filter(|&need| need > dispatcher_epoch)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::config_paths::{config_entry_name, config_path};

    const NIGHTLY: &str = r#"
name: nightly-integration
job_type: code
cron: "0 2 * * *"
description: Run the nightly integration suite.
"#;

    /// A schedule supplying the `rollback` shape: one required `sha` matching a
    /// declared pattern, and one optional `service` left to its default.
    const WITH_INPUTS: &str = r#"
name: n
job_type: rollback
cron: "0 2 * * *"
description: Roll it back.
min_dispatcher: 3
inputs:
  sha: 4f9c1ab
"#;

    fn parse(yaml: &str) -> Schedule {
        Schedule::parse(yaml).unwrap()
    }

    /// The job type a schedule fires, built from the same YAML an author would
    /// write, so the declaration these rules read is the parsed one.
    fn target(work_type: &str, inputs_block: &str) -> JobType {
        JobType::parse(&format!(
            "name: rollback\nimage: img:latest\nmin_dispatcher: 2\n\
             work:\n  type: {work_type}\n  prompt: p.md\n{inputs_block}"
        ))
        .unwrap()
    }

    /// The `rollback` declaration #311 motivates the feature with.
    fn rollback_inputs() -> String {
        "inputs:\n  - name: sha\n    type: string\n    required: true\n    \
         pattern: '^[0-9a-f]{7,40}$'\n  - name: service\n    type: enum\n    \
         values: [web, worker]\n    default: web\n"
            .to_string()
    }

    #[test]
    fn parses_the_design_example_with_its_defaults() {
        let schedule = parse(NIGHTLY);
        assert_eq!(schedule.name, "nightly-integration");
        assert_eq!(schedule.job_type, "code");
        assert_eq!(schedule.cron, "0 2 * * *");
        assert!(schedule.enabled, "enabled defaults to true");
        assert_eq!(schedule.title, None);
        assert_eq!(schedule.job_title(), "nightly-integration");
        assert_eq!(schedule.min_dispatcher, None);
        assert!(schedule.validate("nightly-integration").is_empty());
        assert!(schedule.cron_expr().is_ok());
    }

    #[test]
    fn optional_fields_override_their_defaults() {
        let schedule = parse(
            "name: n\njob_type: code\ncron: '0 2 * * *'\nenabled: false\ntitle: Nightly suite\n",
        );
        assert!(!schedule.enabled);
        assert_eq!(schedule.job_title(), "Nightly suite");
        assert!(schedule.validate("n").is_empty());
    }

    #[test]
    fn a_schedule_lives_in_the_config_directory_under_either_layout() {
        assert_eq!(
            config_path(&format!("{SCHEDULES_DIR}/nightly.yaml")),
            ".chug/schedules/nightly.yaml"
        );
        assert_eq!(
            config_entry_name(".chug/schedules/nightly.yaml", SCHEDULES_DIR),
            Some("nightly.yaml")
        );
        assert_eq!(
            config_entry_name("schedules/nightly.yaml", SCHEDULES_DIR),
            Some("nightly.yaml")
        );
    }

    #[test]
    fn required_fields_are_required_at_parse() {
        for bad in [
            "job_type: code\ncron: '0 2 * * *'\n",
            "name: n\ncron: '0 2 * * *'\n",
            "name: n\njob_type: code\n",
        ] {
            assert!(Schedule::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn the_name_must_equal_the_file_stem() {
        let schedule = parse(NIGHTLY);
        let errs = schedule.validate("nightly");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].to_string().contains("must equal the file stem"),
            "{errs:?}"
        );

        let blank = parse("name: ' '\njob_type: code\ncron: '0 2 * * *'\n");
        assert!(
            blank
                .validate("n")
                .iter()
                .any(|e| e.to_string().contains("'name' is required")),
            "a blank name is missing, not mismatched"
        );
    }

    #[test]
    fn a_malformed_cron_is_a_field_rule_error_naming_the_field() {
        let schedule = parse("name: n\njob_type: code\ncron: '0 2 * *'\n");
        let errs = schedule.validate("n");
        assert_eq!(errs.len(), 1, "{errs:?}");
        let message = errs[0].to_string();
        assert!(message.contains("'cron' is invalid"), "{message}");
        assert!(message.contains("found 4"), "{message}");
    }

    #[test]
    fn every_violation_is_reported_not_just_the_first() {
        let schedule = parse("name: other\njob_type: ' '\ncron: nope\ntitle: '  '\n");
        let errs = schedule.validate("n");
        let fields: Vec<String> = errs.iter().map(std::string::ToString::to_string).collect();
        assert_eq!(fields.len(), 4, "{fields:?}");
        for expected in ["'name'", "'job_type'", "'cron'", "'title'"] {
            assert!(
                fields.iter().any(|e| e.contains(expected)),
                "{expected} missing from {fields:?}"
            );
        }
    }

    #[test]
    fn description_is_required_only_for_an_agent_target() {
        let agent = target("agent", "");
        let with_description = parse(NIGHTLY);
        assert!(with_description.validate_against_target(&agent).is_empty());

        let without = parse("name: n\njob_type: deploy\ncron: '0 2 * * *'\n");
        assert!(
            without
                .validate_against_target(&target("command", ""))
                .is_empty()
        );
        assert!(
            without
                .validate_against_target(&target("human", ""))
                .is_empty()
        );
        let errs = without.validate_against_target(&agent);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].to_string().contains("'description' is required"),
            "{errs:?}"
        );

        let blank = parse("name: n\njob_type: code\ncron: '0 2 * * *'\ndescription: '  '\n");
        assert_eq!(blank.validate_against_target(&agent).len(), 1);
    }

    /// The slice-C shape: a supplied map parses into [`Schedule::inputs`],
    /// clears the §1.1 rules, and survives a YAML round trip.
    #[test]
    fn a_schedule_parses_and_validates_its_supplied_inputs() {
        let schedule = parse(WITH_INPUTS);
        assert_eq!(
            schedule.inputs,
            BTreeMap::from([("sha".to_string(), "4f9c1ab".to_string())])
        );
        assert!(schedule.validate("n").is_empty(), "{schedule:?}");
        assert!(
            schedule.config_warnings().is_empty(),
            "inputs is not unknown"
        );
        let yaml = serde_yaml::to_string(&schedule).unwrap();
        assert_eq!(Schedule::parse(&yaml).unwrap(), schedule);
    }

    /// The value rules are [`crate::inputs`]'s, not a second copy: over-count,
    /// the charset, the length bound and the name form all decide here exactly
    /// as they do for an API-supplied map.
    #[test]
    fn supplied_input_values_clear_the_shared_shape_rules() {
        let with = |block: &str| {
            parse(&format!(
                "name: n\njob_type: rollback\ncron: '0 2 * * *'\nmin_dispatcher: 3\ninputs:\n{block}"
            ))
        };
        let over_count = (0..=crate::inputs::INPUTS_COUNT_MAX)
            .map(|i| format!("  input_{i}: v\n"))
            .collect::<String>();
        for (block, expected) in [
            (over_count.as_str(), "exceeds the limit"),
            ("  sha: 'a;rm -rf'\n", "outside the allowed charset"),
            ("  CHUG_SHA: 4f9c1ab\n", "is malformed"),
            ("  chug_input_sha: 4f9c1ab\n", "is not declared"),
        ] {
            let schedule = with(block);
            let errs: Vec<String> = schedule
                .validate("n")
                .iter()
                .chain(
                    schedule
                        .validate_against_target(&target("command", &rollback_inputs()))
                        .iter(),
                )
                .map(std::string::ToString::to_string)
                .collect();
            assert!(
                errs.iter().any(|e| e.contains(expected)),
                "{expected:?} missing from {errs:?}"
            );
        }
        let over_length = format!("  sha: {}\n", "a".repeat(crate::INPUT_VALUE_LEN_MAX + 1));
        assert!(
            with(&over_length)
                .validate("n")
                .iter()
                .any(|e| e.to_string().contains("over the 256-character limit"))
        );
    }

    /// The §14 gate that keeps the N-1 drop from being silent: a dispatcher
    /// that cannot see `inputs:` refuses the file on `min_dispatcher` instead
    /// of firing the occurrence without the values.
    #[test]
    fn supplying_inputs_requires_the_schedule_inputs_epoch() {
        let ungated = parse(&WITH_INPUTS.replace("min_dispatcher: 3\n", ""));
        let errs = ungated.validate("n");
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(
            errs[0].to_string().contains("'min_dispatcher' is required"),
            "{errs:?}"
        );
        assert!(errs[0].to_string().contains("inputs"), "{errs:?}");

        let stale = parse(&WITH_INPUTS.replace("min_dispatcher: 3", "min_dispatcher: 2"));
        assert_eq!(
            stale.validate("n").len(),
            1,
            "the job-type epoch is not enough"
        );
        assert_eq!(
            parse(WITH_INPUTS).requires_dispatcher(crate::SCHEDULE_INPUTS_SCHEMA_EPOCH - 1),
            Some(crate::SCHEDULE_INPUTS_SCHEMA_EPOCH),
            "an N-1 dispatcher refuses the file rather than dropping its inputs"
        );
    }

    /// The rules that need the target's declaration: an undeclared name, a
    /// value failing the declared `pattern`/`values`, and a required input the
    /// schedule never supplies — each decidable at config load.
    #[test]
    fn supplied_inputs_are_judged_against_the_target_declaration() {
        let declared = target("command", &rollback_inputs());
        assert!(
            parse(WITH_INPUTS)
                .validate_against_target(&declared)
                .is_empty()
        );

        let with = |block: &str| {
            parse(&format!(
                "name: n\njob_type: rollback\ncron: '0 2 * * *'\nmin_dispatcher: 3\ninputs:\n{block}"
            ))
        };
        for (block, expected) in [
            ("  region: eu\n", "is not declared by this job type"),
            ("  sha: not_hex\n", "does not match the declared pattern"),
            (
                "  sha: 4f9c1ab\n  service: database\n",
                "not one of the declared values",
            ),
            (
                "  service: web\n",
                "is required but the schedule supplies no value",
            ),
        ] {
            let errs: Vec<String> = with(block)
                .validate_against_target(&declared)
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            assert!(
                errs.iter().any(|e| e.contains(expected)),
                "{expected:?} missing from {errs:?}"
            );
        }
    }

    /// The feature is off, not merely unused: a schedule with no `inputs:`
    /// parses, validates and serializes byte-identically to one written before
    /// the field existed.
    #[test]
    fn a_schedule_without_inputs_is_unchanged_by_the_field() {
        let schedule = parse(NIGHTLY);
        assert!(schedule.inputs.is_empty());
        assert!(schedule.validate("nightly-integration").is_empty());
        assert!(
            schedule
                .validate_against_target(&target("agent", ""))
                .is_empty()
        );
        let yaml = serde_yaml::to_string(&schedule).unwrap();
        assert!(!yaml.contains("inputs"), "{yaml}");
        assert!(parse(NIGHTLY).requires_dispatcher(0).is_none());
    }

    #[test]
    fn unknown_top_level_fields_are_warnings_not_parse_errors() {
        let schedule = parse("name: n\njob_type: code\ncron: '0 2 * * *'\ntimezone: UTC\n");
        assert!(schedule.validate("n").is_empty());
        let warnings = schedule.config_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].field, "timezone");
        assert!(warnings[0].to_string().contains("timezone"));
    }

    #[test]
    fn min_dispatcher_gates_a_file_ahead_of_the_binary() {
        let schedule = parse("name: n\njob_type: code\ncron: '0 2 * * *'\nmin_dispatcher: 7\n");
        assert_eq!(schedule.requires_dispatcher(6), Some(7));
        assert_eq!(schedule.requires_dispatcher(7), None);
        assert_eq!(schedule.requires_dispatcher(8), None);
        assert_eq!(parse(NIGHTLY).requires_dispatcher(0), None);
    }

    #[test]
    fn round_trips_through_yaml() {
        let schedule = parse(NIGHTLY);
        let yaml = serde_yaml::to_string(&schedule).unwrap();
        assert_eq!(Schedule::parse(&yaml).unwrap(), schedule);
    }
}

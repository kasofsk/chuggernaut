//! Job inputs judged against their declaration (spec §1.1 `inputs:`, §2.2;
//! design #311 Decisions 3 and 6).
//!
//! Two pure rules live here, and they are two because they answer at two
//! different moments:
//!
//! 1. [`input_errors`] — the **semantic** verdict on a supplied map. Every name
//!    is declared, every `required` input has a value, every `enum` value is in
//!    its list, every `string` matches its `pattern`. It needs the job type, so
//!    it runs where the type is loaded at a ref: release-time validation and the
//!    Ready-transition re-check (which is why the type may have changed under a
//!    job between the two — the same function decides both).
//! 2. [`fill_input_defaults`] — the **default materialization**, add-only, run
//!    exactly once by the write that first records `base_ref`. A declared
//!    `default` is a value the platform writes onto the record, not a create-form
//!    pre-fill: otherwise the value that actually ran appears on no audit surface,
//!    which is the whole point of `Job.inputs` being the effective set.
//!
//! The *shape* rules a value clears whatever its declaration — charset, length,
//! name form — belong to [`types::inputs`], which the creation pass (422) also
//! uses; nothing here re-states them.
//!
//! - **Accepts:** a job type's declared [`Input`]s and a supplied
//!   `BTreeMap<String, String>` (plus, for the fill, `&mut` that map).
//! - **Emits:** [`ValidationError`]s under `field: "inputs.{name}"`, and the
//!   add-only fill's mutation.
//! - **Guarantees:** pure and synchronous, no I/O, no clock; the fill only ever
//!   *inserts* keys absent from the map, asserted at the write site, so a
//!   supplied value can never be overwritten by a default; the declaration is
//!   read, never written — a resolved input can no more reach the job type than
//!   the job type can reach the input map.
//! - **Spec:** §1.1 (the `inputs:` field rules and the `Job` record), §2.2 (the
//!   release-time and Ready-transition passes), §10.3 (the audit trail);
//!   design #311 Decisions 3, 5, 6.

use crate::release::ValidationError;
use std::collections::BTreeMap;
use types::Input;

/// The `field` prefix every input error reports under (§6.5 dot notation):
/// `inputs.{name}`, so the operator's form can highlight the offending field
/// rather than the block.
const INPUT_FIELD_PREFIX: &str = "inputs";

/// The semantic verdict on a job's supplied inputs against the type that
/// declares them (spec §2.2 pass 1 and pass 2). Reports every violation, not the
/// first: a release rejection should name each field the operator must fix.
///
/// A missing **optional** input is never an error — with a declared `default` it
/// resolves at the Ready transition ([`fill_input_defaults`]), and without one it
/// is simply absent (`absent means absent`, #311 Decision 4).
pub fn input_errors(
    job_seq: Option<u64>,
    declared: &[Input],
    supplied: &BTreeMap<String, String>,
) -> Vec<ValidationError> {
    let mut errs = Vec::new();
    for (name, value) in supplied {
        let Some(input) = declared.iter().find(|d| &d.name == name) else {
            errs.push(input_error(
                job_seq,
                name,
                "is not declared by this job type",
            ));
            continue;
        };
        if let Err(e) = types::inputs::check_value(input, value) {
            errs.push(input_error(job_seq, name, &e.to_string()));
        }
    }
    for input in declared {
        if input.required && !supplied.contains_key(&input.name) {
            errs.push(input_error(
                job_seq,
                &input.name,
                "is required but no value was supplied",
            ));
        }
    }
    // Negative space (STYLE.md Tier 2 #2): every error this function produces is
    // addressed to one named input, so a caller can route it to a form field.
    debug_assert!(
        errs.iter().all(|e| e.field.starts_with(INPUT_FIELD_PREFIX)),
        "an input error must name its input"
    );
    errs
}

/// One `inputs.{name}` violation in the §6.5 error vocabulary.
fn input_error(job_seq: Option<u64>, name: &str, message: &str) -> ValidationError {
    ValidationError::new(
        job_seq,
        format!("{INPUT_FIELD_PREFIX}.{name}"),
        format!("input '{name}' {message}"),
    )
}

/// Materialize declared defaults onto a job's input map — **add-only** (design
/// #311 Decision 3, "when a default becomes a value").
///
/// Called by exactly one decision: the Ready transition that *first* records
/// `base_ref`, against the job type loaded at that same ref. Every declared input
/// the creator did not supply and that declares a `default` gains it; a supplied
/// value is never touched, which is an assert at this write site rather than a
/// merge policy — a collision would mean the immutability invariant broke
/// upstream, not that a default should quietly win.
///
/// Returns the names filled, in declaration order, for the caller's audit event.
pub fn fill_input_defaults(
    declared: &[Input],
    inputs: &mut BTreeMap<String, String>,
) -> Vec<String> {
    debug_assert!(
        declared.len() <= types::INPUTS_COUNT_MAX,
        "{} declared inputs exceeds the {} bound — the type should not have validated",
        declared.len(),
        types::INPUTS_COUNT_MAX,
    );
    let supplied_count = inputs.len();
    let mut filled = Vec::new();
    for input in declared {
        let Some(default) = &input.default else {
            continue;
        };
        // A supplied value wins by being left alone; only an absent key is
        // written. The two lines are deliberately in this order so the assert
        // below is about the write that just happened.
        if inputs.contains_key(&input.name) {
            continue;
        }
        let previous = inputs.insert(input.name.clone(), default.clone());
        debug_assert!(
            previous.is_none(),
            "the default fill overwrote a supplied value for input '{}'",
            input.name,
        );
        filled.push(input.name.clone());
    }
    // Postcondition: add-only. The map grew by exactly the keys reported filled,
    // and nothing else moved.
    debug_assert_eq!(
        inputs.len(),
        supplied_count + filled.len(),
        "the default fill is add-only",
    );
    filled
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Tier-1 coverage (`testing.md`): the validation verdict and the fill rule
    //! are pure functions of a declaration and a map, so nothing here needs NATS,
    //! Docker or a ref. The create → release → Ready sequence they compose into
    //! is pinned tier-2 (`crates/dispatcher/tests/inputs.rs`).
    use super::*;
    use types::InputKind;

    fn declared() -> Vec<Input> {
        vec![
            Input {
                name: "sha".into(),
                r#type: InputKind::String,
                required: true,
                default: None,
                values: vec![],
                pattern: Some("^[0-9a-f]{7,40}$".into()),
                description: None,
            },
            Input {
                name: "service".into(),
                r#type: InputKind::Enum,
                required: false,
                default: Some("web".into()),
                values: vec!["web".into(), "worker".into(), "bot".into()],
                pattern: None,
                description: None,
            },
        ]
    }

    fn supplied(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn fields(errs: &[ValidationError]) -> Vec<String> {
        errs.iter().map(|e| e.field.clone()).collect()
    }

    /// A type declaring nothing and a job supplying nothing is the whole tree
    /// today: no errors, no fill, byte-identical behavior.
    #[test]
    fn a_job_with_no_inputs_is_untouched() {
        let mut inputs = BTreeMap::new();
        assert_eq!(input_errors(Some(1), &[], &inputs), vec![]);
        assert_eq!(fill_input_defaults(&[], &mut inputs), Vec::<String>::new());
        assert!(inputs.is_empty());
        // A type that declares inputs but requires none also leaves an empty map
        // empty — an optional input with no default is *absent*, not blank.
        let optional = vec![Input {
            name: "note".into(),
            r#type: InputKind::String,
            required: false,
            default: None,
            values: vec![],
            pattern: None,
            description: None,
        }];
        assert_eq!(input_errors(Some(1), &optional, &inputs), vec![]);
        assert!(fill_input_defaults(&optional, &mut inputs).is_empty());
        assert!(inputs.is_empty());
    }

    /// The release-time verdict, one error per named input.
    #[test]
    fn missing_required_input_is_reported_under_its_own_field() {
        let errs = input_errors(Some(7), &declared(), &supplied(&[("service", "web")]));
        assert_eq!(fields(&errs), vec!["inputs.sha"]);
        assert_eq!(errs[0].job_seq, Some(7));
        assert!(errs[0].message.contains("required"), "{errs:?}");
    }

    #[test]
    fn undeclared_enum_and_pattern_violations_each_name_their_input() {
        let errs = input_errors(
            Some(7),
            &declared(),
            &supplied(&[
                ("sha", "not-hex"),
                ("service", "database"),
                ("region", "eu"),
            ]),
        );
        assert_eq!(
            fields(&errs),
            vec!["inputs.region", "inputs.service", "inputs.sha"],
            "{errs:?}"
        );
        let rendered = format!("{errs:?}");
        assert!(
            rendered.contains("is not declared by this job type"),
            "{rendered}"
        );
        assert!(
            rendered.contains("not one of the declared values"),
            "{rendered}"
        );
        assert!(
            rendered.contains("does not match the declared pattern"),
            "{rendered}"
        );
    }

    /// The charset floor is part of the effective check at release too, not only
    /// at creation — a record written before the rule existed still fails here.
    #[test]
    fn a_value_outside_the_charset_fails_the_semantic_pass_as_well() {
        let errs = input_errors(Some(7), &declared(), &supplied(&[("sha", "a;rm -rf")]));
        assert_eq!(fields(&errs), vec!["inputs.sha"]);
        assert!(errs[0].message.contains("charset"), "{errs:?}");
    }

    /// A declared `default` satisfies the presence check, so a missing optional
    /// input is never a release error (#311 Decision 3).
    #[test]
    fn a_supplied_required_input_with_defaults_pending_validates_clean() {
        assert_eq!(
            input_errors(Some(7), &declared(), &supplied(&[("sha", "4f9c1ab")])),
            vec![]
        );
    }

    /// The invariant: the fill only inserts keys ABSENT from the map.
    #[test]
    fn the_default_fill_never_overwrites_a_supplied_value() {
        let mut inputs = supplied(&[("sha", "4f9c1ab"), ("service", "worker")]);
        let filled = fill_input_defaults(&declared(), &mut inputs);
        assert!(filled.is_empty(), "nothing was absent");
        assert_eq!(
            inputs,
            supplied(&[("sha", "4f9c1ab"), ("service", "worker")]),
            "a supplied value survives the fill untouched"
        );
    }

    /// …and it does fill the absent one, leaving the supplied one alone.
    #[test]
    fn the_default_fill_adds_only_the_absent_declared_defaults() {
        let mut inputs = supplied(&[("sha", "4f9c1ab")]);
        let filled = fill_input_defaults(&declared(), &mut inputs);
        assert_eq!(filled, vec!["service"]);
        assert_eq!(inputs, supplied(&[("sha", "4f9c1ab"), ("service", "web")]));
        // Idempotent: run again (a second Ready transition would be a bug, but
        // the rule is add-only, so it cannot corrupt what it already wrote).
        assert!(fill_input_defaults(&declared(), &mut inputs).is_empty());
        assert_eq!(inputs, supplied(&[("sha", "4f9c1ab"), ("service", "web")]));
    }

    /// An input with no declared `default` is never invented: absent stays
    /// absent, so `set -u` catches it and `${X:-fallback}` means what it says.
    #[test]
    fn an_input_without_a_default_is_never_materialized() {
        let mut inputs = BTreeMap::new();
        let filled = fill_input_defaults(&declared(), &mut inputs);
        assert_eq!(filled, vec!["service"]);
        assert_eq!(inputs, supplied(&[("service", "web")]));
        assert!(!inputs.contains_key("sha"), "no empty string for 'sha'");
    }
}

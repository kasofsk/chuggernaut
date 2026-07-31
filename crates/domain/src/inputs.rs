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
//! Two more pure rules cover what happens to a *resolved* map — the delivery
//! half of the feature, which is composition rather than judgement:
//!
//! 3. [`inject_input_env`] — one `CHUG_INPUT_{NAME}` key per resolved value,
//!    written last into a container env and under the collision assert (§4.1,
//!    design #311 Decision 4).
//! 4. [`stamp_event_inputs`] — the §10.3 audit fragment: `job-created` carries
//!    what the originator asked for, the Ready-transition event what actually
//!    ran.
//!
//! The *shape* rules a value clears whatever its declaration — charset, length,
//! name form — belong to [`types::inputs`], which the creation pass (422) also
//! uses; nothing here re-states them.
//!
//! - **Accepts:** a job type's declared [`Input`]s and a supplied
//!   `BTreeMap<String, String>` (plus, for the fill, `&mut` that map); a
//!   container env under assembly; an event payload under assembly.
//! - **Emits:** [`ValidationError`]s under `field: "inputs.{name}"`, the
//!   add-only fill's mutation, the injected env keys, and the event fragment.
//! - **Guarantees:** pure and synchronous, no I/O, no clock; the fill only ever
//!   *inserts* keys absent from the map, asserted at the write site, so a
//!   supplied value can never be overwritten by a default; the declaration is
//!   read, never written — a resolved input can no more reach the job type than
//!   the job type can reach the input map; a job with no inputs is touched by
//!   nothing here, so its env and its events stay byte-identical to a tree
//!   without the feature.
//! - **Spec:** §1.1 (the `inputs:` field rules and the `Job` record), §2.2 (the
//!   release-time, Ready-transition and launch-time passes), §4.1 (container
//!   env), §10.3 (the audit trail); design #311 Decisions 3, 4, 5, 6.

use crate::release::ValidationError;
use std::collections::{BTreeMap, HashMap};
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
    debug_assert_eq!(
        inputs.len(),
        supplied_count + filled.len(),
        "the default fill is add-only",
    );
    filled
}

/// Deliver a job's resolved inputs into a container env (spec §4.1, design #311
/// Decision 4) — one `CHUG_INPUT_{NAME_UPPERCASE}` key per entry, and nothing
/// else.
///
/// **Called last**, after the platform variables, the §6.3 origin stamps, the
/// declared vars and the decrypted secrets, and asserting at this insertion site
/// that the namespace was empty when it arrived: `CHUG_INPUT_*` belongs to
/// inputs alone, and §5.3's reserved prefix (extended to vars) is what makes
/// that hold in production rather than by luck. A collision would mean an
/// invariant broke upstream, not that a value should quietly win.
///
/// **Absent means absent.** The map holds exactly the inputs with a *resolved*
/// value ([`fill_input_defaults`]), so a declared optional input with neither a
/// supplied value nor a `default` gets **no key at all** — never an empty
/// string. That is what lets a `set -eu` script fail loudly on `$CHUG_INPUT_SHA`
/// instead of acting on a blank argument, and what makes
/// `${CHUG_INPUT_X:-fallback}` mean what its author expects.
///
/// Returns the names it **refused** — a value outside the default charset, or a
/// malformed name. Neither can reach here for a job that cleared §2.2's
/// launch-time pass (`work::EntryFailure::BadInput` parks such a job instead of
/// launching it), so the list is empty in production; it exists because a
/// record written before the rule existed must lose the key rather than hand a
/// script a value three passes rejected — the same defense in depth the reserved
/// secret/var names get at injection. The caller logs what it names.
pub fn inject_input_env(
    env: &mut HashMap<String, String>,
    inputs: &BTreeMap<String, String>,
) -> Vec<String> {
    debug_assert!(
        !env.keys().any(|k| k.starts_with(types::INPUT_ENV_PREFIX)),
        "a non-input source inserted a {} key: {:?}",
        types::INPUT_ENV_PREFIX,
        env.keys()
            .filter(|k| k.starts_with(types::INPUT_ENV_PREFIX))
            .collect::<Vec<_>>(),
    );
    let before = env.len();
    let mut refused = Vec::new();
    for (name, value) in inputs {
        if !types::inputs::name_is_well_formed(name)
            || types::inputs::check_value_charset(value).is_err()
        {
            refused.push(name.clone());
            continue;
        }
        let previous = env.insert(types::input_env_key(name), value.clone());
        debug_assert!(
            previous.is_none(),
            "input '{name}' collided on {}",
            types::input_env_key(name),
        );
    }
    debug_assert_eq!(
        env.len(),
        before + inputs.len() - refused.len(),
        "input injection is one key per resolved value",
    );
    refused
}

/// Stamp a job's input map onto an event payload (spec §6.3, §10.3; design #311
/// Decision 6): `job-created` carries the **supplied** set, the Ready-transition
/// event that pins `base_ref` the **effective** one, and the difference between
/// the two is exactly the materialized defaults — which is the whole reason to
/// materialize them.
///
/// A job with no inputs stamps **nothing**, so every event of every job that
/// exists today is byte-identical to what it was: the event-side twin of
/// [`inject_input_env`]'s absent-means-absent.
pub fn stamp_event_inputs(extra: &mut serde_json::Value, inputs: &BTreeMap<String, String>) {
    if inputs.is_empty() {
        return;
    }
    let Some(object) = extra.as_object_mut() else {
        debug_assert!(false, "an event payload is a JSON object, got {extra}");
        return;
    };
    let previous = object.insert("inputs".to_string(), serde_json::json!(inputs));
    debug_assert!(
        previous.is_none(),
        "the event payload already carried an 'inputs' field",
    );
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

    /// One key per resolved value, under the reserved namespace, added to
    /// whatever the earlier sources composed — and nothing else touched.
    #[test]
    fn injection_adds_one_reserved_key_per_resolved_input() {
        let mut env = HashMap::from([
            ("JOB_ID".to_string(), "7".to_string()),
            ("DEPLOY_KEY".to_string(), "secret".to_string()),
        ]);
        let refused = inject_input_env(
            &mut env,
            &supplied(&[("sha", "4f9c1ab"), ("image_tag", "ghcr.io/org/img:sha")]),
        );
        assert!(refused.is_empty());
        assert_eq!(
            env,
            HashMap::from([
                ("JOB_ID".to_string(), "7".to_string()),
                ("DEPLOY_KEY".to_string(), "secret".to_string()),
                ("CHUG_INPUT_SHA".to_string(), "4f9c1ab".to_string()),
                (
                    "CHUG_INPUT_IMAGE_TAG".to_string(),
                    "ghcr.io/org/img:sha".to_string()
                ),
            ]),
        );
    }

    /// The delivery half of **absent means absent**: an empty map — every job
    /// that exists today — leaves the env byte-identical, so the feature is off
    /// rather than merely unused.
    #[test]
    fn a_job_with_no_inputs_leaves_the_env_byte_identical() {
        let before = HashMap::from([
            ("JOB_ID".to_string(), "7".to_string()),
            ("CHUG_PHASE".to_string(), "Work".to_string()),
        ]);
        let mut env = before.clone();
        assert!(inject_input_env(&mut env, &BTreeMap::new()).is_empty());
        assert_eq!(env, before);
        let mut inputs = BTreeMap::new();
        fill_input_defaults(
            &[Input {
                name: "note".into(),
                r#type: InputKind::String,
                required: false,
                default: None,
                values: vec![],
                pattern: None,
                description: None,
            }],
            &mut inputs,
        );
        inject_input_env(&mut env, &inputs);
        assert_eq!(env, before, "no CHUG_INPUT_NOTE, not even an empty one");
    }

    /// Defense in depth at the insertion site: a value that no longer clears the
    /// charset is **refused**, not injected. Such a job parks at §2.2's
    /// launch-time pass, so this is only reachable for a record written before
    /// the rule existed — and losing the key is what makes the script fail loudly
    /// rather than act on a rejected value.
    #[test]
    fn a_value_outside_the_charset_is_refused_rather_than_injected() {
        let mut env = HashMap::new();
        let refused = inject_input_env(
            &mut env,
            &supplied(&[("sha", "a;rm -rf"), ("service", "web")]),
        );
        assert_eq!(refused, vec!["sha"]);
        assert_eq!(
            env,
            HashMap::from([("CHUG_INPUT_SERVICE".to_string(), "web".to_string())]),
            "the good value still lands; the refused one leaves no key at all"
        );
    }

    /// The §10.3 audit fragment: present when the job carries inputs, absent —
    /// not empty — when it does not.
    #[test]
    fn the_event_stamp_is_omitted_entirely_for_an_input_free_job() {
        let mut extra = serde_json::json!({ "state": "Ready" });
        stamp_event_inputs(&mut extra, &BTreeMap::new());
        assert_eq!(extra, serde_json::json!({ "state": "Ready" }));

        stamp_event_inputs(&mut extra, &supplied(&[("sha", "4f9c1ab")]));
        assert_eq!(
            extra,
            serde_json::json!({ "state": "Ready", "inputs": { "sha": "4f9c1ab" } }),
        );
    }
}

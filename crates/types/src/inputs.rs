//! Job-input **value** rules (spec §1.1 `inputs:`, design #311 Decision 5).
//!
//! The declaration lives in [`crate::job_type::Input`]; what a *value* may be
//! lives here, because three separate passes need the same answer — release
//! validation, the Blocked→Ready re-check, and the launch-time re-check
//! immediately before injection (spec §2.2). One implementation, so they cannot
//! drift into three subtly different charsets.
//!
//! An input value is an **identifier, not prose**: the default charset excludes
//! whitespace, quotes, backticks and every shell metacharacter, because a value
//! can reach a `run:` script that itself crosses further shells. A declared
//! `pattern` may only *narrow* that floor — the effective check is
//! `charset AND pattern`, and that is a rule, not a convention.
//!
//! - **Accepts:** a declared [`Input`] and a candidate value, or a whole
//!   supplied map at creation.
//! - **Emits:** [`InputValueError`] / [`SuppliedInputError`], naming the rule
//!   the value broke.
//! - **Guarantees:** pure and total — no I/O, no async, no interior state, no
//!   panics; the charset is checked first and unconditionally, so no
//!   declaration can widen it.
//! - **Spec:** §1.1 (field rules), §2.2 (the three validation passes), §5.3.

use crate::job_type::{Input, InputKind};
use std::collections::BTreeMap;
use thiserror::Error;

/// The default charset, as a regex over the whole value. Kept beside the
/// checker so the generated JSON Schema (`chuggernaut schema job-type`)
/// documents exactly what [`check_value_charset`] accepts. Alphanumerics plus
/// seven punctuation characters, which covers the real shapes
/// (`ghcr.io/org/img:sha`, `img@sha256:…`, `4f9c1ab`, `feature/x`) and nothing
/// else.
pub const INPUT_VALUE_PATTERN: &str = r"^[A-Za-z0-9._:/@+-]{1,256}$";

/// The shape of a declared input *name*, as a regex. Lowercase-only so
/// `name.to_uppercase()` is injective over a type's declared names — without
/// that rule `image_tag` and `IMAGE_TAG` would both map to the one
/// `CHUG_INPUT_IMAGE_TAG` env key.
pub const INPUT_NAME_PATTERN: &str = r"^[a-z][a-z0-9_]*$";

/// The one env namespace inputs are delivered under (spec §4.1, design #311
/// Decision 4). It sits *inside* the `CHUG_` prefix §5.3 reserves for both
/// secrets and vars, which is what makes the namespace collision-proof by
/// construction rather than by precedence rule: no project-declared name can
/// land here at all.
pub const INPUT_ENV_PREFIX: &str = "CHUG_INPUT_";

/// The env key one input is delivered as: `sha` → `CHUG_INPUT_SHA`.
///
/// Injective over well-formed names ([`INPUT_NAME_PATTERN`]) — uppercasing a
/// lowercase-only alphabet cannot collapse two names — which is the whole
/// reason the name rule excludes uppercase. The precondition is asserted rather
/// than assumed: a malformed name reaching here would mean the creation pass
/// (§2.2) was bypassed, and two inputs could then claim one key.
#[must_use]
pub fn input_env_key(name: &str) -> String {
    debug_assert!(
        name_is_well_formed(name),
        "input name {name:?} is malformed; its env key would not be injective",
    );
    format!("{INPUT_ENV_PREFIX}{}", name.to_uppercase())
}

/// Longest accepted input value (design #311 Decision 2). Characters, which is
/// also bytes: the charset is ASCII-only. A hard error, never a truncation —
/// a silently shortened SHA is a wrong external action.
pub const INPUT_VALUE_LEN_MAX: usize = 256;

/// Most inputs one job type may declare (design #311 Decision 2). A hard error;
/// a type wanting more is a type doing two jobs.
pub const INPUTS_COUNT_MAX: usize = 16;

/// Compiled-program ceiling for a declared `pattern` (STYLE.md Tier 2 #3 —
/// everything is bounded). A pattern that narrows an identifier needs a few
/// hundred bytes; 64 KiB is far above any real one, so a pathological
/// declaration fails validation loudly instead of costing memory on every
/// validation pass.
const PATTERN_SIZE_LIMIT_BYTES: usize = 64 * 1024;

/// Why a candidate input value was refused. One variant per rule, so the
/// caller's diagnostic names the rule rather than restating the value.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InputValueError {
    #[error("value is empty (an input value is an identifier, never blank)")]
    Empty,
    #[error("value is {len} characters, over the {max}-character limit", max = INPUT_VALUE_LEN_MAX)]
    TooLong { len: usize },
    #[error(
        "value contains {ch:?}, which is outside the allowed charset {pattern} \
         (inputs are identifiers, not prose)",
        pattern = INPUT_VALUE_PATTERN
    )]
    Charset { ch: char },
    #[error("value {value:?} is not one of the declared values [{}]", values.join(", "))]
    NotDeclared { value: String, values: Vec<String> },
    #[error("value {value:?} does not match the declared pattern {pattern:?}")]
    PatternMismatch { value: String, pattern: String },
    #[error("declared pattern {pattern:?} is not a usable regex: {reason}")]
    PatternInvalid { pattern: String, reason: String },
}

/// Whether `c` is inside the default charset. Hand-rolled rather than
/// regex-driven because this runs over every value in every pass and needs no
/// allocation; `charset_matches_the_documented_pattern` holds it and
/// [`INPUT_VALUE_PATTERN`] in sync.
#[must_use]
pub fn value_char_allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '@' | '+' | '-')
}

/// The floor every input value clears whatever its declaration: non-empty, at
/// most [`INPUT_VALUE_LEN_MAX`] characters, and inside the default charset.
/// This is the check the launch path re-runs immediately before injection, so it
/// deliberately consults no declaration at all.
pub fn check_value_charset(value: &str) -> Result<(), InputValueError> {
    if value.is_empty() {
        return Err(InputValueError::Empty);
    }
    let len = value.chars().count();
    if len > INPUT_VALUE_LEN_MAX {
        return Err(InputValueError::TooLong { len });
    }
    if let Some(ch) = value.chars().find(|c| !value_char_allowed(*c)) {
        return Err(InputValueError::Charset { ch });
    }
    debug_assert!(value.is_ascii(), "an accepted input value is ASCII");
    Ok(())
}

/// The **effective** check for one declared input: the charset floor AND the
/// declaration's own narrowing (`values` for an `enum`, `pattern` for a
/// `string`). The charset runs first and unconditionally — that is what makes
/// "a pattern may only narrow" true by construction rather than by review.
pub fn check_value(input: &Input, value: &str) -> Result<(), InputValueError> {
    check_value_charset(value)?;
    match input.r#type {
        InputKind::Enum => {
            if !input.values.iter().any(|v| v == value) {
                return Err(InputValueError::NotDeclared {
                    value: value.to_string(),
                    values: input.values.clone(),
                });
            }
        }
        InputKind::String => {
            if let Some(pattern) = &input.pattern
                && !pattern_compile(pattern)?.is_match(value)
            {
                return Err(InputValueError::PatternMismatch {
                    value: value.to_string(),
                    pattern: pattern.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Whether a declared `pattern` is a usable whole-value regex. Checked at parse
/// time so an unusable control is an authoring error rather than a value that
/// mysteriously never validates.
pub fn check_pattern(pattern: &str) -> Result<(), InputValueError> {
    pattern_compile(pattern).map(|_| ())
}

/// A declared `pattern` compiled as a whole-value match. The anchoring is the
/// platform's, not the author's: `pattern` is documented as "must match the
/// whole value", and the non-capturing group makes that hold for an alternation
/// (`a|bb`) too. An author who anchors anyway is unaffected.
fn pattern_compile(pattern: &str) -> Result<regex::Regex, InputValueError> {
    regex::RegexBuilder::new(&format!("^(?:{pattern})$"))
        .size_limit(PATTERN_SIZE_LIMIT_BYTES)
        .build()
        .map_err(|e| InputValueError::PatternInvalid {
            pattern: pattern.to_string(),
            reason: e.to_string(),
        })
}

/// Why a *supplied* input map was refused at creation (spec §2.2, design #311
/// Decision 3). One variant per rule the creation pass can decide **without the
/// job type file** — which is exactly why these are the checks that run there.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SuppliedInputError {
    #[error("{count} supplied inputs exceeds the limit of {max}", max = INPUTS_COUNT_MAX)]
    TooMany { count: usize },
    #[error(
        "input name {name:?} is malformed (names must match {pattern})",
        pattern = INPUT_NAME_PATTERN
    )]
    Name { name: String },
    #[error("input '{name}': {source}")]
    Value {
        name: String,
        #[source]
        source: InputValueError,
    },
}

/// The creation-time shape check (spec §2.2 creation pass): every supplied name
/// is well-formed, every value clears the charset floor and the length bound,
/// and the map is no larger than a job type may declare.
///
/// **Shape only.** Whether a name is *declared* by the job type, whether a
/// `required` input has a value, and whether a value satisfies its declaration's
/// `values`/`pattern` are release-time questions — they need the type file at a
/// ref, which creation deliberately does not read ("wiring validated at release,
/// not creation"). Reports the first violation: the create reply carries one
/// message, and a malformed map is fixed one field at a time on the form.
pub fn check_supplied(inputs: &BTreeMap<String, String>) -> Result<(), SuppliedInputError> {
    if inputs.len() > INPUTS_COUNT_MAX {
        return Err(SuppliedInputError::TooMany {
            count: inputs.len(),
        });
    }
    for (name, value) in inputs {
        if !name_is_well_formed(name) {
            return Err(SuppliedInputError::Name { name: name.clone() });
        }
        check_value_charset(value).map_err(|source| SuppliedInputError::Value {
            name: name.clone(),
            source,
        })?;
    }
    Ok(())
}

/// Whether a declared input name is well-formed ([`INPUT_NAME_PATTERN`]).
#[must_use]
pub fn name_is_well_formed(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn string_input(pattern: Option<&str>) -> Input {
        Input {
            name: "sha".into(),
            r#type: InputKind::String,
            required: true,
            default: None,
            values: vec![],
            pattern: pattern.map(str::to_string),
            description: None,
        }
    }

    fn enum_input(values: &[&str]) -> Input {
        Input {
            name: "service".into(),
            r#type: InputKind::Enum,
            required: true,
            default: None,
            values: values.iter().map(|v| (*v).to_string()).collect(),
            pattern: None,
            description: None,
        }
    }

    #[test]
    fn charset_matches_the_documented_pattern() {
        let re = regex::Regex::new(INPUT_VALUE_PATTERN).unwrap();
        for c in (0u8..=127).map(char::from).chain(['é', 'λ', '\u{1F600}']) {
            assert_eq!(
                value_char_allowed(c),
                re.is_match(&c.to_string()),
                "charset disagreement on {c:?}"
            );
        }
    }

    #[test]
    fn name_shape_matches_the_documented_pattern() {
        let re = regex::Regex::new(INPUT_NAME_PATTERN).unwrap();
        for name in [
            "sha",
            "image_tag",
            "a",
            "x1",
            "IMAGE_TAG",
            "1sha",
            "_sha",
            "image-tag",
            "image.tag",
            "",
            "shá",
        ] {
            assert_eq!(
                name_is_well_formed(name),
                re.is_match(name),
                "name-shape disagreement on {name:?}"
            );
        }
    }

    /// The invariant delivery depends on (design #311 Decision 4): the
    /// name → env-key mapping is injective over well-formed names, so no two
    /// declared inputs can claim one `CHUG_INPUT_*` key.
    #[test]
    fn env_keys_are_injective_over_well_formed_names() {
        let names = [
            "sha",
            "s",
            "image_tag",
            "imagetag",
            "image_tag2",
            "a1_b2",
            "service",
        ];
        let keys: std::collections::BTreeSet<String> =
            names.iter().map(|n| input_env_key(n)).collect();
        assert_eq!(keys.len(), names.len(), "{keys:?} collapsed {names:?}");
        assert_eq!(input_env_key("image_tag"), "CHUG_INPUT_IMAGE_TAG");
        assert!(keys.iter().all(|k| k.starts_with(INPUT_ENV_PREFIX)));
        assert!(!name_is_well_formed("IMAGE_TAG"));
        assert_eq!("image_tag".to_uppercase(), "IMAGE_TAG".to_uppercase());
    }

    #[test]
    fn accepts_the_real_shapes_and_rejects_shell_metacharacters() {
        for good in [
            "ghcr.io/org/img:sha",
            "img@sha256:0123abcd",
            "4f9c1ab",
            "feature/x",
            "1.2.3+build",
            "web",
        ] {
            assert_eq!(check_value_charset(good), Ok(()), "should accept {good:?}");
        }
        for bad in [
            "a b", "a\nb", "a\tb", "a'b", "a\"b", "a`b", "a\\b", "a$b", "a;b", "a|b", "a&b", "a<b",
            "a>b", "a(b", "a)b", "a{b", "a}b", "a*b", "a?b", "a!b", "a#b", "a%b", "a=b", "a,b",
            "a[b",
        ] {
            assert!(
                matches!(
                    check_value_charset(bad),
                    Err(InputValueError::Charset { .. })
                ),
                "should reject {bad:?}"
            );
        }
        assert_eq!(check_value_charset(""), Err(InputValueError::Empty));
    }

    #[test]
    fn length_bound_is_a_hard_error_at_the_boundary() {
        assert!(
            INPUT_VALUE_PATTERN.contains(&format!("{{1,{INPUT_VALUE_LEN_MAX}}}")),
            "{INPUT_VALUE_PATTERN} does not advertise the {INPUT_VALUE_LEN_MAX} bound"
        );
        let at_limit = "a".repeat(INPUT_VALUE_LEN_MAX);
        assert_eq!(check_value_charset(&at_limit), Ok(()));
        let over = "a".repeat(INPUT_VALUE_LEN_MAX + 1);
        assert_eq!(
            check_value_charset(&over),
            Err(InputValueError::TooLong {
                len: INPUT_VALUE_LEN_MAX + 1
            }),
            "one character over the bound is an error, not a truncation"
        );
    }

    #[test]
    fn pattern_narrows_the_charset_and_can_never_widen_it() {
        let permissive = string_input(Some(".*"));
        assert!(matches!(
            check_value(&permissive, "a;rm -rf"),
            Err(InputValueError::Charset { .. })
        ));
        let sha = string_input(Some("^[0-9a-f]{7,40}$"));
        assert_eq!(check_value(&sha, "4f9c1ab"), Ok(()));
        for bad in ["-rf", "/etc/shadow", "4F9C1AB", "4f9c1"] {
            assert!(
                matches!(
                    check_value(&sha, bad),
                    Err(InputValueError::PatternMismatch { .. })
                ),
                "pattern should reject {bad:?}"
            );
        }
    }

    #[test]
    fn pattern_must_match_the_whole_value() {
        let unanchored = string_input(Some("[0-9a-f]{7}"));
        assert_eq!(check_value(&unanchored, "4f9c1ab"), Ok(()));
        assert!(check_value(&unanchored, "4f9c1abzz").is_err());
        let alternation = string_input(Some("a|bb"));
        assert_eq!(check_value(&alternation, "bb"), Ok(()));
        assert!(check_value(&alternation, "bba").is_err());
    }

    #[test]
    fn unusable_pattern_is_reported_not_panicked() {
        let broken = string_input(Some("[unclosed"));
        assert!(matches!(
            check_pattern("[unclosed"),
            Err(InputValueError::PatternInvalid { .. })
        ));
        assert!(matches!(
            check_value(&broken, "abc"),
            Err(InputValueError::PatternInvalid { .. })
        ));
        assert_eq!(check_pattern("^[0-9a-f]{7,40}$"), Ok(()));
    }

    /// The creation pass (422) decides exactly the rules that need no job type
    /// file: count, name shape, charset, length. A supplied map that is only
    /// *semantically* wrong — an undeclared name, an out-of-list enum value —
    /// passes here and is caught at release, where the declaration is readable.
    #[test]
    fn creation_shape_check_covers_the_declaration_free_rules() {
        let ok = BTreeMap::from([
            ("sha".to_string(), "4f9c1ab".to_string()),
            ("image_tag".to_string(), "ghcr.io/org/img:sha".to_string()),
        ]);
        assert_eq!(check_supplied(&ok), Ok(()));
        assert_eq!(check_supplied(&BTreeMap::new()), Ok(()));

        for bad_name in ["SHA", "1sha", "image-tag", ""] {
            assert_eq!(
                check_supplied(&BTreeMap::from([(bad_name.to_string(), "x".to_string())])),
                Err(SuppliedInputError::Name {
                    name: bad_name.to_string()
                })
            );
        }

        assert_eq!(
            check_supplied(&BTreeMap::from([("sha".into(), "a;rm -rf".into())])),
            Err(SuppliedInputError::Value {
                name: "sha".into(),
                source: InputValueError::Charset { ch: ';' },
            })
        );
        let over = "a".repeat(INPUT_VALUE_LEN_MAX + 1);
        assert_eq!(
            check_supplied(&BTreeMap::from([("sha".into(), over)])),
            Err(SuppliedInputError::Value {
                name: "sha".into(),
                source: InputValueError::TooLong {
                    len: INPUT_VALUE_LEN_MAX + 1
                },
            })
        );

        let at_limit: BTreeMap<String, String> = (0..INPUTS_COUNT_MAX)
            .map(|i| (format!("input_{i}"), "v".to_string()))
            .collect();
        assert_eq!(check_supplied(&at_limit), Ok(()));
        let over_limit: BTreeMap<String, String> = (0..=INPUTS_COUNT_MAX)
            .map(|i| (format!("input_{i}"), "v".to_string()))
            .collect();
        assert_eq!(
            check_supplied(&over_limit),
            Err(SuppliedInputError::TooMany {
                count: INPUTS_COUNT_MAX + 1
            })
        );

        assert_eq!(
            check_supplied(&BTreeMap::from([("nobody_declared_me".into(), "x".into())])),
            Ok(())
        );
    }

    #[test]
    fn enum_value_must_be_declared() {
        let service = enum_input(&["web", "worker", "bot"]);
        assert_eq!(check_value(&service, "worker"), Ok(()));
        assert_eq!(
            check_value(&service, "database"),
            Err(InputValueError::NotDeclared {
                value: "database".into(),
                values: vec!["web".into(), "worker".into(), "bot".into()],
            })
        );
        assert!(check_value(&enum_input(&[]), "web").is_err());
    }
}

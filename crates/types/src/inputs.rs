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
//! - **Accepts:** a declared [`Input`] and a candidate value.
//! - **Emits:** [`InputValueError`], naming the rule the value broke.
//! - **Guarantees:** pure and total — no I/O, no async, no interior state, no
//!   panics; the charset is checked first and unconditionally, so no
//!   declaration can widen it.
//! - **Spec:** §1.1 (field rules), §2.2 (the three validation passes), §5.3.

use crate::job_type::{Input, InputKind};
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
    // Characters, not bytes: the charset is ASCII-only, so a multi-byte
    // character is out of it regardless, and reporting the character is what an
    // author can act on.
    let len = value.chars().count();
    if len > INPUT_VALUE_LEN_MAX {
        return Err(InputValueError::TooLong { len });
    }
    if let Some(ch) = value.chars().find(|c| !value_char_allowed(*c)) {
        return Err(InputValueError::Charset { ch });
    }
    // Postcondition, negative space (STYLE.md Tier 2 #2): an accepted value is
    // ASCII, which is what lets the length bound double as a byte bound and what
    // every downstream env/prompt consumer relies on.
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
        // The hand-rolled predicate and the regex the JSON Schema advertises are
        // two statements of one rule; this is what keeps them one rule.
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
        // Every excluded class from design #311 Decision 5 layer 2: whitespace,
        // quotes, backtick, backslash, and the shell metacharacters.
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
        // The advertised pattern carries the same bound as the constant.
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
        // A wide-open pattern does not buy back a metacharacter: the charset is
        // checked first, unconditionally.
        let permissive = string_input(Some(".*"));
        assert!(matches!(
            check_value(&permissive, "a;rm -rf"),
            Err(InputValueError::Charset { .. })
        ));
        // The rollback case: a hex SHA and nothing else, which is what rules out
        // the leading `-` and `/` the charset alone permits.
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
        // Unanchored by the author, anchored by the platform — a partial match
        // is not a match, and an alternation still means the whole value.
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
        // An enum with nothing declared admits nothing — the field rules reject
        // the declaration, and the value check agrees.
        assert!(check_value(&enum_input(&[]), "web").is_err());
    }
}

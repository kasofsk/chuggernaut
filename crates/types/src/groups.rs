//! Job **group** name rules (spec §1.1 `groups:`, design #321 Decision 2).
//!
//! A group is an operator annotation — "this job is part of that" — and nothing
//! else: it is [inert to execution](crate::job::Job::groups), so unlike a
//! knowledge tag it has no resolution path, no registry and no referential
//! integrity. What is left to decide is the *shape* of the string, and that is
//! decided here so all three write paths (create, the Draft edit, and the
//! `req.jobs.groups.*` verb) share one implementation rather than three
//! subtly different ones.
//!
//! Shaped after [`crate::inputs`] on purpose: one idiom for "a bounded
//! operator-supplied string set", so there is nothing for
//! `.chug/tasks/check-duplication.sh` to find and nothing for a reader to
//! learn twice.
//!
//! - **Accepts:** a candidate group name, or a whole job's list of them.
//! - **Emits:** [`GroupsError`], naming the rule the list broke, and the
//!   `docs/design/` path a `design/`-namespaced name conventionally refers to.
//! - **Guarantees:** pure and total — no I/O, no async, no interior state, no
//!   panics; hard errors, never truncation; [`design_doc_path`] returns a
//!   repo-relative path with no `..` segment for every name it accepts.
//! - **Spec:** §1.1 (field rules), §6.2 (`PUT .../jobs/{seq}/groups`).

use std::collections::BTreeSet;
use thiserror::Error;

/// The shape of a group name, as a regex over the whole name. Kept beside the
/// checker so the generated JSON Schema documents exactly what
/// [`name_is_well_formed`] accepts. Lowercase alphanumerics plus four
/// punctuation characters, which covers the real shapes (`design/311-job-inputs`,
/// `beacon-import`, `ops/fleet-refresh`) and nothing else; the leading character
/// is narrowed so a name can never open with the `/` or `.` that would make it
/// read as a path fragment.
pub const GROUP_NAME_PATTERN: &str = r"^[a-z0-9][a-z0-9._/-]*$";

/// Longest accepted group name (design #321 Decision 2). Characters, which is
/// also bytes: the charset is ASCII-only. A hard error, never a truncation — a
/// silently shortened name is a second group of one.
pub const GROUP_NAME_LEN_MAX: usize = 128;

/// Most groups one job may belong to (design #321 Decision 2). A hard error; a
/// job that is part of nine things is a job nobody can read the membership of.
pub const GROUPS_COUNT_MAX: usize = 8;

/// The namespace prefix whose members conventionally name a design document
/// (design #321 Decision 2). A convention, not a platform rule: a name outside
/// it is an ordinary group, and a name inside it whose doc does not exist is
/// still a working group (spec §4.4's posture for a knowledge tag with no file).
pub const DESIGN_GROUP_PREFIX: &str = "design/";

/// Where a [`DESIGN_GROUP_PREFIX`] group's document lives in the project repo.
pub const DESIGN_DOC_DIR: &str = "docs/design/";

/// Why a job's `groups` list was refused. One variant per rule, so the caller's
/// diagnostic names the rule rather than restating the list.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GroupsError {
    #[error("{count} groups exceeds the limit of {max}", max = GROUPS_COUNT_MAX)]
    TooMany { count: usize },
    #[error(
        "group name {name:?} is malformed (names must match {pattern})",
        pattern = GROUP_NAME_PATTERN
    )]
    Name { name: String },
    #[error(
        "group name is {len} characters, over the {max}-character limit",
        max = GROUP_NAME_LEN_MAX
    )]
    TooLong { len: usize },
    #[error("group {name:?} is listed twice on one job")]
    Duplicate { name: String },
}

/// Whether `c` may appear after a group name's first character. Hand-rolled
/// rather than regex-driven because this runs over every name on every write and
/// needs no allocation; `charset_matches_the_documented_pattern` holds it and
/// [`GROUP_NAME_PATTERN`] in sync.
#[must_use]
pub fn name_char_allowed(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '/' | '-')
}

/// Whether a group name is well-formed ([`GROUP_NAME_PATTERN`]): non-empty,
/// opening on a lowercase alphanumeric, and inside the charset thereafter.
#[must_use]
pub fn name_is_well_formed(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(name_char_allowed)
}

/// The rules one name clears: the length bound first (so a pathological value
/// reports the bound it broke rather than whichever character happened to be
/// outside the charset), then the shape.
pub fn check_name(name: &str) -> Result<(), GroupsError> {
    // Characters, not bytes: the charset is ASCII-only, so a multi-byte
    // character is out of it regardless, and the count is what an operator can
    // act on.
    let len = name.chars().count();
    if len > GROUP_NAME_LEN_MAX {
        return Err(GroupsError::TooLong { len });
    }
    if !name_is_well_formed(name) {
        return Err(GroupsError::Name {
            name: name.to_string(),
        });
    }
    // Postcondition, negative space (STYLE.md Tier 2 #2): an accepted name is
    // ASCII, which is what lets the length bound double as a byte bound.
    debug_assert!(name.is_ascii(), "an accepted group name is ASCII");
    Ok(())
}

/// The whole check every write path runs over a job's resulting list: the count
/// bound, then each name's length and shape, then uniqueness within the one job.
///
/// Reports the first violation — a reply carries one message, and a malformed
/// list is fixed one name at a time on the form. There is deliberately **no**
/// second, semantic pass anywhere: unlike an input, a group has no declaration
/// to be checked against and no registry to exist in (design #321 Decision 2).
pub fn check_groups(groups: &[String]) -> Result<(), GroupsError> {
    if groups.len() > GROUPS_COUNT_MAX {
        return Err(GroupsError::TooMany {
            count: groups.len(),
        });
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for name in groups {
        check_name(name)?;
        if !seen.insert(name.as_str()) {
            return Err(GroupsError::Duplicate { name: name.clone() });
        }
    }
    // Postcondition: an accepted list is exactly as long as its distinct set,
    // which is what lets every reader treat `groups` as a set without sorting it.
    debug_assert_eq!(
        seen.len(),
        groups.len(),
        "an accepted list has no duplicate"
    );
    Ok(())
}

/// The design document a `design/`-namespaced group conventionally refers to:
/// `design/311-job-inputs` → `docs/design/311-job-inputs.md`. `None` for every
/// other namespace, which is every group that names no document.
///
/// One implementation, because the naming convention is a joint fact: the group
/// endpoint writes names by it and the group *read* (slice B) resolves them by
/// it against the repo. That read is why the accepted stem is a single flat
/// segment: `.` and `/` are both inside [`GROUP_NAME_PATTERN`], so
/// `design/../../etc/passwd` is a shape-legal name, and a path built from it
/// would escape `docs/design/`. Refusing it here means no caller has to
/// remember to.
#[must_use]
pub fn design_doc_path(name: &str) -> Option<String> {
    let stem = name.strip_prefix(DESIGN_GROUP_PREFIX)?;
    if stem.is_empty() || stem.contains('/') || stem.starts_with('.') {
        return None;
    }
    let path = format!("{DESIGN_DOC_DIR}{stem}.md");
    debug_assert!(
        !path.contains(".."),
        "a resolved design path never escapes {DESIGN_DOC_DIR}"
    );
    Some(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    /// The hand-rolled predicate and the regex the JSON Schema advertises are two
    /// statements of one rule; this is what keeps them one rule. Every character
    /// is checked in both positions, because the leading one is narrower.
    #[test]
    fn charset_matches_the_documented_pattern() {
        let re = regex::Regex::new(GROUP_NAME_PATTERN).unwrap();
        for c in (0u8..=127).map(char::from).chain(['é', 'λ', '\u{1F600}']) {
            let alone = c.to_string();
            assert_eq!(
                name_is_well_formed(&alone),
                re.is_match(&alone),
                "leading-character disagreement on {c:?}"
            );
            let trailing = format!("a{c}");
            assert_eq!(
                name_is_well_formed(&trailing),
                re.is_match(&trailing),
                "charset disagreement on {c:?}"
            );
        }
        assert!(!name_is_well_formed(""), "an empty name is not a name");
        assert!(!re.is_match(""));
    }

    #[test]
    fn accepts_the_real_shapes_and_rejects_prose() {
        for good in [
            "design/311-job-inputs",
            "beacon-import",
            "ops/fleet-refresh",
            "v2.1",
            "a",
            "0",
            "under_score",
        ] {
            assert_eq!(check_name(good), Ok(()), "should accept {good:?}");
        }
        // Uppercase, a leading separator, whitespace, and the punctuation a
        // label picks up when someone types a sentence instead of a name.
        for bad in [
            "Design/311",
            "/design",
            ".hidden",
            "-lead",
            "beacon import",
            "beacon\timport",
            "design:311",
            "design#311",
            "design/311!",
            "café",
            "",
        ] {
            assert_eq!(
                check_name(bad),
                Err(GroupsError::Name {
                    name: bad.to_string()
                }),
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn length_bound_is_a_hard_error_at_the_boundary() {
        let at_limit = "a".repeat(GROUP_NAME_LEN_MAX);
        assert_eq!(check_name(&at_limit), Ok(()));
        let over = "a".repeat(GROUP_NAME_LEN_MAX + 1);
        assert_eq!(
            check_name(&over),
            Err(GroupsError::TooLong {
                len: GROUP_NAME_LEN_MAX + 1
            }),
            "one character over the bound is an error, not a truncation"
        );
        // The bound is on characters, so a multi-byte name is measured the way
        // an operator counts it — and still fails the charset.
        assert_eq!(
            check_name(&"é".repeat(GROUP_NAME_LEN_MAX + 1)),
            Err(GroupsError::TooLong {
                len: GROUP_NAME_LEN_MAX + 1
            })
        );
    }

    #[test]
    fn count_bound_is_a_hard_error_at_the_boundary() {
        let at_limit: Vec<String> = (0..GROUPS_COUNT_MAX)
            .map(|i| format!("group-{i}"))
            .collect();
        assert_eq!(check_groups(&at_limit), Ok(()));
        let mut over = at_limit;
        over.push("one-too-many".into());
        assert_eq!(
            check_groups(&over),
            Err(GroupsError::TooMany {
                count: GROUPS_COUNT_MAX + 1
            })
        );
        assert_eq!(check_groups(&[]), Ok(()), "ungrouped is the common case");
    }

    /// Uniqueness is per job, not global: two jobs share a group by construction
    /// (that is what a group *is*), but one job listing it twice is a malformed
    /// list — the count bound would be spent on nothing, and every reader treats
    /// the list as a set.
    #[test]
    fn a_name_is_listed_at_most_once_per_job() {
        assert_eq!(
            check_groups(&names(&["design/311-job-inputs", "beacon-import"])),
            Ok(())
        );
        assert_eq!(
            check_groups(&names(&["beacon-import", "design/x", "beacon-import"])),
            Err(GroupsError::Duplicate {
                name: "beacon-import".into()
            })
        );
    }

    /// The first violation is reported, and it names the rule: a malformed name
    /// inside an otherwise legal list is a `Name` error, not a count error.
    #[test]
    fn the_reported_error_names_the_rule_that_broke() {
        assert_eq!(
            check_groups(&names(&["ok", "NOT OK"])),
            Err(GroupsError::Name {
                name: "NOT OK".into()
            })
        );
        assert_eq!(
            check_groups(&[String::from("ok"), "x".repeat(GROUP_NAME_LEN_MAX + 1)]),
            Err(GroupsError::TooLong {
                len: GROUP_NAME_LEN_MAX + 1
            })
        );
        // Each message names its own rule, so a 422 body is actionable without
        // the reader knowing the validator's source.
        assert!(
            GroupsError::TooMany { count: 9 }
                .to_string()
                .contains(&GROUPS_COUNT_MAX.to_string())
        );
        assert!(
            GroupsError::Name { name: "X".into() }
                .to_string()
                .contains(GROUP_NAME_PATTERN)
        );
    }

    /// The naming convention, in the one place it is implemented: the stem is
    /// the doc's basename, and only a `design/` name resolves at all.
    #[test]
    fn design_names_resolve_to_their_doc_path() {
        assert_eq!(
            design_doc_path("design/311-job-inputs").as_deref(),
            Some("docs/design/311-job-inputs.md")
        );
        assert_eq!(
            design_doc_path("design/321-job-groups").as_deref(),
            Some("docs/design/321-job-groups.md")
        );
        for other in [
            "beacon-import",
            "ops/fleet-refresh",
            "designs/311",
            "design",
        ] {
            assert_eq!(
                design_doc_path(other),
                None,
                "{other:?} names no design document"
            );
        }
    }

    /// The traversal case, which is why the stem must be one flat segment: `.`
    /// and `/` are both inside the name charset, so these are *shape-legal*
    /// names whose naive path would escape `docs/design/` on the read side.
    #[test]
    fn a_design_name_can_never_escape_the_design_directory() {
        for escaping in [
            "design/../../etc/passwd",
            "design/..",
            "design/.ssh/id_rsa",
            "design/sub/dir",
            "design/",
        ] {
            assert_eq!(check_name(escaping), Ok(()), "{escaping:?} is shape-legal");
            assert_eq!(
                design_doc_path(escaping),
                None,
                "{escaping:?} must not resolve to a path"
            );
        }
    }
}

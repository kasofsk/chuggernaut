//! Cursor paging for a job's diff (spec §6.2). A `DiffResponse` holds the whole
//! unified diff in one string — job #342's was 1.3MB — and a NATS reply cannot
//! carry more than `max_payload`, so the diff subject serves byte-offset pages
//! instead of one message that can never be published.
//!
//! - **Accepts:** a [`DiffResponse`] and a byte cursor into its diff text.
//! - **Emits:** a [`DiffPage`] — the diffstat summary (first page only), the
//!   text from the cursor on, the next cursor, a digest of the whole diff, and
//!   whether the diff is exhausted.
//! - **Guarantees:** pure and total; offsets are byte offsets into one diff
//!   text, so they are stable for as long as the digest is unchanged, every
//!   page before `done` advances, and `data` never exceeds
//!   [`DIFF_PAGE_ESCAPED_MAX`] escaped bytes.
//! - **Spec:** §6.2.

use crate::{DiffResponse, FileStat};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Escaped-length budget for one page's `data`, a quarter of NATS's 1MB
/// `max_payload`. It is measured against the JSON-escaped length because one
/// diff byte can cost six payload bytes, and the headroom carries the summary
/// and the whole-diff copy that ride alongside it.
pub const DIFF_PAGE_ESCAPED_MAX: usize = 256 * 1024;

/// One cursor page of a job's diff (spec §6.2). The summary and the
/// whole-diff copy appear only on a page that can carry them entire.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DiffPage {
    /// Per-file diffstat, on the first page only.
    pub files: Vec<FileStat>,
    /// Where `data` ends in the diff text — the next `since`.
    pub offset: u64,
    /// The diff text from the requested cursor up to `offset`.
    pub data: String,
    /// True when `offset` is the end of the diff.
    pub done: bool,
    /// SHA-256 of the whole diff text, on every page. A caller whose pages
    /// disagree on it is reading a diff that moved and must restart at `0`.
    pub digest: String,
    /// The whole diff for callers that do not page; empty unless this page is
    /// the entire diff.
    pub diff: String,
}

impl DiffPage {
    /// Page `response` from byte cursor `since`, capping `data` at
    /// [`DIFF_PAGE_ESCAPED_MAX`] escaped bytes. A cursor at or past the end
    /// yields an empty page marked `done`, distinguishable from a legitimate
    /// last page by its `digest`.
    #[must_use]
    pub fn slice(response: &DiffResponse, since: u64) -> Self {
        let text = &response.diff;
        let start = char_boundary_at_or_before(text, since as usize);
        let end = page_end(text, start);
        assert!(end >= start, "a diff page cannot run backwards");
        assert!(
            end > start || start == text.len(),
            "a diff page short of the end must advance"
        );
        let done = end == text.len();
        let data = text[start..end].to_string();
        DiffPage {
            files: if start == 0 {
                response.files.clone()
            } else {
                Vec::new()
            },
            offset: end as u64,
            diff: if start == 0 && done {
                data.clone()
            } else {
                String::new()
            },
            digest: diff_digest(text),
            data,
            done,
        }
    }
}

/// Content identity of a whole diff text, as lowercase hex. The diff is
/// regenerated per page from live refs, so a caller needs this to tell pages of
/// one diff from pages of a diff that changed underneath it.
fn diff_digest(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex.len(), 64, "a sha-256 digest is 64 hex characters");
    hex
}

/// The end of the page starting at `start`: as much text as the escaped budget
/// holds, always on a character boundary.
fn page_end(text: &str, start: usize) -> usize {
    let mut escaped = 0usize;
    for (i, c) in text[start..].char_indices() {
        escaped += escaped_len(c);
        if escaped > DIFF_PAGE_ESCAPED_MAX {
            return start + i;
        }
    }
    text.len()
}

/// Bytes `c` occupies once JSON-escaped, over-estimating the control characters
/// serde writes as a two-byte escape. Over-estimating keeps the reply under the
/// cap; under-estimating would not.
fn escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// Floor `at` to a character boundary of `text`, clamped to its length, so a
/// cursor a caller invented cannot split a character.
fn char_boundary_at_or_before(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(diff: &str) -> DiffResponse {
        DiffResponse {
            files: vec![FileStat {
                path: "src/lib.rs".into(),
                additions: 1,
                deletions: 0,
            }],
            diff: diff.to_string(),
        }
    }

    fn read_to_end(response: &DiffResponse) -> (String, usize) {
        let mut text = String::new();
        let mut since = 0u64;
        for pages in 1..=64 {
            let page = DiffPage::slice(response, since);
            text.push_str(&page.data);
            since = page.offset;
            if page.done {
                return (text, pages);
            }
        }
        panic!("paging did not terminate within 64 pages");
    }

    #[test]
    fn a_small_diff_is_one_complete_page() {
        let small = response("--- a\n+++ b\n+hello\n");
        let page = DiffPage::slice(&small, 0);
        assert!(page.done);
        assert_eq!(page.data, small.diff);
        assert_eq!(page.diff, small.diff);
        assert_eq!(page.offset, small.diff.len() as u64);
        assert_eq!(page.files.len(), 1);
    }

    #[test]
    fn a_large_diff_pages_to_completion() {
        let big = response(&"+a line of diff text\n".repeat(60_000));
        let first = DiffPage::slice(&big, 0);
        assert!(!first.done);
        assert!(first.diff.is_empty(), "an incomplete page carries no copy");
        assert_eq!(first.files.len(), 1);
        let later = DiffPage::slice(&big, first.offset);
        assert!(later.files.is_empty(), "the summary rides the first page");
        let (text, pages) = read_to_end(&big);
        assert!(pages > 1, "a 1.2MB diff must take several pages");
        assert_eq!(text, big.diff);
    }

    #[test]
    fn offsets_are_stable_across_calls() {
        let big = response(&"+a line of diff text\n".repeat(60_000));
        let first = DiffPage::slice(&big, 0);
        assert_eq!(DiffPage::slice(&big, 0), first);
        let refetched = DiffPage::slice(&big, first.offset);
        assert_eq!(DiffPage::slice(&big, first.offset), refetched);
    }

    #[test]
    fn every_page_of_one_diff_carries_the_same_digest() {
        let big = response(&"+a line of diff text\n".repeat(60_000));
        let first = DiffPage::slice(&big, 0);
        let later = DiffPage::slice(&big, first.offset);
        assert_eq!(first.digest, later.digest);
        assert_eq!(first.digest.len(), 64);
    }

    #[test]
    fn a_diff_that_shrank_under_a_cursor_does_not_read_as_finished() {
        let big = response(&"+a line of diff text\n".repeat(60_000));
        let first = DiffPage::slice(&big, 0);
        assert!(!first.done, "the fixture must need more than one page");
        let stale = DiffPage::slice(&response("+one line\n"), first.offset);
        assert!(stale.done && stale.data.is_empty());
        assert_ne!(
            stale.digest, first.digest,
            "a page cut from a shrunken diff must not pass for this diff's end"
        );
    }

    #[test]
    fn a_rewrite_of_the_same_length_changes_the_digest() {
        let before = response(&"+a line of diff text\n".repeat(60_000));
        let after = response(&"+a line of diff TEXT\n".repeat(60_000));
        assert_eq!(before.diff.len(), after.diff.len());
        assert_ne!(
            DiffPage::slice(&before, 0).digest,
            DiffPage::slice(&after, 0).digest
        );
    }

    #[test]
    fn escaping_shrinks_a_page_below_the_raw_budget() {
        let quoted = response(&"\"".repeat(DIFF_PAGE_ESCAPED_MAX));
        let page = DiffPage::slice(&quoted, 0);
        assert!(!page.done);
        assert_eq!(page.data.len(), DIFF_PAGE_ESCAPED_MAX / 2);
        let (text, _) = read_to_end(&quoted);
        assert_eq!(text, quoted.diff);
    }

    #[test]
    fn a_cursor_at_or_past_the_end_is_empty_and_done() {
        let small = response("+one line\n");
        let end = small.diff.len() as u64;
        for since in [end, end + 1, u64::MAX] {
            let page = DiffPage::slice(&small, since);
            assert!(page.done && page.data.is_empty());
            assert_eq!(page.offset, end);
            assert!(page.diff.is_empty() && page.files.is_empty());
        }
    }

    #[test]
    fn a_cursor_inside_a_character_floors_to_its_boundary() {
        let wide = response("+é\n");
        let page = DiffPage::slice(&wide, 2);
        assert!(page.done);
        assert_eq!(page.data, "é\n");
    }
}

//! §4.4 upfront knowledge injection, pure half: what a `knowledge:` entry
//! names, and the block the documents it resolves to compose into.
//!
//! An entry naming a markdown file is a **page** — the repo's own document,
//! delivered as itself rather than restated in a tag, which is what keeps one
//! definition of a rule in the tree (design #415 D4).
//!
//! - **Accepts:** a job type's `knowledge:` defaults, a job's
//!   `knowledge_tags`, and the documents those entries resolved to.
//! - **Emits:** the read each entry names — a repo page or a config-root tag
//!   file — and the `## Project Knowledge` system-prompt block.
//! - **Guarantees:** pure and total; no I/O and no state. Entry order is
//!   declaration order with the first occurrence of a repeat kept, so the same
//!   entries over the same documents compose the same bytes.
//! - **Spec:** §4.4.

use std::collections::HashSet;

/// Where a `knowledge:` entry's text lives.
pub(crate) enum Source {
    /// A repo page named by path (`STYLE.md`), read verbatim at `base_ref`.
    Page(String),
    /// The config-root-relative tag file a bare name resolves to
    /// (`tags/rust.md`).
    Tag(String),
}

/// The read `entry` names: a page when the entry names a markdown file, else
/// the tag file for that name.
pub(crate) fn source(entry: &str) -> Source {
    assert!(!entry.is_empty(), "knowledge entry must not be empty");
    let source = if entry.ends_with(".md") {
        Source::Page(entry.to_string())
    } else {
        Source::Tag(format!("tags/{entry}.md"))
    };
    assert!(
        source_path(&source).ends_with(".md"),
        "knowledge entry '{entry}' must resolve to a markdown file"
    );
    source
}

/// The path a source reads, whichever kind it is.
fn source_path(source: &Source) -> &str {
    match source {
        Source::Page(path) | Source::Tag(path) => path,
    }
}

/// §4.4's union: the type's declared defaults followed by the job's own tags,
/// each entry kept once at its first occurrence.
pub(crate) fn entries<'a>(defaults: &'a [String], tags: &'a [String]) -> Vec<&'a str> {
    let mut seen = HashSet::new();
    let entries: Vec<&str> = defaults
        .iter()
        .chain(tags.iter())
        .map(String::as_str)
        .filter(|entry| !entry.is_empty() && seen.insert(*entry))
        .collect();
    assert!(
        entries.len() <= defaults.len() + tags.len(),
        "deduplication never invents an entry"
    );
    assert_eq!(entries.len(), seen.len(), "an entry survives at most once");
    entries
}

/// The `## Project Knowledge` block: one `### {entry}` section per resolved
/// document, in the order given. `None` when nothing resolved.
pub(crate) fn block(sections: &[(&str, String)]) -> Option<String> {
    if sections.is_empty() {
        return None;
    }
    let mut block = String::from("## Project Knowledge\n");
    for (entry, content) in sections {
        assert!(!entry.is_empty(), "a section must name its entry");
        block.push_str(&format!("\n### {entry}\n{content}\n"));
    }
    assert!(
        block.starts_with("## Project Knowledge\n"),
        "the block is what §4.4 names it"
    );
    Some(block)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn path_of(entry: &str) -> String {
        source_path(&source(entry)).to_string()
    }

    /// An entry naming a markdown file reads that page verbatim; a bare name
    /// stays the tag it always was.
    #[test]
    fn entry_resolves_page_by_path_and_tag_by_name() {
        assert!(matches!(source("STYLE.md"), Source::Page(_)));
        assert_eq!(path_of("STYLE.md"), "STYLE.md");
        assert_eq!(
            path_of("docs/implementation-notes.md"),
            "docs/implementation-notes.md"
        );
        assert!(matches!(source("rust"), Source::Tag(_)));
        assert_eq!(path_of("rust"), "tags/rust.md");
        assert_eq!(
            path_of("payments/stripe-integration"),
            "tags/payments/stripe-integration.md"
        );
    }

    /// The union is ordered and deduplicated: type defaults first, a repeat
    /// kept at its first occurrence.
    #[test]
    fn entries_dedupe_in_declaration_order() {
        let defaults = ["STYLE.md".to_string(), "rust".to_string()];
        let tags = ["rust".to_string(), "".to_string(), "NORTH-STAR.md".into()];
        assert_eq!(
            entries(&defaults, &tags),
            vec!["STYLE.md", "rust", "NORTH-STAR.md"]
        );
        assert!(entries(&[], &[]).is_empty());
    }

    /// Nothing resolved is no block at all — an agent with no knowledge gets
    /// no empty heading.
    #[test]
    fn block_of_nothing_is_none() {
        assert!(block(&[]).is_none());
    }

    /// The composed bytes are golden: the same entries over the same documents
    /// produce one exact string, so identical-knowledge jobs share a prompt.
    #[test]
    fn block_composes_golden_bytes() {
        let sections = [
            ("STYLE.md", "the rules\n".to_string()),
            ("NORTH-STAR.md", "the target".to_string()),
        ];
        let composed = block(&sections).expect("two sections");
        assert_eq!(
            composed,
            "## Project Knowledge\n\n### STYLE.md\nthe rules\n\n\n### NORTH-STAR.md\nthe target\n"
        );
        assert_eq!(block(&sections).as_deref(), Some(composed.as_str()));
    }

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repo root")
    }

    /// This repo's own agent job types inject the blessed practices as the
    /// pages that define them — the golden that pins the prompt every agent
    /// job here receives.
    #[test]
    fn repo_agent_job_types_inject_the_defining_pages() {
        let root = repo_root();
        for job_type in ["code", "design", "docs", "web"] {
            let yaml = std::fs::read_to_string(
                root.join(".chug/jobs")
                    .join(job_type)
                    .with_extension("yaml"),
            )
            .expect("job type");
            let declared = types::JobType::parse(&yaml).expect("parses").knowledge;
            assert_eq!(
                declared,
                vec!["STYLE.md".to_string(), "NORTH-STAR.md".to_string()],
                "{job_type} injects the pages, not a restatement of them"
            );
            for entry in entries(&declared, &[]) {
                let Source::Page(path) = source(entry) else {
                    panic!("{entry} must be a page, not a tag");
                };
                assert!(root.join(&path).is_file(), "{path} must exist to inject");
            }
        }
    }
}

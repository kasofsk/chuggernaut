//! The project's repo-versioned chuggernaut config (spec §1.1): job types,
//! prompts, reusable tasks and knowledge tags live in the project repo under
//! `.chug/`, and the dispatcher reads them through the `vcs` port.
//!
//! One module owns the layout so every reader resolves it the same way: the
//! canonical `.chug/` location first, then the bare repo-root path used by
//! repos — and by pinned `base_ref`s — that predate the config root
//! (`types::config_paths`).
//!
//! - **Accepts:** a config-root-relative path (or directory) and a git ref.
//! - **Emits:** file content and the entries a repo tree carries, each paired
//!   with the path it resolved to — so a caller passing a config file on to a
//!   plain file read never has to guess the layout a second time.
//! - **Guarantees:** reads only, no state writes; `.chug/` shadows the
//!   repo-root layout, so a name resolves to exactly one file and lists once.
//! - **Spec:** §1.1, §2.2, §4.4.

use vcs::RepoManager;

/// A config file as resolved: the path it was found at, and its content.
pub(crate) struct ConfigFile {
    pub path: String,
    pub content: String,
}

/// One entry of a config directory: its stem (`code` for `.chug/jobs/code.yaml`)
/// and the path that stem resolved to.
pub(crate) struct ConfigEntry {
    pub stem: String,
    pub path: String,
}

/// Read a config-root-relative file (`jobs/code.yaml`) at `reference`, taking
/// the first candidate location that exists. `None` means the file is at
/// neither location — a genuine miss, not a layout question.
pub(crate) async fn read_file(
    repos: &RepoManager,
    owner: &str,
    project: &str,
    reference: &str,
    relative: &str,
) -> vcs::Result<Option<ConfigFile>> {
    for path in types::config_path_candidates(relative) {
        if let Some(content) = repos.read_file_at(owner, project, reference, &path).await? {
            return Ok(Some(ConfigFile { path, content }));
        }
    }
    Ok(None)
}

/// Every blob sitting directly in config directory `dir` with extension
/// `suffix`, at either location, sorted by stem and deduplicated — a file
/// present in both layouts is one entry resolved to the `.chug/` copy, the same
/// one [`read_file`] would take. Nested paths are not entries (the config
/// directories are flat).
pub(crate) fn entries(tree: &[vcs::TreeEntry], dir: &str, suffix: &str) -> Vec<ConfigEntry> {
    assert!(
        suffix.starts_with('.'),
        "entry suffix '{suffix}' must include its dot"
    );
    let mut ranked: Vec<(String, usize, String)> = tree
        .iter()
        .filter(|e| e.r#type == "blob")
        .filter_map(|e| {
            let name = types::config_entry_name(&e.path, dir)?;
            let stem = name.strip_suffix(suffix)?;
            (!stem.is_empty()).then(|| {
                let rank = entries_path_rank(&e.path, &format!("{dir}/{name}"));
                (stem.to_string(), rank, e.path.clone())
            })
        })
        .collect();
    ranked.sort();
    ranked.dedup_by(|a, b| a.0 == b.0);
    ranked
        .into_iter()
        .map(|(stem, _, path)| ConfigEntry { stem, path })
        .collect()
}

/// Where `path` sits in the resolution order for `relative` — 0 for the `.chug/`
/// copy, 1 for the repo-root one. Derived from `config_path_candidates` so the
/// shadowing a listing shows and the one a read performs cannot drift.
fn entries_path_rank(path: &str, relative: &str) -> usize {
    let candidates = types::config_path_candidates(relative);
    let rank = candidates.iter().position(|c| c == path);
    assert!(
        rank.is_some(),
        "'{path}' matched config directory entry '{relative}' but is no candidate for it"
    );
    rank.unwrap_or(candidates.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(path: &str) -> vcs::TreeEntry {
        vcs::TreeEntry {
            path: path.to_string(),
            r#type: "blob".to_string(),
            size: None,
        }
    }

    fn stems_and_paths(entries: &[ConfigEntry]) -> Vec<(&str, &str)> {
        entries
            .iter()
            .map(|e| (e.stem.as_str(), e.path.as_str()))
            .collect()
    }

    /// Both layouts list, each entry carrying the path it resolved to — and a
    /// stem present in both is one entry, resolved to `.chug/` the way a read
    /// would resolve it. The UI reads a listed entry back by that path, so a
    /// stale one renders an unreadable tag.
    #[test]
    fn entries_span_both_layouts_and_dedupe() {
        let tree = vec![
            blob(".chug/jobs/code.yaml"),
            blob("jobs/code.yaml"),
            blob("jobs/deploy.yaml"),
            blob(".chug/jobs/_defaults.yaml"),
        ];
        assert_eq!(
            stems_and_paths(&entries(&tree, "jobs", ".yaml")),
            vec![
                ("_defaults", ".chug/jobs/_defaults.yaml"),
                ("code", ".chug/jobs/code.yaml"),
                ("deploy", "jobs/deploy.yaml"),
            ]
        );
    }

    /// An unmigrated repo lists at the repo root — the layout the fallback
    /// exists for.
    #[test]
    fn entries_resolve_to_the_repo_root_when_that_is_all_there_is() {
        let tree = vec![blob("tags/rust.md")];
        assert_eq!(
            stems_and_paths(&entries(&tree, "tags", ".md")),
            vec![("rust", "tags/rust.md")]
        );
    }

    /// Negative space: trees, nested files, other directories and other
    /// extensions never become entries.
    #[test]
    fn entries_exclude_non_entries() {
        let tree = vec![
            vcs::TreeEntry {
                path: ".chug/jobs".to_string(),
                r#type: "tree".to_string(),
                size: None,
            },
            blob(".chug/jobs/team/code.yaml"),
            blob(".chug/prompts/work/code.md"),
            blob(".chug/jobs/README.md"),
            blob("crates/jobs/code.yaml"),
        ];
        assert!(entries(&tree, "jobs", ".yaml").is_empty());
    }
}

//! Where a project keeps its chuggernaut config (spec §1.1).
//!
//! Job types, prompts, reusable tasks and knowledge tags are project-owned and
//! repo-versioned, so the platform reads them out of the project repo by path.
//! Those paths all hang off one config root — `.chug/` — which keeps the
//! platform's files in one place instead of scattered across the repo root,
//! the way `.github/` does for GitHub.
//!
//! Repos created before the config root existed keep their files at the repo
//! root (`jobs/code.yaml` rather than `.chug/jobs/code.yaml`), and a job
//! launched before the move pins a `base_ref` that still has that layout. So
//! every *read* resolves through [`config_path_candidates`]: `.chug/` first,
//! the bare root path second. Only the first candidate is ever written.

/// The directory a project keeps its chuggernaut config under.
pub const CONFIG_DIR: &str = ".chug";

/// The canonical location of a config file — `.chug/{relative}`.
///
/// `relative` is config-root-relative (`jobs/code.yaml`), never already
/// prefixed and never absolute.
#[must_use]
pub fn config_path(relative: &str) -> String {
    assert!(!relative.is_empty(), "config path must not be empty");
    assert!(
        !relative.starts_with('/') && !relative.starts_with(CONFIG_DIR),
        "config path '{relative}' must be config-root-relative"
    );
    format!("{CONFIG_DIR}/{relative}")
}

/// Every location `relative` may live at, most-canonical first: `.chug/`, then
/// the bare repo-root path used by repos that predate the config root. Readers
/// take the first candidate that exists; there is no third location, so a miss
/// on both is a genuine miss.
#[must_use]
pub fn config_path_candidates(relative: &str) -> [String; 2] {
    let candidates = [config_path(relative), relative.to_string()];
    assert!(
        candidates[0] != candidates[1],
        "config path candidates must be distinct"
    );
    candidates
}

/// The entry name `path` carries when it sits directly inside config directory
/// `dir`, in either location: `.chug/jobs/code.yaml` and `jobs/code.yaml` both
/// yield `code.yaml` for `dir = "jobs"`.
///
/// Nested paths (`jobs/team/code.yaml`) yield `None` — the config directories
/// are flat, so a nested file is not an entry. Used to scan a repo tree, where
/// both layouts can appear at once and the caller dedupes by name.
#[must_use]
pub fn config_entry_name<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    assert!(!dir.is_empty(), "config directory must not be empty");
    assert!(!dir.contains('/'), "config directory '{dir}' must be flat");
    let rest = path
        .strip_prefix(&format!("{CONFIG_DIR}/{dir}/"))
        .or_else(|| path.strip_prefix(&format!("{dir}/")))?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_path_is_under_the_config_root() {
        assert_eq!(config_path("jobs/code.yaml"), ".chug/jobs/code.yaml");
        assert_eq!(config_path("tags/rust.md"), ".chug/tags/rust.md");
    }

    #[test]
    fn candidates_prefer_the_config_root_over_the_repo_root() {
        assert_eq!(
            config_path_candidates("jobs/_defaults.yaml"),
            [
                ".chug/jobs/_defaults.yaml".to_string(),
                "jobs/_defaults.yaml".to_string()
            ]
        );
    }

    #[test]
    fn entry_name_reads_both_layouts() {
        assert_eq!(
            config_entry_name(".chug/jobs/code.yaml", "jobs"),
            Some("code.yaml")
        );
        assert_eq!(
            config_entry_name("jobs/code.yaml", "jobs"),
            Some("code.yaml")
        );
        assert_eq!(config_entry_name("tags/rust.md", "tags"), Some("rust.md"));
    }

    /// Negative space: nothing outside a config directory, and nothing nested
    /// inside one, is an entry.
    #[test]
    fn entry_name_rejects_non_entries() {
        assert_eq!(config_entry_name("crates/jobs/code.yaml", "jobs"), None);
        assert_eq!(config_entry_name("jobs/team/code.yaml", "jobs"), None);
        assert_eq!(config_entry_name(".chug/jobs/team/code.yaml", "jobs"), None);
        assert_eq!(config_entry_name("jobs/", "jobs"), None);
        assert_eq!(config_entry_name("prompts/work/code.md", "jobs"), None);
    }
}

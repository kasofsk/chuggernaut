//! Node-side agent-CLI discovery (design #490 D3).
//!
//! accepts: the `PATH` the daemon itself was started with — on a Mac the
//! `AGENT_PATH` `deploy/prod/install-worker-launchd.sh` renders into the launchd
//! plist; emits: whether this node can serve an agent host launch, advertised in
//! `NodeCapabilities`, and the refusal a launch gets when it cannot; guarantees:
//! the probe resolves a bare name on `PATH` the way `exec` does — first match,
//! execute bit required — and reads the filesystem rather than running what it
//! found.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The command `agent::claude` execs (`crates/agent/src/claude.rs`), spelled
/// here rather than imported because `worker` does not depend on `agent`. The
/// agreement is unpinned on purpose: see `docs/implementation-notes.md`.
pub const AGENT_CLI_BIN: &str = "claude";

/// Entry cap on one `PATH` scan (docs/reference/style.md Tier 2 rule 3): a `PATH`
/// longer than this is read up to the bound rather than walked unboundedly at
/// boot.
const PATH_ENTRIES_MAX: usize = 256;

/// What a host-capable node found when it looked for the agent CLI on its own
/// `PATH`. A node property assembled at boot from the machine itself — never
/// operator-typed, for the reason design #309 gives about config that relocates
/// a physical fact.
#[derive(Debug, Clone, Default)]
pub struct AgentCli {
    path: Option<PathBuf>,
    searched: Vec<PathBuf>,
}

impl AgentCli {
    /// The daemon's own `PATH`, scanned as a host-capable node scans it at boot.
    pub fn discover() -> Self {
        Self::discover_on(std::env::var_os("PATH").as_deref())
    }

    /// The same scan against an arbitrary `PATH`, which is what makes discovery
    /// testable against a fixture tree instead of the machine's own. A `PATH`
    /// that is absent or empty finds nothing, never a panic.
    pub fn discover_on(path: Option<&OsStr>) -> Self {
        let searched: Vec<PathBuf> = path
            .map(|p| std::env::split_paths(p).take(PATH_ENTRIES_MAX).collect())
            .unwrap_or_default();
        let found = searched
            .iter()
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(AGENT_CLI_BIN))
            .find(|candidate| is_executable_file(candidate));
        debug_assert!(
            found.as_ref().is_none_or(|p| p.ends_with(AGENT_CLI_BIN)),
            "discovery reports the binary it resolved"
        );
        Self {
            path: found,
            searched,
        }
    }

    /// Whether this node can serve an agent host launch at all, which is exactly
    /// what it advertises in `NodeCapabilities`.
    pub fn present(&self) -> bool {
        self.path.is_some()
    }

    /// Where the CLI was resolved, for the boot log and the operator; never a
    /// path any launch is given, since the task execs the bare name itself.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The refusal an agent-shaped host launch gets on a node whose `PATH`
    /// carries no CLI (design #490 D5): named, so the operator reads which
    /// capability is missing instead of a `claude: command not found` inside a
    /// task's log.
    pub fn missing(&self, node: &str) -> String {
        format!(
            "launch is agent-shaped and node {node} discovered no {AGENT_CLI_BIN} on the daemon's \
             own PATH ({}) — a host task has no image to carry one, so this is refused here rather \
             than exec'd into a command-not-found (design #490 D3/D5); install the CLI on that \
             PATH and restart the daemon, which probes at boot",
            self.searched_display()
        )
    }

    /// The `PATH` the scan actually read, as the refusal prints it: an empty one
    /// is named as such rather than shown as an empty string.
    fn searched_display(&self) -> String {
        if self.searched.is_empty() {
            return "empty".to_string();
        }
        self.searched
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// Whether a candidate is something the node could actually exec: a regular file
/// (following symlinks, which is how a CLI installed by symlink resolves) that
/// carries an execute bit.
fn is_executable_file(candidate: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(candidate) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    meta.is_file()
}

/// One `PATH` directory holding `claude` in the shape `cli` names — absent,
/// present but not executable, or runnable. Every test of this scheme runs
/// against a fixture tree rather than the machine's own `PATH`, so the suite
/// says the same thing on a Mac with the CLI installed as on a Linux evaluator
/// without it.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a test fixture that cannot be built fails the test"
)]
pub fn path_fixture(root: &Path, name: &str, cli: Option<u32>) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(mode) = cli {
        let bin = dir.join(AGENT_CLI_BIN);
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = mode;
    }
    dir
}

/// The `PATH` a fixture scan is handed, joined the way the environment carries
/// it.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a test fixture that cannot be built fails the test"
)]
pub fn fixture_path(dirs: &[PathBuf]) -> std::ffi::OsString {
    std::env::join_paths(dirs).unwrap()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a test fixture that cannot be built fails the test"
)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chug-agent-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn path_dir(root: &Path, name: &str, cli: Option<u32>) -> PathBuf {
        path_fixture(root, name, cli)
    }

    fn joined(dirs: &[PathBuf]) -> std::ffi::OsString {
        fixture_path(dirs)
    }

    /// The probe answers the question a launch asks: a `PATH` carrying an
    /// executable CLI resolves it, and one that does not reports absence — the
    /// `gumbo-air-0` measurement (#490 M3) is the second case.
    #[test]
    fn a_path_carrying_the_cli_resolves_it_and_one_without_reports_absence() {
        let root = temp_dir("resolves");
        let empty = path_dir(&root, "usr-bin", None);
        let with_cli = path_dir(&root, "local-bin", Some(0o755));

        let found = AgentCli::discover_on(Some(&joined(&[empty.clone(), with_cli.clone()])));
        assert!(found.present());
        assert_eq!(found.path(), Some(with_cli.join(AGENT_CLI_BIN).as_path()));

        let missing = AgentCli::discover_on(Some(&joined(std::slice::from_ref(&empty))));
        assert!(!missing.present());
        assert_eq!(missing.path(), None);
        let refusal = missing.missing("air");
        assert!(refusal.contains(AGENT_CLI_BIN), "{refusal}");
        assert!(refusal.contains(&empty.display().to_string()), "{refusal}");
    }

    /// Existence is not runnability (docs/reference/style.md Tier 2 rule 7): a
    /// non-executable file and a directory of that name are both absence, since
    /// neither is what the task's `exec` would find.
    #[test]
    fn a_present_but_unrunnable_candidate_is_absence() {
        let root = temp_dir("unrunnable");
        let not_executable = path_dir(&root, "no-x", Some(0o644));
        let as_directory = root.join("as-dir");
        std::fs::create_dir_all(as_directory.join(AGENT_CLI_BIN)).unwrap();

        #[cfg(unix)]
        assert!(
            !AgentCli::discover_on(Some(&joined(std::slice::from_ref(&not_executable)))).present(),
            "a file with no execute bit is not a CLI"
        );
        assert!(!AgentCli::discover_on(Some(&joined(&[as_directory]))).present());
        assert!(!AgentCli::discover_on(None).present(), "no PATH at all");
        assert!(!AgentCli::discover_on(Some(std::ffi::OsStr::new(""))).present());
        let _ = not_executable;
    }

    /// The scan is bounded, and the bound is not a silent truncation: what was
    /// read is what the refusal names.
    #[test]
    fn the_scan_stops_at_its_bound() {
        let root = temp_dir("bound");
        let mut dirs: Vec<PathBuf> = (0..PATH_ENTRIES_MAX)
            .map(|i| path_dir(&root, &format!("d{i}"), None))
            .collect();
        let past_the_bound = path_dir(&root, "past", Some(0o755));
        dirs.push(past_the_bound);

        let cli = AgentCli::discover_on(Some(&joined(&dirs)));
        assert!(!cli.present(), "an entry past the bound is not scanned");
        assert_eq!(cli.searched.len(), PATH_ENTRIES_MAX);
    }
}

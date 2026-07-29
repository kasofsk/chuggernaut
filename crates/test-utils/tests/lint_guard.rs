//! STYLE.md Tier 1 lint-gate guards — the clippy half of the invariants whose
//! dependency-graph half lives in `boundary_guard.rs` (refactor-plan A4).
//!
//! `clippy::too_many_lines`, `unwrap_used` and `expect_used` are all
//! allow-by-default, so they only reach `.chug/tasks/ci.sh`'s
//! `cargo clippy --workspace --all-targets -- -D warnings` through two pieces
//! of configuration that nothing else in the build would miss if they went
//! away: the `[workspace.lints.clippy]` table, and each member opting in with
//! `lints.workspace = true`. A crate added without that opt-in compiles,
//! tests, and lints clean while silently sitting outside the gate — which is
//! exactly the drift these tests exist to catch.
//!
//! They read the committed manifests as text rather than through `cargo
//! metadata`, because `cargo metadata` does not report the resolved lint
//! table and the opt-in is the thing under test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// STYLE.md Tier 1, "Function length: 70 lines".
const LINE_LIMIT: &str = "70";

/// The three denies STYLE.md Tier 1 requires the gate to enforce.
const TIER_ONE_LINTS: [&str; 3] = ["too_many_lines", "unwrap_used", "expect_used"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("test-utils sits at <root>/crates/test-utils")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = workspace_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The `members = [...]` paths from the root manifest.
fn workspace_members() -> Vec<String> {
    let manifest = read("Cargo.toml");
    let list = manifest
        .split_once("members = [")
        .expect("root Cargo.toml declares `members = [`")
        .1
        .split_once(']')
        .expect("`members` list is closed")
        .0;
    let members: Vec<String> = list
        .split(',')
        .map(|entry| entry.trim().trim_matches('"').to_owned())
        .filter(|entry| !entry.is_empty())
        .collect();
    assert!(!members.is_empty(), "no workspace members parsed");
    members
}

#[test]
fn clippy_toml_pins_the_style_line_limit() {
    let clippy_toml = read("clippy.toml");
    let configured = clippy_toml
        .lines()
        .filter_map(|line| line.trim().strip_prefix("too-many-lines-threshold"))
        .map(|value| value.trim_start_matches([' ', '=']).trim())
        .next()
        .expect("clippy.toml sets `too-many-lines-threshold`");
    assert_eq!(
        configured, LINE_LIMIT,
        "clippy.toml's `too-many-lines-threshold` must match STYLE.md Tier 1 \
         (70 lines); change both or neither"
    );
}

#[test]
fn workspace_denies_the_tier_one_lints() {
    let manifest = read("Cargo.toml");
    let table = manifest
        .split_once("[workspace.lints.clippy]")
        .expect("root Cargo.toml has a `[workspace.lints.clippy]` table")
        .1;
    // Stop at the next table header so a deny further down the file cannot
    // masquerade as one inside the lint table.
    let table = table.split_once("\n[").map_or(table, |(head, _)| head);
    for lint in TIER_ONE_LINTS {
        assert!(
            table.contains(&format!("{lint} = \"deny\"")),
            "[workspace.lints.clippy] must contain `{lint} = \"deny\"` — \
             STYLE.md Tier 1 is non-negotiable and the lint is allow-by-default"
        );
    }
}

#[test]
fn every_member_opts_into_the_workspace_lints() {
    let mut missing = Vec::new();
    for member in workspace_members() {
        let manifest = read(&format!("{member}/Cargo.toml"));
        let opts_in = manifest
            .split_once("[lints]")
            .is_some_and(|(_, rest)| rest.contains("workspace = true"));
        if !opts_in {
            missing.push(member);
        }
    }
    assert!(
        missing.is_empty(),
        "these members lack `[lints]\\nworkspace = true`, so the Tier 1 clippy \
         denies do not apply to them: {missing:?}"
    );
}

/// A crate-level `#![allow]` of any Tier 1 lint would silence the gate for a
/// whole crate. Violations are annotated per site instead, so the debt stays
/// greppable and new code cannot inherit an exemption. Test *targets* keep
/// their file-level allow — STYLE.md scopes the panic lints to "outside
/// tests" — so only `src/` is swept, and only at column zero, where a crate
/// root's inner attributes live.
#[test]
fn no_crate_level_allow_of_the_tier_one_lints() {
    let mut offenders = Vec::new();
    for member in workspace_members() {
        let src = workspace_root().join(&member).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for path in files {
            let source = std::fs::read_to_string(&path).expect("read crate source file");
            for (index, line) in source.lines().enumerate() {
                if line.starts_with("#![allow(") && TIER_ONE_LINTS.iter().any(|l| line.contains(l))
                {
                    offenders.push(format!("{}:{}", path.display(), index + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "crate-level allows of the Tier 1 lints defeat the ratchet — annotate \
         the specific violation sites instead: {offenders:?}"
    );
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

//! STYLE.md Tier 1 dependency-boundary guards, enforced on the existing
//! `cargo test --workspace` in `tasks/ci.sh` — same route as the
//! `committed_schemas_are_current` drift test (refactor-plan A3).
//!
//! The crate-graph rules are read straight off `cargo metadata`'s resolve
//! graph so they cannot drift from `Cargo.toml`:
//!
//! - only `store` depends on `async-nats` (it is the sole NATS crate);
//! - `api` never depends on `dispatcher` outside dev-deps;
//! - `types` stays sync — no `tokio`/`async-nats` anywhere in its subtree.
//!
//! The source-level `.await` guard pins `state.rs` pure (extends to the whole
//! domain crate once C1 lands).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("test-utils sits at <root>/crates/test-utils")
        .to_path_buf()
}

/// The resolve graph, keyed for the checks below: the set of workspace-member
/// package ids, a package-id → name map, and each node's resolved deps.
struct Graph {
    workspace: HashSet<String>,
    name_of: HashMap<String, String>,
    /// package id → its resolved deps, each `(pkg id, is_dev_only)`.
    deps_of: HashMap<String, Vec<(String, bool)>>,
}

fn load_graph() -> Graph {
    // Reuse the same cargo that launched the test; `cargo metadata` reads the
    // committed `Cargo.lock`, so the resolve graph is deterministic and needs
    // no network.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("run `cargo metadata`");
    assert!(
        out.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("parse cargo metadata json");

    let workspace: HashSet<String> = meta["workspace_members"]
        .as_array()
        .expect("workspace_members")
        .iter()
        .map(|v| v.as_str().expect("member id").to_string())
        .collect();

    let name_of: HashMap<String, String> = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| {
            (
                p["id"].as_str().expect("pkg id").to_string(),
                p["name"].as_str().expect("pkg name").to_string(),
            )
        })
        .collect();

    let deps_of: HashMap<String, Vec<(String, bool)>> = meta["resolve"]["nodes"]
        .as_array()
        .expect("resolve.nodes")
        .iter()
        .map(|n| {
            let id = n["id"].as_str().expect("node id").to_string();
            let deps = n["deps"]
                .as_array()
                .expect("node deps")
                .iter()
                .map(|d| {
                    let pkg = d["pkg"].as_str().expect("dep pkg").to_string();
                    // A dep is dev-only when every edge to it is `kind: "dev"`
                    // (normal/build edges carry `kind: null`/`"build"`).
                    let dev_only = d["dep_kinds"]
                        .as_array()
                        .expect("dep_kinds")
                        .iter()
                        .all(|k| k["kind"].as_str() == Some("dev"));
                    (pkg, dev_only)
                })
                .collect();
            (id, deps)
        })
        .collect();

    Graph {
        workspace,
        name_of,
        deps_of,
    }
}

impl Graph {
    fn id_of(&self, crate_name: &str) -> String {
        self.workspace
            .iter()
            .find(|id| self.name_of.get(*id).map(String::as_str) == Some(crate_name))
            .unwrap_or_else(|| panic!("workspace crate `{crate_name}` not found"))
            .clone()
    }

    /// Every package name reachable from `root` through the resolve graph,
    /// including `root` itself.
    fn subtree_names(&self, root: &str) -> HashSet<&str> {
        let mut seen = HashSet::new();
        let mut stack = vec![root.to_string()];
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            for (pkg, _dev) in self.deps_of.get(&cur).into_iter().flatten() {
                stack.push(pkg.clone());
            }
        }
        seen.iter()
            .filter_map(|id| self.name_of.get(id).map(String::as_str))
            .collect()
    }
}

/// STYLE.md Tier 1: `store` is the only crate that talks NATS. No other
/// workspace crate may list `async-nats` as a direct dependency.
///
/// The name spells the crate hyphenated, not as the `async_nats` import path,
/// so `topology_guard` (which forbids that identifier in test sources) reads
/// this file as clean — `boundary_guard` inspects the crate by name, it never
/// imports the client.
#[test]
fn only_store_depends_on_the_nats_crate() {
    let g = load_graph();
    let mut offenders = Vec::new();
    for id in &g.workspace {
        let name = &g.name_of[id];
        if name == "store" {
            continue;
        }
        for (pkg, _dev) in g.deps_of.get(id).into_iter().flatten() {
            if g.name_of.get(pkg).map(String::as_str) == Some("async-nats") {
                offenders.push(name.clone());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "only `store` may depend on async-nats (STYLE.md Tier 1), but these \
         workspace crates do: {offenders:?}"
    );
}

/// STYLE.md Tier 1: `api` never depends on `dispatcher` outside dev-deps.
#[test]
fn api_does_not_depend_on_dispatcher_outside_dev() {
    let g = load_graph();
    let api = g.id_of("api");
    for (pkg, dev_only) in g.deps_of.get(&api).into_iter().flatten() {
        if g.name_of.get(pkg).map(String::as_str) == Some("dispatcher") {
            assert!(
                *dev_only,
                "`api` must not depend on `dispatcher` outside dev-deps \
                 (STYLE.md Tier 1)"
            );
        }
    }
}

/// STYLE.md Tier 1: `types` stays sync — no async runtime in its subtree, so
/// neither `tokio` nor `async-nats` may appear anywhere it resolves to.
#[test]
fn types_subtree_is_sync() {
    let g = load_graph();
    let names = g.subtree_names(&g.id_of("types"));
    let mut offenders: Vec<&str> = ["tokio", "async-nats"]
        .into_iter()
        .filter(|forbidden| names.contains(*forbidden))
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "`types` must stay sync (STYLE.md Tier 1) but its dependency subtree \
         pulls in: {offenders:?}"
    );
}

/// STYLE.md Tier 1 / NORTH-STAR: the domain code is pure and synchronous.
/// `state.rs` is the transition table — it must contain zero `.await`.
/// (Extends to the whole `domain/` crate when refactor-plan C1 lands.)
#[test]
fn state_rs_has_zero_await() {
    let path = workspace_root().join("crates/dispatcher/src/state.rs");
    let src = std::fs::read_to_string(&path).expect("read state.rs");
    let offenders: Vec<usize> = src
        .lines()
        .enumerate()
        // Skip comment lines — the module's `//!` header names `.await` to
        // state the guarantee this test enforces.
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| line.contains(".await"))
        .map(|(i, _)| i + 1)
        .collect();
    assert!(
        offenders.is_empty(),
        "crates/dispatcher/src/state.rs must be pure/synchronous (zero \
         `.await`), but found `.await` on line(s): {offenders:?}"
    );
}

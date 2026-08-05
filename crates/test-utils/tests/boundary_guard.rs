//! docs/reference/style.md Tier 1 dependency-boundary guards, enforced on the existing
//! `cargo test --workspace` in `.chug/tasks/ci.sh` — same route as the
//! `committed_schemas_are_current` drift test (refactor-plan A3).
//!
//! The crate-graph rules are read straight off `cargo metadata`'s resolve
//! graph so they cannot drift from `Cargo.toml`:
//!
//! - only `store` depends on `async-nats` (it is the sole NATS crate);
//! - `api` never depends on `dispatcher` outside dev-deps;
//! - `types` stays sync — no `tokio`/`async-nats` anywhere in its subtree;
//! - `chuggernaut-domain` stays sync the same way (refactor-plan C1): the
//!   pure core cannot even *reach* a runtime, so purity holds by
//!   construction.
//! - `chuggernaut-platform-ops` — the first *context* crate (refactor-plan
//!   C9) — declares exactly the edges its charter allows, and never one back
//!   to `dispatcher`.
//!
//! The source-level `.await` guard sweeps every file of the domain crate.

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
        "only `store` may depend on async-nats (docs/reference/style.md Tier 1), but these \
         workspace crates do: {offenders:?}"
    );
}

/// docs/reference/style.md Tier 1: `api` never depends on `dispatcher` outside dev-deps.
#[test]
fn api_does_not_depend_on_dispatcher_outside_dev() {
    let g = load_graph();
    let api = g.id_of("api");
    for (pkg, dev_only) in g.deps_of.get(&api).into_iter().flatten() {
        if g.name_of.get(pkg).map(String::as_str) == Some("dispatcher") {
            assert!(
                *dev_only,
                "`api` must not depend on `dispatcher` outside dev-deps \
                 (docs/reference/style.md Tier 1)"
            );
        }
    }
}

/// docs/reference/style.md Tier 1: `types` stays sync — no async runtime in its subtree, so
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
        "`types` must stay sync (docs/reference/style.md Tier 1) but its dependency subtree \
         pulls in: {offenders:?}"
    );
}

/// docs/reference/style.md Tier 1 / NORTH-STAR §1: the domain crate's dependency subtree is
/// as sync as `types`' — purity by construction (refactor-plan C1): code that
/// cannot resolve `tokio` or `async-nats` cannot drift into I/O.
#[test]
fn domain_subtree_is_sync() {
    let g = load_graph();
    let names = g.subtree_names(&g.id_of("chuggernaut-domain"));
    let mut offenders: Vec<&str> = ["tokio", "async-nats", "store", "vcs", "auth"]
        .into_iter()
        .filter(|forbidden| names.contains(*forbidden))
        .collect();
    offenders.sort_unstable();
    assert!(
        offenders.is_empty(),
        "`chuggernaut-domain` must stay pure/sync (docs/reference/style.md Tier 1, \
         refactor-plan C1) but its dependency subtree pulls in: {offenders:?}"
    );
}

/// The workspace crates `crate_name` depends on directly, split by whether
/// every edge to them is dev-only. Third-party packages are filtered out: the
/// context rules below are about the shape of the *internal* crate graph.
fn workspace_deps(g: &Graph, crate_name: &str) -> (HashSet<String>, HashSet<String>) {
    let id = g.id_of(crate_name);
    let mut normal = HashSet::new();
    let mut dev = HashSet::new();
    for (pkg, dev_only) in g.deps_of.get(&id).into_iter().flatten() {
        if !g.workspace.contains(pkg) {
            continue;
        }
        let name = g.name_of[pkg].clone();
        if *dev_only {
            dev.insert(name)
        } else {
            normal.insert(name)
        };
    }
    (normal, dev)
}

/// NORTH-STAR §1 / refactor-plan C9: the platform-ops context is a **leaf** of
/// the internal crate graph. Its charter is the platform's own observability
/// and housekeeping, driven through ports it is handed — so it may depend on
/// the port crates and on nothing else, and above all not on `dispatcher`.
///
/// The allow-list is spelled out rather than merely forbidding `dispatcher`,
/// because the way this boundary actually erodes is by acquiring one more
/// "harmless" edge at a time until the context is a second lifecycle crate.
/// Widening it is a deliberate, reviewable edit — which is the point.
#[test]
fn platform_ops_declares_only_its_charter_edges() {
    const ALLOWED: [&str; 5] = ["types", "store", "vcs", "container", "agent"];

    let g = load_graph();
    let (normal, dev) = workspace_deps(&g, "chuggernaut-platform-ops");

    let mut unexpected: Vec<&str> = normal
        .iter()
        .map(String::as_str)
        .filter(|name| !ALLOWED.contains(name))
        .collect();
    unexpected.sort_unstable();
    assert!(
        unexpected.is_empty(),
        "`chuggernaut-platform-ops` may depend only on {ALLOWED:?} \
         (NORTH-STAR §1, refactor-plan C9) but also depends on: {unexpected:?}"
    );

    assert!(
        !dev.contains("dispatcher"),
        "`chuggernaut-platform-ops` must not depend on `dispatcher`, dev-deps \
         included — the context arrow points one way (refactor-plan C9)"
    );
}

/// The other half of the C9 seam: `dispatcher` depends on the context crate,
/// so anything that ever pointed the arrow the other way would be a cycle
/// cargo rejects. Asserting the edge exists keeps the two tests honest — a
/// `chuggernaut-platform-ops` that nothing consumed would pass the rule above
/// vacuously.
#[test]
fn dispatcher_consumes_the_platform_ops_context() {
    let g = load_graph();
    let (normal, _dev) = workspace_deps(&g, "dispatcher");
    assert!(
        normal.contains("chuggernaut-platform-ops"),
        "`dispatcher` must depend on the platform-ops context crate \
         (refactor-plan C9); found: {normal:?}"
    );
}

/// docs/reference/style.md Tier 1 / NORTH-STAR §1: the domain code is pure and synchronous —
/// zero `.await` in any source file of `crates/domain` (refactor-plan C1;
/// grew out of the pre-C1 `state.rs`-only guard).
#[test]
fn domain_crate_has_zero_await() {
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read domain src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let root = workspace_root().join("crates/domain/src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no .rs files under crates/domain/src — did the crate move?"
    );

    let mut offenders = Vec::new();
    for path in files {
        let src = std::fs::read_to_string(&path).expect("read domain source file");
        for (i, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains(".await") {
                offenders.push(format!("{}:{}", path.display(), i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "crates/domain must be pure/synchronous (zero `.await`), but found \
         `.await` at: {offenders:?}"
    );
}

/// Spec §14.3: the merge-time skew gate reads the repo and the binary's own
/// constant — never the platform API, a token, or an environment variable.
/// The three files implementing it are swept for the means to try, so it cannot
/// regain the fail-open the CI-side gate has (job #421).
#[test]
fn the_merge_time_skew_gate_consults_no_network() {
    const IMPLEMENTATION: [&str; 3] = [
        "crates/types/src/version.rs",
        "crates/domain/src/decide/merge_gate.rs",
        "crates/dispatcher/src/release.rs",
    ];
    const FORBIDDEN: [&str; 6] = [
        "CHUG_API_URL",
        "reqwest",
        "std::env",
        "env::var",
        "http://",
        "https://",
    ];

    let mut offenders = Vec::new();
    for relative in IMPLEMENTATION {
        let path = workspace_root().join(relative);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {relative}: {e} — did the skew gate move?"));
        for (i, line) in src.lines().enumerate() {
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    offenders.push(format!("{relative}:{}: {needle}", i + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the merge-time skew gate must compare the branch's config against this \
         binary's CONFIG_SCHEMA_EPOCH with no network and no environment \
         (spec §14.3), but found: {offenders:?}"
    );
}

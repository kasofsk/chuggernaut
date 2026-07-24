//! #206 acceptance guard: no test may talk NATS directly or hand-build
//! topology names. Namespacing lives in `store` — every bucket, stream,
//! object-store, and subject name a test touches must flow through a
//! `NatsStore` handle (namespaced or deliberately empty-prefix), never
//! through `async_nats` directly. A test that bypasses the store would run
//! green solo and collide on the communal gate server, resurrecting exactly
//! the cross-test interference #206 eliminated.

use std::path::{Path, PathBuf};

/// Substrings that mark a direct-NATS bypass in TEST code. `async_nats` is
/// the client crate itself; `jetstream::` catches re-exported paths.
const FORBIDDEN: &[&str] = &["async_nats", "jetstream::"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("test-utils sits at <root>/crates/test-utils")
        .to_path_buf()
}

#[test]
fn test_sources_never_use_nats_directly() {
    let crates_dir = workspace_root().join("crates");
    let mut offenders = Vec::new();
    for krate in std::fs::read_dir(&crates_dir).expect("crates dir") {
        let tests = krate.expect("dir entry").path().join("tests");
        let Ok(entries) = std::fs::read_dir(&tests) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            // The guard's own source holds the needles by necessity.
            if path.file_name().is_some_and(|n| n == "topology_guard.rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read test source");
            for needle in FORBIDDEN {
                for (i, line) in src.lines().enumerate() {
                    if line.contains(needle) && !line.trim_start().starts_with("//") {
                        offenders.push(format!(
                            "{}:{}: {}",
                            path.strip_prefix(&crates_dir).unwrap_or(&path).display(),
                            i + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "test sources must go through `store` (NatsStore), never async_nats \
         directly — the namespace prefix (#206) only exists inside store:\n{}",
        offenders.join("\n")
    );
}

//! Shared test support for the golden-trace fixtures (refactor-plan B3).
//!
//! [`assert_trace`] compares a captured [`dispatcher::trace::Trace`] against a
//! committed `tests/traces/<name>.yaml` fixture, regenerating it in place when
//! `UPDATE_TRACES=1` is set. Living test-side keeps the YAML dependency out of
//! the production crate — the recorder itself ([`dispatcher::trace`]) needs only
//! `serde`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::trace::TraceSink;

/// Directory holding the committed golden-trace fixtures, resolved from the
/// dispatcher crate root so the path is stable regardless of the cwd `cargo
/// test` runs from.
fn traces_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/traces")
}

/// Assert the sink's captured trace matches `tests/traces/<name>.yaml`.
///
/// With `UPDATE_TRACES=1` set the fixture is (re)written from the captured
/// trace instead of compared — the documented regeneration path. Otherwise a
/// missing fixture or any mismatch (an added, removed, or reordered transition
/// or effect) fails the test with a diff-friendly assertion.
pub fn assert_trace(sink: &TraceSink, name: &str) {
    let path = traces_dir().join(format!("{name}.yaml"));
    let actual = sink.snapshot();
    let actual_yaml = serde_yaml::to_string(&actual).expect("serialize trace to YAML");

    if std::env::var_os("UPDATE_TRACES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("create traces dir");
        std::fs::write(&path, &actual_yaml).expect("write golden trace");
        eprintln!("UPDATE_TRACES: wrote {}", path.display());
        return;
    }

    let expected_yaml = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden trace {}: {e}\nregenerate with: UPDATE_TRACES=1 cargo test -p dispatcher --test golden_traces",
            path.display()
        )
    });
    let expected: dispatcher::trace::Trace =
        serde_yaml::from_str(&expected_yaml).expect("parse golden trace fixture");

    assert_eq!(
        actual, expected,
        "golden trace mismatch for {name}.\n--- captured ---\n{actual_yaml}\n--- expected ---\n{expected_yaml}\nIf this change is intended, regenerate with: UPDATE_TRACES=1 cargo test -p dispatcher --test golden_traces",
    );
}

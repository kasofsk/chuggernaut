//! Shared test support for the dispatcher's integration tier.
//!
//! - [`assert_invariants`] / [`assert_invariants_of`] surface
//!   [`dispatcher::invariants::check_invariants`] failures after a `Core` message,
//!   naming the broken rule, the offending job, and the message that broke it
//!   (refactor-plan B1a). Every integration test that drives a `Core` calls one of
//!   them after every message, so state corruption surfaces at the message that
//!   introduced it rather than at a distant assert.
//! - [`assert_trace`] compares a captured [`dispatcher::trace::Trace`] against a
//!   committed `tests/traces/<name>.yaml` fixture, regenerating it in place when
//!   `UPDATE_TRACES=1` is set. Living test-side keeps the YAML dependency out of
//!   the production crate — the recorder itself ([`dispatcher::trace`]) needs
//!   only `serde`.
//!
//! ## Which of the two to call
//!
//! A test that keeps its `Core` in-process calls [`assert_invariants`], which
//! checks live state through [`dispatcher::core::Core::state`]. A test that hands
//! its `Core` to [`dispatcher::core::spawn`] no longer has one to check, so it
//! spawns through [`spawn_checked`] instead and calls [`assert_invariants_of`] on
//! the sink that comes back. That path is strictly stronger: the check runs inside
//! the actor after *every* message, including the container exits and scans no
//! test sends by hand.
//!
//! Draining the sink is a plain mutex read — deliberately **not** a round trip
//! through the actor, which would let it finish its post-message drains before the
//! test's next observation and so change the timing these tests pin.
//!
//! A spawned `Core` also handles messages no test call initiates — container
//! exits, scans, KV watches — and those land in the sink after the last call-site
//! drain. So every spawn-driven test ends with one more
//! [`assert_invariants_of`]: without it a breach recorded during the final
//! `wait_for_*` would be dropped when the sink goes out of scope. The tail check
//! is additive, never a substitute for the per-call ones — attribution to the
//! message that broke the state comes from [`Breach::message`], which the
//! per-call drains keep narrow.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use dispatcher::core::{Core, CoreHandle};
use dispatcher::invariants::{Breach, InvariantSink};
use dispatcher::trace::TraceSink;

/// [`dispatcher::core::spawn`] with the invariant check turned on: attach a fresh
/// log to `core`, start the actor, and hand back the handle plus the log to drain.
///
/// The one wiring point every spawn-driving test uses, so "the checker runs" is a
/// property of how the `Core` was started rather than something each call site has
/// to remember.
pub fn spawn_checked(mut core: Core) -> (CoreHandle, InvariantSink) {
    let sink = InvariantSink::new();
    core.attach_invariant_sink(sink.clone());
    (dispatcher::core::spawn(core), sink)
}

/// Assert every dispatcher data invariant holds against a `Core` the test owns
/// in-process (docs/reference/contracts.md §3,
/// docs/design/215-refactor-plan.md B1/B1a); call it after each `Core` call.
///
/// [`dispatcher::invariants::check_invariants`] is the single source of truth for
/// what "always/never" means; this is only the assertion hook.
pub fn assert_invariants(core: &Core) {
    report(vec![Breach {
        message: "the call just made",
        violations: dispatcher::invariants::check_invariants(&core.state()),
    }]);
}

/// Assert nothing has broken an invariant inside the actor since the last call —
/// the [`assert_invariants`] counterpart for a spawned `Core`. Drains `sink`, so a
/// single broken message fails once rather than failing every later assertion.
pub fn assert_invariants_of(sink: &InvariantSink) {
    report(sink.drain());
}

/// Fail naming the message, the broken rule, and the offending job or queue
/// entry — one line per violation. Kept apart from the two entry points so both
/// render a breach identically.
fn report(breaches: Vec<Breach>) {
    let lines: Vec<String> = breaches
        .iter()
        .flat_map(|b| {
            b.violations
                .iter()
                .map(move |v| format!("  after {}: {}: {}", b.message, v.invariant, v.detail))
        })
        .collect();
    assert!(
        lines.is_empty(),
        "{} dispatcher invariant violation(s):\n{}",
        lines.len(),
        lines.join("\n"),
    );
}

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

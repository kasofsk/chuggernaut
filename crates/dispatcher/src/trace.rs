//! Golden decision traces (refactor-plan B3, `contracts.md` "mining intent").
//!
//! A [`TraceSink`] is a test-only observability hook: a shared, cheaply-cloned
//! recorder a test attaches to a [`Core`](crate::core::Core) so every decision
//! the dispatcher makes is captured as structured data. It records the two
//! universal write funnels — `Core::set_state` (every §2.1 transition) and
//! `Core::publish` (every `job-events` effect) — plus the escalation task
//! writes, grouped into per-event *steps* the test delimits with
//! [`TraceSink::begin`]. The captured [`Trace`] serializes to the YAML fixtures
//! under `tests/traces/`, which pin behavior during Track C decider extraction:
//! a carved-out decider must reproduce the same `(transitions, effects)`, so the
//! trace is the contract the extraction preserves.
//!
//! - **Accepts:** nothing at construction — a test builds a `TraceSink`, hands a
//!   clone to `Core::attach_trace`, and marks step boundaries with `begin`.
//! - **Emits:** a [`Trace`] snapshot (`serde`-serializable) via
//!   [`TraceSink::snapshot`]; the comparison/regeneration against the on-disk
//!   fixture lives test-side (`tests/common`), so no YAML dependency reaches
//!   production.
//! - **Guarantees:** inert in production — a `Core` with no attached sink pays a
//!   single `Option` check per funnel and records nothing. The recorded data is
//!   deterministic: job seqs and states only, no timestamps or generated ids, so
//!   a committed fixture never flakes.
//! - **Spec:** §2.1 (transitions), §6.3 (the event trail); refactor-plan B3.
//!
//! ## Regenerating fixtures
//!
//! The comparison helper (`tests/common/mod.rs::assert_trace`) rewrites the
//! fixture in place when `UPDATE_TRACES=1` is set, so an intended behavior
//! change is re-baselined with:
//!
//! ```sh
//! UPDATE_TRACES=1 cargo test -p dispatcher --test golden_traces
//! ```
//!
//! Review the resulting diff like any other — an unexpected transition or effect
//! appearing there is the signal the trace exists to catch.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use types::JobState;

/// One §2.1 state transition the dispatcher performed — a single `set_state`
/// call, recorded as `{job, from, to}` (no timestamp: terminal stamps are
/// masked out so the fixture stays deterministic).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceTransition {
    pub job: u64,
    pub from: JobState,
    pub to: JobState,
}

/// One step of a scenario: the driving event plus the transitions and effects it
/// produced. Mirrors the B3 fixture shape `{incoming event, expected
/// transitions, expected effects}` — the initial state is the cumulative result
/// of the prior steps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceStep {
    /// The incoming event that drove this step (a `Core` call the test made),
    /// labelled by the test via [`TraceSink::begin`].
    pub event: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<TraceTransition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<String>,
}

/// A whole scenario's trace: the ordered steps a test drove. This is the unit
/// serialized to a `tests/traces/*.yaml` fixture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub steps: Vec<TraceStep>,
}

#[derive(Debug, Default)]
struct TraceInner {
    steps: Vec<TraceStep>,
}

/// A shared trace recorder handed to a `Core` under test. Clones share the
/// same log (an `Arc<Mutex<_>>`), so a test keeps a clone while the spawned
/// actor owns another and both write to one trace.
#[derive(Debug, Clone, Default)]
pub struct TraceSink(Arc<Mutex<TraceInner>>);

impl TraceSink {
    /// A fresh, empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a new step for the incoming event `event`. Every transition and
    /// effect recorded until the next `begin` attaches to this step. A test
    /// calls this immediately before the `Core` call it is pinning.
    pub fn begin(&self, event: impl Into<String>) {
        let mut inner = self.lock();
        inner.steps.push(TraceStep {
            event: event.into(),
            ..Default::default()
        });
    }

    /// Record a state transition. No-op when no step is open (e.g. transitions
    /// from restart reconciliation, before the test calls [`begin`](Self::begin)).
    pub(crate) fn transition(&self, job: u64, from: JobState, to: JobState) {
        let mut inner = self.lock();
        if let Some(step) = inner.steps.last_mut() {
            step.transitions.push(TraceTransition { job, from, to });
        }
    }

    /// Record an effect (a port action other than a transition: an event
    /// publish, a task write). No-op when no step is open.
    pub(crate) fn effect(&self, effect: impl Into<String>) {
        let mut inner = self.lock();
        if let Some(step) = inner.steps.last_mut() {
            step.effects.push(effect.into());
        }
    }

    /// A snapshot of the trace so far, for the comparison helper.
    pub fn snapshot(&self) -> Trace {
        Trace {
            steps: self.lock().steps.clone(),
        }
    }

    /// Lock the shared log. A poisoned mutex means a test thread panicked mid
    /// record; recovering the guard lets the panic surface as the real failure
    /// rather than a lock-poison red herring.
    fn lock(&self) -> std::sync::MutexGuard<'_, TraceInner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! The recorder is plain in-memory data (no NATS/Docker), so its grouping
    //! and no-op-without-a-step contract are unit-testable directly.
    use super::*;

    #[test]
    fn groups_records_under_the_open_step() {
        let sink = TraceSink::new();
        sink.begin("release_job build");
        sink.transition(1, JobState::Frozen, JobState::Ready);
        sink.effect("PublishEvent job-released");
        sink.begin("release_job deploy");
        sink.transition(2, JobState::Frozen, JobState::Blocked);

        let trace = sink.snapshot();
        assert_eq!(trace.steps.len(), 2);
        assert_eq!(trace.steps[0].event, "release_job build");
        assert_eq!(trace.steps[0].transitions.len(), 1);
        assert_eq!(trace.steps[0].effects, vec!["PublishEvent job-released"]);
        assert_eq!(trace.steps[1].transitions[0].to, JobState::Blocked);
        assert!(trace.steps[1].effects.is_empty());
    }

    #[test]
    fn records_before_any_step_are_dropped() {
        let sink = TraceSink::new();
        sink.transition(9, JobState::Ready, JobState::Work);
        sink.effect("PublishEvent job-created");
        assert!(sink.snapshot().steps.is_empty());
    }
}

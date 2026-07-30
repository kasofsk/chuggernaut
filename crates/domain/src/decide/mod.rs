//! The decider layer (refactor-plan C1, `contracts.md` §2, NORTH-STAR §1) —
//! one module per lifecycle phase, each a pure function
//! `decide(view, event) -> (Vec<Transition>, Vec<Effect>)`.
//!
//! A decider takes a **read-only view** of the relevant job/graph state plus
//! the driving event, and returns values: the §2.1 transitions to apply and
//! the effects to run. It never performs an effect (STYLE.md Tier 2 #1) and
//! never holds `&mut Core`. The dispatcher's shim is the fixed four-step
//! shape every phase repeats:
//!
//! 1. gather the read inputs into the view (`next_task_id`, the clock, the
//!    target job) — reads feed the view, they are not effects;
//! 2. call the decider;
//! 3. apply each [`Transition`] via `Core::set_state` (the one §2.1 funnel:
//!    `assert_transition`, trace, `jobs.put`, graph);
//! 4. run each [`Effect`](crate::Effect) via `Core::interpret`.
//!
//! Transitions apply **before** effects: the state flip is the decision
//! committed to the record; tasks and events are its downstream artifacts,
//! and restart reconciliation re-derives any artifact a crash between the
//! two writes lost (the escalation heal in the dispatcher's `reconcile`).
//!
//! [`escalation`] is the worked template (refactor-plan C1) every later
//! phase decider — [`merge_gate`], [`wrapup`], [`ready`], [`eval`], `work` —
//! copies.
//!
//! - **Accepts:** a phase view (read-only borrows + pre-read scalars) and an
//!   event.
//! - **Emits:** `(Vec<Transition>, Vec<Effect>)` — values only.
//! - **Guarantees:** pure and synchronous; asserts liberally (STYLE.md Tier 2
//!   #2) on argument shape and postconditions.
//! - **Spec:** §2.1 (transitions), §3 (the decisions); `contracts.md` §2.

use types::{Job, JobState};

pub mod authoring;
pub mod escalation;
pub mod eval;
pub mod merge_gate;
pub mod ready;
pub mod work;
pub mod wrapup;

/// One §2.1 state change a decider decided: the job record to persist (with
/// any decision fields already stamped on it) and the target state. The shim
/// applies it through `Core::set_state`, so it still passes
/// [`assert_transition`](crate::state::assert_transition) and the golden-trace
/// transition funnel — a decider cannot smuggle a write past either.
#[derive(Debug)]
pub struct Transition {
    /// The job as it must be persisted, minus the state change itself —
    /// boxed for the same reason [`Effect`](crate::Effect) boxes jobs.
    pub job: Box<Job>,
    /// The §2.1 target state.
    pub to: JobState,
}

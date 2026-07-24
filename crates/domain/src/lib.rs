//! The pure domain — Chuggernaut's functional core (NORTH-STAR §1, `contracts.md`).
//!
//! This crate is the "functional core" half of a functional-core / imperative-shell
//! split: the dispatcher (`crates/dispatcher`) is the shell that owns I/O and the
//! single-writer loop; everything here is pure, synchronous, value-in/value-out
//! code with no `tokio`, `async-nats`, `store`, `vcs`, or `auth` dependency.
//! Purity is enforced *by construction* — the crate cannot reach a runtime — and
//! machine-checked by `test-utils/tests/boundary_guard.rs`
//! (`domain_subtree_is_sync` + the zero-`.await` sweep over `src/`).
//!
//! # The decider template (refactor-plan C1)
//!
//! A **decider** is a pure function that makes one lifecycle phase's decision:
//!
//! ```text
//! decide_<phase>(view of state, event) -> (Vec<Transition>, Vec<Effect>)
//! ```
//!
//! It takes a **read-only view** of the relevant job/graph state (never `&mut
//! Core`) plus the driving event, and returns the [`Transition`]s to apply and
//! the [`Effect`]s to run — it **never performs an effect itself** (STYLE.md
//! Tier 2 #1). The dispatcher's imperative shell does the rest, and its call
//! site shrinks to a fixed shape:
//!
//! 1. gather the read inputs (`next_task_id`, `now`, the target job) — reads
//!    feed the view, they are not effects;
//! 2. call the decider;
//! 3. apply each [`Transition`] via `Core::set_state`;
//! 4. run each [`Effect`] via `Core::interpret`.
//!
//! [`decide::escalation::decide`] is the worked template every later
//! phase-decider (`merge_gate`, `wrapup`, `ready`, `eval`, `work`) copies. Its
//! "view" is the narrowest honest borrow — `&Job`, the target job — and wider
//! phases grow a wider view from that same seam.
//!
//! Deciders assert liberally (STYLE.md Tier 2 #2): arguments, postconditions,
//! and negative space (e.g. never escalate an already-terminal job).

pub mod decide;
pub mod effects;
pub mod graph;
pub mod queue;
pub mod release;
pub mod state;

pub use effects::Effect;

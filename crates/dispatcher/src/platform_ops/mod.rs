//! **platform-ops** — the bounded context that keeps the *platform itself*
//! observable and tidy, as distinct from driving any one job's lifecycle
//! (NORTH-STAR §1, refactor-plan C8).
//!
//! Its members share a charter: they answer "what does the fleet look like
//! right now, and what did a finished container leave behind" — the operator's
//! view of the machine, plus the housekeeping that stops it filling up. None of
//! them decides a state transition; [`cd`] and [`fleet`] publish snapshots the
//! UI reads, [`harvest`] copies artifacts out and reclaims disk, and [`seed`]
//! is the starter template baked into the binary. That is why they sit outside
//! the §2.1 lifecycle modules: a bug here degrades visibility or disk, never
//! job correctness.
//!
//! The context is a compile/visibility boundary only — it is not a second
//! writer and not a second process. [`cd`] and [`fleet`] run on the actor
//! thread as `impl Core` slices (the single writer publishes); [`harvest`] runs
//! off it on cloned handles precisely so it can write no state.
//!
//! - **Accepts:** the periodic scan tick, task launch/exit events, and exited
//!   containers.
//! - **Emits:** `platform`-bucket KV snapshots (config/deploy drift, fleet
//!   occupancy), harvested artifacts back to the actor, and the embedded
//!   project starter template.
//! - **Guarantees:** no member drives a job transition; snapshots are rebuilt
//!   from live state rather than bookkeeping, and are written only when their
//!   bytes change.
//! - **Spec:** §3.1, §3.2, §3.6, §12.2; CD plan C.

pub mod cd;
pub mod fleet;
pub(crate) mod harvest;
pub mod seed;

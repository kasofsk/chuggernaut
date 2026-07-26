//! **platform-ops** — the bounded context that keeps the *platform itself*
//! observable and tidy, as distinct from driving any one job's lifecycle
//! (NORTH-STAR §1, refactor-plan C8/C9).
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
//! C8 gave the context a directory inside `dispatcher`; C9 gave it this crate.
//! The graduation rule (refactor-plan, "no _speculative_ crate splits") is that
//! a boundary becomes a crate once its interface no longer needs `&mut Core` —
//! so nothing here takes one. What the context still needs from the
//! single-writer loop is named explicitly and narrowly:
//!
//! - the ports it drives, taken as arguments (`ContainerBackend`, `NatsStore`,
//!   `TaskStore`, `RepoManager`, `ArtifactStore`);
//! - [`fleet::FleetView`] — the roster, the launch-queue depth, and a
//!   [`fleet::JobLookup`] the caller implements over its in-memory graphs;
//! - [`cd::ConfigSnapshot`] — republish state the caller owns and lends back
//!   for the duration of one refresh.
//!
//! The crate is a compile/visibility boundary only — not a second writer and
//! not a second process. [`cd`] and [`fleet`] are called on the actor thread
//! (the single writer publishes); [`harvest`] is called off it on cloned
//! handles precisely so it can write no state. Neither writes a job or task
//! record: the KV this context owns is the `platform` bucket's two snapshot
//! keys plus the artifact object store.
//!
//! - **Accepts:** the periodic scan tick, task launch/exit events, and exited
//!   containers.
//! - **Emits:** `platform`-bucket KV snapshots (config/deploy drift, fleet
//!   occupancy), harvested artifacts back to the caller, and the embedded
//!   project starter template.
//! - **Guarantees:** no member drives a job transition and none takes
//!   `&mut Core`; snapshots are rebuilt from live state rather than
//!   bookkeeping, and are written only when their bytes change.
//! - **Spec:** §3.1, §3.2, §3.6, §12.2; CD plan C.

pub mod cd;
pub mod fleet;
pub mod harvest;
pub mod seed;

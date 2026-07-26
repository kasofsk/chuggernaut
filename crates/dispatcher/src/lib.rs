//! The core orchestrator (spec Parts 2, 3, 13).
//!
//! Single-writer by construction: `core` owns all mutable state inside one tokio
//! task; handlers, container monitors, and scan timers communicate with it only
//! via an mpsc channel. There is no lock to misuse because there is no shared
//! mutable state.
//!
//! Every module here opens with a contract-style header (accepts / emits /
//! guarantees / spec §); `MODULES.md` at the repo root is the one-line
//! registry of these scoping-eligible modules.
//!
//! Two of them are **named contexts** rather than single modules
//! (NORTH-STAR §1): [`platform_ops`] (fleet/CD/harvest/seed — the platform's
//! own observability and housekeeping) and [`forge_ingest`] (origin/GitHub/
//! triage — where work crosses the platform's edge). Each carries the same
//! contract header for the context as a whole.

pub mod channel;
pub mod config;
pub mod core;
pub mod eval;
pub mod exec;
pub mod forge_ingest;
pub mod handlers;
pub mod interpret;
pub mod invariants;
pub mod launch_queue;
pub mod platform_ops;
pub mod ready;
pub mod reconcile;
pub mod release;
pub mod run;
pub mod scan;
pub mod trace;

// The pure domain (refactor-plan C1) lives in `chuggernaut-domain`; re-exported
// here so existing `crate::{state,graph,queue,effects,escalation}::*` call
// sites stay stable. (`escalation` moved under the decider layer.)
pub use chuggernaut_domain::decide::escalation;
pub use chuggernaut_domain::{decide, effects, graph, queue, state};

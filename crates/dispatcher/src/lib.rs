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
//! (NORTH-STAR §1): platform-ops (fleet/CD/harvest/seed — the platform's own
//! observability and housekeeping) and [`forge_ingest`] (origin/GitHub/triage —
//! where work crosses the platform's edge). Each carries the same contract
//! header for the context as a whole. The first has since graduated to its own
//! crate (`chuggernaut-platform-ops`, refactor-plan C9) — it needed no
//! `&mut Core` — leaving [`platform_ops`] here as the adapter that lends it the
//! views it does take.

pub mod capacity;
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
pub mod project_config;
pub mod ready;
pub mod reconcile;
pub mod release;
pub mod run;
pub mod scan;
pub mod schedules;
pub mod trace;
pub mod workload;

pub use chuggernaut_domain::decide::escalation;
pub use chuggernaut_domain::{decide, effects, graph, inputs, queue, state};

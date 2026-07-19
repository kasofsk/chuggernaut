//! The core orchestrator (spec Parts 2, 3, 13).
//!
//! Single-writer by construction: `core` owns all mutable state inside one tokio
//! task; handlers, container monitors, and scan timers communicate with it only
//! via an mpsc channel. There is no lock to misuse because there is no shared
//! mutable state.

pub mod channel;
pub mod config;
pub mod core;
pub mod escalation;
pub mod eval;
pub mod exec;
pub mod factory;
pub mod graph;
pub mod handlers;
pub(crate) mod harvest;
pub mod launch;
pub mod queue;
pub mod reconcile;
pub mod release;
pub mod run;
pub mod scan;
pub mod seed;
pub mod state;

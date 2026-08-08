//! Worker-node execution (spec §3.1): the `chuggernaut worker` daemon runs on
//! a fleet node, dials OUT to NATS, and executes container operations against
//! its local Docker socket — no Docker endpoint is ever exposed on a network.
//! [`backend::FleetBackend`] is the dispatcher-side counterpart: a
//! [`container::ContainerBackend`] that proxies ops to worker nodes over
//! per-node request-reply subjects and drives docker-endpoint nodes directly.
//!
//! Launch requests are small messages by design: static artifacts (channel
//! binary, agent images) are provisioned node-locally at deploy time and
//! referenced by name; see `types::worker`.

pub mod backend;
pub mod capacity;
pub mod config;
pub mod daemon;
pub mod nix;
pub mod route;
pub mod xcode;

pub use backend::FleetBackend;
pub use config::{WorkerConfig, WorkerMode};
pub use daemon::run;

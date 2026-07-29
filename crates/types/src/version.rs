//! Cross-service wire-surface versions and the schema-evolution contract
//! (spec §14 "Config & version skew").
//!
//! With auto-deploy (#108) every rollout has a mixed-version window: workers
//! refresh and the dispatcher restarts from the *same* SHA, but not
//! atomically. Job-type config is read *live* from the default branch (a
//! per-consumer forge, repo-versioned by design), so config can also move
//! ahead of the running binary the moment a config change merges — which is
//! exactly what burned every `web` job on 2026-07-22 (#63 added a `wrap_up`
//! section the running dispatcher's strict parser rejected; first victim #69).
//!
//! The contract these constants anchor:
//!
//! 1. **One SHA per deploy.** All services build from a single git SHA; there
//!    are no independently-pinned per-service versions. Skew is therefore
//!    bounded to *one deploy generation* (the update.sh leg order — workers,
//!    then dispatcher).
//!
//! 2. **Every cross-service wire surface tolerates N±1 skew.** The worker RPC
//!    ([`crate::worker`] ops), the channel protocol, and the job-type config
//!    schema must each survive one generation of skew in either direction:
//!    changes are *additive-only*, and an unknown op / unknown field degrades
//!    gracefully (log + fallback) instead of crashing or escalating. The
//!    2026-07-22 `logs_tail` (unknown worker op) and `wrap_up` (unknown config
//!    field) incidents are the counterexamples this rule exists to prevent.
//!
//! 3. **A change that cannot satisfy N-1 compat must say so and fail CI.** Bump
//!    the relevant version constant *in the same commit* as a breaking change;
//!    the merge-time check (`chuggernaut validate --deployed-epoch`, wired in
//!    `.chug/tasks/ci.sh`) compares a config's declared [`JobType::min_dispatcher`]
//!    against the *deployed* dispatcher's [`CONFIG_SCHEMA_EPOCH`] and fails the
//!    config's own CI with "requires dispatcher >= X; deploy first or gate it"
//!    rather than merging a time bomb.
//!
//! Each constant is a single monotonically-increasing integer. Additive changes
//! (a new optional config field, a new worker op the old side can ignore) do
//! **not** bump it — the whole point is that the old side tolerates them. Bump
//! only for a change the previous generation genuinely cannot handle, and only
//! alongside the code that makes the new side reject the old.

/// The job-type YAML schema epoch the running dispatcher understands (spec
/// §14). A job type may declare [`crate::JobType::min_dispatcher`]; when that
/// exceeds this epoch the config is ahead of the binary and the job parks with
/// a platform-level diagnostic instead of being launched (see the dispatcher
/// launch path) — and its own CI fails at merge time if it can reach a
/// deployed dispatcher advertising an older epoch.
///
/// Bumped only when the schema gains a field the *previous* dispatcher
/// generation cannot safely ignore. Tolerated additive changes (a new optional
/// top-level field, ignored-with-a-warning by older binaries) do not bump it.
pub const CONFIG_SCHEMA_EPOCH: u32 = 1;

/// The worker-node RPC protocol version ([`crate::worker`] ops, spec §3.1).
/// The daemon logs-and-fallbacks on an unknown op rather than crashing, so an
/// additive op does not bump this; a breaking change to an existing op's shape
/// does, in the same commit.
pub const WORKER_RPC_VERSION: u32 = 1;

/// The channel MCP ↔ dispatcher protocol version (spec §4.2). Additive request
/// kinds degrade gracefully on the old side; bump only for a breaking change.
pub const CHANNEL_PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn epochs_are_positive() {
        // A guard so a future edit that zeroes one of these is caught: the
        // deployed-version contract assumes positive, comparable epochs. Held
        // in a runtime slice so this is a real assertion, not a const one.
        for (name, epoch) in [
            ("config", CONFIG_SCHEMA_EPOCH),
            ("worker-rpc", WORKER_RPC_VERSION),
            ("channel", CHANNEL_PROTOCOL_VERSION),
        ] {
            assert!(epoch >= 1, "{name} epoch must be >= 1");
        }
    }
}

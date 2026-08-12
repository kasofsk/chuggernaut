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
//! 3. **A change that cannot satisfy N-1 compat must say so and be refused at
//!    merge.** Bump the relevant version constant *in the same commit* as a
//!    breaking change; the dispatcher then refuses to merge a branch whose
//!    config declares a [`JobType::min_dispatcher`] above the epoch it runs
//!    (spec §14.3), because it performs the merge and needs no API call to know
//!    its own. `.chug/tasks/ci.sh`'s gate and `chuggernaut validate
//!    --deployed-epoch` run the same comparison earlier as an advisory signal.
//!
//! Each constant is a single monotonically-increasing integer. Additive changes
//! (a new optional config field, a new worker op the old side can ignore) do
//! **not** bump it — the whole point is that the old side tolerates them. Bump
//! only for a change the previous generation genuinely cannot handle, and only
//! alongside the code that makes the new side reject the old.

/// The job-type YAML schema epoch the running dispatcher understands (spec
/// §14): a config declaring a higher [`crate::JobType::min_dispatcher`] is
/// ahead of this binary, so its job parks pre-Work (§14.2) and its branch is
/// refused at the merge gate (§14.3).
///
/// Bumped only when the schema gains a field the *previous* dispatcher
/// generation cannot safely ignore — never for a tolerated additive change, and
/// the per-feature constants below are where a reader finds what each epoch
/// bought.
pub const CONFIG_SCHEMA_EPOCH: u32 = 7;

/// The epoch at which job `inputs:` landed (#311, spec §1.1). A job type
/// declaring a non-empty `inputs:` must declare `min_dispatcher` at least this
/// high — [`crate::JobType::validate`] enforces it, so an author cannot omit the
/// declaration that makes the skew legible to a dispatcher which cannot see
/// `inputs:` at all.
///
/// Deliberately its own constant rather than a read of [`CONFIG_SCHEMA_EPOCH`]:
/// it is frozen at the epoch the feature shipped, so a later bump for an
/// unrelated feature does not retroactively raise what an existing `inputs:`
/// config has to declare.
pub const INPUTS_SCHEMA_EPOCH: u32 = 2;

/// The epoch at which a **schedule's** `inputs:` landed (#311 slice C, spec
/// §1.1): a schedule supplying a non-empty map must declare `min_dispatcher` at
/// least this high ([`crate::Schedule::validate`]), or a dispatcher that cannot
/// see the field fires the occurrence with the values dropped.
///
/// Above [`INPUTS_SCHEMA_EPOCH`] rather than a reuse of it — a dispatcher at
/// epoch 2 understands a job type's `inputs:` and still drops a schedule's.
pub const SCHEDULE_INPUTS_SCHEMA_EPOCH: u32 = 3;

/// The epoch at which the `runtime:` block landed (#309 §3, #373 Decision 2,
/// spec §1.1). Any `runtime:` beyond a bare `mode: container` must declare
/// `min_dispatcher` at least this high ([`crate::JobType::validate`]), because
/// an N-1 dispatcher tolerates the whole unknown block, keeps the still-present
/// `image`, and would run the job containerized against the image's toolchain.
pub const RUNTIME_SCHEMA_EPOCH: u32 = 4;

/// The epoch at which `workload_identities:` landed (design #313 A5, spec
/// §1.1): a container block declaring one must declare `min_dispatcher` at
/// least this high ([`crate::JobType::validate`]), because the nested blocks
/// carry `deny_unknown_fields` and an N-1 dispatcher rejects the whole config
/// and parks every job of the type (§14.2) instead of dropping the field.
/// Frozen at the epoch the feature shipped, like the three constants above, so
/// a later bump for an unrelated feature never retroactively raises what an
/// existing declaration must carry.
pub const WORKLOAD_IDENTITY_SCHEMA_EPOCH: u32 = 5;

/// The epoch at which the per-agent `tools:` grant landed (design #533 S1, spec
/// §1.1): an agent block declaring one must declare `min_dispatcher` at least
/// this high ([`crate::JobType::validate`]), for the same
/// `deny_unknown_fields` reason [`WORKLOAD_IDENTITY_SCHEMA_EPOCH`] exists.
/// Frozen at the epoch the feature shipped, like the four constants above.
pub const TOOLS_SCHEMA_EPOCH: u32 = 6;

/// The epoch at which per-level `secret_files:` landed (design #529 S3, spec
/// §1.1, §8.2): a level declaring one must declare `min_dispatcher` at least
/// this high ([`crate::JobType::validate`]), for the same `deny_unknown_fields`
/// reason [`WORKLOAD_IDENTITY_SCHEMA_EPOCH`] exists — an N-1 dispatcher rejects
/// the whole config rather than dropping the field, and a dispatcher that did
/// drop it would deliver the secret by the env the declaration exists to avoid.
/// Frozen at the epoch the feature shipped, like the five constants above.
pub const SECRET_FILES_SCHEMA_EPOCH: u32 = 7;

/// A config file that requires a newer dispatcher than the one reading it
/// (spec §14.2): where the file was found, the epoch it declares, and the
/// epoch running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSkew {
    pub path: String,
    pub needed: u32,
    pub running: u32,
}

/// The `min_dispatcher` a config file declares (spec §14.2), `None` when it
/// declares none or is no YAML mapping at all.
///
/// Deliberately not a [`crate::JobType`] or [`crate::Schedule`] parse — a config
/// ahead of this binary is exactly the file a strict parse rejects, and
/// `min_dispatcher` is top-level in both shapes, so one tolerant probe serves
/// both.
pub fn declared_min_dispatcher(yaml: &str) -> Option<u32> {
    #[derive(serde::Deserialize)]
    struct Declaration {
        #[serde(default)]
        min_dispatcher: Option<u32>,
    }
    serde_yaml::from_str::<Declaration>(yaml)
        .ok()
        .and_then(|d| d.min_dispatcher)
}

/// The epoch `yaml` needs when it declares one above `dispatcher_epoch` — the
/// file-text mirror of [`crate::JobType::requires_dispatcher`], for a caller
/// holding config text it must not parse strictly (spec §14.2).
pub fn config_requires_dispatcher(yaml: &str, dispatcher_epoch: u32) -> Option<u32> {
    declared_min_dispatcher(yaml).filter(|&need| need > dispatcher_epoch)
}

/// The worker-node RPC protocol version ([`crate::worker`] ops, spec §3.1).
/// The daemon logs-and-fallbacks on an unknown op rather than crashing, so an
/// additive op does not bump this; a breaking change to an existing op's shape
/// does, in the same commit — **2** is design #309 P1 making
/// [`crate::worker::WorkerLaunchRequest::image`] optional, which a v1 daemon
/// rejects as an unparseable payload.
pub const WORKER_RPC_VERSION: u32 = 2;

/// The channel MCP ↔ dispatcher protocol version (spec §4.2). Additive request
/// kinds degrade gracefully on the old side; bump only for a breaking change.
pub const CHANNEL_PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn epochs_are_positive() {
        for (name, epoch) in [
            ("config", CONFIG_SCHEMA_EPOCH),
            ("worker-rpc", WORKER_RPC_VERSION),
            ("channel", CHANNEL_PROTOCOL_VERSION),
        ] {
            assert!(epoch >= 1, "{name} epoch must be >= 1");
        }
    }

    const JOB_TYPE: &str =
        "name: deploy\nimage: img:latest\nwork:\n  type: command\n  run: ./go.sh\n";
    const SCHEDULE: &str = "name: nightly\njob_type: deploy\ncron: '0 2 * * *'\n";

    fn declaring(yaml: &str, epoch: u32) -> String {
        format!("{yaml}min_dispatcher: {epoch}\n")
    }

    /// The comparison the merge-time gate runs (spec §14.3): only a
    /// declaration *above* the running epoch is skew, and a schedule file is
    /// read exactly like a job type.
    #[test]
    fn config_skew_is_a_declaration_above_the_running_epoch() {
        for file in [JOB_TYPE, SCHEDULE] {
            assert_eq!(config_requires_dispatcher(&declaring(file, 6), 5), Some(6));
            assert_eq!(config_requires_dispatcher(&declaring(file, 5), 5), None);
            assert_eq!(config_requires_dispatcher(&declaring(file, 4), 5), None);
            assert_eq!(config_requires_dispatcher(file, 5), None);
        }
    }

    /// The probe must read the declaration off a config the strict parsers
    /// reject — an unknown top-level field, an unknown key inside a nested
    /// block — because that is precisely the file it exists to catch.
    #[test]
    fn the_declaration_reads_off_a_config_this_binary_cannot_parse() {
        let ahead = "name: deploy\nimage: img:latest\nmin_dispatcher: 9\n\
                     work:\n  type: command\n  run: ./go.sh\n  teleport: yes\nfuture_block:\n  a: 1\n";
        assert!(crate::JobType::parse(ahead).is_err());
        assert_eq!(declared_min_dispatcher(ahead), Some(9));
        assert_eq!(
            config_requires_dispatcher(ahead, CONFIG_SCHEMA_EPOCH),
            Some(9)
        );
    }

    /// Negative space: a file that is no YAML mapping declares nothing rather
    /// than reporting a bogus epoch.
    #[test]
    fn a_non_mapping_file_declares_nothing() {
        assert_eq!(declared_min_dispatcher("- one\n- two\n"), None);
        assert_eq!(declared_min_dispatcher("min_dispatcher: soon\n"), None);
        assert_eq!(declared_min_dispatcher(""), None);
    }

    #[test]
    fn feature_epochs_are_understood_by_this_binary() {
        let feature_epochs: Vec<(&str, u32)> = vec![
            ("inputs", INPUTS_SCHEMA_EPOCH),
            ("schedule-inputs", SCHEDULE_INPUTS_SCHEMA_EPOCH),
            ("runtime", RUNTIME_SCHEMA_EPOCH),
            ("workload-identity", WORKLOAD_IDENTITY_SCHEMA_EPOCH),
            ("tools", TOOLS_SCHEMA_EPOCH),
            ("secret-files", SECRET_FILES_SCHEMA_EPOCH),
        ];
        for (name, epoch) in feature_epochs {
            assert!(
                epoch <= CONFIG_SCHEMA_EPOCH,
                "{name} epoch {epoch} is ahead of this binary's config epoch {CONFIG_SCHEMA_EPOCH}"
            );
        }
    }
}

//! The design #309 §7 and #543 D2 placement predicates against **this repo's
//! own** `.chug/jobs/`, on the same `cargo test --workspace` route as the other
//! guards here: every job type in the tree is placed exactly as it was before
//! either predicate existed.
//!
//! §7 claims the requirement is "a no-op for the container fleet and for every
//! job type in `.chug/jobs/` today", and #543 §5 claims the same of the `envs`
//! match. This measures both claims rather than repeating them: each level of
//! each job type is placed twice on a fleet shaped like the live one — once
//! carrying the limits and environment the level resolves to, once carrying
//! neither — and the two answers must be identical.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use container::{
    CONTAINER_ONLY_MODES, LaunchRequirements, NO_ENVS, NodeLoad, PlacementCandidate,
    PlacementPolicy, choose_placement,
};
use std::path::{Path, PathBuf};
use types::JobType;
use types::job_type::{Level, RuntimeMode};

fn jobs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("test-utils sits at <root>/crates/test-utils")
        .join(".chug/jobs")
}

/// Every job type the repo declares, `_defaults.yaml` aside — it is a defaults
/// document rather than a job type, and the `ci` evaluator it appends carries
/// its own image and no resources of its own.
fn job_types() -> Vec<(String, JobType)> {
    let mut found: Vec<(String, JobType)> = std::fs::read_dir(jobs_dir())
        .expect("read .chug/jobs")
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .filter(|p| p.file_name().is_some_and(|n| n != "_defaults.yaml"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let yaml = std::fs::read_to_string(&p).unwrap();
            let parsed =
                JobType::parse(&yaml).unwrap_or_else(|e| panic!("parse .chug/jobs/{name}: {e}"));
            (name, parsed)
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(found.len() > 5, "the repo declares its own job types");
    found
}

const HOST_AND_CONTAINER: &[RuntimeMode] = &[RuntimeMode::Container, RuntimeMode::Host];

/// What `gumbo-air-0` advertises in `NodeCapabilities.envs` (job #489), and what
/// `mac-proof.yaml` declares as its `runtime.env` — the live pair design #543 D2
/// matches.
const AIR_ENVS: &[&str] = &["xcode:26.5"];

/// The live fleet's shape (`deploy/prod/README.md`): docker-endpoint and
/// container-only worker nodes, plus the one dual-mode Mac, which advertises
/// `resources_enforced: true` because it has a docker daemon and its discovered
/// Xcode set in `envs`. `air` is a real node name here because `mac-proof.yaml`
/// pins to it.
fn fleet(air_envs: &[String]) -> [PlacementCandidate<'_>; 3] {
    let load = |running, free| Some(NodeLoad { running, free });
    [
        PlacementCandidate {
            index: 0,
            name: "mini",
            load: load(0, 2),
            modes: CONTAINER_ONLY_MODES,
            resources_enforced: true,
            envs: NO_ENVS,
        },
        PlacementCandidate {
            index: 1,
            name: "nuc",
            load: load(1, 3),
            modes: CONTAINER_ONLY_MODES,
            resources_enforced: true,
            envs: NO_ENVS,
        },
        PlacementCandidate {
            index: 2,
            name: "air",
            load: load(0, 1),
            modes: HOST_AND_CONTAINER,
            resources_enforced: true,
            envs: air_envs,
        },
    ]
}

fn air_envs() -> Vec<String> {
    AIR_ENVS.iter().map(|e| (*e).to_string()).collect()
}

fn placement(pin: Option<&str>, required: LaunchRequirements<'_>) -> String {
    match choose_placement(
        PlacementPolicy::Busyness,
        &fleet(&air_envs()),
        pin,
        required,
    ) {
        Ok(index) => format!("node {index}"),
        Err(e) => e.to_string(),
    }
}

/// The regression guard for the fleet: with both predicates in force, every
/// level of every job type in `.chug/jobs/` lands where it landed without them.
#[test]
fn the_predicate_moves_no_job_type_in_this_repo() {
    for (file, job_type) in job_types() {
        let limits = job_type
            .resources
            .as_ref()
            .is_some_and(|r| r.cpu.is_some() || r.memory.is_some());
        let pin = job_type.placement_node();
        let mut levels = vec![Level::Work, Level::WrapUp];
        levels.extend(job_type.eval.iter().map(Level::Eval));
        for level in levels {
            let mode = job_type.level_mode(level);
            let env = job_type.level_runtime_env(level);
            let required = LaunchRequirements {
                mode,
                resource_limits: limits,
                env,
            };
            assert_eq!(
                placement(pin, required),
                placement(pin, mode.into()),
                "{file} ({mode:?}, limits {limits}, env {env:?}) is placed differently than it \
                 was before design #309 §7's and #543 D2's predicates"
            );
        }
    }
}

/// Why the guard above is green, stated as its own assertion so a job type that
/// breaks the condition fails here — where the reason is named — rather than
/// only in the placement comparison: no level that resolves to host mode
/// declares a bound no host node can apply (design #309 §7's corollary).
#[test]
fn no_host_level_in_this_repo_declares_a_bound_no_node_can_apply() {
    let mut hosts = 0;
    for (file, job_type) in job_types() {
        let limits = job_type
            .resources
            .as_ref()
            .is_some_and(|r| r.cpu.is_some() || r.memory.is_some());
        let mut levels = vec![Level::Work, Level::WrapUp];
        levels.extend(job_type.eval.iter().map(Level::Eval));
        for level in levels {
            if job_type.level_mode(level) == RuntimeMode::Host {
                hosts += 1;
                assert!(
                    !limits,
                    "{file} runs a host level and declares resources.cpu/memory, which no host \
                     node bounds — the two are now mutually exclusive (design #309 §7)"
                );
            }
        }
    }
    assert!(
        hosts > 0,
        "mac-proof.yaml is the host job type this guard exists for; if it is gone, so is the case"
    );
}

/// Why the env half of `the_predicate_moves_no_job_type_in_this_repo` is green,
/// stated as its own assertion:
/// every node-interpreted `runtime.env` this repo declares is one the live fleet
/// advertises (design #543 §5, and `mac-proof` is its one consumer).
#[test]
fn every_declared_env_in_this_repo_is_advertised_by_the_live_fleet() {
    let advertised = air_envs();
    let mut declared = 0;
    for (file, job_type) in job_types() {
        let mut levels = vec![Level::Work, Level::WrapUp];
        levels.extend(job_type.eval.iter().map(Level::Eval));
        for level in levels {
            let Some(env) = job_type
                .level_runtime_env(level)
                .filter(|env| types::job_type::env_is_node_advertised(env))
            else {
                continue;
            };
            declared += 1;
            assert!(
                advertised.iter().any(|a| a == env),
                "{file} declares runtime.env {env:?} and no node in the live fleet advertises it, \
                 so design #543 D2's match would queue its launches"
            );
        }
    }
    assert!(
        declared > 0,
        "mac-proof.yaml is the job type this guard exists for; if it is gone, so is the case"
    );
}

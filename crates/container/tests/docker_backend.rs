//! Tier-2 tests: DockerBackend against the local Docker daemon. Skips when
//! Docker is unavailable. The behavioral assertions live in
//! `test_utils::backend_suite` — shared with the worker fleet backend, which
//! must satisfy the identical contract (spec §3.1).

use container::docker::DockerBackend;
use test_utils::backend_suite as suite;

fn docker() -> Option<DockerBackend> {
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return None;
    }
    Some(DockerBackend::local(8).expect("local backend"))
}

#[tokio::test]
async fn logs_capture_both_streams_after_exit() {
    let Some(be) = docker() else { return };
    suite::logs_capture_both_streams_after_exit(&be, "local").await;
}

#[tokio::test]
async fn exit_codes_round_trip() {
    let Some(be) = docker() else { return };
    suite::exit_codes_round_trip(&be).await;
}

#[tokio::test]
async fn env_file_injection_and_copy_out() {
    let Some(be) = docker() else { return };
    suite::env_file_injection_and_copy_out(&be).await;
}

#[tokio::test]
async fn inspect_kill_and_not_found() {
    let Some(be) = docker() else { return };
    suite::inspect_kill_and_not_found(&be, "local").await;
}

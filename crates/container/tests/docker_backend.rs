//! Tier-2 tests: DockerBackend against the local Docker daemon. Skips when
//! Docker is unavailable. The behavioral assertions live in
//! `test_utils::backend_suite` — shared with the worker fleet backend, which
//! must satisfy the identical contract (spec §3.1).

use container::ContainerBackend;
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

/// The leak fix: after harvesting, `remove` reclaims the overlay and the
/// startup sweep sees the exited container until then. `remove` is idempotent.
#[tokio::test]
async fn remove_reclaims_exited_container_and_is_idempotent() {
    let Some(be) = docker() else { return };
    let id = be.launch(suite::cfg("exit 0")).await.unwrap();
    assert_eq!(be.wait(&id).await.unwrap(), 0);

    // While exited-but-present it is a sweep candidate.
    assert!(
        be.list_managed_exited().await.unwrap().contains(&id),
        "exited managed container should show up for the startup sweep"
    );

    be.remove(&id).await.unwrap();
    // Gone: inspect no longer finds it, and it drops out of the sweep list.
    assert!(be.inspect(&id).await.unwrap().is_none());
    assert!(!be.list_managed_exited().await.unwrap().contains(&id));
    // Removing an already-gone container is a no-op, not an error.
    be.remove(&id).await.unwrap();
}

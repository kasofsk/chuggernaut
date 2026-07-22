//! Tier-2 tests: DockerBackend against the local Docker daemon. Skips when
//! Docker is unavailable. The behavioral assertions live in
//! `test_utils::backend_suite` — shared with the worker fleet backend, which
//! must satisfy the identical contract (spec §3.1).

use container::docker::{DockerBackend, DockerNodeConfig};
use container::{ContainerBackend, ContainerLaunchConfig};
use test_utils::backend_suite as suite;

fn docker() -> Option<DockerBackend> {
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return None;
    }
    Some(DockerBackend::local(8).expect("local backend"))
}

/// A two-node fleet: the real local daemon plus an unreachable `tcp://` node
/// standing in for a worker whose SSH tunnel is down at boot.
fn one_up_one_down() -> Option<DockerBackend> {
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return None;
    }
    DockerBackend::new(vec![
        DockerNodeConfig {
            name: "local".into(),
            endpoint: "unix:///var/run/docker.sock".into(),
            slots: 8,
        },
        DockerNodeConfig {
            // Refuses connections immediately — the out-of-service node.
            name: "down".into(),
            endpoint: "tcp://127.0.0.1:1".into(),
            slots: 8,
        },
    ])
    .ok()
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

/// §3.1/§3.6 degrade: an unreachable node does not block startup, is excluded
/// from placement, and a pin onto it fails — while the healthy node still
/// launches. A single-node all-down fleet keeps the fail-fast rule.
#[tokio::test]
async fn startup_degrades_and_placement_excludes_down_node() {
    let Some(be) = one_up_one_down() else { return };

    // One node down, one up ⇒ startup succeeds (does not error).
    be.ping_all()
        .await
        .expect("degrade: start with one node up");
    // The down node is marked out of service in the snapshot.
    let avail = be.availability();
    assert!(!avail.iter().find(|(n, _)| n == "down").unwrap().1);
    assert!(avail.iter().find(|(n, _)| n == "local").unwrap().1);

    // Unpinned launch skips the down node and lands on local.
    let id = be.launch(suite::cfg("exit 0")).await.unwrap();
    assert!(id.starts_with("local/"), "placed off the down node: {id}");
    assert_eq!(be.wait(&id).await.unwrap(), 0);
    be.remove(&id).await.unwrap();

    // A pin to the out-of-service node fails without spillover to local.
    let err = be
        .launch(pinned("exit 0", "down"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no free slots on node down"), "{err}");

    // A pin to an unknown node names the known nodes.
    let err = be
        .launch(pinned("exit 0", "mini"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown node \"mini\""), "{err}");

    // A pin to the healthy node places (and routes) there.
    let id = be.launch(pinned("exit 0", "local")).await.unwrap();
    assert!(id.starts_with("local/"), "{id}");
    assert_eq!(be.wait(&id).await.unwrap(), 0);
    be.remove(&id).await.unwrap();
}

/// The all-down case keeps the fail-fast rule: a lone unreachable node aborts
/// startup (no Docker needed — the connection simply refuses).
#[tokio::test]
async fn all_nodes_down_fails_startup() {
    let be = DockerBackend::new(vec![DockerNodeConfig {
        name: "down".into(),
        endpoint: "tcp://127.0.0.1:1".into(),
        slots: 4,
    }])
    .expect("construct");
    assert!(be.ping_all().await.is_err(), "all-down must fail fast");
}

fn pinned(cmd: &str, node: &str) -> ContainerLaunchConfig {
    ContainerLaunchConfig {
        node: Some(node.into()),
        ..suite::cfg(cmd)
    }
}

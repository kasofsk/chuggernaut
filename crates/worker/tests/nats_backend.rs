//! Tier-2: the worker daemon + fleet backend against real NATS (container)
//! and the local Docker daemon. The fleet backend must satisfy the same
//! behavioral contract as DockerBackend, through the proxy.

use container::ContainerBackend;
use container::docker::DockerNodeConfig;
use test_utils::backend_suite as suite;
use test_utils::nats::NatsTestServer;
use test_utils::require_nats;
use worker::{FleetBackend, WorkerConfig};

/// In-process daemon (node "w1", local Docker) + fleet backend over it, or
/// None to skip (no Docker).
async fn setup(
    server: &NatsTestServer,
    artifact_bytes: &[u8],
) -> Option<(FleetBackend, tokio::task::JoinHandle<()>)> {
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return None;
    }

    let dir = std::env::temp_dir().join(format!(
        "chug-worker-test-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let artifact_path = dir.join("chuggernaut-channel");
    std::fs::write(&artifact_path, artifact_bytes).unwrap();

    let config = WorkerConfig {
        node: "w1".into(),
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: artifact_path,
        cache_dir: None,
    };
    let daemon = tokio::spawn(async move {
        if let Err(e) = worker::run(config).await {
            eprintln!("daemon exited: {e}");
        }
    });

    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "w1".into(),
            endpoint: "worker".into(),
            slots: 8,
        }],
        store,
    )
    .unwrap();

    // Wait for the daemon's subscription to be live: a ghost inspect answered
    // with an op-level result (Ok(None)) proves the round trip.
    for _ in 0..100 {
        if fleet.inspect(&"w1/deadbeef".to_string()).await.is_ok() {
            return Some((fleet, daemon));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("worker daemon never became reachable");
}

fn local_docker_endpoint() -> String {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        return host;
    }
    for candidate in [
        "/var/run/docker.sock".to_string(),
        format!(
            "{}/.docker/run/docker.sock",
            std::env::var("HOME").unwrap_or_default()
        ),
        format!(
            "{}/.colima/default/docker.sock",
            std::env::var("HOME").unwrap_or_default()
        ),
    ] {
        if std::path::Path::new(&candidate).exists() {
            return format!("unix://{candidate}");
        }
    }
    "unix:///var/run/docker.sock".into()
}

#[tokio::test]
async fn contract_suite_through_the_proxy() {
    let server = require_nats!();
    let Some((fleet, daemon)) = setup(&server, b"#!/bin/sh\nexit 0\n").await else {
        return;
    };
    suite::run_all(&fleet, "w1").await;
    daemon.abort();
}

#[tokio::test]
async fn local_artifact_substitution_and_unknown_artifact() {
    let server = require_nats!();
    let Some((fleet, daemon)) = setup(&server, b"#!/bin/sh\necho artifact-ran\n").await else {
        return;
    };

    // Known artifact: the worker substitutes its local bytes; the container
    // executes them — proving mode and contents both arrived.
    let mut config = suite::cfg("/usr/local/bin/chuggernaut-channel > /out.txt");
    config.files = vec![container::InjectedFile {
        container_path: "/usr/local/bin/chuggernaut-channel".into(),
        contents: vec![], // bytes intentionally absent — the tag carries it
        mode: 0o755,
        artifact: Some(types::worker::ARTIFACT_CHANNEL.into()),
    }];
    let id = fleet.launch(config).await.unwrap();
    assert_eq!(fleet.wait(&id).await.unwrap(), 0);
    let out = fleet.copy_file(&id, "/out.txt").await.unwrap().unwrap();
    assert_eq!(out, b"artifact-ran\n");
    suite::rm(&id);

    // Unknown artifact name: launch fails with a clear error, no container.
    let mut config = suite::cfg("true");
    config.files = vec![container::InjectedFile {
        container_path: "/x".into(),
        contents: vec![],
        mode: 0o644,
        artifact: Some("no-such-artifact".into()),
    }];
    let err = fleet.launch(config).await.unwrap_err();
    assert!(
        err.to_string().contains("no-such-artifact"),
        "unexpected: {err}"
    );
    daemon.abort();
}

#[tokio::test]
async fn remove_and_exited_sweep_through_the_proxy() {
    let server = require_nats!();
    let Some((fleet, daemon)) = setup(&server, b"#!/bin/sh\nexit 0\n").await else {
        return;
    };
    let id = fleet.launch(suite::cfg("exit 0")).await.unwrap();
    assert_eq!(fleet.wait(&id).await.unwrap(), 0);
    assert!(
        fleet.list_managed_exited().await.unwrap().contains(&id),
        "exited container visible to the sweep through the proxy"
    );
    fleet.remove(&id).await.unwrap();
    assert!(!fleet.list_managed_exited().await.unwrap().contains(&id));
    // Idempotent, like the direct backend.
    fleet.remove(&id).await.unwrap();
    daemon.abort();
}

#[tokio::test]
async fn out_of_service_worker_fails_placement_not_startup() {
    let server = require_nats!();
    // No daemon at all: startup succeeds (soft-fail), launch reports no slots.
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "ghost".into(),
            endpoint: "worker".into(),
            slots: 4,
        }],
        store,
    )
    .unwrap();
    fleet.startup_check().await.unwrap(); // must NOT error
    let err = fleet.launch(suite::cfg("true")).await.unwrap_err();
    assert!(
        err.to_string().contains("no free slots"),
        "unexpected: {err}"
    );
}

#[tokio::test]
async fn payload_guard_rejects_bulk_inline_files() {
    let server = require_nats!();
    let Some((fleet, daemon)) = setup(&server, b"x").await else {
        return;
    };
    // A multi-MB inline file must be refused client-side (static artifacts
    // belong node-local), not sent to NATS to bounce off max_payload.
    let mut config = suite::cfg("true");
    config.files = vec![container::InjectedFile {
        container_path: "/big".into(),
        contents: vec![0u8; 2 * 1024 * 1024],
        mode: 0o644,
        artifact: None,
    }];
    let err = fleet.launch(config).await.unwrap_err();
    assert!(err.to_string().contains("node-local"), "unexpected: {err}");
    daemon.abort();
}

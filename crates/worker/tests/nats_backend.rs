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
        refresh_script: None,
        refresh_git_url: None,
        refresh_git_key: "/data/keys/worker_git".into(),
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

/// A stand-in worker daemon: answers `ping` (the only op startup capacity
/// probing needs) over NATS, no Docker involved. Lets the fleet's startup and
/// placement paths be exercised without a real daemon or docker socket.
async fn mock_worker(store: &store::NatsStore, node: &str) -> tokio::task::JoinHandle<()> {
    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(node))
        .await
        .expect("subscribe mock worker");
    // Ensure the SUB reached the server before any ping is published.
    store.client().flush().await.expect("flush sub");
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            if req.subject.ends_with(".ping") {
                let reply = types::worker::WorkerReply::Ok {
                    value: types::worker::PingOk {
                        running: 0,
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        artifacts: std::collections::HashMap::new(),
                    },
                };
                req.respond(serde_json::to_vec(&reply).unwrap()).await;
            }
        }
    })
}

/// A responding worker gives the fleet live capacity, so startup succeeds and a
/// second, unreachable worker is soft-failed — a pin onto it fails *placement*,
/// not startup (spec §3.1/§3.6).
#[tokio::test]
async fn worker_capacity_starts_fleet_and_dead_worker_fails_placement() {
    let server = require_nats!();
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let mock = mock_worker(&store, "up").await;
    let fleet = FleetBackend::new(
        vec![
            DockerNodeConfig {
                name: "up".into(),
                endpoint: "worker".into(),
                slots: 4,
            },
            DockerNodeConfig {
                name: "ghost".into(),
                endpoint: "worker".into(),
                slots: 4,
            },
        ],
        store,
    )
    .unwrap();

    fleet.startup_check().await.unwrap(); // "up" responds ⇒ starts
    // availability() still reports every node (spec §3.1 snapshot).
    let avail = fleet.availability();
    assert_eq!(avail.len(), 2);
    assert!(avail.iter().any(|(n, up)| n == "up" && *up));
    assert!(avail.iter().any(|(n, up)| n == "ghost" && !*up));

    // A pin onto the out-of-service worker fails placement, not startup.
    let mut cfg = suite::cfg("true");
    cfg.node = Some("ghost".into());
    let err = fleet.launch(cfg).await.unwrap_err();
    assert!(
        err.to_string().contains("no free slots"),
        "unexpected: {err}"
    );
    mock.abort();
}

/// The prod outage, end-to-end: a 0-slot docker placeholder (unreachable here,
/// no docker needed) beside a responding worker must NOT veto startup — capacity
/// is a fleet property, evaluated once across transports.
#[tokio::test]
async fn zero_slot_docker_does_not_veto_live_worker_fleet() {
    let server = require_nats!();
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let mock = mock_worker(&store, "air").await;
    let fleet = FleetBackend::new(
        vec![
            DockerNodeConfig {
                name: "local".into(),
                endpoint: "tcp://127.0.0.1:1".into(),
                slots: 0,
            },
            DockerNodeConfig {
                name: "air".into(),
                endpoint: "worker".into(),
                slots: 4,
            },
        ],
        store,
    )
    .unwrap();
    fleet.startup_check().await.unwrap();
    mock.abort();
}

/// No reachable node has capacity anywhere (0-slot docker + unreachable worker)
/// ⇒ refuse to start (spec §3.6).
#[tokio::test]
async fn no_reachable_capacity_fails_startup() {
    let server = require_nats!();
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = FleetBackend::new(
        vec![
            DockerNodeConfig {
                name: "local".into(),
                endpoint: "tcp://127.0.0.1:1".into(),
                slots: 0,
            },
            DockerNodeConfig {
                name: "ghost".into(),
                endpoint: "worker".into(),
                slots: 4,
            },
        ],
        store,
    )
    .unwrap();
    assert!(fleet.startup_check().await.is_err());
}

/// Spawn a worker daemon (node `node`, local Docker) with an optional
/// self-refresh script. Returns the join handle; the caller builds its own
/// fleet/RPC over the same NATS server.
fn spawn_daemon(
    server: &NatsTestServer,
    node: &str,
    channel_bytes: &[u8],
    refresh_script: Option<std::path::PathBuf>,
) -> tokio::task::JoinHandle<()> {
    let dir = std::env::temp_dir().join(format!(
        "chug-worker-daemon-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let channel = dir.join("chuggernaut-channel");
    std::fs::write(&channel, channel_bytes).unwrap();
    // A refresh-capable daemon (script wired) also gets a git credential so the
    // accept path is exercised; the skip-without-credential path is covered by
    // `refresh_reports_skip_without_git_credential`.
    let (refresh_git_url, refresh_git_key) = if refresh_script.is_some() {
        let key = dir.join("worker_git");
        std::fs::write(&key, b"fake-key").unwrap();
        (Some("ssh://git@front:2222/acme/chug.git".to_string()), key)
    } else {
        (None, "/data/keys/worker_git".into())
    };
    let config = WorkerConfig {
        node: node.into(),
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: channel,
        cache_dir: None,
        refresh_script,
        refresh_git_url,
        refresh_git_key,
    };
    tokio::spawn(async move {
        if let Err(e) = worker::run(config).await {
            eprintln!("daemon exited: {e}");
        }
    })
}

/// Write an executable no-op refresh script (build/swap both exit 0) so the
/// daemon's refresh sequence runs without touching Docker or the real script.
fn fake_refresh_script() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "chug-fake-refresh-{}-{:x}.sh",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

async fn fleet_over(store: store::NatsStore, node: &str, slots: u32) -> FleetBackend {
    FleetBackend::new(
        vec![DockerNodeConfig {
            name: node.into(),
            endpoint: "worker".into(),
            slots,
        }],
        store,
    )
    .unwrap()
}

/// Wait for a daemon's subscription to be live (a ghost inspect round-trips).
async fn await_reachable(fleet: &FleetBackend, node: &str) {
    for _ in 0..100 {
        if fleet.inspect(&format!("{node}/deadbeef")).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("worker daemon never became reachable");
}

/// A task's `wait` stream survives a daemon restart and still delivers the exit
/// (spec §3.1 drain guarantee): job containers run on Docker, not in the daemon,
/// so replacing the daemon mid-job leaves the container running and the
/// dispatcher's poll-based `wait` re-attaches over the new daemon.
#[tokio::test]
async fn wait_survives_daemon_restart() {
    let server = require_nats!();
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }
    let daemon = spawn_daemon(&server, "w1", b"x", None);
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = fleet_over(store, "w1", 8).await;
    await_reachable(&fleet, "w1").await;

    // A container that outlives the daemon restart, then exits non-zero.
    let id = fleet.launch(suite::cfg("sleep 4; exit 7")).await.unwrap();

    // Start waiting, then yank and replace the daemon underneath the wait.
    let wait_fleet = &fleet;
    let waiter = wait_fleet.wait(&id);
    tokio::pin!(waiter);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    daemon.abort();
    let _ = daemon.await;
    // Container is still running on Docker; bring a fresh daemon up on the node.
    let daemon2 = spawn_daemon(&server, "w1", b"x", None);
    await_reachable(&fleet, "w1").await;

    // The exit is still delivered through the new daemon.
    let code = waiter.await.unwrap();
    assert_eq!(code, 7, "exit delivered across the daemon restart");
    suite::rm(&id);
    daemon2.abort();
}

/// The self-refresh RPC over the proxy: unconfigured nodes reject it, and a
/// configured node accepts and quiesces the swap window so new launches are
/// refused (retryable NoCapacity) — never interrupting in-flight containers.
#[tokio::test]
async fn refresh_rpc_and_quiesce_window() {
    use store::worker::WorkerRpc;
    use types::worker::RefreshRequest;

    let server = require_nats!();
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }

    // 1. No script wired ⇒ refresh is cleanly rejected as unconfigured.
    let daemon = spawn_daemon(&server, "w1", b"x", None);
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = fleet_over(store.clone(), "w1", 8).await;
    await_reachable(&fleet, "w1").await;
    let rpc = WorkerRpc::new(store.clone(), "w1");
    let err = rpc
        .refresh(&RefreshRequest {
            sha: "deadbeef".into(),
            tag: "prod".into(),
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("self-refresh script"),
        "unexpected: {err}"
    );
    daemon.abort();
    let _ = daemon.await;

    // 2. Script wired ⇒ refresh is accepted and the swap window quiesces
    //    launches. The no-op script leaves the daemon quiesced (a real swap
    //    replaces the process here), so a subsequent launch is refused.
    let script = fake_refresh_script();
    let daemon = spawn_daemon(&server, "w1", b"x", Some(script));
    await_reachable(&fleet, "w1").await;
    let rpc = WorkerRpc::new(store, "w1");
    let ok = rpc
        .refresh(&RefreshRequest {
            sha: "cafef00d".into(),
            tag: "prod".into(),
        })
        .await
        .unwrap();
    assert!(ok.accepted);

    // A second refresh while one is converging is reported as not-accepted
    // (not an error, not drift) so the deploy caller skips the swap-wait.
    let again = rpc
        .refresh(&RefreshRequest {
            sha: "cafef00d".into(),
            tag: "prod".into(),
        })
        .await
        .unwrap();
    assert!(
        !again.accepted,
        "concurrent refresh must report not-accepted"
    );

    // Poll: once build+quiesce+swap have run, launches are refused.
    let mut refused = false;
    for _ in 0..30 {
        match fleet.launch(suite::cfg("true")).await {
            Ok(id) => {
                // Not quiesced yet (or a real container slipped in) — clean up.
                suite::rm(&id);
            }
            Err(e) => {
                assert!(e.to_string().contains("refreshing"), "unexpected: {e}");
                refused = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(refused, "launches must be refused during the swap window");
    daemon.abort();
}

/// A refresh RPC to a node with a script but NO git credential reports the skip
/// in the reply (spec §3.1 / #114) — `accepted == false`, `skipped == Some(..)`
/// — instead of accepting and silently no-oping in the background. This is what
/// lets the deploy surface a missing-credential node loudly.
#[tokio::test]
async fn refresh_reports_skip_without_git_credential() {
    use store::worker::WorkerRpc;
    use types::worker::RefreshRequest;

    let server = require_nats!();
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }

    // Script wired, but no WORKER_REFRESH_GIT_URL / key: the exact prod #114
    // shape. Build the config inline (spawn_daemon would provision a credential).
    let dir = std::env::temp_dir().join(format!(
        "chug-worker-skip-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let channel = dir.join("chuggernaut-channel");
    std::fs::write(&channel, b"x").unwrap();
    let config = WorkerConfig {
        node: "w1".into(),
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: channel,
        cache_dir: None,
        refresh_script: Some(fake_refresh_script()),
        refresh_git_url: None,
        refresh_git_key: dir.join("worker_git"), // absent
    };
    let daemon = tokio::spawn(async move {
        if let Err(e) = worker::run(config).await {
            eprintln!("daemon exited: {e}");
        }
    });

    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = fleet_over(store.clone(), "w1", 8).await;
    await_reachable(&fleet, "w1").await;

    let rpc = WorkerRpc::new(store, "w1");
    let ok = rpc
        .refresh(&RefreshRequest {
            sha: "cafef00d".into(),
            tag: "prod".into(),
        })
        .await
        .unwrap();
    assert!(
        !ok.accepted,
        "a credential-less node must not accept refresh"
    );
    assert!(
        ok.skipped
            .as_deref()
            .is_some_and(|r| r.contains("git credential")),
        "skip reason must name the missing credential: {:?}",
        ok.skipped
    );

    // The skip must not have quiesced the node — launches still flow.
    let id = fleet.launch(suite::cfg("true")).await.unwrap();
    suite::rm(&id);
    daemon.abort();
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

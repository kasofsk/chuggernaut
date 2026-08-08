//! Tier-2: the worker daemon + fleet backend against real NATS (container)
//! and the local Docker daemon. The fleet backend must satisfy the same
//! behavioral contract as DockerBackend, through the proxy.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use container::ContainerBackend;
use container::PlacementPolicy;
use container::docker::DockerNodeConfig;
use test_utils::backend_suite as suite;
use test_utils::nats::NatsTestServer;
use worker::config::{
    ANDROID_SDK_DIR_DEFAULT, NIX_CLIENT_DEFAULT, NIX_DAEMON_SOCKET_DEFAULT,
    NIX_REALISE_TIMEOUT_SECS_DEFAULT, NIX_STORE_DIR_DEFAULT,
};
use worker::{FleetBackend, WorkerConfig, WorkerMode};

/// The `WORKER_SLOTS_MAX` every daemon spawned here boots with (spec §3.1): above
/// the boot `slots: 4`, so the boot value is never clamped, and low enough that a
/// request above the ceiling is easy to state.
const DAEMON_SLOTS_MAX: u32 = 6;

/// A fresh directory this process alone owns, for a test's node-local files.
/// Canonicalized, so a node whose store prefix is derived from it still matches
/// a realise target the daemon resolves.
fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

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

    let dir = unique_temp_dir("chug-worker-test");
    let artifact_path = dir.join("chuggernaut-channel");
    std::fs::write(&artifact_path, artifact_bytes).unwrap();

    let config = WorkerConfig {
        node: "w1".into(),
        slots: 4,
        slots_max: DAEMON_SLOTS_MAX,
        modes: vec![WorkerMode::Container],
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: artifact_path,
        cache_dir: None,
        host_root: std::env::temp_dir().join("chug-host-root-test"),
        kvm_device: None,
        kvm_projects: vec![],
        android_sdk_dir: ANDROID_SDK_DIR_DEFAULT.into(),
        flutter_dir: None,
        jdk_dir: None,
        nix_gcroots_dir: None,
        nix_projects: Vec::new(),
        nix_flake_client: "/nix/var/nix/profiles/system/sw/bin/nix".into(),
        nix_client: NIX_CLIENT_DEFAULT.into(),
        nix_daemon_socket: NIX_DAEMON_SOCKET_DEFAULT.into(),
        nix_store_dir: NIX_STORE_DIR_DEFAULT.into(),
        nix_realise_timeout_secs: NIX_REALISE_TIMEOUT_SECS_DEFAULT,
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
        PlacementPolicy::default(),
    )
    .unwrap();

    test_utils::wait::poll_async_default("worker daemon w1 to become reachable", || async {
        fleet.inspect(&"w1/deadbeef".to_string()).await.ok()
    })
    .await;
    Some((fleet, daemon))
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
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let Some((fleet, daemon)) = setup(&server, b"#!/bin/sh\nexit 0\n").await else {
        return;
    };
    suite::run_all(&fleet, "w1").await;
    daemon.abort();
}

#[tokio::test]
async fn local_artifact_substitution_and_unknown_artifact() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let Some((fleet, daemon)) = setup(&server, b"#!/bin/sh\necho artifact-ran\n").await else {
        return;
    };

    let mut config = suite::cfg("/usr/local/bin/chuggernaut-channel > /out.txt");
    config.files = vec![container::InjectedFile {
        container_path: "/usr/local/bin/chuggernaut-channel".into(),
        contents: vec![],
        mode: 0o755,
        artifact: Some(types::worker::ARTIFACT_CHANNEL.into()),
    }];
    let id = fleet.launch(config).await.unwrap();
    assert_eq!(fleet.wait(&id).await.unwrap(), 0);
    let out = fleet.copy_file(&id, "/out.txt").await.unwrap().unwrap();
    assert_eq!(out, b"artifact-ran\n");
    suite::rm(&id);

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
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
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
    store.client().flush().await.expect("flush sub");
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            if req.subject.ends_with(".ping") {
                let reply = types::worker::WorkerReply::Ok {
                    value: types::worker::PingOk {
                        running: 0,
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        artifacts: std::collections::HashMap::new(),
                        slots: None,
                        slots_max: None,
                        capacity_epoch: None,
                        capacity_generation: None,
                        refresh_outcome: None,
                        refresh_progress: None,
                        capabilities: None,
                    },
                };
                req.respond(serde_json::to_vec(&reply).unwrap()).await;
            }
        }
    })
}

/// A stand-in worker on the job-2 build: its `ping` reply carries the node's
/// capacity and the `(epoch, generation)` pair that orders it (spec §3.1 slot
/// source). Used to exercise the pull transport — the half that makes the
/// 2026-07-26 denied-publish failure self-correcting.
async fn capacity_worker(
    store: &store::NatsStore,
    node: &str,
    slots: u32,
) -> tokio::task::JoinHandle<()> {
    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(node))
        .await
        .expect("subscribe capacity worker");
    store.client().flush().await.expect("flush sub");
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            if req.subject.ends_with(".ping") {
                let reply = types::worker::WorkerReply::Ok {
                    value: types::worker::PingOk {
                        running: 0,
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        artifacts: std::collections::HashMap::new(),
                        slots: Some(slots),
                        slots_max: Some(8),
                        capacity_epoch: Some(1_769_000_000_123),
                        capacity_generation: Some(0),
                        refresh_outcome: None,
                        refresh_progress: None,
                        capabilities: None,
                    },
                };
                req.respond(serde_json::to_vec(&reply).unwrap()).await;
            }
        }
    })
}

/// A stand-in worker that models a real node's live count: it answers `launch`
/// (bumping an internal counter and returning a synthetic id) and reports that
/// counter as `ping.running`. No Docker — but the ping accounting placement
/// reads is exactly what a real daemon returns, so this exercises the real
/// cross-node busyness decision without a docker socket.
async fn counting_worker(store: &store::NatsStore, node: &str) -> tokio::task::JoinHandle<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(node))
        .await
        .expect("subscribe counting worker");
    store.client().flush().await.expect("flush sub");
    let node = node.to_string();
    tokio::spawn(async move {
        let running = Arc::new(AtomicU32::new(0));
        while let Some(req) = sub.next().await {
            if req.subject.ends_with(".ping") {
                let reply = types::worker::WorkerReply::Ok {
                    value: types::worker::PingOk {
                        running: running.load(Ordering::SeqCst),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        artifacts: std::collections::HashMap::new(),
                        slots: None,
                        slots_max: None,
                        capacity_epoch: None,
                        capacity_generation: None,
                        refresh_outcome: None,
                        refresh_progress: None,
                        capabilities: None,
                    },
                };
                req.respond(serde_json::to_vec(&reply).unwrap()).await;
            } else if req.subject.ends_with(".launch") {
                let n = running.fetch_add(1, Ordering::SeqCst) + 1;
                let reply = types::worker::WorkerReply::Ok {
                    value: types::worker::LaunchOk {
                        id: format!("{node}/c{n}"),
                    },
                };
                req.respond(serde_json::to_vec(&reply).unwrap()).await;
            }
        }
    })
}

/// Busyness placement over the *real* accounting (spec §3.1/#153): two launches
/// dispatched concurrently — as the dispatcher does, each agent launch on its
/// own spawned task — must land on different nodes. Before the in-flight
/// reservation both `place()` calls read the same `running: 0` and busyness tied
/// them onto the first node (prod 2026-07-22: back-to-back releases both went to
/// `air` while `nuc` sat idle); the choose-under-lock reservation fixes it.
/// The unit test over `choose_placement` passed throughout because it feeds a
/// mocked `running: 1` that the un-reserved live count never actually produced
/// in the race window.
#[tokio::test]
async fn busyness_places_concurrent_launches_on_distinct_nodes() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let air = counting_worker(&store, "air").await;
    let nuc = counting_worker(&store, "nuc").await;
    let fleet = FleetBackend::new(
        vec![
            DockerNodeConfig {
                name: "air".into(),
                endpoint: "worker".into(),
                slots: 4,
            },
            DockerNodeConfig {
                name: "nuc".into(),
                endpoint: "worker".into(),
                slots: 4,
            },
        ],
        store,
        PlacementPolicy::Busyness,
    )
    .unwrap();

    let cfg = || suite::cfg("true");
    let (a, b) = tokio::join!(fleet.launch(cfg()), fleet.launch(cfg()));
    let a = a.unwrap();
    let b = b.unwrap();
    let na = a.split_once('/').unwrap().0;
    let nb = b.split_once('/').unwrap().0;
    air.abort();
    nuc.abort();
    assert_ne!(
        na, nb,
        "busyness sent both concurrent launches to {na}; the second should have gone to the idle node"
    );
}

/// A responding worker gives the fleet live capacity, so startup succeeds and a
/// second, unreachable worker is soft-failed — a pin onto it fails *placement*,
/// not startup (spec §3.1/§3.6).
#[tokio::test]
async fn worker_capacity_starts_fleet_and_dead_worker_fails_placement() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
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
        PlacementPolicy::default(),
    )
    .unwrap();

    fleet.startup_check().await.unwrap();
    let avail = fleet.availability();
    assert_eq!(avail.len(), 2);
    assert!(avail.iter().any(|(n, up)| n == "up" && *up));
    assert!(avail.iter().any(|(n, up)| n == "ghost" && !*up));

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
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
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
        PlacementPolicy::default(),
    )
    .unwrap();
    fleet.startup_check().await.unwrap();
    mock.abort();
}

/// No reachable node has capacity anywhere (0-slot docker + unreachable worker)
/// ⇒ refuse to start (spec §3.6).
#[tokio::test]
async fn no_reachable_capacity_fails_startup() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
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
        PlacementPolicy::default(),
    )
    .unwrap();
    assert!(fleet.startup_check().await.is_err());
}

/// The ordering `startup_check` depends on and design #293 §1 calls load-bearing:
/// `probe_worker` applies ping-reported capacity to the slot cell on the reply
/// path, and only THEN does the gate read that cell. The seed here is
/// `|worker|0` — the §7 recommendation — so the number the fleet ends up serving
/// can ONLY have come from the ping. Read it back the way the gate does, before
/// any placement has run.
#[tokio::test]
async fn ping_reported_slots_reach_the_startup_gate() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let mock = capacity_worker(&store, "air", 2).await;
    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "air".into(),
            endpoint: "worker".into(),
            slots: 0,
        }],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();

    fleet.startup_check().await.unwrap();

    let air = fleet
        .fleet_status()
        .into_iter()
        .find(|n| n.name == "air")
        .expect("air is in the fleet");
    assert_eq!(
        air.slots,
        Some(2),
        "the gate's slot cell must hold the ping-reported number, not the 0 seed"
    );
    let capacity = air.capacity.expect("a worker node carries provenance");
    assert_eq!(capacity.source(), types::worker::CapacitySource::Node);
    assert!(capacity.observed_at.is_some());
    assert_eq!(capacity.mark, (1_769_000_000_123, 0));
    assert_eq!(capacity.slots_max, Some(8));
    mock.abort();
}

/// The incident's representation (design #293 §7/§8): a node that answers pings
/// on a pre-field build reports no capacity at all, so the `DOCKER_NODES` seed
/// keeps standing in — and the fleet snapshot says `seed`, never confirmed,
/// instead of being indistinguishable from a healthy node.
#[tokio::test]
async fn never_reporting_worker_is_visible_as_seed_sourced() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let mock = mock_worker(&store, "air").await;
    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "air".into(),
            endpoint: "worker".into(),
            slots: 2,
        }],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();

    fleet.startup_check().await.unwrap();

    let air = fleet
        .fleet_status()
        .into_iter()
        .find(|n| n.name == "air")
        .expect("air is in the fleet");
    assert!(
        air.available,
        "it answers its pings — that is the signature"
    );
    assert_eq!(air.slots, Some(2), "the seed is still what we place on");
    let capacity = air.capacity.expect("a worker node carries provenance");
    assert_eq!(capacity.source(), types::worker::CapacitySource::Seed);
    assert_eq!(capacity.observed_at, None);
    mock.abort();
}

/// A stand-in worker that advertises what it can do on its `ping` reply (design
/// #309 §4) — the authoritative transport, ingested inside `probe_worker`. It
/// also answers `launch` with an id naming itself, so a placement decision is
/// readable off the launch's return value.
async fn capable_worker(
    store: &store::NatsStore,
    node: &str,
    capabilities: types::worker::NodeCapabilities,
) -> tokio::task::JoinHandle<()> {
    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(node))
        .await
        .expect("subscribe capable worker");
    store.client().flush().await.expect("flush sub");
    let name = node.to_string();
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            if req.subject.ends_with(".launch") {
                let reply = types::worker::WorkerReply::Ok {
                    value: types::worker::LaunchOk {
                        id: format!("{name}/placed"),
                    },
                };
                req.respond(serde_json::to_vec(&reply).unwrap()).await;
            }
            if req.subject.ends_with(".ping") {
                let reply = types::worker::WorkerReply::Ok {
                    value: types::worker::PingOk {
                        running: 0,
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        artifacts: std::collections::HashMap::new(),
                        slots: Some(1),
                        slots_max: Some(1),
                        capacity_epoch: Some(1_769_000_000_123),
                        capacity_generation: Some(0),
                        refresh_outcome: None,
                        refresh_progress: None,
                        capabilities: Some(capabilities.clone()),
                    },
                };
                req.respond(serde_json::to_vec(&reply).unwrap()).await;
            }
        }
    })
}

fn host_capable() -> types::worker::NodeCapabilities {
    types::worker::NodeCapabilities {
        modes: vec![
            types::job_type::RuntimeMode::Container,
            types::job_type::RuntimeMode::Host,
        ],
        platform: "macos/aarch64".into(),
        resources_enforced: false,
        leases: Vec::new(),
        envs: vec!["xcode:26.5".into()],
        agent_cli: true,
    }
}

/// The slice-5 contract on the wire (design #309 §4): a node's advertisement
/// reaches the dispatcher on the `ping` reply path, inside `probe_worker` and
/// before the startup gate reads the node, while a node that advertises nothing
/// reads as the absent defaults — container-only, limits enforced.
#[tokio::test]
async fn ping_advertised_capabilities_reach_the_fleet() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let capable = capable_worker(&store, "mac", host_capable()).await;
    let silent = mock_worker(&store, "nuc").await;
    let fleet = FleetBackend::new(
        vec![
            DockerNodeConfig {
                name: "mac".into(),
                endpoint: "worker".into(),
                slots: 0,
            },
            DockerNodeConfig {
                name: "nuc".into(),
                endpoint: "worker".into(),
                slots: 2,
            },
        ],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();

    fleet.startup_check().await.unwrap();

    assert_eq!(
        fleet.node_capabilities("mac"),
        Some(host_capable()),
        "the ping's advertisement is what the fleet reads"
    );
    assert!(
        fleet.node_capabilities("mac").is_some_and(|c| c.agent_cli),
        "a node that discovered an agent CLI says so (design #490 D3)"
    );
    assert_eq!(
        fleet.node_capabilities("nuc"),
        Some(types::worker::NodeCapabilities::absent()),
        "a node that says nothing reads container-only with limits enforced"
    );
    assert!(
        fleet.node_capabilities("nuc").is_some_and(|c| !c.agent_cli),
        "a daemon predating the probe promises no agent CLI"
    );
    assert_eq!(fleet.node_capabilities("absent-node"), None);
    capable.abort();
    silent.abort();
}

/// Ping is authoritative (design #309 §4): a node joins by announce and is
/// classified by it, then the first pull corrects the classification and no
/// later announce can undo it.
#[tokio::test]
async fn ping_capabilities_win_over_the_announce() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let capable = capable_worker(&store, "mac", host_capable()).await;
    let fleet = FleetBackend::new(vec![], store, PlacementPolicy::default()).unwrap();

    let announced = types::worker::NodeCapabilities {
        platform: "linux/x86_64".into(),
        ..types::worker::NodeCapabilities::absent()
    };
    let observation = types::CapacityObservation::from_announce(&types::worker::WorkerAnnounce {
        node: "mac".into(),
        slots: 1,
        slots_max: Some(1),
        capacity_epoch: Some(1_769_000_000_000),
        capacity_generation: Some(0),
        version: "0.1.0".into(),
        capabilities: Some(announced.clone()),
    });
    assert!(fleet.register_worker("mac", observation, None, Some(announced.clone())));
    assert_eq!(
        fleet.node_capabilities("mac"),
        Some(announced.clone()),
        "the join's advertisement stands until something is pulled"
    );

    fleet.startup_check().await.unwrap();
    assert_eq!(fleet.node_capabilities("mac"), Some(host_capable()));

    assert!(!fleet.register_worker("mac", observation, None, Some(announced)));
    assert_eq!(
        fleet.node_capabilities("mac"),
        Some(host_capable()),
        "an announce may not undo what the pull transport reported"
    );
    capable.abort();
}

/// A docker-endpoint node is a direct socket the fleet never pings, so it would
/// read as absent forever; its capabilities are synthesized from the node kind
/// instead (design #309 §4). No daemon and no Docker are involved — the
/// derivation is from the roster.
#[tokio::test]
async fn docker_endpoint_capabilities_are_synthesized() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let dir = unique_temp_dir("chug-fleet-caps");
    let socket = dir.join("docker.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "local".into(),
            endpoint: format!("unix://{}", socket.display()),
            slots: 2,
        }],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();

    let caps = fleet
        .node_capabilities("local")
        .expect("a docker-endpoint node is in the fleet");
    assert_eq!(caps, types::worker::NodeCapabilities::docker_endpoint());
    assert!(
        caps.resources_enforced,
        "a docker endpoint demonstrably enforces cpu/memory"
    );
    assert!(!caps.serves(types::job_type::RuntimeMode::Host));
}

/// A host launch (no `image`, design #309 §5a) reaches the one node advertising
/// the mode, past a container-only node the busyness policy would otherwise
/// prefer — and a container launch on the same fleet still places by load.
#[tokio::test]
async fn a_host_launch_is_placed_on_the_node_that_advertises_host() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let mac = capable_worker(&store, "mac", host_capable()).await;
    let nuc = capable_worker(&store, "nuc", types::worker::NodeCapabilities::absent()).await;
    let fleet = FleetBackend::new(
        vec![
            DockerNodeConfig {
                name: "mac".into(),
                endpoint: "worker".into(),
                slots: 1,
            },
            DockerNodeConfig {
                name: "nuc".into(),
                endpoint: "worker".into(),
                slots: 1,
            },
        ],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();

    let mut host = suite::cfg("true");
    host.image = None;
    assert_eq!(
        fleet.launch(host).await.unwrap(),
        "mac/placed",
        "only mac advertises host mode"
    );
    assert_eq!(
        fleet.launch(suite::cfg("true")).await.unwrap(),
        "mac/placed",
        "a container launch is placed by load alone, and mac ties first by name"
    );
    mac.abort();
    nuc.abort();
}

/// A fleet no node of which advertises the mode answers the launch with the
/// configuration-error `NoCapacity` message, not the busy-fleet one (#309 §5a).
#[tokio::test]
async fn a_container_only_fleet_names_the_mode_it_cannot_serve() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let nuc = capable_worker(&store, "nuc", types::worker::NodeCapabilities::absent()).await;
    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "nuc".into(),
            endpoint: "worker".into(),
            slots: 1,
        }],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();

    let mut host = suite::cfg("true");
    host.image = None;
    let err = fleet.launch(host).await.unwrap_err();
    assert!(
        matches!(err, container::BackendError::NoCapacity(_)),
        "the mode is unserved, not the fleet busy: {err}"
    );
    assert!(
        err.to_string().starts_with("no node advertises host mode"),
        "{err}"
    );
    nuc.abort();
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
    let (refresh_git_url, refresh_git_key) = if refresh_script.is_some() {
        let key = dir.join("worker_git");
        std::fs::write(&key, b"fake-key").unwrap();
        (Some("ssh://git@front:2222/acme/chug.git".to_string()), key)
    } else {
        (None, "/data/keys/worker_git".into())
    };
    let config = WorkerConfig {
        node: node.into(),
        slots: 4,
        slots_max: DAEMON_SLOTS_MAX,
        modes: vec![WorkerMode::Container],
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: channel,
        cache_dir: None,
        host_root: std::env::temp_dir().join("chug-host-root-test"),
        kvm_device: None,
        kvm_projects: vec![],
        android_sdk_dir: ANDROID_SDK_DIR_DEFAULT.into(),
        flutter_dir: None,
        jdk_dir: None,
        nix_gcroots_dir: None,
        nix_projects: Vec::new(),
        nix_flake_client: "/nix/var/nix/profiles/system/sw/bin/nix".into(),
        nix_client: NIX_CLIENT_DEFAULT.into(),
        nix_daemon_socket: NIX_DAEMON_SOCKET_DEFAULT.into(),
        nix_store_dir: NIX_STORE_DIR_DEFAULT.into(),
        nix_realise_timeout_secs: NIX_REALISE_TIMEOUT_SECS_DEFAULT,
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
    refresh_script_with("exit 0\n")
}

/// Write an executable refresh script with `body`, invoked by the daemon as
/// `script <phase> [sha] [tag]` exactly as `worker-refresh.sh` is.
fn refresh_script_with(body: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "chug-fake-refresh-{}-{:x}.sh",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
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
        PlacementPolicy::default(),
    )
    .unwrap()
}

/// Wait for a daemon's subscription to be live (a ghost inspect round-trips).
/// RPC liveness is not KV-watchable, so a tightened async poll (#206 principle 3).
async fn await_reachable(fleet: &FleetBackend, node: &str) {
    test_utils::wait::poll_async_default(
        format!("worker daemon {node} to become reachable"),
        || async { fleet.inspect(&format!("{node}/deadbeef")).await.ok() },
    )
    .await;
}

/// A task's `wait` stream survives a daemon restart and still delivers the exit
/// (spec §3.1 drain guarantee): job containers run on Docker, not in the daemon,
/// so replacing the daemon mid-job leaves the container running and the
/// dispatcher's poll-based `wait` re-attaches over the new daemon.
#[tokio::test]
async fn wait_survives_daemon_restart() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }
    let daemon = spawn_daemon(&server, "w1", b"x", None);
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = fleet_over(store, "w1", 8).await;
    await_reachable(&fleet, "w1").await;

    let id = fleet.launch(suite::cfg("sleep 4; exit 7")).await.unwrap();

    let wait_fleet = &fleet;
    let waiter = wait_fleet.wait(&id);
    tokio::pin!(waiter);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    daemon.abort();
    let _ = daemon.await;
    let daemon2 = spawn_daemon(&server, "w1", b"x", None);
    await_reachable(&fleet, "w1").await;

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

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }

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

    test_utils::wait::poll_async(
        std::time::Duration::from_secs(30),
        "launches to be refused during the swap window",
        || async {
            match fleet.launch(suite::cfg("true")).await {
                Ok(id) => {
                    suite::rm(&id);
                    None
                }
                Err(e) => {
                    assert!(e.to_string().contains("refreshing"), "unexpected: {e}");
                    Some(())
                }
            }
        },
    )
    .await;
    daemon.abort();
}

/// `refresh_cancel` over the real RPC (spec §3.1, ticket #254) — the deploy's
/// abort path when the fan-out has already lost a node. Two properties make it
/// safe to fire at a fleet that is mid-build, and neither is checkable from the
/// gate's flag algebra alone:
///
/// 1. It only touches the refresh it NAMES. A node converging on some other
///    SHA — a concurrent deploy, a hand-run refresh — must not be aborted by
///    this deploy's cleanup, so the SHA in the request is a guard, not
///    decoration.
/// 2. When it does fire, the refresh ends under the `cancelled` stage, which is
///    what makes [`types::worker::REFRESH_STAGE_CANCELLED`] a contract rather
///    than a name only the daemon uses: the deploy's CLI reads that stage back
///    off `ping` and reports "FAILED at cancelled", so a node the deploy itself
///    stopped is never diagnosed as a node whose build is broken.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn refresh_cancel_aborts_only_its_own_sha() {
    use store::worker::WorkerRpc;
    use types::worker::{
        REFRESH_STAGE_CANCELLED, RefreshCancelRequest, RefreshRequest, RefreshResult,
    };

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }

    let script = refresh_script_with(concat!(
        "if [ \"$1\" = build ]; then\n",
        "  echo 'worker-refresh: phase build-image 1/3 worker'\n",
        "  sleep 120\n",
        "fi\n",
        "exit 0\n",
    ));
    let daemon = spawn_daemon(&server, "w1", b"x", Some(script));
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = fleet_over(store.clone(), "w1", 8).await;
    await_reachable(&fleet, "w1").await;
    let rpc = WorkerRpc::new(store, "w1");

    assert!(
        rpc.refresh(&RefreshRequest {
            sha: "cafef00d".into(),
            tag: "prod".into(),
        })
        .await
        .unwrap()
        .accepted
    );
    test_utils::wait::poll_async(
        std::time::Duration::from_secs(30),
        "the refresh build to start",
        || async {
            let progress = rpc.ping().await.ok()?.refresh_progress?;
            (progress.phase == "build-image 1/3 worker").then_some(())
        },
    )
    .await;

    let other = rpc
        .refresh_cancel(&RefreshCancelRequest {
            sha: "deadbeef".into(),
        })
        .await
        .unwrap();
    assert!(!other.cancelled, "a cancel for another sha must not fire");
    assert!(
        other.note.contains("cafef00d") && other.note.contains("deadbeef"),
        "the note must name both shas: {}",
        other.note
    );
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(
        rpc.ping().await.unwrap().refresh_outcome.unwrap().result,
        RefreshResult::InProgress,
        "a declined cancel must leave the refresh converging"
    );

    let ours = rpc
        .refresh_cancel(&RefreshCancelRequest {
            sha: "cafef00d".into(),
        })
        .await
        .unwrap();
    assert!(ours.cancelled, "unexpected decline: {}", ours.note);
    let stage = test_utils::wait::poll_async(
        std::time::Duration::from_secs(60),
        "the cancelled refresh to reach its terminal verdict",
        || async {
            match rpc.ping().await.ok()?.refresh_outcome?.result {
                RefreshResult::Failed { stage, .. } => Some(stage),
                _ => None,
            }
        },
    )
    .await;
    assert_eq!(
        stage, REFRESH_STAGE_CANCELLED,
        "a cancelled refresh must not be reported as a broken build"
    );
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

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }

    let dir = unique_temp_dir("chug-worker-skip");
    let channel = dir.join("chuggernaut-channel");
    std::fs::write(&channel, b"x").unwrap();
    let config = WorkerConfig {
        node: "w1".into(),
        slots: 4,
        slots_max: DAEMON_SLOTS_MAX,
        modes: vec![WorkerMode::Container],
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: channel,
        cache_dir: None,
        host_root: std::env::temp_dir().join("chug-host-root-test"),
        kvm_device: None,
        kvm_projects: vec![],
        android_sdk_dir: ANDROID_SDK_DIR_DEFAULT.into(),
        flutter_dir: None,
        jdk_dir: None,
        nix_gcroots_dir: None,
        nix_projects: Vec::new(),
        nix_flake_client: "/nix/var/nix/profiles/system/sw/bin/nix".into(),
        nix_client: NIX_CLIENT_DEFAULT.into(),
        nix_daemon_socket: NIX_DAEMON_SOCKET_DEFAULT.into(),
        nix_store_dir: NIX_STORE_DIR_DEFAULT.into(),
        nix_realise_timeout_secs: NIX_REALISE_TIMEOUT_SECS_DEFAULT,
        refresh_script: Some(fake_refresh_script()),
        refresh_git_url: None,
        refresh_git_key: dir.join("worker_git"),
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

    let id = fleet.launch(suite::cfg("true")).await.unwrap();
    suite::rm(&id);
    daemon.abort();
}

/// A node with per-task nix GC roots on, whose nix client never returns: every
/// boot precondition is present in `dir` so the daemon starts — including a
/// toolchain that RESOLVES INTO the node's store, the shape a parent bind
/// preserves — and a one-second realise bound makes the launch refusal prompt.
fn nix_rooted_config(dir: &std::path::Path, nats_url: &str) -> WorkerConfig {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir.join("gcroots")).unwrap();
    std::fs::create_dir_all(dir.join("store").join("aaaa-toolchain")).unwrap();
    std::os::unix::fs::symlink(
        dir.join("store").join("aaaa-toolchain"),
        dir.join("toolchain"),
    )
    .unwrap();
    std::fs::write(dir.join("socket"), b"").unwrap();
    std::fs::write(dir.join("chuggernaut-channel"), b"x").unwrap();
    let client = dir.join("nix-store");
    std::fs::write(&client, b"#!/bin/sh\nsleep 60\n").unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o755)).unwrap();

    WorkerConfig {
        node: "w1".into(),
        slots: 4,
        slots_max: DAEMON_SLOTS_MAX,
        modes: vec![WorkerMode::Container],
        nats_url: nats_url.to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: dir.join("chuggernaut-channel"),
        cache_dir: None,
        host_root: std::env::temp_dir().join("chug-host-root-test"),
        kvm_device: Some("/dev/null".into()),
        kvm_projects: vec!["acme/nix".into()],
        android_sdk_dir: dir.join("toolchain"),
        flutter_dir: None,
        jdk_dir: None,
        nix_gcroots_dir: Some(dir.join("gcroots")),
        nix_projects: Vec::new(),
        nix_flake_client: "/nix/var/nix/profiles/system/sw/bin/nix".into(),
        nix_client: client,
        nix_daemon_socket: dir.join("socket"),
        nix_store_dir: dir.join("store"),
        nix_realise_timeout_secs: 1,
        refresh_script: None,
        refresh_git_url: None,
        refresh_git_key: dir.join("worker_git"),
    }
}

/// A realise that breaks the node's bound REFUSES the launch over the real wire
/// (design #373 3c): the dispatcher-facing error is `BackendError::Launch`
/// naming `WORKER_NIX_REALISE_TIMEOUT_SECS`, never `NoCapacity` — a realise that
/// timed out will not be faster on a requeue — and no root survives the refusal.
#[tokio::test]
async fn a_realise_over_the_bound_refuses_the_launch_as_launch_not_capacity() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }

    let dir = unique_temp_dir("chug-worker-nix");
    let gcroots = dir.join("gcroots");
    let config = nix_rooted_config(&dir, server.url());
    let daemon = tokio::spawn(async move {
        if let Err(e) = worker::run(config).await {
            eprintln!("daemon exited: {e}");
        }
    });

    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let fleet = fleet_over(store, "w1", 8).await;
    await_reachable(&fleet, "w1").await;

    let mut admitted = suite::cfg("true");
    admitted.env.insert("JOB_PROJECT".into(), "acme/nix".into());
    admitted
        .env
        .insert("CHUG_TASK_ID".into(), "nixbound".into());
    let err = fleet
        .launch(admitted)
        .await
        .expect_err("a realise past the bound must refuse the launch");
    match &err {
        container::BackendError::Launch(message) => assert!(
            message.contains("WORKER_NIX_REALISE_TIMEOUT_SECS"),
            "the refusal must name the bound: {message}"
        ),
        other => panic!("a realise refusal must be Launch, got {other:?}"),
    }
    assert!(
        !gcroots.join("task-nixbound").exists(),
        "a refused realise leaves no root behind"
    );

    let unadmitted = fleet.launch(suite::cfg("true")).await.unwrap();
    suite::rm(&unadmitted);
    daemon.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `copy_file` reply bound over the real wire (design
/// `362-binary-artifacts.md` S0): a file over
/// [`store::worker::MAX_COPY_FILE_BYTES`] comes back PROMPTLY
/// as a named error where it used to publish a reply nothing could carry and
/// block the caller for the full 60s `OP_TIMEOUT`, and a file AT the bound
/// still round-trips whole.
#[tokio::test]
async fn oversized_copy_file_fails_fast_with_a_named_error() {
    use store::worker::{COPY_FILE_TOO_LARGE, MAX_COPY_FILE_BYTES};

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let Some((fleet, daemon)) = setup(&server, b"x").await else {
        return;
    };
    let over = MAX_COPY_FILE_BYTES + 1;
    let id = fleet
        .launch(suite::cfg(&format!(
            "head -c {MAX_COPY_FILE_BYTES} /dev/zero > /at-bound; head -c {over} /dev/zero > /over"
        )))
        .await
        .unwrap();
    assert_eq!(fleet.wait(&id).await.unwrap(), 0);

    let at_bound = fleet.copy_file(&id, "/at-bound").await.unwrap().unwrap();
    assert_eq!(
        at_bound.len(),
        MAX_COPY_FILE_BYTES,
        "a file at the bound must still round-trip whole"
    );

    let started = std::time::Instant::now();
    let err = fleet.copy_file(&id, "/over").await.unwrap_err();
    let elapsed = started.elapsed();
    let message = err.to_string();
    assert!(
        message.contains(COPY_FILE_TOO_LARGE),
        "the error must be named: {message}"
    );
    for expected in ["/over", &over.to_string(), &MAX_COPY_FILE_BYTES.to_string()] {
        assert!(
            message.contains(expected),
            "the error must carry {expected}: {message}"
        );
    }
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "an oversized copy_file must fail fast, not wait out OP_TIMEOUT: {elapsed:?}"
    );

    suite::rm(&id);
    daemon.abort();
}

/// The chunked read over the real wire (design `362-binary-artifacts.md` S1):
/// an output archive several single-shot replies long comes back whole and
/// byte-exact, one past the caller's own ceiling is refused with the same named
/// error, and an absent path is still `None` rather than a failure.
#[tokio::test]
async fn chunked_copy_file_carries_a_multi_reply_archive() {
    use store::worker::{COPY_FILE_TOO_LARGE, MAX_COPY_FILE_BYTES};

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let Some((fleet, daemon)) = setup(&server, b"x").await else {
        return;
    };
    let whole = MAX_COPY_FILE_BYTES * 3 + 17;
    let cap = whole + 1;
    let id = fleet
        .launch(suite::cfg(&format!(
            "head -c {whole} /dev/urandom > /out.tar.gz; sha256sum /out.tar.gz > /out.sha"
        )))
        .await
        .unwrap();
    assert_eq!(fleet.wait(&id).await.unwrap(), 0);

    let got = fleet
        .copy_file_chunked(&id, "/out.tar.gz", cap)
        .await
        .unwrap()
        .expect("the archive is present");
    assert_eq!(
        got.len(),
        whole,
        "a file spanning several replies must come back whole"
    );
    let expected = String::from_utf8(fleet.copy_file(&id, "/out.sha").await.unwrap().unwrap())
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(
        format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&got)),
        expected,
        "the reassembled archive must be byte-identical, not merely the right length"
    );

    let err = fleet
        .copy_file_chunked(&id, "/out.tar.gz", whole - 1)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(COPY_FILE_TOO_LARGE) && err.contains(&whole.to_string()),
        "a file past the caller's ceiling must be refused by name: {err}"
    );

    assert!(
        fleet
            .copy_file_chunked(&id, "/no-such-output.tar.gz", cap)
            .await
            .unwrap()
            .is_none(),
        "an absent archive is a silent None, never an error"
    );

    suite::rm(&id);
    daemon.abort();
}

/// A scripted worker daemon serving exactly the two ops the transcript harvest
/// takes (design #490 D1a), over one in-memory file, with
/// `serves_find_file: false` standing in for the N-1 node the caller must
/// degrade against. No Docker: the point is the **wire**, so this runs wherever
/// NATS does.
async fn transcript_daemon(
    store: &store::NatsStore,
    node: &str,
    path: &str,
    bytes: Vec<u8>,
    serves_find_file: bool,
) -> tokio::task::JoinHandle<()> {
    use store::worker::MAX_COPY_FILE_BYTES;
    use types::worker::{
        CopyFileChunkOk, CopyFileChunkRequest, FindFileOk, FindFileRequest, WorkerError,
        WorkerReply, b64_encode,
    };

    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(node))
        .await
        .unwrap();
    store.client().flush().await.unwrap();
    let path = path.to_string();
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            let body = if req.subject.ends_with(".find_file") && serves_find_file {
                let ask: FindFileRequest = serde_json::from_slice(&req.payload).unwrap();
                let hit = path.starts_with(&format!("{}/", ask.dir.trim_end_matches('/')))
                    && path.ends_with(&format!("/{}", ask.name));
                serde_json::to_vec(&WorkerReply::Ok {
                    value: FindFileOk {
                        paths: if hit { vec![path.clone()] } else { Vec::new() },
                    },
                })
                .unwrap()
            } else if req.subject.ends_with(".copy_file_chunk") {
                let ask: CopyFileChunkRequest = serde_json::from_slice(&req.payload).unwrap();
                let start = (ask.offset as usize).min(bytes.len());
                let end = start.saturating_add(MAX_COPY_FILE_BYTES).min(bytes.len());
                serde_json::to_vec(&WorkerReply::Ok {
                    value: CopyFileChunkOk {
                        data_b64: Some(b64_encode(&bytes[start..end])),
                        total_len: bytes.len() as u64,
                    },
                })
                .unwrap()
            } else {
                serde_json::to_vec(&WorkerReply::<()>::Err {
                    error: WorkerError::Other {
                        message: format!(
                            "{} {:?} on {}",
                            types::worker::UNKNOWN_OP,
                            req.subject.rsplit('.').next(),
                            req.subject
                        ),
                    },
                })
                .unwrap()
            };
            req.respond(body).await;
        }
    })
}

/// Design #490 slice 1 over the real wire, without a container runtime: the
/// transcript is resolved by the session id the platform supplied and then read
/// in slices, so a file **larger than** `copy_file`'s single-reply bound — the
/// size at which the platform has been silently losing every long work-agent
/// session — arrives whole and byte-exact.
#[tokio::test]
async fn the_resolved_transcript_survives_the_size_that_was_losing_it() {
    use store::worker::{MAX_COPY_FILE_BYTES, copy_file_over_bound};

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let dir = "/chuggernaut/claude/projects";
    let name = "0d9e-slice-one.jsonl";
    let path = format!("{dir}/-workspace/{name}");
    let size = MAX_COPY_FILE_BYTES * 2 + 17;
    let bytes: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
    let daemon = transcript_daemon(&store, "w1", &path, bytes.clone(), true).await;

    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "w1".into(),
            endpoint: "worker".into(),
            slots: 4,
        }],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();
    let id = "w1/c0ffee".to_string();

    assert!(
        copy_file_over_bound(&path, size).is_some(),
        "this test is only meaningful for a transcript the unchunked op refuses"
    );
    assert_eq!(
        fleet.find_file(&id, dir, name).await.unwrap(),
        vec![path.clone()],
        "the session id resolves without computing the CLI's directory slug"
    );
    let got = fleet
        .copy_file_chunked(&id, &path, store::MAX_BLOB_BYTES)
        .await
        .unwrap()
        .expect("the transcript is present");
    assert_eq!(
        got, bytes,
        "the transcript must arrive whole and byte-exact"
    );

    assert!(
        fleet
            .find_file(&id, dir, "absent.jsonl")
            .await
            .unwrap()
            .is_empty(),
        "a name nothing carries is an empty list, never an error"
    );
    daemon.abort();
}

/// The other half of D1a, and the reason the op ships with **no**
/// `WORKER_RPC_VERSION` bump: a daemon that predates it answers `unknown op`
/// rather than crashing, and the marker survives the wire into the caller's
/// error — which is what `Harvester::computed_fallback` degrades on.
#[tokio::test]
async fn a_daemon_that_predates_find_file_answers_unknown_op() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let daemon = transcript_daemon(&store, "w1", "/unused", Vec::new(), false).await;

    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "w1".into(),
            endpoint: "worker".into(),
            slots: 4,
        }],
        store,
        PlacementPolicy::default(),
    )
    .unwrap();
    let err = fleet
        .find_file(
            &"w1/c0ffee".to_string(),
            "/chuggernaut/claude/projects",
            "s.jsonl",
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(types::worker::UNKNOWN_OP) && err.contains("find_file"),
        "the caller degrades on this text, so it must name the op: {err}"
    );
    daemon.abort();
}

/// Design #490 slice 1's acceptance criterion, over the real wire: a transcript
/// **larger than** `copy_file`'s single-reply bound — the size at which the
/// platform has been silently dropping every long work-agent session — is
/// resolved by the session id the platform supplied and harvested whole. The
/// last assertion is the defect itself: the same file through the unchunked op
/// is still the named refusal the `Err` arm was warning about and discarding.
#[tokio::test]
async fn an_over_bound_transcript_resolves_by_session_id_and_survives_whole() {
    use store::worker::{COPY_FILE_TOO_LARGE, MAX_COPY_FILE_BYTES};

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let Some((fleet, daemon)) = setup(&server, b"x").await else {
        return;
    };
    let dir = "/chuggernaut/claude/projects";
    let session = "0d9e-slice-one.jsonl";
    let size = MAX_COPY_FILE_BYTES + 4096;
    let id = fleet
        .launch(suite::cfg(&format!(
            "mkdir -p {dir}/-workspace && head -c {size} /dev/urandom > {dir}/-workspace/{session}"
        )))
        .await
        .unwrap();
    assert_eq!(fleet.wait(&id).await.unwrap(), 0);

    let resolved = fleet.find_file(&id, dir, session).await.unwrap();
    assert_eq!(
        resolved,
        vec![format!("{dir}/-workspace/{session}")],
        "the session id resolves without computing the CLI's directory slug"
    );

    let bytes = fleet
        .copy_file_chunked(&id, &resolved[0], store::MAX_BLOB_BYTES)
        .await
        .unwrap()
        .expect("the transcript is present");
    assert_eq!(
        bytes.len(),
        size,
        "a transcript over the single-reply bound must arrive whole"
    );

    let err = fleet
        .copy_file(&id, &resolved[0])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(COPY_FILE_TOO_LARGE),
        "the unchunked read is the call that was losing it: {err}"
    );

    suite::rm(&id);
    daemon.abort();
}

#[tokio::test]
async fn payload_guard_rejects_bulk_inline_files() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let Some((fleet, daemon)) = setup(&server, b"x").await else {
        return;
    };
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

/// Read announces off the subject until `node` reports `slots`, bounded well
/// under [the daemon's ~15s heartbeat] so arriving at all proves the announce was
/// triggered by the adoption rather than by the next tick.
async fn await_announce(
    sub: &mut store::RequestSubscription,
    node: &str,
    slots: u32,
) -> types::worker::WorkerAnnounce {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let req = sub.next().await.expect("announce subject live");
            let announce: types::worker::WorkerAnnounce =
                serde_json::from_slice(&req.payload).unwrap();
            if announce.node == node && announce.slots == slots {
                return announce;
            }
        }
    })
    .await
    .expect("an adopted slot count must not wait for the next announce tick")
}

/// The `set_slots` round trip (spec §3.1 operator capacity control): a real
/// daemon over a real nats-server reports its capacity on `ping`, adopts a value
/// within `slots_max` while bumping the capacity generation, **re-announces
/// immediately** rather than waiting out the ~15s heartbeat, and REFUSES a value
/// above the ceiling with a reason the caller can surface — leaving the adopted
/// number in force. Tier 2 because the wire path is the contract: two transports
/// of one source, and the ordering key they share.
#[tokio::test]
async fn set_slots_adopts_re_announces_and_refuses_above_the_ceiling() {
    use store::worker::WorkerRpc;
    use types::worker::SetSlotsRequest;

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }
    let daemon = spawn_daemon(&server, "cap1", b"x", None);
    let store = store::NatsStore::connect(server.url()).await.unwrap();
    let rpc = WorkerRpc::new(store.clone(), "cap1");

    let ping = test_utils::wait::poll_async_default("worker cap1 to answer ping", || async {
        rpc.ping().await.ok()
    })
    .await;
    assert_eq!(ping.slots, Some(4), "the boot WORKER_SLOTS value");
    assert_eq!(ping.slots_max, Some(DAEMON_SLOTS_MAX));
    assert_eq!(ping.capacity_generation, Some(0), "nothing adopted yet");
    let epoch = ping.capacity_epoch.unwrap();
    assert!(epoch > 1_577_836_800_000, "epoch not in ms: {epoch}");

    let mut announces = store
        .subscribe_requests(&store::subjects::worker_announce())
        .await
        .unwrap();
    store.client().flush().await.unwrap();

    let adopted = rpc.set_slots(&SetSlotsRequest { slots: 2 }).await.unwrap();
    assert!(adopted.accepted, "2 is under the ceiling: {adopted:?}");
    assert_eq!(adopted.slots, 2);
    assert_eq!(adopted.slots_max, DAEMON_SLOTS_MAX);
    assert_eq!(
        adopted.capacity_generation, 1,
        "adoption bumps the generation"
    );
    assert_eq!(adopted.capacity_epoch, epoch, "the epoch is stamped once");
    assert_eq!(adopted.note, None);

    let announce = await_announce(&mut announces, "cap1", 2).await;
    assert_eq!(announce.slots_max, Some(DAEMON_SLOTS_MAX));
    assert_eq!(announce.capacity_epoch, Some(epoch));
    assert_eq!(
        announce.capacity_generation,
        Some(1),
        "push and pull carry the same ordering key"
    );

    let refused = rpc
        .set_slots(&SetSlotsRequest {
            slots: DAEMON_SLOTS_MAX + 1,
        })
        .await
        .unwrap();
    assert!(!refused.accepted, "the node is the authority: {refused:?}");
    assert!(
        refused
            .note
            .as_deref()
            .is_some_and(|n| n.contains(&DAEMON_SLOTS_MAX.to_string())),
        "the reason must carry the max for the UI: {refused:?}"
    );
    assert_eq!(refused.slots, 2, "the adopted value stays in force");
    assert_eq!(refused.capacity_generation, 1, "a refusal bumps nothing");

    let ping = rpc.ping().await.unwrap();
    assert_eq!((ping.slots, ping.capacity_generation), (Some(2), Some(1)));
    daemon.abort();
}

/// A host node whose capacity is not 1 refuses to come up, naming the settings
/// (design #309 P0). `local_backend` is the only place a `WORKER_MODES` entry
/// becomes a backend, so the §2 option (iii) exclusion is enforced there rather
/// than advertising slots two host tasks would collide on `/workspace` using.
#[tokio::test]
async fn host_mode_without_one_slot_refuses_to_start() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let config = WorkerConfig {
        node: "w1".into(),
        slots: 4,
        slots_max: DAEMON_SLOTS_MAX,
        modes: vec![WorkerMode::Container, WorkerMode::Host],
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: "/nonexistent/chuggernaut-channel".into(),
        cache_dir: None,
        host_root: std::env::temp_dir().join("chug-host-root-test"),
        kvm_device: None,
        kvm_projects: vec![],
        android_sdk_dir: ANDROID_SDK_DIR_DEFAULT.into(),
        flutter_dir: None,
        jdk_dir: None,
        nix_gcroots_dir: None,
        nix_projects: Vec::new(),
        nix_flake_client: "/nix/var/nix/profiles/system/sw/bin/nix".into(),
        nix_client: NIX_CLIENT_DEFAULT.into(),
        nix_daemon_socket: NIX_DAEMON_SOCKET_DEFAULT.into(),
        nix_store_dir: NIX_STORE_DIR_DEFAULT.into(),
        nix_realise_timeout_secs: NIX_REALISE_TIMEOUT_SECS_DEFAULT,
        refresh_script: None,
        refresh_git_url: None,
        refresh_git_key: "/data/keys/worker_git".into(),
    };
    let err = worker::run(config).await.unwrap_err().to_string();
    assert!(err.contains("WORKER_MODES"), "unexpected: {err}");
    assert!(err.contains("host"), "must name the mode: {err}");
    assert!(err.contains("WORKER_SLOTS=1"), "must name the fix: {err}");
    assert!(
        err.contains("one host task per node"),
        "must name the rule it enforces: {err}"
    );
}

/// A node that cannot create a supervision unit refuses to advertise `host`
/// (design #440 D3), because a daemon-parented task is killed by the restart
/// that swaps the daemon. Asserted only where the node genuinely cannot — on a
/// systemd host the probe succeeds and there is no refusal to observe.
#[tokio::test]
async fn host_mode_without_a_supervision_unit_refuses_to_start() {
    let Err(reason) = container::host::probe_supervision().await else {
        eprintln!("skipping: this machine CAN create a supervision unit, so there is no refusal");
        return;
    };
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let config = WorkerConfig {
        node: "w1".into(),
        slots: 1,
        slots_max: 1,
        modes: vec![WorkerMode::Host],
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: "/nonexistent/chuggernaut-channel".into(),
        cache_dir: None,
        host_root: std::env::temp_dir().join("chug-host-supervision-test"),
        kvm_device: None,
        kvm_projects: vec![],
        android_sdk_dir: ANDROID_SDK_DIR_DEFAULT.into(),
        flutter_dir: None,
        jdk_dir: None,
        nix_gcroots_dir: None,
        nix_projects: Vec::new(),
        nix_flake_client: "/nix/var/nix/profiles/system/sw/bin/nix".into(),
        nix_client: NIX_CLIENT_DEFAULT.into(),
        nix_daemon_socket: NIX_DAEMON_SOCKET_DEFAULT.into(),
        nix_store_dir: NIX_STORE_DIR_DEFAULT.into(),
        nix_realise_timeout_secs: NIX_REALISE_TIMEOUT_SECS_DEFAULT,
        refresh_script: None,
        refresh_git_url: None,
        refresh_git_key: "/data/keys/worker_git".into(),
    };
    let err = worker::run(config).await.unwrap_err().to_string();
    assert!(err.contains("w1"), "must name the node: {err}");
    assert!(
        err.contains("WORKER_MODES=host"),
        "must name the mode: {err}"
    );
    assert!(
        err.contains(&reason),
        "must carry the probe's reason: {err}"
    );
}

/// A node whose `WORKER_KVM` device is absent refuses to come up, naming the
/// device (design #367 §2.3) — the same fail-loud shape a declared mode without
/// a backend gets. A node that advertises a capability it cannot serve would
/// instead fail every allow-listed launch at container create, one job at a
/// time.
#[tokio::test]
async fn declared_kvm_without_the_device_refuses_to_start() {
    if !suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return;
    }
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let config = WorkerConfig {
        node: "w1".into(),
        slots: 4,
        slots_max: DAEMON_SLOTS_MAX,
        modes: vec![WorkerMode::Container],
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: "/nonexistent/chuggernaut-channel".into(),
        cache_dir: None,
        host_root: std::env::temp_dir().join("chug-host-root-test"),
        kvm_device: Some("/dev/definitely-not-kvm".into()),
        kvm_projects: vec!["acme/beacon".into()],
        android_sdk_dir: ANDROID_SDK_DIR_DEFAULT.into(),
        flutter_dir: None,
        jdk_dir: None,
        nix_gcroots_dir: None,
        nix_projects: Vec::new(),
        nix_flake_client: "/nix/var/nix/profiles/system/sw/bin/nix".into(),
        nix_client: NIX_CLIENT_DEFAULT.into(),
        nix_daemon_socket: NIX_DAEMON_SOCKET_DEFAULT.into(),
        nix_store_dir: NIX_STORE_DIR_DEFAULT.into(),
        nix_realise_timeout_secs: NIX_REALISE_TIMEOUT_SECS_DEFAULT,
        refresh_script: None,
        refresh_git_url: None,
        refresh_git_key: "/data/keys/worker_git".into(),
    };
    let err = worker::run(config).await.unwrap_err().to_string();
    assert!(err.contains("WORKER_KVM"), "unexpected: {err}");
    assert!(
        err.contains("/dev/definitely-not-kvm"),
        "must name the device: {err}"
    );
}

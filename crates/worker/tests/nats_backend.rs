//! Tier-2: the worker daemon + fleet backend against real NATS (container)
//! and the local Docker daemon. The fleet backend must satisfy the same
//! behavioral contract as DockerBackend, through the proxy.

use container::ContainerBackend;
use container::PlacementPolicy;
use container::docker::DockerNodeConfig;
use test_utils::backend_suite as suite;
use test_utils::nats::NatsTestServer;
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
        slots: 4,
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
        PlacementPolicy::default(),
    )
    .unwrap();

    // Wait for the daemon's subscription to be live: a ghost inspect answered
    // with an op-level result (Ok(None)) proves the round trip. RPC liveness is
    // not KV-watchable, so a tightened async poll (#206 principle 3).
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
                        refresh_outcome: None,
                        refresh_progress: None,
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
                        refresh_outcome: None,
                        refresh_progress: None,
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
        slots: 4,
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

    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
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

    // Once build+quiesce+swap have run, launches are refused. RPC-observed
    // state (not KV) → tightened async poll; reaching past it is the assertion
    // (#206 principle 3).
    test_utils::wait::poll_async(
        std::time::Duration::from_secs(30),
        "launches to be refused during the swap window",
        || async {
            match fleet.launch(suite::cfg("true")).await {
                Ok(id) => {
                    // Not quiesced yet (or a real container slipped in) — clean up.
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

    // A build that BLOCKS, and announces its phase first so the test can tell
    // the script is really running before it cancels (past that line the
    // daemon has published the build's process group). The `sleep` is a CHILD
    // of the script shell: only a process-GROUP signal takes it down — which is
    // exactly what the cancel promises, and what step 2 below measures.
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
    // RPC-observed state, so a tightened async poll (#206 principle 3).
    test_utils::wait::poll_async(
        std::time::Duration::from_secs(30),
        "the refresh build to start",
        || async {
            let progress = rpc.ping().await.ok()?.refresh_progress?;
            (progress.phase == "build-image 1/3 worker").then_some(())
        },
    )
    .await;

    // 1. A cancel naming a DIFFERENT sha is declined, and — the part that
    //    matters — changes nothing: the node keeps converging on its own target.
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

    // 2. A cancel naming OUR sha fires, and the refresh reaches its terminal
    //    verdict under the `cancelled` stage. Reaching it QUICKLY is also the
    //    proof that the whole process group died: the script's `sleep 120`
    //    outlives a signal sent to the shell alone, so a kill that missed the
    //    group would leave this poll to time out.
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
        slots: 4,
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
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
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

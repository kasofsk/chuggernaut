//! Tier-2 tests for runtime dynamic worker registration (spec §3.1): a worker
//! announces itself over NATS and the dispatcher merges it into the live fleet
//! with no restart. Announcements arrive as `Msg::WorkerAnnounce` into the
//! single-writer actor (driven here via `CoreHandle::announce_worker`), so
//! scheduling and the NoCapacity launch queue see the new capacity immediately.
//! Heartbeat loss marks a node unschedulable without touching its running
//! containers. The `FakeBackend` models capacity: `register_worker` supplies it
//! (clears the NoCapacity refusal), `mark_worker_unschedulable` removes it.

use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateJobRequest, spawn};
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{FleetStatus, WorkerNode};

const CMD_WORK: &str = r#"
name: cmd-work
image: img:latest
work:
  type: command
  run: ./build.sh
"#;

fn worker_node(name: &str, slots: u32, version: Option<&str>) -> WorkerNode {
    WorkerNode {
        name: name.into(),
        endpoint: "worker".into(),
        slots,
        available: true,
        version: version.map(Into::into),
    }
}

fn running(id: &str, job: u64, task: u64) -> container::RunningContainer {
    container::RunningContainer {
        id: id.into(),
        project: Some("acme/api".into()),
        job: Some(job),
        task: Some(task),
    }
}

/// Spawn a core with the given seed roster, heartbeat timeout, and backend.
async fn spawn_core(
    server: &test_utils::nats::NatsTestServer,
    store: &NatsStore,
    roster: Vec<WorkerNode>,
    heartbeat_timeout: Option<Duration>,
    backend: Arc<FakeBackend>,
) -> (CoreHandle, TempRepo) {
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/cmd-work.yaml", CMD_WORK.as_bytes(), "type")
        .await;
    clone.push("main").await;

    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        backend,
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            worker_heartbeat_timeout: heartbeat_timeout,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_fleet_roster(roster);
    (spawn(core), repo)
}

async fn release_cmd_work(handle: &CoreHandle) -> u64 {
    let created = handle
        .create_job(CreateJobRequest {
            owner: "acme".into(),
            project: "api".into(),
            r#type: "cmd-work".into(),
            title: String::new(),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            knowledge_tags: vec![],
            eval: vec![],
            timeout: None,
            model: None,
            factory: None,
            draft: false,
        })
        .await
        .unwrap();
    handle.release_job("acme", "api", created.id).await.unwrap();
    created.id
}

async fn wait_for_fleet(store: &NatsStore, pred: impl Fn(&FleetStatus) -> bool) -> FleetStatus {
    let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    for _ in 0..100 {
        if let Some(fleet) = bucket
            .get_json::<FleetStatus>("fleet.status")
            .await
            .unwrap()
            && pred(&fleet)
        {
            return fleet;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for fleet.status");
}

/// Poll a backend predicate (e.g. a launch appearing) until it holds.
async fn wait_until(pred: impl Fn() -> bool) {
    for _ in 0..100 {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for backend condition");
}

fn node<'a>(fleet: &'a FleetStatus, name: &str) -> &'a types::FleetNode {
    fleet
        .nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("node {name} missing from {:?}", fleet.nodes))
}

/// A launch the fleet refuses for no capacity is queued; a worker announcing
/// afterward supplies capacity, and the queued launch drains onto it with no
/// restart — the core of dynamic registration.
#[tokio::test]
async fn announce_adds_capacity_and_drains_launch_queue() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // A one-node fleet that is at capacity: every launch is refused.
    let backend = Arc::new(FakeBackend::new());
    backend.fail_launch_no_capacity_if(|_| Some("full".into()));
    let (handle, _repo) = spawn_core(
        &server,
        &store,
        vec![worker_node("air", 1, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;

    // The command work launch is refused and queued.
    release_cmd_work(&handle).await;
    wait_for_fleet(&store, |f| f.queue_depth >= 1).await;
    assert!(backend.launches().is_empty(), "nothing launched while full");

    // A new worker announces: capacity appears, and the actor re-drains the
    // launch queue on the same turn — the queued launch fires onto the fleet.
    handle
        .announce_worker("nuc".into(), 2, "0.1.0+nuc".into())
        .await
        .unwrap();

    wait_until(|| !backend.launches().is_empty()).await;
    let fleet = wait_for_fleet(&store, |f| f.queue_depth == 0).await;
    assert_eq!(fleet.queue_depth, 0, "queue drained once capacity appeared");
    // The announce reached the backend, and the new node is live in the roster.
    assert!(
        backend
            .registered()
            .iter()
            .any(|(n, s, _)| n == "nuc" && *s == 2)
    );
    assert_eq!(node(&fleet, "nuc").slots, Some(2));
}

/// Heartbeat loss stops NEW placements on a dynamically-announced node but never
/// touches a container already running there: the running slot stays tracked in
/// occupancy, and a fresh launch queues for other capacity instead.
#[tokio::test]
async fn heartbeat_loss_stops_placement_but_preserves_running() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // A container is already running on `nuc` (seeded into the live set).
    let backend = Arc::new(FakeBackend::new());
    backend.seed_managed_running([running("nuc/live", 7, 1)]);

    // Zero-seed fleet with a tiny heartbeat timeout so the very next scan after
    // an announce treats it as lapsed.
    let (handle, _repo) = spawn_core(
        &server,
        &store,
        vec![],
        Some(Duration::from_millis(1)),
        backend.clone(),
    )
    .await;

    // `nuc` announces: it joins the fleet and supplies capacity.
    handle
        .announce_worker("nuc".into(), 2, "0.1.0+nuc".into())
        .await
        .unwrap();
    // Its running container shows occupied.
    wait_for_fleet(&store, |f| {
        f.nodes.iter().any(|n| n.name == "nuc" && n.occupied == 1)
    })
    .await;

    // The heartbeat lapses (no re-announce): the scan marks `nuc` unschedulable.
    handle.trigger_scan().await.unwrap();
    wait_until(|| backend.unschedulable().iter().any(|n| n == "nuc")).await;

    // A new launch finds no capacity and queues — placement stopped …
    release_cmd_work(&handle).await;
    let fleet = wait_for_fleet(&store, |f| f.queue_depth >= 1).await;
    // … while the already-running container is still tracked, and the node reads
    // as unavailable (down) rather than gone.
    let nuc = node(&fleet, "nuc");
    assert_eq!(nuc.occupied, 1, "running container preserved");
    assert!(!nuc.available, "deregistered node shows down");
    assert!(
        backend.killed().is_empty(),
        "a lost node's container is never killed"
    );
}

/// Static seed and a live announcement merge by name, and the live announcement
/// wins: re-announcing a seeded worker updates its slot count and version in the
/// live fleet, and a brand-new name joins.
#[tokio::test]
async fn static_and_dynamic_merge_precedence() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    // Seed `air` at 4 slots (a DOCKER_NODES worker entry).
    let (handle, _repo) = spawn_core(
        &server,
        &store,
        vec![worker_node("air", 4, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;
    wait_for_fleet(&store, |f| f.nodes.iter().any(|n| n.name == "air")).await;

    // Re-announce `air` at 5 slots (the air 4→5 case): the live value wins.
    handle
        .announce_worker("air".into(), 5, "0.2.0+air".into())
        .await
        .unwrap();
    // And a brand-new node joins.
    handle
        .announce_worker("nuc".into(), 2, "0.1.0+nuc".into())
        .await
        .unwrap();

    let fleet = wait_for_fleet(&store, |f| {
        node_slots(f, "air") == Some(Some(5)) && f.nodes.iter().any(|n| n.name == "nuc")
    })
    .await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(5), "announcement wins over the seed");
    assert_eq!(air.version.as_deref(), Some("0.2.0+air"));
    assert_eq!(node(&fleet, "nuc").slots, Some(2));
    // Both announces reached the backend registry.
    let reg = backend.registered();
    assert!(reg.iter().any(|(n, s, _)| n == "air" && *s == 5));
    assert!(reg.iter().any(|(n, s, _)| n == "nuc" && *s == 2));
}

/// The #60 interaction: the dispatcher boots with zero configured nodes and
/// starts fine; a launch queues via NoCapacity until a worker announces, then
/// drains — capacity appears entirely at runtime.
#[tokio::test]
async fn zero_seed_boot_then_announce() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // No capacity at all: launches are refused until a node announces.
    let backend = Arc::new(FakeBackend::new());
    backend.fail_launch_no_capacity_if(|_| Some("no nodes yet".into()));
    let (handle, _repo) = spawn_core(&server, &store, vec![], None, backend.clone()).await;

    // A launch on the empty fleet queues rather than failing.
    release_cmd_work(&handle).await;
    wait_for_fleet(&store, |f| f.queue_depth >= 1).await;

    // The first worker announces: it becomes live fleet membership and its
    // capacity drains the queued launch.
    handle
        .announce_worker("air".into(), 4, "0.1.0+air".into())
        .await
        .unwrap();
    wait_until(|| !backend.launches().is_empty()).await;
    let fleet = wait_for_fleet(&store, |f| f.queue_depth == 0).await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(4));
    assert!(air.available);
}

/// A backend that cannot route to announced nodes (single-node Docker) drops a
/// stray announce: no phantom worker is inserted into the roster, so the fleet
/// snapshot never grows a node the backend could never place work on.
#[tokio::test]
async fn non_fleet_backend_drops_stray_announce() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn() else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // A backend that does not support dynamic workers (a Docker deployment).
    let backend = Arc::new(FakeBackend::new());
    backend.disable_dynamic_workers();
    let (handle, _repo) = spawn_core(&server, &store, vec![], None, backend.clone()).await;

    // Its boot fleet publishes with no nodes.
    wait_for_fleet(&store, |f| f.nodes.is_empty()).await;

    // A misconfigured worker announces — the dispatcher must drop it.
    handle
        .announce_worker("ghost".into(), 2, "0.1.0+ghost".into())
        .await
        .unwrap();
    // Give the actor a turn to process and republish (a Ping round-trips it).
    handle.ping().await.unwrap();

    let fleet = wait_for_fleet(&store, |_| true).await;
    assert!(
        !fleet.nodes.iter().any(|n| n.name == "ghost"),
        "phantom worker leaked into the roster: {:?}",
        fleet.nodes
    );
    assert!(
        backend.registered().is_empty(),
        "announce should not reach a non-fleet backend"
    );
}

/// Helper: the fleet node's `slots` field wrapped so `Some(None)` (present but
/// capless) and absent are distinguishable in the predicate above.
fn node_slots(fleet: &FleetStatus, name: &str) -> Option<Option<u32>> {
    fleet.nodes.iter().find(|n| n.name == name).map(|n| n.slots)
}

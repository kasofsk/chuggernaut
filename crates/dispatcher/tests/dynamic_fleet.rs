//! Tier-2 tests for runtime dynamic worker registration (spec §3.1): a worker
//! announces itself over NATS and the dispatcher merges it into the live fleet
//! with no restart. Announcements arrive as `Msg::WorkerAnnounce` into the
//! single-writer actor (driven here via `CoreHandle::announce_worker`), so
//! scheduling and the NoCapacity launch queue see the new capacity immediately.
//! Heartbeat loss marks a node unschedulable without touching its running
//! containers. The `FakeBackend` models capacity: `register_worker` supplies it
//! (clears the NoCapacity refusal), `mark_worker_unschedulable` removes it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateSpec};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{FleetStatus, Job, JobState, Task, TaskKind, TaskPhase, TaskState, WorkerNode};

mod common;
use common::{assert_invariants_of, spawn_checked};

const CMD_WORK: &str = r#"
name: cmd-work
image: img:latest
work:
  type: command
  run: ./build.sh
"#;

/// Job/task records backing a seeded live container, so the §3.6 startup sweep
/// classifies it as a re-attachable live task instead of reaping it as an
/// orphan (the race that made the heartbeat test flaky).
fn seeded_job(id: u64) -> Job {
    Job {
        id,
        project: "acme/api".into(),
        r#type: "cmd-work".into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        members: vec![],
        batch_id: None,
        state: JobState::Work,
        branch: format!("job/{id}"),
        base_ref: None,
        knowledge_tags: vec![],
        eval: vec![],
        require_approval: false,
        timeout: None,
        model: None,
        claim_next: false,
        escalation: None,
        factory: None,
        schedule: None,
        created_at: Utc::now(),
        ready_at: Some(Utc::now()),
        completed_at: None,
        inputs: Default::default(),
        groups: vec![],
        task_time_ms: None,
    }
}

fn seeded_work_task(id: u64, job_seq: u64, container_id: &str) -> Task {
    Task {
        id,
        job_seq,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Agent {
            provider: "claude".into(),
            model: None,
            prompt: "prompts/impl.md".into(),
        },
        state: TaskState::Running,
        attempt: 1,
        evaluator: None,
        stage: 0,
        performed_by: None,
        label: None,
        container_id: Some(container_id.into()),
        rework_reason: None,
        infra_loss: false,
        session_id: None,
        reviewed_tip: None,
        result: None,
        created_at: Utc::now(),
        started_at: Some(Utc::now()),
        completed_at: None,
        pending_reason: None,
        queued_at: None,
    }
}

/// A node's announce heartbeat, ordered at `(epoch, generation)` — the pair the
/// dispatcher sequences observations by (spec §3.1 slot source). The default
/// pair here is a fresh epoch at generation 0, i.e. what a just-started daemon
/// publishes; tests that care about ordering set it explicitly.
fn announce(node: &str, slots: u32, version: &str) -> types::worker::WorkerAnnounce {
    announce_at(node, slots, version, (1_000, 0))
}

fn announce_at(
    node: &str,
    slots: u32,
    version: &str,
    (epoch, generation): (u64, u64),
) -> types::worker::WorkerAnnounce {
    types::worker::WorkerAnnounce {
        node: node.into(),
        slots,
        slots_max: Some(8),
        capacity_epoch: Some(epoch),
        capacity_generation: Some(generation),
        version: version.into(),
    }
}

fn worker_node(name: &str, slots: u32, version: Option<&str>) -> WorkerNode {
    WorkerNode {
        name: name.into(),
        endpoint: "worker".into(),
        slots,
        available: true,
        version: version.map(Into::into),
        refresh_outcome: None,
        capacity_source: None,
        capacity_observed_at: None,
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
) -> (CoreHandle, TempRepo, InvariantSink) {
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
    let (handle, sink) = spawn_checked(core);
    (handle, repo, sink)
}

async fn release_cmd_work(handle: &CoreHandle, sink: &InvariantSink) -> u64 {
    let created = handle
        .create_job(CreateSpec {
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
            require_approval: false,
            timeout: None,
            model: None,
            factory: None,
            schedule: None,
            inputs: Default::default(),
            groups: vec![],
            draft: false,
        })
        .await
        .unwrap();
    assert_invariants_of(sink);
    handle.release_job("acme", "api", created.id).await.unwrap();
    assert_invariants_of(sink);
    created.id
}

async fn wait_for_fleet(store: &NatsStore, pred: impl Fn(&FleetStatus) -> bool) -> FleetStatus {
    let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    let watch = bucket.watch("fleet.status").await.unwrap();
    let initial = || async {
        bucket
            .get_json::<FleetStatus>("fleet.status")
            .await
            .unwrap()
    };
    test_utils::wait::kv_wait::<FleetStatus, _, _, _, _>(
        watch,
        test_utils::wait::DEFAULT_TIMEOUT,
        "fleet.status matching predicate",
        initial,
        |fleet| pred(fleet).then(|| fleet.clone()),
    )
    .await
}

/// Wait for a backend predicate (e.g. a launch appearing) — in-memory state, so
/// a tightened poll (#206 principle 3).
async fn wait_until(pred: impl Fn() -> bool) {
    test_utils::wait::poll_default("backend condition", || pred().then_some(())).await;
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
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.fail_launch_no_capacity_if(|_| Some("full".into()));
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 1, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;

    release_cmd_work(&handle, &sink).await;
    wait_for_fleet(&store, |f| f.queue_depth >= 1).await;
    assert!(backend.launches().is_empty(), "nothing launched while full");

    handle
        .announce_worker(announce("nuc", 2, "0.1.0+nuc"))
        .await
        .unwrap();
    assert_invariants_of(&sink);

    wait_until(|| !backend.launches().is_empty()).await;
    let fleet = wait_for_fleet(&store, |f| f.queue_depth == 0).await;
    assert_eq!(fleet.queue_depth, 0, "queue drained once capacity appeared");
    assert!(
        backend
            .registered()
            .iter()
            .any(|(n, s, _)| n == "nuc" && *s == 2)
    );
    assert_eq!(node(&fleet, "nuc").slots, Some(2));
    assert_invariants_of(&sink);
}

/// Heartbeat loss stops NEW placements on a dynamically-announced node but never
/// touches a container already running there: the running slot stays tracked in
/// occupancy, and a fresh launch queues for other capacity instead.
#[tokio::test]
async fn heartbeat_loss_stops_placement_but_preserves_running() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.seed_managed_running([running("nuc/live", 7, 1)]);
    backend.seed_running(["nuc/live".to_string()]);
    store
        .jobs()
        .await
        .unwrap()
        .put(&seeded_job(7))
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&seeded_work_task(1, 7, "nuc/live"))
        .await
        .unwrap();

    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![],
        Some(Duration::from_millis(1)),
        backend.clone(),
    )
    .await;

    handle
        .announce_worker(announce("nuc", 2, "0.1.0+nuc"))
        .await
        .unwrap();
    assert_invariants_of(&sink);
    wait_for_fleet(&store, |f| {
        f.nodes.iter().any(|n| n.name == "nuc" && n.occupied == 1)
    })
    .await;

    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    wait_until(|| backend.unschedulable().iter().any(|n| n == "nuc")).await;

    release_cmd_work(&handle, &sink).await;
    let fleet = wait_for_fleet(&store, |f| f.queue_depth >= 1).await;
    let nuc = node(&fleet, "nuc");
    assert_eq!(nuc.occupied, 1, "running container preserved");
    assert!(!nuc.available, "deregistered node shows down");
    assert!(
        backend.killed().is_empty(),
        "a lost node's container is never killed"
    );
    assert_invariants_of(&sink);
}

/// Static seed and a live announcement merge by name, and the live announcement
/// wins: re-announcing a seeded worker updates its slot count and version in the
/// live fleet, and a brand-new name joins.
#[tokio::test]
async fn static_and_dynamic_merge_precedence() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 4, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;
    wait_for_fleet(&store, |f| f.nodes.iter().any(|n| n.name == "air")).await;

    handle
        .announce_worker(announce("air", 5, "0.2.0+air"))
        .await
        .unwrap();
    assert_invariants_of(&sink);
    handle
        .announce_worker(announce("nuc", 2, "0.1.0+nuc"))
        .await
        .unwrap();
    assert_invariants_of(&sink);

    let fleet = wait_for_fleet(&store, |f| {
        node_slots(f, "air") == Some(Some(5)) && f.nodes.iter().any(|n| n.name == "nuc")
    })
    .await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(5), "announcement wins over the seed");
    assert_eq!(air.version.as_deref(), Some("0.2.0+air"));
    assert_eq!(node(&fleet, "nuc").slots, Some(2));
    let reg = backend.registered();
    assert!(reg.iter().any(|(n, s, _)| n == "air" && *s == 5));
    assert!(reg.iter().any(|(n, s, _)| n == "nuc" && *s == 2));
    assert_invariants_of(&sink);
}

/// The ordering pair survives the whole announce path — subscriber → `Msg` →
/// actor → backend — and the fleet snapshot carries the provenance (spec §3.1
/// slot source, design #293 §7/§8). A seeded node reads `seed` until it reports;
/// a stale re-announce (same epoch, lower generation) is discarded rather than
/// lowering the number the fleet is placing on.
#[tokio::test]
async fn announce_ordering_and_provenance_reach_the_snapshot() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.set_fleet_status([container::NodeStatus {
        name: "air".into(),
        available: true,
        version: Some("0.1.0+air".into()),
        refresh_outcome: None,
        slots: Some(2),
        capacity: Some(types::worker::ObservedCapacity::default()),
    }]);
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 2, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;

    let seeded = wait_for_fleet(&store, |f| f.nodes.iter().any(|n| n.name == "air")).await;
    let air = node(&seeded, "air");
    assert_eq!(air.slots, Some(2));
    assert_eq!(air.capacity_source, Some(types::CapacitySource::Seed));
    assert_eq!(air.capacity_observed_at, None);

    handle
        .announce_worker(announce_at("air", 4, "0.2.0+air", (1_000, 3)))
        .await
        .unwrap();
    assert_invariants_of(&sink);
    let observed = wait_for_fleet(&store, |f| node_slots(f, "air") == Some(Some(4))).await;
    let air = node(&observed, "air");
    assert_eq!(air.capacity_source, Some(types::CapacitySource::Node));
    assert!(air.capacity_observed_at.is_some());

    handle
        .announce_worker(announce_at("air", 1, "0.2.0+air", (1_000, 2)))
        .await
        .unwrap();
    handle.ping().await.unwrap();
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    let after = wait_for_fleet(&store, |_| true).await;
    assert_eq!(
        node(&after, "air").slots,
        Some(4),
        "a stale announce lowered the fleet's capacity"
    );
}

/// The #60 interaction: the dispatcher boots with zero configured nodes and
/// starts fine; a launch queues via NoCapacity until a worker announces, then
/// drains — capacity appears entirely at runtime.
#[tokio::test]
async fn zero_seed_boot_then_announce() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.fail_launch_no_capacity_if(|_| Some("no nodes yet".into()));
    let (handle, _repo, sink) = spawn_core(server, &store, vec![], None, backend.clone()).await;

    release_cmd_work(&handle, &sink).await;
    wait_for_fleet(&store, |f| f.queue_depth >= 1).await;

    handle
        .announce_worker(announce("air", 4, "0.1.0+air"))
        .await
        .unwrap();
    assert_invariants_of(&sink);
    wait_until(|| !backend.launches().is_empty()).await;
    let fleet = wait_for_fleet(&store, |f| f.queue_depth == 0).await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(4));
    assert!(air.available);
    assert_invariants_of(&sink);
}

/// A backend that cannot route to announced nodes (single-node Docker) drops a
/// stray announce: no phantom worker is inserted into the roster, so the fleet
/// snapshot never grows a node the backend could never place work on.
#[tokio::test]
async fn non_fleet_backend_drops_stray_announce() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.disable_dynamic_workers();
    let (handle, _repo, sink) = spawn_core(server, &store, vec![], None, backend.clone()).await;

    wait_for_fleet(&store, |f| f.nodes.is_empty()).await;

    handle
        .announce_worker(announce("ghost", 2, "0.1.0+ghost"))
        .await
        .unwrap();
    assert_invariants_of(&sink);
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
    assert_invariants_of(&sink);
}

/// A worker's ping-reported refresh outcome (ticket #187) flows through the
/// backend's live `fleet_status` into the published `FleetStatus`, so a failed
/// self-refresh becomes durable, queryable platform state rather than a
/// node-local log line. A node with no reported outcome stays `None` (absent-
/// field back-compat: an old ping omits the field → no outcome).
#[tokio::test]
async fn ping_refresh_outcome_lands_in_fleet_status() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.set_fleet_status([
        container::NodeStatus {
            name: "air".into(),
            available: true,
            version: Some("0.1.0+old".into()),
            refresh_outcome: Some(types::worker::RefreshOutcome {
                accepted_at: chrono::Utc::now(),
                finished_at: Some(chrono::Utc::now()),
                result: types::worker::RefreshResult::Failed {
                    stage: "build".into(),
                    error_tail: "cargo build exited 101".into(),
                },
                from_sha: "old".into(),
                to_sha: "target".into(),
            }),
            slots: Some(4),
            capacity: Some(types::worker::ObservedCapacity {
                mark: (1_000, 0),
                slots_max: Some(8),
                observed_at: Some(chrono::Utc::now()),
            }),
        },
        container::NodeStatus {
            name: "nuc".into(),
            available: true,
            version: Some("0.1.0+old".into()),
            refresh_outcome: None,
            slots: Some(2),
            capacity: Some(types::worker::ObservedCapacity {
                mark: (1_000, 0),
                slots_max: Some(8),
                observed_at: Some(chrono::Utc::now()),
            }),
        },
    ]);
    let (handle, _repo, sink) = spawn_core(server, &store, vec![], None, backend.clone()).await;

    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);

    let fleet = wait_for_fleet(&store, |f| {
        f.nodes
            .iter()
            .any(|n| n.name == "air" && n.refresh_outcome.is_some())
    })
    .await;

    let air = node(&fleet, "air");
    match air.refresh_outcome.as_ref().map(|o| &o.result) {
        Some(types::worker::RefreshResult::Failed { stage, error_tail }) => {
            assert_eq!(stage, "build");
            assert!(error_tail.contains("101"), "error tail: {error_tail}");
        }
        other => panic!("expected a failed refresh outcome for air, got {other:?}"),
    }
    assert_eq!(air.refresh_outcome.as_ref().unwrap().to_sha, "target");

    assert!(
        node(&fleet, "nuc").refresh_outcome.is_none(),
        "a node with no reported refresh outcome must read None"
    );
    assert_invariants_of(&sink);
}

/// Helper: the fleet node's `slots` field wrapped so `Some(None)` (present but
/// capless) and absent are distinguishable in the predicate above.
fn node_slots(fleet: &FleetStatus, name: &str) -> Option<Option<u32>> {
    fleet.nodes.iter().find(|n| n.name == name).map(|n| n.slots)
}

/// A docker-endpoint node the operator might try to edit — `DOCKER_NODES` owns
/// its capacity outright (design #293 §7), so the edit must be refused rather
/// than silently doing nothing.
fn docker_node(name: &str, slots: u32) -> WorkerNode {
    WorkerNode {
        endpoint: "unix:///var/run/docker.sock".into(),
        ..worker_node(name, slots, None)
    }
}

/// The command path end to end (design #293 §3): the operator's ask is persisted
/// as intent in the `platform` bucket, pushed to the node without the actor ever
/// blocking on the RPC, and the node's adoption converges the snapshot — with
/// `slots` (what the scheduler uses) and `slots_desired` (what was asked)
/// reported as the distinct things they are.
#[tokio::test]
async fn capacity_intent_is_persisted_pushed_and_converges() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 2, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;
    handle
        .announce_worker(announce("air", 4, "0.1.0+air"))
        .await
        .unwrap();
    wait_for_fleet(&store, |f| node_slots(f, "air") == Some(Some(4))).await;

    let ack = handle
        .set_node_capacity("air", 2, "operator@example.com")
        .await
        .unwrap();
    assert_eq!(ack.desired, 2);
    assert_eq!(
        ack.observed,
        Some(4),
        "the 202 names what is still in force"
    );
    assert_eq!(ack.state, types::CapacityState::Pending);
    assert_invariants_of(&sink);

    let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    let record: types::FleetCapacity = bucket
        .get_json("fleet.capacity")
        .await
        .unwrap()
        .expect("intent persisted");
    let air_intent = record.nodes.get("air").expect("air intent");
    assert_eq!(air_intent.slots, 2);
    assert_eq!(air_intent.set_by, "operator@example.com");

    wait_until(|| backend.slot_commands() == vec![("air".to_string(), 2)]).await;
    let fleet = wait_for_fleet(&store, |f| {
        f.nodes
            .iter()
            .any(|n| n.name == "air" && n.capacity_state == Some(types::CapacityState::Converged))
    })
    .await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(2), "the scheduler follows the observation");
    assert_eq!(air.slots_desired, Some(2));
    assert_eq!(air.capacity_note, None);
    assert_invariants_of(&sink);
}

/// A capacity edit against a docker-endpoint node is a **409**, and against a
/// node the fleet does not hold a 404 — never a silent no-op (design #293 §7).
#[tokio::test]
async fn capacity_edit_is_refused_for_docker_and_unknown_nodes() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![docker_node("local", 4), worker_node("air", 2, None)],
        None,
        backend.clone(),
    )
    .await;

    let conflict = handle
        .set_node_capacity("local", 1, "operator@example.com")
        .await
        .expect_err("a docker endpoint must refuse the edit");
    assert!(
        matches!(conflict, dispatcher::core::CoreError::Conflict(_)),
        "expected 409 Conflict, got {conflict:?}"
    );
    let missing = handle
        .set_node_capacity("ghost", 1, "operator@example.com")
        .await
        .expect_err("an unknown node must 404");
    assert!(
        matches!(missing, dispatcher::core::CoreError::NotFound(_)),
        "expected 404 NotFound, got {missing:?}"
    );
    assert!(backend.slot_commands().is_empty());
    let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    assert!(
        bucket
            .get_json::<types::FleetCapacity>("fleet.capacity")
            .await
            .unwrap()
            .is_none(),
        "a refused edit must record no intent"
    );
    assert_invariants_of(&sink);
}

/// A refusal is terminal (design #293 §4): the reason surfaces in the snapshot
/// and the dispatcher stops re-pushing a number the node will not take — however
/// many scan ticks pass — until the operator changes it.
#[tokio::test]
async fn refused_capacity_is_terminal_and_surfaces_its_reason() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.script_slot_reply(
        "air",
        test_utils::SlotReply::Refuse {
            slots_max: 4,
            note: "requested 8 slots exceeds this node's maximum of 4".into(),
        },
    );
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 2, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;
    handle
        .announce_worker(announce("air", 2, "0.1.0+air"))
        .await
        .unwrap();
    wait_for_fleet(&store, |f| node_slots(f, "air") == Some(Some(2))).await;

    handle
        .set_node_capacity("air", 8, "operator@example.com")
        .await
        .unwrap();
    let fleet = wait_for_fleet(&store, |f| {
        f.nodes
            .iter()
            .any(|n| n.name == "air" && n.capacity_state == Some(types::CapacityState::Rejected))
    })
    .await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(2), "the refused value never took effect");
    assert_eq!(air.slots_desired, Some(8));
    assert!(
        air.capacity_note
            .as_deref()
            .is_some_and(|note| note.contains("maximum of 4")),
        "the daemon's reason must reach the UI: {:?}",
        air.capacity_note
    );

    for _ in 0..10 {
        handle.trigger_scan().await.unwrap();
    }
    assert_eq!(
        backend.slot_commands(),
        vec![("air".to_string(), 8)],
        "a refused number must never be re-pushed"
    );

    handle
        .set_node_capacity("air", 4, "operator@example.com")
        .await
        .unwrap();
    wait_until(|| backend.slot_commands().len() == 2).await;
    assert_eq!(backend.slot_commands()[1], ("air".to_string(), 4));
    assert_invariants_of(&sink);
}

/// Intent outlives the node, but the reconciler does not chase it (design #293
/// §4). A name dropped from `DOCKER_NODES` — or a dynamic node that never
/// re-announced after this dispatcher started — is still in the persisted record,
/// because intent is exactly what should survive a node's absence. What must not
/// survive is the RPC: intent is the one table that never shrinks, so a
/// decommissioned name that kept being pushed to would burn one failed push per
/// scan tick forever.
#[tokio::test]
async fn intent_for_a_node_the_fleet_lost_is_kept_but_never_pushed() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let mut record = types::FleetCapacity::default();
    for (node, slots) in [("ghost", 3), ("air", 2)] {
        record.nodes.insert(
            node.into(),
            types::NodeCapacityIntent {
                slots,
                set_by: "operator@example.com".into(),
                set_at: Utc::now(),
            },
        );
    }
    let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    bucket.put_json("fleet.capacity", &record).await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.script_slot_reply("air", test_utils::SlotReply::AdoptWithoutObserving);
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 2, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;
    handle
        .announce_worker(announce("air", 4, "0.1.0+air"))
        .await
        .unwrap();
    wait_for_fleet(&store, |f| node_slots(f, "air") == Some(Some(4))).await;

    for _ in 0..10 {
        handle.trigger_scan().await.unwrap();
    }
    wait_until(|| !backend.slot_commands().is_empty()).await;
    let pushes = backend.slot_commands();
    assert!(
        pushes
            .iter()
            .all(|(node, slots)| node == "air" && *slots == 2),
        "a node the fleet no longer holds must never be pushed to: {pushes:?}"
    );

    let after: types::FleetCapacity = bucket
        .get_json("fleet.capacity")
        .await
        .unwrap()
        .expect("intent still persisted");
    assert_eq!(
        after.nodes.get("ghost").map(|i| i.slots),
        Some(3),
        "intent for an absent node is remembered, not deleted"
    );
    assert_invariants_of(&sink);
}

/// The other half of the same rule: intent is re-asserted the moment a node the
/// roster has *never* held announces itself (design #293 §4). A dynamically
/// registered worker is in no `DOCKER_NODES` seed, so it enters the roster only
/// through its own announce — and the reconciler declines to push to a node the
/// roster does not hold. The re-assert therefore has to run *after* the announce
/// has merged the node in; running it before would find no entry, decide
/// `unacknowledged`, and silently defer the operator's number to a scan tick.
///
/// No `trigger_scan` here, and the wait below is bounded well under the 30s scan
/// interval — both deliberately. The scan tick would eventually restore the
/// number anyway, so a test that waited it out would pass on the very ordering
/// bug it exists to catch.
#[tokio::test]
async fn a_first_announce_re_asserts_intent_without_waiting_for_a_scan() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let mut record = types::FleetCapacity::default();
    record.nodes.insert(
        "nuc".into(),
        types::NodeCapacityIntent {
            slots: 2,
            set_by: "operator@example.com".into(),
            set_at: Utc::now(),
        },
    );
    let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    bucket.put_json("fleet.capacity", &record).await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    let (handle, _repo, sink) = spawn_core(server, &store, vec![], None, backend.clone()).await;

    handle
        .announce_worker(announce("nuc", 4, "0.1.0+nuc"))
        .await
        .unwrap();

    const BEFORE_ANY_SCAN_TICK: Duration = Duration::from_secs(10);
    test_utils::wait::poll(
        BEFORE_ANY_SCAN_TICK,
        "the announce's own capacity push",
        || (backend.slot_commands() == vec![("nuc".to_string(), 2)]).then_some(()),
    )
    .await;
    let fleet = wait_for_fleet(&store, |f| {
        f.nodes
            .iter()
            .any(|n| n.name == "nuc" && n.capacity_state == Some(types::CapacityState::Converged))
    })
    .await;
    assert_eq!(
        node(&fleet, "nuc").slots,
        Some(2),
        "the operator's number is in force without a scan tick"
    );
    assert_invariants_of(&sink);
}

/// The placement invariant, end to end (design #293 §2): every launch the
/// dispatcher decides is made inside a placement window, and reading intent
/// inside one panics. So a fleet that *has* intent must still launch normally —
/// through the initial launch path and through the launch-queue resume path,
/// which is the one that would most plausibly grow an intent-aware admission
/// check. If either path ever consults `fleet.capacity`, the actor panics here
/// and no container is ever launched.
#[tokio::test]
async fn launches_are_decided_without_reading_capacity_intent() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 1, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;
    handle
        .announce_worker(announce("air", 4, "0.1.0+air"))
        .await
        .unwrap();
    wait_for_fleet(&store, |f| node_slots(f, "air") == Some(Some(4))).await;
    handle
        .set_node_capacity("air", 2, "operator@example.com")
        .await
        .unwrap();

    backend.fail_launch_no_capacity_if(|_| Some("full".into()));
    release_cmd_work(&handle, &sink).await;
    wait_for_fleet(&store, |f| f.queue_depth >= 1).await;

    backend.fail_launch_no_capacity_if(|_| None);
    handle
        .announce_worker(announce("nuc", 2, "0.1.0+nuc"))
        .await
        .unwrap();
    wait_until(|| !backend.launches().is_empty()).await;
    wait_for_fleet(&store, |f| f.queue_depth == 0).await;
    assert_invariants_of(&sink);
}

/// A node that acknowledges `set_slots` and never reports the value — an old
/// build, or one that adopts and reverts — must keep being re-asserted, must
/// never read as converged, and must be pushed at most once per scan tick
/// (design #293 §4).
#[tokio::test]
async fn silently_ignored_capacity_is_re_pushed_but_bounded_per_tick() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.script_slot_reply("air", test_utils::SlotReply::AdoptWithoutObserving);
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        vec![worker_node("air", 2, Some("0.1.0+air"))],
        None,
        backend.clone(),
    )
    .await;
    handle
        .announce_worker(announce("air", 4, "0.1.0+air"))
        .await
        .unwrap();
    wait_for_fleet(&store, |f| node_slots(f, "air") == Some(Some(4))).await;

    handle
        .set_node_capacity("air", 2, "operator@example.com")
        .await
        .unwrap();
    wait_until(|| !backend.slot_commands().is_empty()).await;

    let ticks = 10;
    for _ in 0..ticks {
        handle.trigger_scan().await.unwrap();
        if backend.slot_commands().len() >= 3 {
            break;
        }
    }
    let pushes = backend.slot_commands();
    assert!(
        pushes.len() >= 2,
        "a diverging node must keep being re-asserted: {pushes:?}"
    );
    assert!(
        pushes.len() <= ticks + 1,
        "at most one push per node per tick: {} pushes over {ticks} ticks",
        pushes.len()
    );
    assert!(
        pushes
            .iter()
            .all(|(node, slots)| node == "air" && *slots == 2)
    );

    let fleet = wait_for_fleet(&store, |f| {
        f.nodes
            .iter()
            .any(|n| n.name == "air" && n.capacity_state.is_some())
    })
    .await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(4), "the observation stands, not the ask");
    assert_eq!(air.slots_desired, Some(2));
    assert_ne!(air.capacity_state, Some(types::CapacityState::Converged));
    assert_invariants_of(&sink);
}

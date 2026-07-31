//! Tier-2 tests for live fleet occupancy (spec §3.1): the dispatcher publishes
//! per-node slot usage — with the running job/task in each busy slot — to the
//! `platform` bucket (`fleet.status`) on launch/exit and after restart
//! re-attachment. Occupancy is rebuilt from the live containers the backend
//! reports, so a fresh (restarted) dispatcher reports true usage, not a stale
//! in-memory count. Crash/occupancy states are constructed directly in KV.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use dispatcher::core::{Core, CoreConfig, CoreHandle, CreateSpec};
use dispatcher::invariants::InvariantSink;
use std::sync::Arc;
use store::NatsStore;
use test_utils::repo::TempRepo;
use test_utils::{FakeBackend, FakeProvider};
use types::{FleetStatus, Job, JobState, Task, TaskKind, TaskPhase, TaskState, WorkerNode};

mod common;
use common::{assert_invariants_of, spawn_checked};

const FLAKY: &str = r#"
name: flaky
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
work_retries: 1
"#;

const CMD_WORK: &str = r#"
name: cmd-work
image: img:latest
work:
  type: command
  run: ./build.sh
"#;

fn roster() -> Vec<WorkerNode> {
    vec![
        WorkerNode {
            name: "air".into(),
            endpoint: "tcp://air:2375".into(),
            slots: 4,
            available: true,
            version: Some("0.1.0+air".into()),
            refresh_outcome: None,
            capacity_source: None,
            capacity_observed_at: None,
        },
        WorkerNode {
            name: "nuc".into(),
            endpoint: "tcp://nuc:2375".into(),
            slots: 2,
            available: true,
            version: None,
            refresh_outcome: None,
            capacity_source: None,
            capacity_observed_at: None,
        },
    ]
}

fn job(id: u64, r#type: &str, state: JobState, base_ref: Option<String>) -> Job {
    Job {
        id,
        project: "acme/api".into(),
        r#type: r#type.into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        members: vec![],
        batch_id: None,
        state,
        branch: format!("job/{id}"),
        base_ref,
        knowledge_tags: vec![],
        eval: vec![],
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

fn work_task(id: u64, job_seq: u64, state: TaskState, container_id: Option<&str>) -> Task {
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
        state,
        attempt: 1,
        evaluator: None,
        stage: 0,
        performed_by: None,
        label: None,
        container_id: container_id.map(Into::into),
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

fn running(id: &str, job: u64, task: u64) -> container::RunningContainer {
    container::RunningContainer {
        id: id.into(),
        project: Some("acme/api".into()),
        job: Some(job),
        task: Some(task),
    }
}

/// Build a repo carrying the given job types and spawn a core with a two-node
/// fleet roster. The caller seeds jobs/tasks and the backend before calling.
async fn spawn_core(
    server: &test_utils::nats::NatsTestServer,
    store: &NatsStore,
    types_yaml: &[(&str, &str)],
    backend: Arc<FakeBackend>,
) -> (CoreHandle, TempRepo, InvariantSink) {
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in types_yaml {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
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
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_fleet_roster(roster());
    let (handle, sink) = spawn_checked(core);
    (handle, repo, sink)
}

/// Poll `fleet.status` until `pred` holds (or time out), returning the snapshot.
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

fn node<'a>(fleet: &'a FleetStatus, name: &str) -> &'a types::FleetNode {
    fleet
        .nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("node {name} missing from {:?}", fleet.nodes))
}

/// A launch occupies a slot with the running job/task; the freed slot after the
/// container exits drops back to idle. Occupancy is read from the live
/// containers the backend reports, so republishing reflects both.
#[tokio::test]
async fn occupancy_reflects_launch_and_exit() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let jobs = store.jobs().await.unwrap();
    jobs.put(&job(
        51,
        "flaky",
        JobState::Escalated,
        Some("deadbeef".into()),
    ))
    .await
    .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&work_task(1, 51, TaskState::Running, Some("air/c1")))
        .await
        .unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.seed_managed_running([running("air/c1", 51, 1)]);
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        &[("jobs/flaky.yaml", FLAKY)],
        backend.clone(),
    )
    .await;

    let fleet = wait_for_fleet(&store, |f| node(f, "air").occupied == 1).await;
    let air = node(&fleet, "air");
    assert_eq!(air.slots, Some(4));
    assert_eq!(air.occupied, 1);
    assert!(air.available);
    assert_eq!(air.version.as_deref(), Some("0.1.0+air"));
    assert_eq!(air.running.len(), 1);
    let slot = &air.running[0];
    assert_eq!(slot.project, "acme/api");
    assert_eq!(slot.job_seq, 51);
    assert_eq!(slot.task_id, 1);
    assert_eq!(slot.task_kind, "work");
    assert_eq!(slot.job_type, "flaky");
    assert_eq!(slot.phase, "escalated");
    assert!(slot.started_at.is_some());

    let nuc = node(&fleet, "nuc");
    assert_eq!((nuc.slots, nuc.occupied), (Some(2), 0));
    assert_eq!(fleet.queue_depth, 0);

    backend.set_managed_running([]);
    handle.trigger_scan().await.unwrap();
    assert_invariants_of(&sink);
    let fleet = wait_for_fleet(&store, |f| node(f, "air").occupied == 0).await;
    assert!(node(&fleet, "air").running.is_empty());
    assert_invariants_of(&sink);
}

/// After a restart the occupancy is rebuilt from the live containers the fleet
/// reports — not from any in-memory state (a fresh process has none). A Work
/// job whose container is still alive re-attaches (§3.6) and shows as occupied.
#[tokio::test]
async fn restart_reattach_rebuilds_occupancy_from_live_containers() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"implement it", "prompt")
        .await;
    clone.push("main").await;
    let head = repo.head().await;
    repo.create_job_branch(1, &head).await;

    store
        .jobs()
        .await
        .unwrap()
        .put(&job(1, "flaky", JobState::Work, Some(head.clone())))
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&work_task(1, 1, TaskState::Running, Some("nuc/live")))
        .await
        .unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.seed_running(["nuc/live".to_string()]);
    backend.seed_managed_running([running("nuc/live", 1, 1)]);

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
        backend.clone(),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_fleet_roster(roster());
    let (_handle, sink) = spawn_checked(core);

    let fleet = wait_for_fleet(&store, |f| node(f, "nuc").occupied == 1).await;
    assert_invariants_of(&sink);
    let slot = &node(&fleet, "nuc").running[0];
    assert_eq!(
        (slot.job_seq, slot.task_id, slot.task_kind.as_str()),
        (1, 1, "work")
    );
    assert_eq!(slot.phase, "work");
    assert!(
        backend.killed().is_empty(),
        "a re-attached container is never reaped"
    );
}

/// The launch capacity queue depth (jobs waiting for a free slot, spec §3.5) is
/// surfaced in the fleet snapshot: a launch the fleet refuses for no capacity is
/// queued, and the depth shows up with no slot occupied.
#[tokio::test]
async fn queue_depth_included() {
    let Some(server) = test_utils::nats::NatsTestServer::shared().await else {
        return;
    };
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let backend = Arc::new(FakeBackend::new());
    backend.fail_launch_no_capacity_if(|_| Some("no free slots".into()));
    let (handle, _repo, sink) = spawn_core(
        server,
        &store,
        &[("jobs/cmd-work.yaml", CMD_WORK)],
        backend.clone(),
    )
    .await;

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
    assert_invariants_of(&sink);
    handle.release_job("acme", "api", created.id).await.unwrap();
    assert_invariants_of(&sink);

    let fleet = wait_for_fleet(&store, |f| f.queue_depth >= 1).await;
    assert_eq!(fleet.queue_depth, 1);
    assert!(
        fleet.nodes.iter().all(|n| n.occupied == 0),
        "nothing launched, so no slot is occupied: {:?}",
        fleet.nodes
    );
    assert_invariants_of(&sink);
}

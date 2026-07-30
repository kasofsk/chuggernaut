//! Tier-2 end-to-end fleet occupancy (spec §3.1) over the *real* accounting:
//! a live `chuggernaut worker` daemon reached over NATS, a real container on the
//! local Docker daemon, the dispatcher core, and the `platform` KV the api
//! serves. The existing `fleet.rs` tests drive the dispatcher's publish/change-
//! detection logic with a `FakeBackend` whose running set is seeded directly;
//! these instead prove the worker-RPC path the prod bug lived in — occupancy is
//! read back through the store exactly as `GET /api/v1/platform/fleet` reads it.
//!
//! Skips unless both NATS and Docker are available (the `e2e!`/`require_nats!`
//! guards handle CI-less environments).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use container::docker::DockerNodeConfig;
use container::{ContainerBackend, ContainerLaunchConfig, PlacementPolicy};
use dispatcher::core::{Core, CoreConfig, spawn};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use store::NatsStore;
use test_utils::FakeProvider;
use test_utils::nats::NatsTestServer;
use types::{FleetStatus, Job, JobState, Task, TaskKind, TaskPhase, TaskState, WorkerNode};
use worker::{FleetBackend, WorkerConfig};

const FLAKY: &str = "name: flaky\nimage: img:latest\nwork:\n  type: agent\n  prompt: prompts/impl.md\n  work_retries: 1\n";

fn local_docker_endpoint() -> String {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        return host;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for candidate in [
        "/var/run/docker.sock".to_string(),
        format!("{home}/.docker/run/docker.sock"),
        format!("{home}/.colima/default/docker.sock"),
    ] {
        if std::path::Path::new(&candidate).exists() {
            return format!("unix://{candidate}");
        }
    }
    "unix:///var/run/docker.sock".into()
}

/// Spawn an in-process worker daemon on node `w1` (local Docker) and a fleet
/// backend routed to it over NATS, or `None` to skip (no Docker).
async fn worker_fleet(
    server: &NatsTestServer,
) -> Option<(FleetBackend, tokio::task::JoinHandle<()>)> {
    if !test_utils::backend_suite::docker_available() {
        eprintln!("skipping: Docker daemon unavailable");
        return None;
    }
    let dir = std::env::temp_dir().join(format!(
        "chug-fleet-e2e-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let artifact = dir.join("chuggernaut-channel");
    std::fs::write(&artifact, b"x").unwrap();

    let config = WorkerConfig {
        node: "w1".into(),
        slots: 4,
        nats_url: server.url().to_string(),
        nats_creds: None,
        docker_endpoint: local_docker_endpoint(),
        channel_binary: artifact,
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

    let store = NatsStore::connect(server.url()).await.unwrap();
    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "w1".into(),
            endpoint: "worker".into(),
            slots: 4,
        }],
        store,
        PlacementPolicy::Busyness,
    )
    .unwrap();
    for _ in 0..100 {
        if fleet.inspect(&"w1/deadbeef".to_string()).await.is_ok() {
            return Some((fleet, daemon));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("worker daemon never became reachable");
}

fn labeled_sleep(project: &str, job: u64, task: u64) -> ContainerLaunchConfig {
    let mut env = HashMap::new();
    env.insert("JOB_PROJECT".into(), project.into());
    env.insert("JOB_ID".into(), job.to_string());
    env.insert("CHUG_TASK_ID".into(), task.to_string());
    ContainerLaunchConfig {
        image: "alpine:3".into(),
        cmd: vec!["sh".into(), "-c".into(), "sleep 120".into()],
        env,
        files: vec![],
        cpu_limit: None,
        memory_limit: Some("128Mi".into()),
        node: None,
    }
}

fn job(id: u64, state: JobState, base_ref: &str) -> Job {
    Job {
        id,
        project: "acme/api".into(),
        r#type: "flaky".into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        members: vec![],
        batch_id: None,
        state,
        branch: format!("job/{id}"),
        base_ref: Some(base_ref.into()),
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        claim_next: false,
        escalation: None,
        factory: None,
        created_at: Utc::now(),
        ready_at: Some(Utc::now()),
        completed_at: None,
        inputs: Default::default(),
        task_time_ms: None,
    }
}

fn work_task(id: u64, job_seq: u64, container_id: &str) -> Task {
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

/// A stale worker daemon reproducing the job/181 prod condition: it answers
/// `ping` (so it is reachable and schedulable, and the fleet starts) and
/// `list_exited`, but *rejects* `list_running` — the op a pre-1334657 daemon
/// never learned. Its live containers are then invisible to occupancy while the
/// node otherwise looks healthy. No Docker needed.
async fn stale_list_running_worker(store: &NatsStore, node: &str) -> tokio::task::JoinHandle<()> {
    let mut sub = store
        .subscribe_requests(&store::subjects::worker_all(node))
        .await
        .expect("subscribe stale worker");
    store.client().flush().await.expect("flush sub");
    tokio::spawn(async move {
        while let Some(req) = sub.next().await {
            let body = if req.subject.ends_with(".ping") {
                serde_json::to_vec(&types::worker::WorkerReply::Ok {
                    value: types::worker::PingOk {
                        running: 0,
                        version: "0.0.0+stale".into(),
                        artifacts: HashMap::new(),
                        refresh_outcome: None,
                        refresh_progress: None,
                    },
                })
                .unwrap()
            } else if req.subject.ends_with(".list_exited") {
                serde_json::to_vec(&types::worker::WorkerReply::Ok {
                    value: types::worker::ListExitedOk { ids: vec![] },
                })
                .unwrap()
            } else {
                // list_running (and anything else) is unsupported on this stale
                // daemon — exactly the op that returns empty/errors in prod.
                serde_json::to_vec(&types::worker::WorkerReply::<()>::Err {
                    error: types::worker::WorkerError::Other {
                        message: "unknown op (stale daemon)".into(),
                    },
                })
                .unwrap()
            };
            req.respond(body).await;
        }
    })
}

/// The job/181 prod outage as a dispatcher-tier regression: a worker that
/// answers `ping` but fails `list_running` used to appear as a false-idle
/// `occupied: 0, available: true` — indistinguishable from an empty node, with
/// no surfaced error. Occupancy now shows it **out of service** instead, so the
/// operator sees the anomaly rather than a phantom idle fleet. RED before the
/// per-node list-failure surfacing (the node read `available: true`).
#[tokio::test]
async fn worker_that_cannot_list_shows_out_of_service_not_idle() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let worker = stale_list_running_worker(&store, "air").await;

    let fleet = FleetBackend::new(
        vec![DockerNodeConfig {
            name: "air".into(),
            endpoint: "worker".into(),
            slots: 4,
        }],
        store.clone(),
        PlacementPolicy::Busyness,
    )
    .unwrap();

    let dir = std::env::temp_dir().join(format!("chug-fleet-stale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(dir),
        Arc::new(fleet),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_fleet_roster(vec![WorkerNode {
        name: "air".into(),
        endpoint: "worker".into(),
        slots: 4,
        available: true,
        version: None,
        refresh_outcome: None,
    }]);
    let _handle = spawn(core);

    // The node is present (not dropped) but shown out of service, not idle.
    let fleet_status = read_fleet(&store, |f| {
        f.nodes.iter().any(|n| n.name == "air" && !n.available)
    })
    .await;
    let air = fleet_status.nodes.iter().find(|n| n.name == "air").unwrap();
    assert!(
        !air.available,
        "a worker whose containers can't be listed must not read as idle-but-available: {air:?}"
    );
    assert_eq!(air.occupied, 0);

    worker.abort();
}

async fn read_fleet(store: &NatsStore, pred: impl Fn(&FleetStatus) -> bool) -> FleetStatus {
    let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
    for _ in 0..100 {
        if let Some(f) = bucket
            .get_json::<FleetStatus>("fleet.status")
            .await
            .unwrap()
            && pred(&f)
        {
            return f;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting on fleet.status");
}

/// A live worker-RPC-launched container is re-counted into occupancy when a
/// fresh dispatcher starts over it (the #178 restart-with-live-containers case),
/// and the freed slot is republished when it exits — read back through the
/// `platform` KV exactly as the api serves it. Rebuilt from the live containers
/// the worker reports, never from in-memory state (a fresh process has none).
#[tokio::test]
// TODO(style): oversized tier-2 test — split when this file is next touched.
#[allow(clippy::too_many_lines)]
async fn occupancy_reflects_worker_rpc_launch_and_exit_through_the_store() {
    let Some(server) = test_utils::nats::NatsTestServer::spawn().await else {
        return;
    };
    let Some((fleet, daemon)) = worker_fleet(&server).await else {
        return;
    };
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    // A real container running on the worker, launched over the NATS proxy.
    let cid = fleet
        .launch(labeled_sleep("acme/api", 51, 1))
        .await
        .unwrap();

    // Seed the crash state: an Escalated job (reconciliation leaves it and its
    // tasks alone) whose Running work task owns that live container.
    let repo = test_utils::repo::TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    clone
        .commit_file("jobs/flaky.yaml", FLAKY.as_bytes(), "type")
        .await;
    clone
        .commit_file("prompts/impl.md", b"impl", "prompt")
        .await;
    clone.push("main").await;
    let repos_root = repo
        .bare_path()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    store
        .jobs()
        .await
        .unwrap()
        .put(&job(51, JobState::Escalated, "deadbeef"))
        .await
        .unwrap();
    store
        .tasks()
        .await
        .unwrap()
        .put(&work_task(1, 51, &cid))
        .await
        .unwrap();

    // A *fresh* core over the live fleet — the restart re-attach path (§3.6): it
    // rebuilds occupancy from the container the worker still reports.
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(repos_root),
        Arc::new(fleet),
        Arc::new(FakeProvider::new()),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .with_fleet_roster(vec![WorkerNode {
        name: "w1".into(),
        endpoint: "worker".into(),
        slots: 4,
        available: true,
        version: None,
        refresh_outcome: None,
    }]);
    let handle = spawn(core);

    // Occupancy rebuilt on re-attach: w1 shows one busy slot running job 51 /
    // task 1, served through the store the api reads.
    let fleet_status = read_fleet(&store, |f| {
        f.nodes.iter().any(|n| n.name == "w1" && n.occupied == 1)
    })
    .await;
    let w1 = fleet_status.nodes.iter().find(|n| n.name == "w1").unwrap();
    assert_eq!(w1.occupied, 1);
    let slot = &w1.running[0];
    assert_eq!(slot.project, "acme/api");
    assert_eq!((slot.job_seq, slot.task_id), (51, 1));
    assert_eq!(slot.task_kind, "work");

    // The container exits (force-removed on the node): the next transition
    // republishes the freed slot.
    test_utils::backend_suite::rm(&cid);
    // Nudge a republish; a scan is occupancy-relevant.
    for _ in 0..50 {
        handle.trigger_scan().await.ok();
        let bucket = store.raw_bucket(store::buckets::PLATFORM).await.unwrap();
        if let Some(f) = bucket
            .get_json::<FleetStatus>("fleet.status")
            .await
            .unwrap()
            && f.nodes.iter().all(|n| n.occupied == 0)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let cleared = store
        .raw_bucket(store::buckets::PLATFORM)
        .await
        .unwrap()
        .get_json::<FleetStatus>("fleet.status")
        .await
        .unwrap()
        .unwrap();
    assert!(
        cleared.nodes.iter().all(|n| n.occupied == 0),
        "freed slot not republished: {:?}",
        cleared.nodes
    );

    daemon.abort();
}

//! Tier-2 integration tests (testing.md): store against a real NATS server
//! in Docker. Skips when the Docker daemon is unavailable.

use chrono::Utc;
use store::NatsStore;
use test_utils::require_nats;
use types::{
    Job, JobState, StepKind, StepRecord, StepStatus, Task, TaskKind, TaskPhase, TaskState,
};

fn job(seq: u64) -> Job {
    Job {
        id: seq,
        project: "acme/api".into(),
        r#type: "implement-endpoint".into(),
        title: String::new(),
        description: String::new(),
        deps: Vec::new(),
        state: JobState::Frozen,
        branch: format!("job/{seq}"),
        base_ref: None,
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        factory: None,
        created_at: Utc::now(),
        ready_at: None,
    }
}

fn task(job_seq: u64, id: u64) -> Task {
    Task {
        id,
        job_seq,
        project: "acme/api".into(),
        phase: TaskPhase::Work,
        cycle: 1,
        kind: TaskKind::Agent {
            provider: "claude".into(),
            model: None,
            prompt: "do the thing".into(),
        },
        state: TaskState::Pending,
        attempt: 1,
        evaluator: None,
        container_id: None,
        session_id: None,
        result: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
    }
}

fn step(n: u32, kind: StepKind) -> StepRecord {
    StepRecord {
        step: n,
        kind,
        iteration: n.div_ceil(2),
        status: StepStatus::Running,
        pass: None,
        findings: None,
        started_at: Utc::now(),
        completed_at: None,
    }
}

#[tokio::test]
async fn topology_and_typed_stores_round_trip() {
    let server = require_nats!();
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    // Idempotent: second call must not fail.
    store.ensure_topology().await.unwrap();

    // Jobs
    let jobs = store.jobs().await.unwrap();
    jobs.put(&job(1)).await.unwrap();
    jobs.put(&job(2)).await.unwrap();
    let got = jobs.get("acme", "api", 1).await.unwrap().unwrap();
    assert_eq!(got.branch, "job/1");
    assert!(jobs.get("acme", "api", 99).await.unwrap().is_none());
    let listed = jobs.list("acme", "api").await.unwrap();
    assert_eq!(listed.iter().map(|j| j.id).collect::<Vec<_>>(), vec![1, 2]);

    // Tasks
    let tasks = store.tasks().await.unwrap();
    tasks.put(&task(1, 1)).await.unwrap();
    tasks.put(&task(1, 2)).await.unwrap();
    let log = tasks.list_for_job("acme", "api", 1).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].id, 2);

    // Counters: sequential per project
    let counters = store.counters().await.unwrap();
    assert_eq!(counters.next("acme", "api").await.unwrap(), 1);
    assert_eq!(counters.next("acme", "api").await.unwrap(), 2);
    assert_eq!(counters.next("acme", "web").await.unwrap(), 1);

    // Rdeps: append is idempotent per dependent
    let rdeps = store.rdeps().await.unwrap();
    rdeps.append("acme", "api", 1, 43).await.unwrap();
    rdeps.append("acme", "api", 1, 77).await.unwrap();
    rdeps.append("acme", "api", 1, 43).await.unwrap();
    assert_eq!(rdeps.get("acme", "api", 1).await.unwrap(), vec![43, 77]);
}

/// The reason artifacts use an object store rather than KV or a req/reply
/// route: a transcript routinely exceeds NATS's 1MB default `max_payload`.
/// This asserts a >1MB blob survives the full gzip+age+chunking round trip, and
/// that a handle without the identity cannot read it back.
#[tokio::test]
async fn artifacts_round_trip_a_blob_larger_than_max_payload() {
    let server = require_nats!();
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();

    let (identity, public) = store::secrets::generate_age_keypair();

    // Incompressible, so the stored bytes really do exceed max_payload —
    // transcript-shaped JSONL would gzip small enough to hide the problem.
    let mut big = Vec::with_capacity(3 * 1024 * 1024);
    let mut x: u32 = 0x1234_5678;
    while big.len() < 3 * 1024 * 1024 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        big.extend_from_slice(&x.to_le_bytes());
    }
    assert!(big.len() > 1024 * 1024, "fixture must exceed max_payload");

    let writer = store
        .artifacts(store::ArtifactCrypto::encrypt_only(&public).unwrap())
        .await
        .unwrap();
    writer
        .put(
            "acme",
            "api",
            42,
            7,
            store::ArtifactKind::SessionTranscript,
            &big,
        )
        .await
        .unwrap();

    let reader = store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();
    let got = reader
        .get("acme", "api", 42, 7, store::ArtifactKind::SessionTranscript)
        .await
        .unwrap();
    assert_eq!(got.as_deref(), Some(big.as_slice()));

    // Missing artifacts read as None, not an error — human tasks have none.
    assert!(
        reader
            .get("acme", "api", 42, 7, store::ArtifactKind::Stdout)
            .await
            .unwrap()
            .is_none()
    );

    // Listing is scoped to the task.
    writer
        .put(
            "acme",
            "api",
            42,
            7,
            store::ArtifactKind::Stdout,
            b"log line",
        )
        .await
        .unwrap();
    writer
        .put(
            "acme",
            "api",
            42,
            71,
            store::ArtifactKind::Stdout,
            b"other task",
        )
        .await
        .unwrap();
    let mut kinds = reader.list_for_task("acme", "api", 42, 7).await.unwrap();
    kinds.sort_by_key(|k| k.as_str());
    assert_eq!(
        kinds,
        vec![
            store::ArtifactKind::SessionTranscript,
            store::ArtifactKind::Stdout
        ]
    );

    // A handle without the identity can fetch bytes but must not read them.
    let blind = store
        .artifacts(store::ArtifactCrypto::encrypt_only(&public).unwrap())
        .await
        .unwrap();
    assert!(
        blind
            .get("acme", "api", 42, 7, store::ArtifactKind::Stdout)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn step_log_upserts_by_step_number() {
    let server = require_nats!();
    let store = NatsStore::connect(server.url()).await.unwrap();
    store.ensure_topology().await.unwrap();
    let steps = store.steps().await.unwrap();

    // No entry → empty log (task without an inline review loop).
    assert!(steps.list("acme", "api", 1, 1).await.unwrap().is_empty());

    // step-started appends…
    steps
        .upsert("acme", "api", 1, 1, step(1, StepKind::AuthorIteration))
        .await
        .unwrap();
    steps
        .upsert("acme", "api", 1, 1, step(2, StepKind::InlineReview))
        .await
        .unwrap();
    // …and the matching step-completed overwrites in place.
    let mut done = step(2, StepKind::InlineReview);
    done.status = StepStatus::Done;
    done.pass = Some(false);
    done.findings = Some(serde_json::json!({"issues": ["missing error handling"]}));
    steps.upsert("acme", "api", 1, 1, done).await.unwrap();

    let log = steps.list("acme", "api", 1, 1).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].status, StepStatus::Done);
    assert_eq!(log[1].pass, Some(false));
}

#[tokio::test]
async fn request_with_retry_survives_late_responder() {
    let server = require_nats!();
    let store = NatsStore::connect(server.url()).await.unwrap();

    // Responder comes up only after the first attempt has failed — the §4.2
    // bounded-retry contract is that the submit eventually lands.
    let responder_store = NatsStore::connect(server.url()).await.unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let mut sub = responder_store
            .client()
            .subscribe("req.work.submit.acme.api.1")
            .await
            .unwrap();
        use futures::StreamExt;
        if let Some(msg) = sub.next().await {
            responder_store
                .client()
                .publish(msg.reply.unwrap(), "ack".into())
                .await
                .unwrap();
        }
    });

    let reply = store
        .request_with_retry(
            "req.work.submit.acme.api.1",
            br#"{"summary":"done"}"#,
            10,
            std::time::Duration::from_millis(200),
        )
        .await
        .unwrap();
    assert_eq!(&reply.payload[..], b"ack");
}

//! Tier-2 integration tests (docs/reference/testing.md): store against a real NATS server
//! in Docker. Skips when the Docker daemon is unavailable.

#![allow(clippy::unwrap_used, clippy::expect_used)]

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
        cover_html: None,
        deps: Vec::new(),
        members: vec![],
        batch_id: None,
        state: JobState::Frozen,
        branch: format!("job/{seq}"),
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
        ready_at: None,
        completed_at: None,
        inputs: Default::default(),
        groups: vec![],
        task_time_ms: None,
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
        label: None,
        stage: 0,
        performed_by: None,
        container_id: None,
        rework_reason: None,
        infra_loss: false,

        pending_reason: None,
        queued_at: None,
        session_id: None,
        reviewed_tip: None,
        workload_identities: vec![],
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
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    store.ensure_topology().await.unwrap();

    let jobs = store.jobs().await.unwrap();
    jobs.put(&job(1)).await.unwrap();
    jobs.put(&job(2)).await.unwrap();
    let got = jobs.get("acme", "api", 1).await.unwrap().unwrap();
    assert_eq!(got.branch, "job/1");
    assert!(jobs.get("acme", "api", 99).await.unwrap().is_none());
    let listed = jobs.list("acme", "api").await.unwrap();
    assert_eq!(listed.iter().map(|j| j.id).collect::<Vec<_>>(), vec![1, 2]);

    let tasks = store.tasks().await.unwrap();
    tasks.put(&task(1, 1)).await.unwrap();
    tasks.put(&task(1, 2)).await.unwrap();
    let log = tasks.list_for_job("acme", "api", 1).await.unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[1].id, 2);

    let counters = store.counters().await.unwrap();
    assert_eq!(counters.next("acme", "api").await.unwrap(), 1);
    assert_eq!(counters.next("acme", "api").await.unwrap(), 2);
    assert_eq!(counters.next("acme", "web").await.unwrap(), 1);

    let rdeps = store.rdeps().await.unwrap();
    rdeps.append("acme", "api", 1, 43).await.unwrap();
    rdeps.append("acme", "api", 1, 77).await.unwrap();
    rdeps.append("acme", "api", 1, 43).await.unwrap();
    assert_eq!(rdeps.get("acme", "api", 1).await.unwrap(), vec![43, 77]);
    rdeps.remove("acme", "api", 1, 43).await.unwrap();
    rdeps.remove("acme", "api", 1, 999).await.unwrap();
    assert_eq!(rdeps.get("acme", "api", 1).await.unwrap(), vec![77]);
}

/// The reason artifacts use an object store rather than KV or a req/reply
/// route: a transcript routinely exceeds NATS's 1MB default `max_payload`.
/// This asserts a >1MB blob survives the full gzip+age+chunking round trip, and
/// that a handle without the identity cannot read it back.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn artifacts_round_trip_a_blob_larger_than_max_payload() {
    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let (identity, public) = store::secrets::generate_age_keypair();

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

    assert!(
        reader
            .get("acme", "api", 42, 7, store::ArtifactKind::Stdout)
            .await
            .unwrap()
            .is_none()
    );

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

/// Operator-uploaded job attachments (a screenshot on a bug report) round-trip
/// through the same gzip+age object store as transcripts, are listed with their
/// content type and original size without opening the blob, are scoped per job,
/// and can be deleted. A larger-than-`max_payload` blob confirms chunking.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "TODO(style): oversized tier-2 test — split when this file is next touched."
)]
async fn job_attachments_round_trip_list_and_delete() {
    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let (identity, public) = store::secrets::generate_age_keypair();
    let writer = store
        .artifacts(store::ArtifactCrypto::encrypt_only(&public).unwrap())
        .await
        .unwrap();
    let reader = store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();

    let mut png = Vec::with_capacity(2 * 1024 * 1024);
    let mut x: u32 = 0x9e37_79b9;
    while png.len() < 2 * 1024 * 1024 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        png.extend_from_slice(&x.to_le_bytes());
    }
    assert!(png.len() > 1024 * 1024, "fixture must exceed max_payload");

    writer
        .put_attachment("acme", "api", 42, "mobile-bug.png", "image/png", &png)
        .await
        .unwrap();
    writer
        .put_attachment(
            "acme",
            "api",
            42,
            "notes.txt",
            "text/plain",
            b"see the crash",
        )
        .await
        .unwrap();
    writer
        .put_attachment("acme", "api", 43, "other.txt", "text/plain", b"unrelated")
        .await
        .unwrap();

    let (meta, bytes) = reader
        .get_attachment("acme", "api", 42, "mobile-bug.png")
        .await
        .unwrap()
        .expect("attachment present");
    assert_eq!(bytes, png);
    assert_eq!(meta.content_type, "image/png");
    assert_eq!(meta.size, png.len() as u64);

    let list = reader.list_attachments("acme", "api", 42).await.unwrap();
    assert_eq!(
        list,
        vec![
            store::Attachment {
                name: "mobile-bug.png".into(),
                content_type: "image/png".into(),
                size: png.len() as u64,
            },
            store::Attachment {
                name: "notes.txt".into(),
                content_type: "text/plain".into(),
                size: 13,
            },
        ]
    );

    assert!(
        reader
            .get_attachment("acme", "api", 42, "nope.png")
            .await
            .unwrap()
            .is_none()
    );

    assert!(
        reader
            .delete_attachment("acme", "api", 42, "notes.txt")
            .await
            .unwrap()
    );
    assert!(
        !reader
            .delete_attachment("acme", "api", 42, "notes.txt")
            .await
            .unwrap()
    );
    let names: Vec<String> = reader
        .list_attachments("acme", "api", 42)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, vec!["mobile-bug.png".to_string()]);
}

/// #196 regression stress: the 2026-07-23 CI hang was a racy tombstone state
/// in async-nats' object store (GET after DELETE parked `read_to_end` forever;
/// the list stream was the other suspect). Hammer the full
/// put/get/list/delete/get-after-delete/double-delete cycle so any recurrence
/// of an unbounded await surfaces here — and surfaces as a loud `StoreError`
/// via the store-level op bound rather than a parked runtime.
#[tokio::test]
async fn job_attachments_stress_cycle() {
    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let (identity, _public) = store::secrets::generate_age_keypair();
    let arts = store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();

    for i in 0..20u32 {
        let name = format!("shot-{i}.png");
        let body = format!("bytes-{i}").into_bytes();
        arts.put_attachment("acme", "api", 7, &name, "image/png", &body)
            .await
            .unwrap();
        let (_, got) = arts
            .get_attachment("acme", "api", 7, &name)
            .await
            .unwrap()
            .expect("just put");
        assert_eq!(got, body);
        let listed = arts.list_attachments("acme", "api", 7).await.unwrap();
        assert!(listed.iter().any(|a| a.name == name), "iter {i}: listed");
        assert!(
            arts.delete_attachment("acme", "api", 7, &name)
                .await
                .unwrap()
        );
        assert!(
            arts.get_attachment("acme", "api", 7, &name)
                .await
                .unwrap()
                .is_none(),
            "iter {i}: get-after-delete absent"
        );
        assert!(
            !arts
                .delete_attachment("acme", "api", 7, &name)
                .await
                .unwrap(),
            "iter {i}: double-delete false"
        );
        let after = arts.list_attachments("acme", "api", 7).await.unwrap();
        assert!(
            after.iter().all(|a| a.name != name),
            "iter {i}: gone from listing"
        );
    }
}

#[tokio::test]
async fn step_log_upserts_by_step_number() {
    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let steps = store.steps().await.unwrap();

    assert!(steps.list("acme", "api", 1, 1).await.unwrap().is_empty());

    steps
        .upsert("acme", "api", 1, 1, step(1, StepKind::AuthorIteration))
        .await
        .unwrap();
    steps
        .upsert("acme", "api", 1, 1, step(2, StepKind::InlineReview))
        .await
        .unwrap();
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
    let prefix = test_utils::unique_prefix();
    let store = NatsStore::connect_namespaced(server.url(), &prefix)
        .await
        .unwrap();

    let responder_store = NatsStore::connect_namespaced(server.url(), &prefix)
        .await
        .unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let mut sub = responder_store
            .subscribe_requests("req.work.submit.acme.api.1")
            .await
            .unwrap();
        if let Some(req) = sub.next().await {
            req.respond("ack").await;
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

/// #206 isolation guard: two namespaced stores on the *same* server share no KV
/// state (writes under one prefix are invisible under another), and a namespaced
/// store never creates the production (un-prefixed) buckets — so the shared
/// per-process server keeps tests apart, and prod names are reserved for prod.
#[tokio::test]
async fn namespaces_isolate_and_reserve_prod_names() {
    let server = require_nats!();

    let a = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    let b = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    a.ensure_topology().await.unwrap();
    b.ensure_topology().await.unwrap();

    a.jobs().await.unwrap().put(&job(1)).await.unwrap();
    assert!(
        b.jobs()
            .await
            .unwrap()
            .get("acme", "api", 1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        a.jobs()
            .await
            .unwrap()
            .get("acme", "api", 1)
            .await
            .unwrap()
            .is_some()
    );

    let prod = NatsStore::connect(server.url()).await.unwrap();
    assert!(
        prod.jobs().await.is_err(),
        "namespaced tests must not create the production `jobs` bucket"
    );
}

/// The prefix scan behind every list call (`Bucket::scan_prefix`) reads each
/// bucket in one pass instead of a key listing plus a get per key. Three things
/// about that are easy to get wrong and invisible if they regress, so pin them:
///
/// 1. **Scoping.** The scan narrows its subject filter to the caller's prefix.
///    A project must never see another project's records — the previous
///    spelling listed the whole bucket and filtered in Rust, so scoping was
///    free; now it is load-bearing.
/// 2. **Tombstones.** `LastPerSubject` delivers a delete/purge marker as the
///    latest revision of a removed key. Its payload is empty, so a deleted job
///    would surface as a deserialization error rather than an absence.
/// 3. **Token alignment.** Subject filters match whole tokens while the prefix
///    is a plain string, so a prefix ending mid-token must not over-match.
#[tokio::test]
async fn prefix_scan_scopes_by_project_and_skips_tombstones() {
    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let jobs = store.jobs().await.unwrap();

    jobs.put(&job(1)).await.unwrap();
    jobs.put(&job(2)).await.unwrap();
    jobs.put(&job(3)).await.unwrap();
    let other = Job {
        project: "acme/web".into(),
        ..job(1)
    };
    jobs.put(&other).await.unwrap();
    let adjacent = Job {
        project: "acme/apiary".into(),
        ..job(9)
    };
    jobs.put(&adjacent).await.unwrap();

    let listed = jobs.list("acme", "api").await.unwrap();
    assert_eq!(
        listed.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the scan must return this project's jobs and only this project's"
    );

    let bucket = store.raw_bucket(store::buckets::JOBS).await.unwrap();
    bucket.delete("acme.api.2").await.unwrap();

    let listed = jobs.list("acme", "api").await.unwrap();
    assert_eq!(
        listed.iter().map(|j| j.id).collect::<Vec<_>>(),
        vec![1, 3],
        "a purged key is an absence, not a decode failure"
    );
    let keys = bucket.keys_with_prefix("acme.api.").await.unwrap();
    assert_eq!(
        keys,
        vec!["acme.api.1".to_string(), "acme.api.3".to_string()],
        "the headers-only spelling must drop tombstones too"
    );

    assert_eq!(jobs.list("acme", "apiary").await.unwrap().len(), 1);
    assert_eq!(jobs.list("acme", "web").await.unwrap().len(), 1);
    assert!(jobs.list("acme", "absent").await.unwrap().is_empty());
}

/// A scan acks nothing, so with the default ack policy the server stops
/// delivering at `max_ack_pending` (1000) and the scan hangs until its
/// per-message deadline. That is exactly what took `/tasks/pending` down once
/// the dogfood project passed 1000 task records (#290) — nothing covered past
/// the cliff, so pin it here just above 1000.
#[tokio::test]
async fn prefix_scan_returns_every_key_past_the_ack_pending_cap() {
    const KEYS: u64 = 1050;

    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let bucket = store.raw_bucket(store::buckets::TASKS).await.unwrap();

    for i in 0..KEYS {
        bucket.put_json(&format!("acme.api.{i}"), &i).await.unwrap();
    }

    let listed: Vec<u64> = bucket.list_prefix("acme.api.").await.unwrap();
    assert_eq!(
        listed.len() as u64,
        KEYS,
        "a scan larger than the default max_ack_pending must complete, not stall"
    );
    let mut sorted = listed;
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        (0..KEYS).collect::<Vec<_>>(),
        "every stored key must come back exactly once"
    );

    let mut stream = store
        .jetstream()
        .get_stream(format!("KV_{}{}", store.prefix(), store::buckets::TASKS))
        .await
        .unwrap();
    assert_eq!(
        stream.info().await.unwrap().state.consumer_count,
        0,
        "a finished scan must delete its ephemeral consumer"
    );
}

/// Pseudo-random bytes that survive the artifact store's gzip, so a size-bound
/// test measures the bound rather than the compressor.
fn incompressible(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 4);
    let mut x: u32 = 0x9e37_79b9;
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Store one artifact of every kind for a task, so a GC assertion can name what
/// went and what stayed.
async fn seed_every_kind(arts: &store::ArtifactStore, seq: u64, task: u64) {
    for (kind, body) in [
        (
            store::ArtifactKind::SessionTranscript,
            &b"{\"type\":\"user\"}"[..],
        ),
        (store::ArtifactKind::Stdout, b"log"),
        (store::ArtifactKind::Output, b"tarball"),
    ] {
        arts.put("acme", "api", seq, task, kind, body)
            .await
            .unwrap();
    }
}

/// Design #362 R1/R2: an output is stored, read and listed like any other
/// artifact, and revoke-time GC removes exactly the outputs — the transcript and
/// stdout of the same task survive, because a revoked job is still an audit
/// record.
#[tokio::test]
async fn outputs_are_gc_able_without_touching_the_audit_record() {
    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();

    let (identity, _public) = store::secrets::generate_age_keypair();
    let arts = store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();

    for (seq, task) in [(42u64, 7u64), (43, 1)] {
        seed_every_kind(&arts, seq, task).await;
    }

    let mut kinds: Vec<&str> = arts
        .list_for_task("acme", "api", 42, 7)
        .await
        .unwrap()
        .iter()
        .map(|k| k.as_str())
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, ["output.tar.gz", "session.jsonl", "stdout.log"]);
    assert_eq!(
        arts.get("acme", "api", 42, 7, store::ArtifactKind::Output)
            .await
            .unwrap()
            .as_deref(),
        Some(b"tarball".as_slice())
    );

    assert_eq!(
        arts.delete_outputs_for_job("acme", "api", 42)
            .await
            .unwrap(),
        1
    );
    assert!(
        arts.get("acme", "api", 42, 7, store::ArtifactKind::Output)
            .await
            .unwrap()
            .is_none()
    );
    for kind in [
        store::ArtifactKind::SessionTranscript,
        store::ArtifactKind::Stdout,
    ] {
        assert!(
            arts.get("acme", "api", 42, 7, kind)
                .await
                .unwrap()
                .is_some(),
            "revoke must never delete {}",
            kind.as_str()
        );
    }
    assert!(
        arts.get("acme", "api", 43, 1, store::ArtifactKind::Output)
            .await
            .unwrap()
            .is_some(),
        "another job's outputs are not this job's to delete"
    );
    assert_eq!(
        arts.delete_outputs_for_job("acme", "api", 42)
            .await
            .unwrap(),
        0,
        "a second revoke finds nothing left"
    );
}

/// The substantive property of the second bucket (design #362 R1): output
/// pressure is contained. With the outputs bucket at its byte ceiling, further
/// outputs are refused and every transcript — stored before or after — is
/// untouched and still readable.
#[tokio::test]
async fn output_pressure_cannot_displace_a_transcript() {
    let server = require_nats!();
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store
        .ensure_topology_with(&store::OutputsRetention {
            max_age: std::time::Duration::from_secs(3600),
            max_bytes: 256 * 1024,
        })
        .await
        .unwrap();

    let (identity, _public) = store::secrets::generate_age_keypair();
    let arts = store
        .artifacts(store::ArtifactCrypto::with_identity(&identity).unwrap())
        .await
        .unwrap();

    let transcript = incompressible(64 * 1024);
    arts.put(
        "acme",
        "api",
        1,
        1,
        store::ArtifactKind::SessionTranscript,
        &transcript,
    )
    .await
    .unwrap();

    let blob = incompressible(64 * 1024);
    let mut refused = None;
    for task in 1..32u64 {
        if let Err(e) = arts
            .put("acme", "api", 9, task, store::ArtifactKind::Output, &blob)
            .await
        {
            refused = Some(e);
            break;
        }
    }
    assert!(
        refused.is_some(),
        "a 256KiB outputs ceiling must refuse 64KiB outputs well inside 32 of them"
    );

    assert_eq!(
        arts.get("acme", "api", 1, 1, store::ArtifactKind::SessionTranscript)
            .await
            .unwrap()
            .as_deref(),
        Some(transcript.as_slice()),
        "a transcript stored before the outputs bucket filled is still readable"
    );
    arts.put(
        "acme",
        "api",
        2,
        1,
        store::ArtifactKind::SessionTranscript,
        &transcript,
    )
    .await
    .expect("a full outputs bucket must not block the artifacts bucket");
}

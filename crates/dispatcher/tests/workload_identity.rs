//! Tier-2 tests for **workload identity delivery** (spec §7.4, §8.3, §10.2/§10.3;
//! design #313 half A slice S4): the launch round trip a declared
//! `workload_identities:` name travels — the `cloud-identities.*` read, the mint
//! at that container's own launch, the two injected files, the one env var, and
//! the audit recorded in the token's place.
//!
//! Everything here needs the single-writer actor and a real store (the record is
//! a KV fact and the audit is a task-record fact), which is what makes it tier 2
//! rather than tier 1 (`docs/reference/testing.md`). The rules themselves are pinned pure in
//! `auth::workload` — the claim set, the ADC document's shape and the
//! `GOOGLE_APPLICATION_CREDENTIALS` rule across zero, one and two identities.
//!
//! It runs on a **private** `nats-server` (`NatsTestServer::spawn`, job #408) and
//! a `FakeBackend`, so the gate runs it with no Docker daemon at all.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use dispatcher::core::{Core, CoreConfig, CreateSpec, EvalSubmission, OidcIssuer};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use store::NatsStore;
use test_utils::repo::{TempRepo, clone_branch_from};
use test_utils::{FakeBackend, FakeProvider};
use types::JobState;

mod common;
use common::spawn_checked;

const ISSUER_PRIVATE: &str = include_str!("../../auth/testdata/jwt_test_key.pem");
const ISSUER_PUBLIC: &str = include_str!("../../auth/testdata/jwt_test_key.pub.pem");
const ISSUER: &str = "https://chug.kasofsk.xyz";
const WRITER_AUDIENCE: &str = "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/chug/providers/chuggernaut";
const DEPLOYER_AUDIENCE: &str = "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/chug/providers/deployer";

/// The worked example from #313 A5: the work container and one evaluator each
/// declare the same identity — so both mint, and the tokens differ exactly in
/// the claims that scope them — beside an evaluator that declares none.
const CLOUD: &str = r#"
name: cloud
image: img:latest
min_dispatcher: 5
work:
  type: agent
  prompt: prompts/impl.md
  workload_identities: [gcp-artifact-writer]
eval:
  - name: ci
    type: command
    run: ./ci.sh
    workload_identities: [gcp-artifact-writer]
  - name: plain
    type: command
    run: ./plain.sh
"#;

/// A work container declaring two identities: two file pairs, and no
/// `GOOGLE_APPLICATION_CREDENTIALS` at all (#313 A3).
const CLOUD_TWO: &str = r#"
name: cloud-two
image: img:latest
min_dispatcher: 5
work:
  type: agent
  prompt: prompts/impl.md
  workload_identities: [gcp-artifact-writer, gcp-deployer]
eval:
  - name: ci
    type: command
    run: ./ci.sh
"#;

/// An **agent** evaluator declaring an identity, so the launch the fleet can
/// refuse (§3.5) is the one holding a credential (#313 A6's audit rule).
const CLOUD_AGENT_EVAL: &str = r#"
name: cloud-agent-eval
image: img:latest
min_dispatcher: 5
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: review
    type: agent
    prompt: prompts/impl.md
    workload_identities: [gcp-artifact-writer]
"#;

/// A type declaring nothing, so a job of it must launch exactly as it did before
/// the feature existed.
const PLAIN: &str = r#"
name: plain
image: img:latest
work:
  type: agent
  prompt: prompts/impl.md
eval:
  - name: ci
    type: command
    run: ./ci.sh
"#;

struct Rig {
    _server: test_utils::nats::NatsTestServer,
    store: NatsStore,
    repo: TempRepo,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
    core: Core,
}

async fn rig() -> Option<Rig> {
    let server = test_utils::nats::NatsTestServer::spawn().await?;
    let store = NatsStore::connect_namespaced(server.url(), &test_utils::unique_prefix())
        .await
        .unwrap();
    store.ensure_topology().await.unwrap();
    let repo = TempRepo::create("acme", "api").await;
    let clone = repo.clone_branch("main").await;
    for (path, content) in [
        ("jobs/cloud.yaml", CLOUD),
        ("jobs/cloud-two.yaml", CLOUD_TWO),
        ("jobs/cloud-agent-eval.yaml", CLOUD_AGENT_EVAL),
        ("jobs/plain.yaml", PLAIN),
        ("prompts/impl.md", "implement it"),
    ] {
        clone.commit_file(path, content.as_bytes(), path).await;
    }
    clone.push("main").await;

    seed_cloud_identities(&store).await;

    let backend = Arc::new(FakeBackend::new());
    backend.put_file("/workspace/eval-result.json", br#"{"ok":true}"#.to_vec());
    let provider = Arc::new(FakeProvider::with_backend(backend.clone()));
    let core = Core::new(
        store.clone(),
        vcs::RepoManager::new(
            repo.bare_path()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf(),
        ),
        backend.clone(),
        provider.clone(),
        CoreConfig {
            repo_url_base: "file:///repos".into(),
            nats_url: server.url().into(),
            oidc_issuer: Some(OidcIssuer {
                private_pem: ISSUER_PRIVATE.into(),
                public_pem: ISSUER_PUBLIC.into(),
                issuer: ISSUER.into(),
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Some(Rig {
        _server: server,
        store,
        repo,
        backend,
        provider,
        core,
    })
}

/// The two operator-managed `cloud-identities.*` records the declarations above
/// resolve against (spec §8.3), each at its own audience.
async fn seed_cloud_identities(store: &NatsStore) {
    let identities = store
        .raw_bucket(store::buckets::CLOUD_IDENTITIES)
        .await
        .unwrap();
    for (name, audience) in [
        ("gcp-artifact-writer", WRITER_AUDIENCE),
        ("gcp-deployer", DEPLOYER_AUDIENCE),
    ] {
        identities
            .put_json(
                &format!("acme.api.{name}"),
                &types::CloudIdentity {
                    audience: audience.into(),
                    service_account: format!("{name}@beacon.iam.gserviceaccount.com"),
                    token_ttl_secs: None,
                },
            )
            .await
            .unwrap();
    }
}

fn req(r#type: &str) -> CreateSpec {
    CreateSpec {
        owner: "acme".into(),
        project: "api".into(),
        r#type: r#type.into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        knowledge_tags: vec![],
        eval: vec![],
        require_approval: false,
        timeout: None,
        model: None,
        factory: None,
        schedule: None,
        members: vec![],
        inputs: BTreeMap::new(),
        groups: vec![],
        draft: false,
    }
}

/// The work-agent hook every launch-reaching test needs: commit on the job branch
/// so the §3.2 finish-line guard sees output and the job proceeds.
fn commit_on_work(rig: &Rig) {
    let bare = rig.repo.bare_path();
    rig.provider.on_run(move |cfg| async move {
        let branch = cfg.env.get("JOB_BRANCH").unwrap().clone();
        let work = clone_branch_from(&bare, &branch).await;
        work.commit_file("src/a.rs", b"job change", "implement")
            .await;
        work.push(&branch).await;
    });
}

/// One job run to Done, and everything its launches are inspected through. The
/// repo and the server are held only to keep them alive for the assertions.
struct Ran {
    _server: test_utils::nats::NatsTestServer,
    _repo: TempRepo,
    store: NatsStore,
    backend: Arc<FakeBackend>,
    provider: Arc<FakeProvider>,
    seq: u64,
}

/// Run one job of `r#type` through to Done.
async fn run_job(r#type: &str) -> Option<Ran> {
    let rig = rig().await?;
    commit_on_work(&rig);
    let (store, backend, provider) = (rig.store.clone(), rig.backend.clone(), rig.provider.clone());
    let (handle, _sink) = spawn_checked(rig.core);
    let job = handle.create_job(req(r#type)).await.unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();
    test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Done).await;
    Some(Ran {
        _server: rig._server,
        _repo: rig.repo,
        store,
        backend,
        provider,
        seq: job.id,
    })
}

/// The headline round trip (#313 A3): a declared identity produces a `0600`
/// token and a `0644` ADC config under `/chuggernaut/cloud/{identity}/`, the
/// token verifies against the issuer's public key, and its claims describe this
/// container of this job.
#[tokio::test]
async fn a_declared_identity_is_delivered_as_two_files_and_one_env_var() {
    let Some(ran) = run_job("cloud").await else {
        return;
    };
    let work = ran.provider.runs()[0].clone();

    let token_file = injected(&work.files, "/chuggernaut/cloud/gcp-artifact-writer/token");
    assert_eq!(token_file.mode, 0o600, "the token is the container's alone");
    let adc_file = injected(
        &work.files,
        "/chuggernaut/cloud/gcp-artifact-writer/adc.json",
    );
    assert_eq!(adc_file.mode, 0o644, "the adc config carries no secret");

    let adc: serde_json::Value = serde_json::from_slice(&adc_file.contents).unwrap();
    assert_eq!(adc["type"], "external_account");
    assert_eq!(adc["audience"], WRITER_AUDIENCE);
    assert_eq!(
        adc["credential_source"]["file"],
        "/chuggernaut/cloud/gcp-artifact-writer/token"
    );
    assert_eq!(
        work.env
            .get("GOOGLE_APPLICATION_CREDENTIALS")
            .map(String::as_str),
        Some("/chuggernaut/cloud/gcp-artifact-writer/adc.json"),
        "exactly one identity is granted, so the vendor env var points at it"
    );

    let claims = verified_claims(&token_file.contents, WRITER_AUDIENCE);
    assert_eq!(claims["iss"], ISSUER);
    assert_eq!(claims["sub"], "project:acme/api:type:cloud");
    assert_eq!(claims["project"], "acme/api");
    assert_eq!(claims["job_type"], "cloud");
    assert_eq!(claims["container"], "work");
    assert_eq!(claims["workload"], "acme/api:cloud:work");
    assert_eq!(claims["phase"], "Work");
}

/// **The feature-is-off path** (#313 A6's assert, stated as a test): an
/// evaluator declaring no identity receives no file under `/chuggernaut/cloud/`
/// and no `GOOGLE_APPLICATION_CREDENTIALS` — even while a *sibling* container of
/// the same job holds one. Nothing is inherited (spec §8.3).
#[tokio::test]
async fn an_undeclared_container_is_delivered_neither_file_nor_env_var() {
    let Some(ran) = run_job("cloud").await else {
        return;
    };
    let plain = launch(&ran, "./plain.sh");
    assert!(
        plain
            .files
            .iter()
            .all(|f| !f.container_path.starts_with("/chuggernaut/cloud")),
        "an undeclared evaluator receives no cloud credential file: {:?}",
        plain
            .files
            .iter()
            .map(|f| &f.container_path)
            .collect::<Vec<_>>()
    );
    assert!(
        !plain.env.contains_key("GOOGLE_APPLICATION_CREDENTIALS"),
        "and no env var pointing at one"
    );
}

/// A job type declaring nothing anywhere launches byte-identically to how it did
/// before the feature existed — no cloud file, no vendor env var, on either
/// container.
#[tokio::test]
async fn a_job_declaring_no_identity_launches_exactly_as_before() {
    let Some(ran) = run_job("plain").await else {
        return;
    };
    let eval = launch(&ran, "./ci.sh");
    let work = ran.provider.runs()[0].clone();
    for (files, env) in [(&eval.files, &eval.env), (&work.files, &work.env)] {
        assert!(
            files
                .iter()
                .all(|f| !f.container_path.starts_with("/chuggernaut/cloud"))
        );
        assert!(!env.contains_key("GOOGLE_APPLICATION_CREDENTIALS"));
    }
}

/// **The structural guarantee** (#313 A3): two containers of one job hold
/// different tokens whose `container` and `workload` claims differ — which is
/// exactly what lets a cloud-side binding on `attribute.workload` tell a work
/// container from an evaluator. Nothing is shared and nothing is inherited.
#[tokio::test]
async fn two_containers_of_one_job_hold_tokens_whose_container_claims_differ() {
    let Some(ran) = run_job("cloud").await else {
        return;
    };
    let work = ran.provider.runs()[0].clone();
    let ci = launch(&ran, "./ci.sh");
    let path = "/chuggernaut/cloud/gcp-artifact-writer/token";
    let work_token = injected(&work.files, path).contents;
    let ci_token = injected(&ci.files, path).contents;
    assert_ne!(
        work_token, ci_token,
        "no token is shared between containers"
    );

    let work_claims = verified_claims(&work_token, WRITER_AUDIENCE);
    let ci_claims = verified_claims(&ci_token, WRITER_AUDIENCE);
    assert_eq!(work_claims["container"], "work");
    assert_eq!(ci_claims["container"], "eval:ci");
    assert_eq!(work_claims["workload"], "acme/api:cloud:work");
    assert_eq!(ci_claims["workload"], "acme/api:cloud:eval:ci");
    assert_eq!(work_claims["sub"], ci_claims["sub"], "one policy identity");
    assert_ne!(work_claims["jti"], ci_claims["jti"], "one jti per mint");
    assert_ne!(work_claims["task_id"], ci_claims["task_id"]);
}

/// **The token is never recorded; its identity is** (spec §10.2/§10.3, #313 A6).
/// The task record and every `job-events` payload carry the identity, the
/// audience, the `sub`, the `workload`, the `jti` and the expiry — and the token
/// itself appears in none of them, nor in the `Debug` rendering a log would take.
#[tokio::test]
async fn the_token_is_absent_from_every_record_and_event_and_its_identity_is_not() {
    let Some(ran) = run_job("cloud").await else {
        return;
    };
    let work = ran.provider.runs()[0].clone();
    let token = String::from_utf8(
        injected(&work.files, "/chuggernaut/cloud/gcp-artifact-writer/token").contents,
    )
    .unwrap();

    let tasks = ran
        .store
        .tasks()
        .await
        .unwrap()
        .list_for_job("acme", "api", ran.seq)
        .await
        .unwrap();
    let work_task = tasks
        .iter()
        .find(|t| t.phase == types::TaskPhase::Work)
        .expect("a work task ran");
    let grant = &work_task.workload_identities[0];
    assert_eq!(grant.identity, "gcp-artifact-writer");
    assert_eq!(grant.audience, WRITER_AUDIENCE);
    assert_eq!(grant.workload, "acme/api:cloud:work");
    assert!(!grant.jti.is_empty() && grant.expires_at > work_task.created_at);

    let records = serde_json::to_string(&tasks).unwrap();
    assert!(
        !records.contains(&token),
        "no task record carries the token"
    );
    assert!(records.contains(&grant.jti), "every record carries its jti");

    let events = ran.store.read_stream("job-events", 500).await.unwrap();
    let events: Vec<serde_json::Value> = events
        .iter()
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    let stream = serde_json::to_string(&events).unwrap();
    assert!(!stream.contains(&token), "no event carries the token");
    assert!(
        stream.contains(&grant.jti),
        "the launch event carries the identity: {stream}"
    );
    assert!(
        !format!("{:?}", work.files).contains(&token),
        "and a logged launch config renders the token as bytes, never as itself"
    );
}

/// Two declared identities: two file pairs, two audiences, and **no**
/// `GOOGLE_APPLICATION_CREDENTIALS` — a silent "first one wins" would make which
/// credential a build used depend on map ordering (#313 A3).
#[tokio::test]
async fn two_identities_deliver_two_pairs_and_set_no_google_env_var() {
    let Some(ran) = run_job("cloud-two").await else {
        return;
    };
    let work = ran.provider.runs()[0].clone();
    let writer = verified_claims(
        &injected(&work.files, "/chuggernaut/cloud/gcp-artifact-writer/token").contents,
        WRITER_AUDIENCE,
    );
    let deployer = verified_claims(
        &injected(&work.files, "/chuggernaut/cloud/gcp-deployer/token").contents,
        DEPLOYER_AUDIENCE,
    );
    assert_eq!(writer["aud"], WRITER_AUDIENCE);
    assert_eq!(deployer["aud"], DEPLOYER_AUDIENCE);
    assert_ne!(writer["jti"], deployer["jti"], "one token per identity");
    injected(&work.files, "/chuggernaut/cloud/gcp-deployer/adc.json");
    assert!(
        !work.env.contains_key("GOOGLE_APPLICATION_CREDENTIALS"),
        "with two identities the script names the path it wants"
    );
}

/// **One placement, one event** (§6.3, §10.3): the fleet refuses the agent
/// evaluator's first launch, so nothing announces a token that reached no
/// container. The queued relaunch mints afresh, and the single `task-launched`
/// carries the `jti` of the token actually delivered — not the deferred one.
#[tokio::test]
async fn a_deferred_agent_launch_announces_only_the_token_it_delivered() {
    let Some(rig) = rig().await else { return };
    commit_on_work(&rig);
    let (store, backend, provider) = (rig.store.clone(), rig.backend.clone(), rig.provider.clone());
    let full = Arc::new(AtomicBool::new(true));
    let f = full.clone();
    backend.fail_launch_no_capacity_if(move |c| {
        (f.load(Ordering::SeqCst) && c.files.iter().any(is_cloud_file))
            .then(|| "no free slots on any node".to_string())
    });
    let (handle, _sink) = spawn_checked(rig.core);
    let job = handle.create_job(req("cloud-agent-eval")).await.unwrap();
    handle.release_job("acme", "api", job.id).await.unwrap();

    let deferred =
        test_utils::wait::task_where(&store, "acme", "api", job.id, "eval queued", |t| {
            t.phase == types::TaskPhase::Evaluation && t.state == types::TaskState::Pending
        })
        .await;
    assert!(deferred.container_id.is_none(), "a queued task holds none");
    let deferred_jti = deferred.workload_identities[0].jti.clone();
    assert!(
        launched_events(&store, deferred.id).await.is_empty(),
        "a refused launch announces no delivery"
    );

    let h = handle.clone();
    let task_id = deferred.id;
    provider.on_run(move |_| async move {
        h.submit_eval(
            "acme",
            "api",
            1,
            task_id,
            EvalSubmission {
                pass: true,
                abort: false,
                structured: None,
                token_usage: None,
                cover_html: None,
            },
        )
        .await
        .unwrap();
    });
    full.store(false, Ordering::SeqCst);
    handle.trigger_scan().await.unwrap();
    test_utils::wait::job_state(&store, "acme", "api", job.id, JobState::Done).await;

    let events = launched_events(&store, task_id).await;
    assert_eq!(events.len(), 1, "one placement, one event: {events:?}");
    let delivered = &events[0]["workload_identities"][0]["jti"];
    assert_ne!(
        delivered.as_str(),
        Some(deferred_jti.as_str()),
        "the relaunch minted afresh"
    );
    let eval = test_utils::wait::task_where(&store, "acme", "api", job.id, "eval done", |t| {
        t.id == task_id && t.state == types::TaskState::Done
    })
    .await;
    assert_eq!(
        delivered.as_str(),
        Some(eval.workload_identities[0].jti.as_str()),
        "the event names the token the record does"
    );
}

/// Every `task-launched` event for one task, in stream order.
async fn launched_events(store: &NatsStore, task_id: u64) -> Vec<serde_json::Value> {
    store
        .read_stream("job-events", 500)
        .await
        .unwrap()
        .iter()
        .filter_map(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .filter(|e| e["event_type"] == "task-launched" && e["task_id"] == task_id)
        .collect()
}

/// Whether an injected file is one of the `/chuggernaut/cloud/` credential pair.
fn is_cloud_file(file: &container::InjectedFile) -> bool {
    file.container_path.starts_with("/chuggernaut/cloud")
}

/// One injected file by path — the assertion that it was injected at all.
fn injected(files: &[container::InjectedFile], path: &str) -> container::InjectedFile {
    files
        .iter()
        .find(|f| f.container_path == path)
        .unwrap_or_else(|| panic!("no injected file at {path}"))
        .clone()
}

/// The claims of an injected token, verified against the issuer's public key and
/// the audience it was minted for — a token that does not verify is not a token.
fn verified_claims(token: &[u8], audience: &str) -> serde_json::Value {
    let token = std::str::from_utf8(token).unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_audience(&[audience]);
    validation.set_issuer(&[ISSUER]);
    jsonwebtoken::decode::<serde_json::Value>(
        token,
        &jsonwebtoken::DecodingKey::from_rsa_pem(ISSUER_PUBLIC.as_bytes()).unwrap(),
        &validation,
    )
    .expect("the delivered token verifies against the issuer key")
    .claims
}

/// One command container's launch, found by the script it runs.
fn launch(ran: &Ran, run: &str) -> container::ContainerLaunchConfig {
    ran.backend
        .launches()
        .into_iter()
        .find(|c| c.cmd.iter().any(|arg| arg.contains(run)))
        .unwrap_or_else(|| panic!("no container ran {run}"))
}

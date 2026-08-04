//! Workload-token minting and delivery at container launch (spec §7.4, §8.3;
//! design #313 A3/A5/A6).
//!
//! - **Accepts:** one container launch — the identities its *own* block
//!   declares, which container it is, and its resolved `task_timeout`.
//! - **Emits:** a [`WorkloadDelivery`] — two injected files per granted
//!   identity, the one env var that points at them, and the
//!   [`types::WorkloadIdentityGrant`] rows recorded in each token's place.
//! - **Guarantees:** one token per (container, identity), minted at that
//!   container's own launch and inherited by nothing, so two containers of one
//!   job hold tokens whose `container` and `workload` claims differ; a
//!   container declaring none receives no file and no env var, asserted at the
//!   injection site every declaration-resolving launch passes through (triage
//!   and escalation launch no declaring block); the mint loop is bounded; the
//!   token value reaches the container and nothing else (§10.2).
//! - **Spec:** §7.4, §8.3, §10.2, §10.3, §4.2.
//!
//! The decision half — which claims a token carries, how long it lives, and the
//! shape of the files that carry it — is pure in `auth::workload`; this module
//! is the I/O around it (STYLE.md Tier 2 #1, contracts.md).

use crate::core::{Core, CoreError, Result};
use auth::workload::{
    CLOUD_CREDENTIAL_DIR, GOOGLE_CREDENTIALS_ENV, WorkloadContainer, WorkloadTokenRequest,
    adc_document, adc_file_path, google_credentials_env, token_file_path,
};
use std::collections::HashMap;
use std::time::Duration;
use types::{JobType, Task, TaskPhase, WorkloadIdentityGrant};

/// The per-container cap on granted identities (STYLE.md Tier 2 #3). A
/// declaration over it fails the launch loudly rather than minting unbounded
/// credentials.
pub(crate) const IDENTITIES_PER_CONTAINER_MAX: usize = 8;

/// What one container launch receives for the identities it declared.
#[derive(Debug, Default)]
pub(crate) struct WorkloadDelivery {
    files: Vec<container::InjectedFile>,
    env: HashMap<String, String>,
    audit: Vec<WorkloadIdentityGrant>,
}

impl WorkloadDelivery {
    /// Merge into one launch's file set and env, returning the audit rows the
    /// caller records in the tokens' place — every launch that resolves a
    /// declaration passes through here, so the empty-declaration invariant is
    /// asserted against the launch's real files (#313 A6). The env half matches
    /// only a path under [`CLOUD_CREDENTIAL_DIR`], because a project may
    /// legitimately name `GOOGLE_APPLICATION_CREDENTIALS` in its own `vars:`
    /// (`docs/implementation-notes.md`).
    pub(crate) fn merge_into(
        self,
        env: &mut HashMap<String, String>,
        files: &mut Vec<container::InjectedFile>,
    ) -> Vec<WorkloadIdentityGrant> {
        let granted_none = self.files.is_empty();
        files.extend(self.files);
        env.extend(self.env);
        assert!(
            !granted_none
                || (!files
                    .iter()
                    .any(|f| f.container_path.starts_with(CLOUD_CREDENTIAL_DIR))
                    && env
                        .get(GOOGLE_CREDENTIALS_ENV)
                        .is_none_or(|path| !path.starts_with(CLOUD_CREDENTIAL_DIR))),
            "a container declaring no cloud identity is delivered no {CLOUD_CREDENTIAL_DIR} \
             credential file and no {GOOGLE_CREDENTIALS_ENV} pointing into it (#313 A6)"
        );
        self.audit
    }

    /// [`Self::merge_into`] for a command container's assembled launch config.
    pub(crate) fn apply(
        self,
        config: &mut container::ContainerLaunchConfig,
    ) -> Vec<WorkloadIdentityGrant> {
        let (env, files) = (&mut config.env, &mut config.files);
        self.merge_into(env, files)
    }
}

/// The identities one task's own block declares, with the container claim they
/// are minted under — the §8.3 per-container scoping rule, resolved from the
/// task rather than from the call site so a queued relaunch resolves it
/// identically. `None` for a task that launches no container of its own.
pub(crate) fn declared_for_task(
    job_type: &JobType,
    task: &Task,
) -> Option<(WorkloadContainer, Vec<String>)> {
    match task.phase {
        TaskPhase::Work => Some((
            WorkloadContainer::Work,
            job_type.work.workload_identities.clone(),
        )),
        TaskPhase::WrapUp => Some((
            WorkloadContainer::WrapUp,
            job_type.wrap_up.workload_identities.clone(),
        )),
        TaskPhase::Evaluation | TaskPhase::MergeGate => {
            let name = task.evaluator.as_ref()?;
            let evaluator = job_type.eval.iter().find(|e| &e.name == name)?;
            Some((
                WorkloadContainer::Evaluator { name: name.clone() },
                evaluator.workload_identities.clone(),
            ))
        }
        TaskPhase::Triage | TaskPhase::Escalation => None,
    }
}

/// The `task-launched` payload for one task, carrying its minted identities
/// when it has any (§6.3, design #313 A6). The field is omitted entirely for a
/// task that minted nothing, so its event is today's event unchanged.
pub(crate) fn task_launched_payload(task: &Task) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "task_id": task.id, "phase": format!("{:?}", task.phase),
    });
    if !task.workload_identities.is_empty()
        && let (Some(object), Ok(grants)) = (
            payload.as_object_mut(),
            serde_json::to_value(&task.workload_identities),
        )
    {
        object.insert("workload_identities".into(), grants);
    }
    payload
}

/// One container launch, as the mint takes it: which job and task, which
/// container of it, what that container's own block declared, and the resolved
/// `task_timeout` the token's TTL is capped against (§7.4).
pub(crate) struct WorkloadLaunch<'a> {
    pub owner: &'a str,
    pub project: &'a str,
    pub seq: u64,
    pub task_id: u64,
    pub job_type: &'a JobType,
    pub container: WorkloadContainer,
    pub declared: &'a [String],
    pub creds_ttl: Duration,
}

impl Core {
    /// Mint one token per declared identity for one container launch (§7.4):
    /// the `cloud-identities.*` read, the TTL resolved against this container's
    /// own `task_timeout`, and the files that carry the result.
    pub(crate) async fn workload_delivery(
        &self,
        launch: &WorkloadLaunch<'_>,
    ) -> Result<WorkloadDelivery> {
        let granted = workload_granted_names(launch.declared);
        if granted.is_empty() {
            return Ok(WorkloadDelivery::default());
        }
        if granted.len() > IDENTITIES_PER_CONTAINER_MAX {
            return Err(CoreError::Config(format!(
                "{}/{}#{} task {} declares {} cloud identities, over the \
                 {IDENTITIES_PER_CONTAINER_MAX} a container may hold",
                launch.owner,
                launch.project,
                launch.seq,
                launch.task_id,
                granted.len()
            )));
        }
        let mut delivery = WorkloadDelivery::default();
        for identity in &granted {
            self.workload_mint_one(launch, identity, &mut delivery)
                .await?;
        }
        if let Some(path) = google_credentials_env(&granted) {
            delivery.env.insert(GOOGLE_CREDENTIALS_ENV.into(), path);
        }
        assert_eq!(
            delivery.files.len(),
            2 * granted.len(),
            "each granted identity is delivered exactly its token and its adc.json"
        );
        assert_eq!(
            delivery.env.len(),
            usize::from(granted.len() == 1),
            "GOOGLE_APPLICATION_CREDENTIALS is set for exactly one identity and never otherwise"
        );
        Ok(delivery)
    }

    /// One identity's mint: its record, its token, the two files that carry it,
    /// and the audit row recorded in the token's place.
    async fn workload_mint_one(
        &self,
        launch: &WorkloadLaunch<'_>,
        identity: &str,
        delivery: &mut WorkloadDelivery,
    ) -> Result<()> {
        let (owner, project, seq) = (launch.owner, launch.project, launch.seq);
        let signer = self.workload_signer.as_ref().ok_or_else(|| {
            CoreError::Config(format!(
                "{owner}/{project}#{seq} declares cloud identity '{identity}', but this \
                 platform has no issuer keypair (oidc_private.pem, spec §12.1)"
            ))
        })?;
        let record = self.cloud_identity(owner, project, identity).await?;
        let request = WorkloadTokenRequest {
            owner: owner.to_string(),
            project: project.to_string(),
            job_type: launch.job_type.name.clone(),
            container: launch.container.clone(),
            job_seq: seq,
            task_id: launch.task_id,
            audience: record.audience.clone(),
            task_timeout_secs: launch.creds_ttl.as_secs(),
            token_ttl_secs_max: record.token_ttl_secs,
        };
        let minted = signer
            .mint(&request, chrono::Utc::now())
            .map_err(|e| CoreError::Config(format!("minting a workload token: {e}")))?;
        let adc = serde_json::to_vec_pretty(&adc_document(identity, &record))?;
        delivery.files.push(container::InjectedFile {
            container_path: token_file_path(identity),
            contents: minted.token().as_bytes().to_vec(),
            mode: 0o600,
            artifact: None,
        });
        delivery.files.push(container::InjectedFile {
            container_path: adc_file_path(identity),
            contents: adc,
            mode: 0o644,
            artifact: None,
        });
        let audit = minted.audit();
        delivery.audit.push(WorkloadIdentityGrant {
            identity: identity.to_string(),
            audience: audit.audience.clone(),
            sub: audit.sub.clone(),
            workload: audit.workload.clone(),
            jti: audit.jti.clone(),
            expires_at: audit.expires_at,
        });
        Ok(())
    }

    /// One `cloud-identities.{owner}.{project}.{name}` record (§8.3). Release
    /// validation already refused a name without one, so a miss here is a
    /// record deleted under a running job.
    async fn cloud_identity(
        &self,
        owner: &str,
        project: &str,
        identity: &str,
    ) -> Result<types::CloudIdentity> {
        self.store
            .raw_bucket(store::buckets::CLOUD_IDENTITIES)
            .await?
            .get_json::<types::CloudIdentity>(&format!("{owner}.{project}.{identity}"))
            .await?
            .ok_or_else(|| {
                CoreError::NotFound(format!(
                    "cloud identity '{identity}' is not set for {owner}/{project}"
                ))
            })
    }

    /// Stamp what one launch minted onto the task record (`tasks.*` KV), in
    /// place of the tokens themselves (§10.2). The event that carries the same
    /// rows is [`Core::publish_task_launched`], fired by whichever site confirms
    /// the placement — so nothing announces a delivery that never happened.
    pub(crate) async fn record_workload_identities(
        &mut self,
        task: &mut Task,
        audit: Vec<WorkloadIdentityGrant>,
    ) -> Result<()> {
        if audit.is_empty() {
            return Ok(());
        }
        assert!(
            audit.iter().all(|g| !g.jti.is_empty()),
            "every recorded grant carries the jti a replay is attributed by (#313 A6)"
        );
        task.workload_identities = audit;
        self.task_put(task).await
    }

    /// [`Self::record_workload_identities`] for a launch path that holds only a
    /// task id — the agent evaluator, whose record is already persisted.
    pub(crate) async fn record_workload_identities_for(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        task_id: u64,
        audit: Vec<WorkloadIdentityGrant>,
    ) -> Result<()> {
        if audit.is_empty() {
            return Ok(());
        }
        let Some(mut task) = self.tasks.get(owner, project, seq, task_id).await? else {
            return Ok(());
        };
        self.record_workload_identities(&mut task, audit).await
    }
}

/// The granted set for a declaration: its names in declared order, each once.
/// A duplicate would deliver one path twice and make the single-identity env
/// rule read as two.
fn workload_granted_names(declared: &[String]) -> Vec<String> {
    let mut granted: Vec<String> = Vec::new();
    for name in declared {
        if !granted.iter().any(|held| held == name) {
            granted.push(name.clone());
        }
    }
    assert!(granted.len() <= declared.len(), "dedup never adds a name");
    granted
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn task(phase: TaskPhase, evaluator: Option<&str>) -> Task {
        Task {
            id: 3,
            job_seq: 1,
            project: "acme/api".into(),
            phase,
            cycle: 1,
            kind: types::TaskKind::Command {
                run: "./x.sh".into(),
            },
            state: types::TaskState::Running,
            attempt: 1,
            evaluator: evaluator.map(String::from),
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            pending_reason: None,
            queued_at: None,
            rework_reason: None,
            infra_loss: false,
            session_id: None,
            reviewed_tip: None,
            workload_identities: vec![],
            result: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    fn job_type() -> JobType {
        serde_yaml::from_str(
            "name: deploy\nimage: img\nmin_dispatcher: 5\n\
             work:\n  type: command\n  run: ./deploy.sh\n  \
             workload_identities: [gcp-deployer]\n\
             wrap_up:\n  run: ./publish.sh\n  workload_identities: [gcp-publisher]\n\
             eval:\n  - name: ci\n    type: command\n    run: ./ci.sh\n",
        )
        .unwrap()
    }

    /// Each container resolves its own declaration and nothing else: the
    /// evaluator that declares none gets none even though `work` declares one
    /// (spec §8.3, the non-inheritance rule).
    #[test]
    fn a_declaration_is_resolved_per_container_and_inherited_by_nothing() {
        let job_type = job_type();
        assert_eq!(
            declared_for_task(&job_type, &task(TaskPhase::Work, None)),
            Some((WorkloadContainer::Work, vec!["gcp-deployer".to_string()]))
        );
        assert_eq!(
            declared_for_task(&job_type, &task(TaskPhase::WrapUp, None)),
            Some((WorkloadContainer::WrapUp, vec!["gcp-publisher".to_string()]))
        );
        assert_eq!(
            declared_for_task(&job_type, &task(TaskPhase::Evaluation, Some("ci"))),
            Some((WorkloadContainer::Evaluator { name: "ci".into() }, vec![])),
            "an evaluator declaring none inherits none from work"
        );
        assert_eq!(
            declared_for_task(&job_type, &task(TaskPhase::Triage, None)),
            None
        );
    }

    /// A repeated name is one grant, so the single-identity env rule reads the
    /// count an operator meant.
    #[test]
    fn a_repeated_declaration_grants_one_identity() {
        assert_eq!(
            workload_granted_names(&["a".to_string(), "a".to_string(), "b".to_string()]),
            ["a", "b"]
        );
        assert!(workload_granted_names(&[]).is_empty());
    }

    fn cloud_file(identity: &str) -> container::InjectedFile {
        container::InjectedFile {
            container_path: token_file_path(identity),
            contents: b"inherited".to_vec(),
            mode: 0o600,
            artifact: None,
        }
    }

    /// The feature-is-off path, asserted at the injection site: a launch that
    /// granted nothing merges nothing and leaves the launch's own files alone.
    #[test]
    fn a_launch_that_granted_nothing_delivers_nothing() {
        let mut env = HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
        let mut files = vec![container::InjectedFile {
            container_path: "/chuggernaut/ssh/id".into(),
            contents: b"key".to_vec(),
            mode: 0o600,
            artifact: None,
        }];
        assert!(
            WorkloadDelivery::default()
                .merge_into(&mut env, &mut files)
                .is_empty()
        );
        assert_eq!((env.len(), files.len()), (1, 1));
    }

    /// The inheritance bug the assert exists to catch: a credential file in the
    /// launch of a container that declared nothing fails the launch loudly
    /// (#313 A6), rather than delivering a credential nobody declared.
    #[test]
    #[should_panic(expected = "credential file")]
    fn a_credential_file_in_an_undeclared_launch_fails_loudly() {
        let (mut env, mut files) = (HashMap::new(), vec![cloud_file("inherited")]);
        WorkloadDelivery::default().merge_into(&mut env, &mut files);
    }

    /// The same assert covers the env var alone — a `GOOGLE_APPLICATION_CREDENTIALS`
    /// pointing into the credential directory with no grant behind it points a
    /// build at a token it never got.
    #[test]
    #[should_panic(expected = "GOOGLE_APPLICATION_CREDENTIALS")]
    fn a_vendor_env_var_in_an_undeclared_launch_fails_loudly() {
        let mut env = HashMap::from([(
            GOOGLE_CREDENTIALS_ENV.to_string(),
            adc_file_path("inherited"),
        )]);
        WorkloadDelivery::default().merge_into(&mut env, &mut Vec::new());
    }

    /// A project's own `GOOGLE_APPLICATION_CREDENTIALS` var — legitimate, since
    /// only `CHUG_` is reserved — survives an undeclared launch rather than
    /// panicking the single writer.
    #[test]
    fn a_project_owned_vendor_env_var_is_not_a_leaked_grant() {
        let mut env = HashMap::from([(
            GOOGLE_CREDENTIALS_ENV.to_string(),
            "/opt/sa.json".to_string(),
        )]);
        assert!(
            WorkloadDelivery::default()
                .merge_into(&mut env, &mut Vec::new())
                .is_empty()
        );
        assert_eq!(env[GOOGLE_CREDENTIALS_ENV], "/opt/sa.json");
    }

    /// A task that minted nothing publishes today's payload, unchanged.
    #[test]
    fn the_launch_payload_omits_the_fields_a_task_minted_nothing_for() {
        let mut task = task(TaskPhase::Work, None);
        let payload = task_launched_payload(&task);
        assert_eq!(payload["task_id"], 3);
        assert!(payload.get("workload_identities").is_none());
        task.workload_identities = vec![WorkloadIdentityGrant {
            identity: "gcp-deployer".into(),
            audience: "//iam.googleapis.com/x".into(),
            sub: "project:acme/api:type:deploy".into(),
            workload: "acme/api:deploy:work".into(),
            jti: "j".into(),
            expires_at: chrono::Utc::now(),
        }];
        let payload = task_launched_payload(&task);
        assert_eq!(
            payload["workload_identities"][0]["identity"],
            "gcp-deployer"
        );
        assert_eq!(payload["workload_identities"][0]["jti"], "j");
    }
}

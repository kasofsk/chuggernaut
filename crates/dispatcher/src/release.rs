//! Release validation, the ref-reading half (spec §2.2): loading and merging
//! `.chug/jobs/*.yaml` at a git ref, and checking prompt paths / KV names. The pure
//! half — the validation-error vocabulary, graph wiring rules, and the
//! additive-evaluator merge — lives in `chuggernaut_domain::release`
//! (refactor-plan C1) and is re-exported here so callers keep one
//! `release::*` surface.
//!
//! - **Accepts:** a job and the config tree at a given ref.
//! - **Emits:** a validation verdict — the merged job type, or static-config
//!   errors.
//! - **Guarantees:** reads only (through the `vcs` port), no state writes;
//!   the same rules run at release and at Blocked→Ready re-validation.
//! - **Spec:** §2.2, §2.3, §14.

pub use chuggernaut_domain::release::{
    APPROVAL_EVALUATOR_NAME, KvNames, SCHEMA_SKEW_FIELD, ValidationError, approval_evaluator,
    wiring_errors, with_job_evaluators,
};
use types::{Evaluator, EvaluatorType, Job, JobType, ProjectDefaults, WorkType};
use vcs::RepoManager;

/// The project-wide defaults overlay, config-root-relative (§1.1).
const DEFAULTS_RELATIVE: &str = "jobs/_defaults.yaml";

/// Load `.chug/jobs/{type}.yaml` at `reference`, apply `.chug/jobs/_defaults.yaml`
/// if present, and run the §1.1 field rules. Returns the merged job type on
/// success so callers validate exactly what will execute.
pub async fn load_job_type(
    repo: &RepoManager,
    owner: &str,
    project: &str,
    reference: &str,
    type_name: &str,
    job_seq: Option<u64>,
) -> Result<JobType, Vec<ValidationError>> {
    let relative = format!("jobs/{type_name}.yaml");
    let path = types::config_path(&relative);
    let Some(content) = read_config(repo, owner, project, reference, &relative).await? else {
        return Err(vec![ValidationError::new(
            job_seq,
            "type",
            format!("job type file '{path}' does not exist at {reference}"),
        )]);
    };
    let job_type = JobType::parse(&content).map_err(|e| {
        vec![ValidationError::new(
            job_seq,
            "type",
            format!("'{path}' failed to parse: {e}"),
        )]
    })?;

    for w in job_type.config_warnings() {
        tracing::warn!(
            file = %path,
            field = %w.field,
            "job-type config warning: {w} (deploy the dispatcher to enable it)"
        );
    }

    if let Some(needed) = job_type.requires_dispatcher(types::CONFIG_SCHEMA_EPOCH) {
        return Err(vec![ValidationError::new(
            job_seq,
            SCHEMA_SKEW_FIELD,
            format!(
                "'{path}' requires dispatcher schema epoch >= {needed} but this dispatcher is at \
                 {}: deploy the newer dispatcher, or land the config behind a version gate",
                types::CONFIG_SCHEMA_EPOCH
            ),
        )]);
    }

    let defaults_path = types::config_path(DEFAULTS_RELATIVE);
    let merged = match read_config(repo, owner, project, reference, DEFAULTS_RELATIVE).await? {
        Some(defaults_yaml) => {
            let defaults = ProjectDefaults::parse(&defaults_yaml).map_err(|e| {
                vec![ValidationError::new(
                    None,
                    "eval",
                    format!("'{defaults_path}' failed to parse: {e}"),
                )]
            })?;
            job_type
                .with_defaults(&defaults)
                .map_err(|e| vec![ValidationError::new(job_seq, "eval", e.to_string())])?
        }
        None => job_type,
    };

    let field_errors: Vec<ValidationError> = merged
        .validate()
        .into_iter()
        .map(|e| ValidationError::new(job_seq, "type", e.to_string()))
        .collect();
    if field_errors.is_empty() {
        Ok(merged)
    } else {
        Err(field_errors)
    }
}

/// Static configuration checks (§2.2): prompt paths exist at `reference`; the
/// job's supplied inputs satisfy the declaration at `reference`; declared secrets
/// and vars exist in KV. Pass `kv: None` for the Blocked→Ready re-validation,
/// which re-checks files only.
///
/// The input check runs on **every** pass through here, unlike the KV one: the
/// declaration is a file-derived fact pinned to `reference`, so a type that grew
/// a `required` input between release and Blocked→Ready must fail the second pass
/// too (design #311 Decision 3).
pub async fn static_errors(
    repo: &RepoManager,
    owner: &str,
    project: &str,
    reference: &str,
    job: &Job,
    job_type: &JobType,
    kv: Option<&KvNames>,
) -> Result<Vec<ValidationError>, Vec<ValidationError>> {
    let seq = Some(job.id);
    let mut errs = Vec::new();
    let mut require_file = Vec::new();

    if let Some(t) = &job.timeout
        && let Err(e) = types::parse_duration(t)
    {
        errs.push(ValidationError::new(seq, "timeout", e.to_string()));
    }

    if job_type.work.r#type == WorkType::Agent {
        if let Some(p) = &job_type.work.prompt {
            require_file.push(("work.prompt".to_string(), p.clone()));
        }
        if let Some(review) = &job_type.work.review {
            require_file.push(("work.review.prompt".to_string(), review.prompt.clone()));
        }
    }
    for (i, e) in job_type.eval.iter().enumerate() {
        if matches!(e.r#type, EvaluatorType::Agent | EvaluatorType::Human)
            && e.name != APPROVAL_EVALUATOR_NAME
            && let Some(p) = &e.prompt
        {
            require_file.push((format!("eval[{i}].prompt"), p.clone()));
        }
    }
    for (field, path) in require_file {
        if read(repo, owner, project, reference, &path)
            .await?
            .is_none()
        {
            errs.push(ValidationError::new(
                seq,
                field,
                format!("prompt file '{path}' does not exist at {reference}"),
            ));
        }
    }

    errs.extend(crate::inputs::input_errors(
        seq,
        &job_type.inputs,
        &job.inputs,
    ));

    if let Some(kv) = kv {
        errs.extend(static_errors_kv(seq, job_type, kv));
    }
    Ok(errs)
}

/// The KV-name half of [`static_errors`] (§2.2), pure so it needs no repo or
/// store: every declared secret and var must exist in the project's buckets, and
/// neither may use a reserved prefix ([`crate::exec::reserved_env_prefix`]).
/// Spec §4.1 has what each prefix protects, and why the rule covers vars as well
/// as secrets (design #311 Decision 4, design #517 S1).
fn static_errors_kv(seq: Option<u64>, job_type: &JobType, kv: &KvNames) -> Vec<ValidationError> {
    let secrets = job_type.work.secrets.iter().chain(
        job_type
            .eval
            .iter()
            .flat_map(|e: &Evaluator| e.secrets.iter()),
    );
    let declared = secrets
        .map(|name| ("secrets", "secret", name, kv.secrets.contains(name)))
        .chain(
            job_type
                .vars
                .iter()
                .map(|name| ("vars", "var", name, kv.vars.contains(name))),
        );
    let mut errs = Vec::new();
    for (field, noun, name, present) in declared {
        if let Some((prefix, why)) = crate::exec::reserved_env_prefix(name) {
            errs.push(ValidationError::new(
                seq,
                field,
                format!("{noun} '{name}' uses the reserved '{prefix}' prefix ({why})"),
            ));
        } else if !present {
            errs.push(ValidationError::new(
                seq,
                field,
                format!("{noun} '{name}' is not set"),
            ));
        }
    }
    errs.extend(static_errors_cloud_identities(seq, job_type, kv));
    errs
}

/// The cloud-identity half of [`static_errors_kv`] (§2.2, design #313 A5):
/// every name declared in a container's `workload_identities:` has a record in
/// the `cloud-identities.*` bucket. This is what the named reference buys — a
/// misdeclared identity fails here with a fixable per-name message instead of
/// at token exchange inside the container.
fn static_errors_cloud_identities(
    seq: Option<u64>,
    job_type: &JobType,
    kv: &KvNames,
) -> Vec<ValidationError> {
    let declared = job_type
        .work
        .workload_identities
        .iter()
        .map(|name| ("work.workload_identities", name))
        .chain(
            job_type
                .wrap_up
                .workload_identities
                .iter()
                .map(|name| ("wrap_up.workload_identities", name)),
        )
        .chain(job_type.eval.iter().flat_map(|e: &Evaluator| {
            e.workload_identities
                .iter()
                .map(|name| ("eval.workload_identities", name))
        }));
    declared
        .filter(|(_, name)| !kv.cloud_identities.contains(*name))
        .map(|(field, name)| {
            ValidationError::new(seq, field, format!("cloud identity '{name}' is not set"))
        })
        .collect()
}

/// The config directories a landing is scanned for version skew (§14.3): a
/// schedule file carries `min_dispatcher` with the same meaning a job type
/// does, and merging either one ahead of the binary breaks the same way.
const SKEW_SCAN_DIRS: [&str; 2] = ["jobs", types::SCHEDULES_DIR];

/// Config files one skew scan reads, per directory (docs/reference/style.md Tier 2 #3).
const SKEW_SCAN_FILES_MAX: usize = 256;

/// The first config file at `reference` declaring a `min_dispatcher` above this
/// binary's [`types::CONFIG_SCHEMA_EPOCH`] (spec §14.2), scanned in path order
/// so the same branch always names the same file.
///
/// Reads the repo and nothing else — no API call, no credential, no
/// environment: the dispatcher performs the merge and already knows its own
/// epoch, which is what makes this half of the gate unable to degrade to a pass
/// (§14.3).
pub async fn branch_config_skew(
    repo: &RepoManager,
    owner: &str,
    project: &str,
    reference: &str,
) -> vcs::Result<Option<types::ConfigSkew>> {
    let tree = repo.tree(owner, project, reference).await?;
    for dir in SKEW_SCAN_DIRS {
        let mut entries = crate::project_config::entries(&tree, dir, ".yaml");
        if entries.len() > SKEW_SCAN_FILES_MAX {
            let refused = entries.split_off(SKEW_SCAN_FILES_MAX).len();
            tracing::error!(
                "{owner}/{project}@{reference} carries more than {SKEW_SCAN_FILES_MAX} \
                 '{dir}' config files; {refused} go unscanned for version skew"
            );
        }
        for entry in entries {
            let Some(content) = repo
                .read_file_at(owner, project, reference, &entry.path)
                .await?
            else {
                continue;
            };
            if let Some(needed) =
                types::config_requires_dispatcher(&content, types::CONFIG_SCHEMA_EPOCH)
            {
                return Ok(Some(types::ConfigSkew {
                    path: entry.path,
                    needed,
                    running: types::CONFIG_SCHEMA_EPOCH,
                }));
            }
        }
    }
    Ok(None)
}

/// [`project_config::read_file`] in the validation-error vocabulary: the config
/// root resolution lives there, the diagnostic wrapping here.
async fn read_config(
    repo: &RepoManager,
    owner: &str,
    project: &str,
    reference: &str,
    relative: &str,
) -> Result<Option<String>, Vec<ValidationError>> {
    crate::project_config::read_file(repo, owner, project, reference, relative)
        .await
        .map(|found| found.map(|file| file.content))
        .map_err(|e| {
            vec![ValidationError::new(
                None,
                relative.to_string(),
                format!("vcs error reading '{relative}': {e}"),
            )]
        })
}

async fn read(
    repo: &RepoManager,
    owner: &str,
    project: &str,
    reference: &str,
    path: &str,
) -> Result<Option<String>, Vec<ValidationError>> {
    repo.read_file_at(owner, project, reference, path)
        .await
        .map_err(|e| {
            vec![ValidationError::new(
                None,
                path.to_string(),
                format!("vcs error reading '{path}': {e}"),
            )]
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn job_type_with(secrets: &[&str], vars: &[&str]) -> JobType {
        let mut jt = JobType::parse(
            "name: deploy\nimage: img:latest\nwork:\n  type: command\n  run: ./deploy.sh\n",
        )
        .unwrap();
        jt.work.secrets = secrets.iter().map(|s| (*s).to_string()).collect();
        jt.vars = vars.iter().map(|v| (*v).to_string()).collect();
        jt
    }

    fn kv(secrets: &[&str], vars: &[&str]) -> KvNames {
        kv_with_identities(secrets, vars, &[])
    }

    fn kv_with_identities(secrets: &[&str], vars: &[&str], identities: &[&str]) -> KvNames {
        KvNames {
            secrets: secrets.iter().map(|s| (*s).to_string()).collect(),
            vars: vars.iter().map(|v| (*v).to_string()).collect(),
            cloud_identities: identities.iter().map(|i| (*i).to_string()).collect(),
        }
    }

    #[test]
    fn declared_names_must_exist_in_kv() {
        let jt = job_type_with(&["DEPLOY_KEY"], &["RUST_EDITION"]);
        assert_eq!(
            static_errors_kv(Some(7), &jt, &kv(&["DEPLOY_KEY"], &["RUST_EDITION"])),
            vec![]
        );
        let errs = static_errors_kv(Some(7), &jt, &kv(&[], &[]));
        let rendered = format!("{errs:?}");
        assert!(
            rendered.contains("secret 'DEPLOY_KEY' is not set"),
            "{rendered}"
        );
        assert!(
            rendered.contains("var 'RUST_EDITION' is not set"),
            "{rendered}"
        );
    }

    #[test]
    fn reserved_prefix_is_rejected_for_vars_as_well_as_secrets() {
        for (secrets, vars, field) in [
            (vec!["CHUG_ORIGIN_PAT"], vec![], "secrets"),
            (vec![], vec!["CHUG_PHASE"], "vars"),
        ] {
            let jt = job_type_with(&secrets, &vars);
            let errs = static_errors_kv(Some(7), &jt, &kv(&["CHUG_ORIGIN_PAT"], &["CHUG_PHASE"]));
            assert_eq!(errs.len(), 1, "{errs:?}");
            assert_eq!(errs[0].field, field);
            assert!(
                errs[0].message.contains("reserved 'CHUG_' prefix"),
                "{errs:?}"
            );
        }
    }

    /// Design #517 S1: `JOB_PROJECT` — and, since S2, `JOB_TYPE` — is what a
    /// node's KVM (and, after S3, docker) grant matches on, so a job type may no
    /// more declare one than it may declare `CHUG_PHASE`: the grant key must not
    /// be movable by project config.
    #[test]
    fn the_dispatcher_composed_job_stamps_cannot_be_declared() {
        for (secrets, vars, field) in [
            (vec!["JOB_PROJECT"], vec![], "secrets"),
            (vec![], vec!["JOB_PROJECT"], "vars"),
            (vec!["JOB_TYPE"], vec![], "secrets"),
            (vec![], vec!["JOB_TYPE"], "vars"),
            (vec![], vec!["JOB_ID"], "vars"),
            (vec![], vec!["JOB_BRANCH"], "vars"),
            (vec![], vec!["JOB_SHA"], "vars"),
            (vec![], vec!["JOB_TASK_ID"], "vars"),
        ] {
            let jt = job_type_with(&secrets, &vars);
            let name = secrets.first().or_else(|| vars.first()).unwrap();
            let errs = static_errors_kv(Some(7), &jt, &kv(&[name], &[name]));
            assert_eq!(errs.len(), 1, "{name} is declarable: {errs:?}");
            assert_eq!(errs[0].field, field);
            assert!(
                errs[0].message.contains(name)
                    && errs[0].message.contains("reserved 'JOB_' prefix")
                    && errs[0].message.contains("allow-list"),
                "the refusal names the variable and why: {errs:?}"
            );
        }
    }

    /// The reservation is a prefix and nothing wider: the unprefixed
    /// dispatcher-composed names stay declarable, so no `.chug/jobs/*.yaml`
    /// that validates today stops validating.
    #[test]
    fn the_reservation_is_no_wider_than_the_two_prefixes() {
        for name in ["BASE_BRANCH", "REPO_URL", "NATS_URL", "JOBS", "CHUGGER"] {
            let jt = job_type_with(&[], &[name]);
            assert_eq!(
                static_errors_kv(Some(7), &jt, &kv(&[], &[name])),
                vec![],
                "{name} is not reserved"
            );
        }
    }

    /// The whole point of the named reference (design #313 A5): a declared
    /// identity with no `cloud-identities.*` record fails at release, per
    /// container and per name, instead of at token exchange inside the
    /// container.
    #[test]
    fn declared_cloud_identities_must_exist_in_kv() {
        let jt = JobType::parse(
            "name: deploy\nimage: img:latest\nmin_dispatcher: 5\n\
             work:\n  type: command\n  run: ./deploy.sh\n  \
             workload_identities: [gcp-artifact-writer]\n\
             wrap_up:\n  run: ./publish.sh\n  workload_identities: [gcp-publisher]\n\
             eval:\n  - name: health\n    type: command\n    run: ./h.sh\n    \
             workload_identities: [gcp-reader]\n",
        )
        .unwrap();
        assert_eq!(jt.validate(), vec![]);
        assert_eq!(
            static_errors_kv(
                Some(7),
                &jt,
                &kv_with_identities(
                    &[],
                    &[],
                    &["gcp-artifact-writer", "gcp-publisher", "gcp-reader"]
                )
            ),
            vec![]
        );

        let errs = static_errors_kv(Some(7), &jt, &kv_with_identities(&[], &[], &["gcp-reader"]));
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert_eq!(errs[0].field, "work.workload_identities");
        assert_eq!(
            errs[0].message,
            "cloud identity 'gcp-artifact-writer' is not set"
        );
        assert_eq!(errs[1].field, "wrap_up.workload_identities");
        assert_eq!(errs[1].message, "cloud identity 'gcp-publisher' is not set");
    }

    /// A cloud identity and a secret of the same name are different things in
    /// different buckets: a set secret never satisfies a declared identity.
    #[test]
    fn a_secret_never_satisfies_a_declared_cloud_identity() {
        let jt = JobType::parse(
            "name: deploy\nimage: img:latest\nmin_dispatcher: 5\n\
             work:\n  type: command\n  run: ./deploy.sh\n  workload_identities: [gcp]\n",
        )
        .unwrap();
        let errs = static_errors_kv(Some(7), &jt, &kv_with_identities(&["gcp"], &["gcp"], &[]));
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].message, "cloud identity 'gcp' is not set");
    }
}

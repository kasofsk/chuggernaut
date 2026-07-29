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
    KvNames, SCHEMA_SKEW_FIELD, ValidationError, wiring_errors, with_job_evaluators,
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

    // Schema-tolerance (spec §14): unknown top-level fields are accepted, not
    // rejected. Surface each loudly so a config that ran ahead of this
    // dispatcher (a new section this binary predates) is visible platform-wide
    // instead of silently ignored — but the job still launches. This is the
    // 2026-07-22 incident's fix: a benign unknown field no longer escalates
    // every job of the type.
    for w in job_type.config_warnings() {
        tracing::warn!(
            file = %path,
            field = %w.field,
            "job-type config warning: {w} (deploy the dispatcher to enable it)"
        );
    }

    // Version-skew gate (spec §14): a config declaring a `min_dispatcher` newer
    // than this binary's schema epoch is config-ahead-of-binary. Refuse it here
    // with a clear, platform-level diagnostic naming the file, field, and
    // needed version. At launch this parks the job pre-Work (Stalled) with the
    // reason rather than burning a launch into a generic validation escalation;
    // at release it blocks the same way. The merge-time CI check
    // (`chuggernaut validate --deployed-epoch`) is the first line of defense so
    // this rarely fires.
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

/// Static configuration checks (§2.2): prompt paths exist at `reference`;
/// declared secrets and vars exist in KV. Pass `check_kv: false` for the
/// Blocked→Ready re-validation, which re-checks files only.
// TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider.
#[allow(clippy::too_many_lines)]
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

    // §1.1 per-job timeout override: parseability validated at release (the
    // string is on the Job, not pinned to a ref), consistent with "wiring
    // validated at release, not creation".
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

    if let Some(kv) = kv {
        let declared_secrets = job_type.work.secrets.iter().chain(
            job_type
                .eval
                .iter()
                .flat_map(|e: &Evaluator| e.secrets.iter()),
        );
        for s in declared_secrets {
            // Origin credentials are dispatcher-only: reserved names never
            // reach a container, so declaring one is a static error.
            if s.starts_with(crate::forge_ingest::origin::RESERVED_SECRET_PREFIX) {
                errs.push(ValidationError::new(
                    seq,
                    "secrets",
                    format!(
                        "secret '{s}' uses the reserved '{}' prefix (dispatcher-only origin credentials)",
                        crate::forge_ingest::origin::RESERVED_SECRET_PREFIX
                    ),
                ));
            } else if !kv.secrets.contains(s) {
                errs.push(ValidationError::new(
                    seq,
                    "secrets",
                    format!("secret '{s}' is not set"),
                ));
            }
        }
        for v in &job_type.vars {
            if !kv.vars.contains(v) {
                errs.push(ValidationError::new(
                    seq,
                    "vars",
                    format!("var '{v}' is not set"),
                ));
            }
        }
    }
    Ok(errs)
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

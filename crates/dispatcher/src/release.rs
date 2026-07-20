//! Release validation (spec §2.2): graph wiring rules plus static
//! configuration checks. Used at release time (against current HEAD) and at
//! the Blocked→Ready re-validation (against the freshly pinned `base_ref`).

use crate::graph::JobGraph;
use serde::Serialize;
use std::collections::HashSet;
use types::{Evaluator, EvaluatorType, Job, JobType, ProjectDefaults, WorkType};
use vcs::RepoManager;

/// §6.5 validation error shape: `field` uses dot notation matching the job
/// type YAML structure; `job_seq` is omitted for errors not tied to a job.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_seq: Option<u64>,
    pub field: String,
    pub message: String,
}

impl ValidationError {
    pub(crate) fn new(
        job_seq: Option<u64>,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            job_seq,
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Everything static validation needs besides the repo: which secret and var
/// names exist in KV. The core fetches these once per validation pass.
pub struct KvNames {
    pub secrets: HashSet<String>,
    pub vars: HashSet<String>,
}

/// Graph wiring rules (§2.2): dependencies exist, no self-edges, no cycles,
/// no duplicates, nothing Revoked. `graph` must already contain the job.
pub fn wiring_errors(job: &Job, graph: &JobGraph) -> Vec<ValidationError> {
    let mut errs = Vec::new();
    let seq = Some(job.id);

    for &upstream in &job.deps {
        match graph.get(upstream) {
            None => errs.push(ValidationError::new(
                seq,
                "deps",
                format!("depends on unknown job #{upstream}"),
            )),
            Some(dep) if dep.state == types::JobState::Revoked => {
                errs.push(ValidationError::new(
                    seq,
                    "deps",
                    format!("depends on revoked job #{upstream}"),
                ));
            }
            Some(_) => {}
        }
        if upstream == job.id {
            errs.push(ValidationError::new(seq, "deps", "job depends on itself"));
        }
    }
    let mut seen = HashSet::new();
    for &upstream in &job.deps {
        if !seen.insert(upstream) {
            errs.push(ValidationError::new(
                seq,
                "deps",
                format!("duplicate dependency #{upstream}"),
            ));
        }
    }
    if graph.creates_cycle(job.id, &job.deps) {
        errs.push(ValidationError::new(
            seq,
            "deps",
            "dependency cycle detected",
        ));
    }
    errs
}

/// Load `jobs/{type}.yaml` at `reference`, apply `jobs/_defaults.yaml` if
/// present, and run the §1.1 field rules. Returns the merged job type on
/// success so callers validate exactly what will execute.
pub async fn load_job_type(
    repo: &RepoManager,
    owner: &str,
    project: &str,
    reference: &str,
    type_name: &str,
    job_seq: Option<u64>,
) -> Result<JobType, Vec<ValidationError>> {
    let path = format!("jobs/{type_name}.yaml");
    let Some(content) = read(repo, owner, project, reference, &path).await? else {
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

    let merged = match read(repo, owner, project, reference, "jobs/_defaults.yaml").await? {
        Some(defaults_yaml) => {
            let defaults = ProjectDefaults::parse(&defaults_yaml).map_err(|e| {
                vec![ValidationError::new(
                    None,
                    "eval",
                    format!("'jobs/_defaults.yaml' failed to parse: {e}"),
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

/// Layer the job's additive evaluators (design-lifecycle.md) on top of the
/// type's list. The type's evaluators are a floor: a name collision is an
/// error, and the merged list must still pass the §1.1 field rules (which
/// also enforces the image fallback for the extras). The base type already
/// validated clean in `load_job_type`, so any error here is the extras'.
pub fn with_job_evaluators(job_type: JobType, job: &Job) -> Result<JobType, Vec<ValidationError>> {
    if job.eval.is_empty() {
        return Ok(job_type);
    }
    let mut merged = job_type;
    let mut errs = Vec::new();
    for e in &job.eval {
        if merged.eval.iter().any(|x| x.name == e.name) {
            errs.push(ValidationError::new(
                Some(job.id),
                "eval.name",
                format!(
                    "job evaluator '{}' collides with a declared evaluator",
                    e.name
                ),
            ));
            continue;
        }
        merged.eval.push(e.clone());
    }
    errs.extend(
        merged
            .validate()
            .into_iter()
            .map(|e| ValidationError::new(Some(job.id), "eval", e.to_string())),
    );
    if errs.is_empty() {
        Ok(merged)
    } else {
        Err(errs)
    }
}

/// Static configuration checks (§2.2): prompt paths exist at `reference`;
/// declared secrets and vars exist in KV. Pass `check_kv: false` for the
/// Blocked→Ready re-validation, which re-checks files only.
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
            if s.starts_with(crate::origin::RESERVED_SECRET_PREFIX) {
                errs.push(ValidationError::new(
                    seq,
                    "secrets",
                    format!(
                        "secret '{s}' uses the reserved '{}' prefix (dispatcher-only origin credentials)",
                        crate::origin::RESERVED_SECRET_PREFIX
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

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
    fn new(job_seq: Option<u64>, field: impl Into<String>, message: impl Into<String>) -> Self {
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

/// Graph wiring rules (§2.2, all five). `graph` must already contain the job.
pub fn wiring_errors(job: &Job, job_type: &JobType, graph: &JobGraph) -> Vec<ValidationError> {
    let mut errs = Vec::new();
    let seq = Some(job.id);

    for (name, upstream) in &job.inputs {
        if graph.get(*upstream).is_none() {
            errs.push(ValidationError::new(
                seq,
                format!("inputs.{name}"),
                format!("input '{name}' references unknown job {upstream}"),
            ));
        }
        if *upstream == job.id {
            errs.push(ValidationError::new(
                seq,
                format!("inputs.{name}"),
                "job references itself",
            ));
        }
        if !job_type.inputs.iter().any(|d| &d.name == name) {
            errs.push(ValidationError::new(
                seq,
                format!("inputs.{name}"),
                format!("input '{name}' is not declared by job type '{}'", job_type.name),
            ));
        }
    }
    for decl in &job_type.inputs {
        if !job.inputs.contains_key(&decl.name) {
            errs.push(ValidationError::new(
                seq,
                format!("inputs.{}", decl.name),
                format!("declared input '{}' is not wired", decl.name),
            ));
        }
    }
    let upstream: Vec<u64> = job.inputs.values().copied().collect();
    if graph.creates_cycle(job.id, &upstream) {
        errs.push(ValidationError::new(seq, "inputs", "dependency cycle detected"));
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
            job_type.with_defaults(&defaults).map_err(|e| {
                vec![ValidationError::new(job_seq, "eval", e.to_string())]
            })?
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
            && let Some(p) = &e.prompt {
                require_file.push((format!("eval[{i}].prompt"), p.clone()));
            }
    }
    for (field, path) in require_file {
        if read(repo, owner, project, reference, &path).await?.is_none() {
            errs.push(ValidationError::new(
                seq,
                field,
                format!("prompt file '{path}' does not exist at {reference}"),
            ));
        }
    }

    if let Some(kv) = kv {
        let declared_secrets = job_type
            .secrets
            .iter()
            .chain(job_type.eval.iter().flat_map(|e: &Evaluator| e.secrets.iter()));
        for s in declared_secrets {
            if !kv.secrets.contains(s) {
                errs.push(ValidationError::new(
                    seq,
                    "secrets",
                    format!("secret '{s}' is not set"),
                ));
            }
        }
        for v in &job_type.vars {
            if !kv.vars.contains(v) {
                errs.push(ValidationError::new(seq, "vars", format!("var '{v}' is not set")));
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

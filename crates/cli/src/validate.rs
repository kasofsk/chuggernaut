//! `chuggernaut validate` — offline validation of repo-authored YAML files,
//! the same parse + §1.1 field rules the platform applies at release. For
//! contributors and CI: catch a broken `jobs/*.yaml` before a job trips over
//! it. Repo-dependent checks (prompt-file existence, secret/var presence)
//! still run at release — this is the static slice only.

use clap::Parser;
use std::path::{Path, PathBuf};
use types::{JobType, ProjectDefaults};

#[derive(Parser)]
pub struct ValidateArgs {
    /// YAML files to validate: jobs/{type}.yaml and/or jobs/_defaults.yaml.
    /// A job type with a `_defaults.yaml` sibling is validated post-merge,
    /// exactly as the dispatcher loads it.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
}

pub fn run(args: ValidateArgs) -> anyhow::Result<()> {
    let mut failed = false;
    for path in &args.files {
        let errors = validate_file(path);
        if errors.is_empty() {
            println!("{}: OK", path.display());
        } else {
            failed = true;
            for e in errors {
                eprintln!("{}: {e}", path.display());
            }
        }
    }
    if failed {
        anyhow::bail!("validation failed");
    }
    Ok(())
}

fn validate_file(path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return vec![format!("read error: {e}")],
    };

    if path.file_name().is_some_and(|n| n == "_defaults.yaml") {
        return match ProjectDefaults::parse(&content) {
            // Field rules for default evaluators are checked where they land:
            // against each job type they merge into.
            Ok(_) => vec![],
            Err(e) => vec![format!("parse error: {e}")],
        };
    }

    let job_type = match JobType::parse(&content) {
        Ok(jt) => jt,
        Err(e) => return vec![format!("parse error: {e}")],
    };

    // Merge the sibling _defaults.yaml if present — validate what will run.
    let merged = match sibling_defaults(path) {
        Some(Ok(defaults)) => match job_type.with_defaults(&defaults) {
            Ok(m) => m,
            Err(e) => return vec![e.to_string()],
        },
        Some(Err(e)) => return vec![format!("_defaults.yaml: {e}")],
        None => job_type,
    };
    merged.validate().iter().map(|e| e.to_string()).collect()
}

fn sibling_defaults(path: &Path) -> Option<Result<ProjectDefaults, serde_yaml::Error>> {
    let defaults = path.parent()?.join("_defaults.yaml");
    let content = std::fs::read_to_string(defaults).ok()?;
    Some(ProjectDefaults::parse(&content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_type_with_sibling_defaults_and_reports_field_rules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("_defaults.yaml"),
            "eval:\n  - name: ci\n    type: command\n    run: ./ci.sh\n",
        )
        .unwrap();
        let good = dir.path().join("good.yaml");
        std::fs::write(
            &good,
            "name: good\nimage: img:latest\nwork:\n  type: agent\n  prompt: p.md\n",
        )
        .unwrap();
        assert_eq!(validate_file(&good), Vec::<String>::new());

        // Missing image: caught for the type itself AND for the merged
        // default evaluator that needs the fallback.
        let bad = dir.path().join("bad.yaml");
        std::fs::write(&bad, "name: bad\nwork:\n  type: agent\n  prompt: p.md\n").unwrap();
        let errors = validate_file(&bad);
        assert!(errors.iter().any(|e| e.contains("'image' is required")), "{errors:?}");

        let unknown = dir.path().join("unknown.yaml");
        std::fs::write(&unknown, "name: u\nbogus_field: 1\nwork:\n  type: command\n  run: x\n")
            .unwrap();
        assert!(validate_file(&unknown)[0].contains("parse error"), "deny_unknown_fields");
    }
}

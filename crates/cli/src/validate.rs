//! `chuggernaut validate` — offline validation of repo-authored YAML files,
//! the same parse + §1.1 field rules the platform applies at release. For
//! contributors and CI: catch a broken `jobs/*.yaml` before a job trips over
//! it. Repo-dependent checks (prompt-file existence, secret/var presence)
//! still run at release — this is the static slice only.
//!
//! It also carries the **merge-time version-skew gate** (spec §14): job-type
//! config is read live from the default branch, so a config that needs a
//! newer dispatcher would otherwise merge and then escalate every job at
//! launch. Pass `--deployed-epoch N` (CI reads it from the running
//! dispatcher's `GET /api/v1/platform/config`) and a config declaring
//! `min_dispatcher > N` fails its *own* CI with "requires dispatcher >= X;
//! deploy first or gate it" instead of merging a time bomb. Unknown top-level
//! fields are reported as warnings (tolerated, not failures) — the config runs,
//! the ignored feature is flagged.

use clap::Parser;
use std::path::{Path, PathBuf};
use types::{JobType, ProjectDefaults};

#[derive(Parser)]
pub struct ValidateArgs {
    /// YAML files to validate: .chug/jobs/{type}.yaml and/or
    /// .chug/jobs/_defaults.yaml.
    /// A job type with a `_defaults.yaml` sibling is validated post-merge,
    /// exactly as the dispatcher loads it.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// The schema epoch of the *deployed* dispatcher a config must be
    /// compatible with (spec §14). A job type declaring `min_dispatcher`
    /// greater than this fails validation with a coordinated-deploy message.
    /// CI reads this from `GET /api/v1/platform/config` (`schema_epoch`);
    /// omitted, it defaults to the epoch compiled into this binary
    /// ([`types::CONFIG_SCHEMA_EPOCH`]), which still catches a config that
    /// requires an epoch newer than the code it ships alongside.
    #[arg(long)]
    pub deployed_epoch: Option<u32>,
}

pub fn run(args: ValidateArgs) -> anyhow::Result<()> {
    let epoch = args.deployed_epoch.unwrap_or(types::CONFIG_SCHEMA_EPOCH);
    let mut failed = false;
    for path in &args.files {
        let Outcome { errors, warnings } = validate_file(path, epoch);
        for w in &warnings {
            eprintln!("{}: warning: {w}", path.display());
        }
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

/// Split result: hard errors fail CI; warnings are surfaced but tolerated.
struct Outcome {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Outcome {
    fn err(msg: impl Into<String>) -> Self {
        Outcome {
            errors: vec![msg.into()],
            warnings: vec![],
        }
    }
}

fn validate_file(path: &Path, deployed_epoch: u32) -> Outcome {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return Outcome::err(format!("read error: {e}")),
    };

    if path.file_name().is_some_and(|n| n == "_defaults.yaml") {
        return match ProjectDefaults::parse(&content) {
            Ok(_) => Outcome {
                errors: vec![],
                warnings: vec![],
            },
            Err(e) => Outcome::err(format!("parse error: {e}")),
        };
    }

    let job_type = match JobType::parse(&content) {
        Ok(jt) => jt,
        Err(e) => return Outcome::err(format!("parse error: {e}")),
    };

    let mut warnings: Vec<String> = job_type
        .config_warnings()
        .iter()
        .map(|w| w.to_string())
        .collect();

    let mut errors = Vec::new();

    if let Some(needed) = job_type.requires_dispatcher(deployed_epoch) {
        errors.push(format!(
            "requires dispatcher schema epoch >= {needed} but the deployed dispatcher is at \
             {deployed_epoch}: deploy the dispatcher first, or land this behind a version gate \
             (coordinated deploy)"
        ));
    }

    let merged = match sibling_defaults(path) {
        Some(Ok(defaults)) => match job_type.with_defaults(&defaults) {
            Ok(m) => m,
            Err(e) => {
                errors.push(e.to_string());
                return Outcome { errors, warnings };
            }
        },
        Some(Err(e)) => {
            errors.push(format!("_defaults.yaml: {e}"));
            return Outcome { errors, warnings };
        }
        None => job_type,
    };
    errors.extend(merged.validate().iter().map(|e| e.to_string()));
    warnings.sort();
    warnings.dedup();
    Outcome { errors, warnings }
}

fn sibling_defaults(path: &Path) -> Option<Result<ProjectDefaults, serde_yaml::Error>> {
    let defaults = path.parent()?.join("_defaults.yaml");
    let content = std::fs::read_to_string(defaults).ok()?;
    Some(ProjectDefaults::parse(&content))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn errors(path: &Path, epoch: u32) -> Vec<String> {
        validate_file(path, epoch).errors
    }

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
        assert_eq!(
            errors(&good, types::CONFIG_SCHEMA_EPOCH),
            Vec::<String>::new()
        );

        let bad = dir.path().join("bad.yaml");
        std::fs::write(&bad, "name: bad\nwork:\n  type: agent\n  prompt: p.md\n").unwrap();
        let errs = errors(&bad, types::CONFIG_SCHEMA_EPOCH);
        assert!(
            errs.iter().any(|e| e.contains("'image' is required")),
            "{errs:?}"
        );
    }

    #[test]
    fn unknown_top_level_field_is_a_warning_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let unknown = dir.path().join("unknown.yaml");
        std::fs::write(
            &unknown,
            "name: u\nbogus_field: 1\nimage: img:latest\nwork:\n  type: command\n  run: x\n",
        )
        .unwrap();
        let outcome = validate_file(&unknown, types::CONFIG_SCHEMA_EPOCH);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(
            outcome.warnings.iter().any(|w| w.contains("bogus_field")),
            "{:?}",
            outcome.warnings
        );
    }

    /// Write a job type declaring `inputs` and return its validation errors.
    /// `head` carries everything above the `inputs:` list, so a test can vary the
    /// `min_dispatcher` declaration as well as the block itself.
    fn input_errors(dir: &Path, head: &str, block: &str) -> Vec<String> {
        let path = dir.join("rollback.yaml");
        std::fs::write(
            &path,
            format!("{head}work:\n  type: command\n  run: ./r.sh\ninputs:\n{block}"),
        )
        .unwrap();
        errors(&path, types::CONFIG_SCHEMA_EPOCH)
    }

    #[test]
    fn validate_covers_the_inputs_block() {
        let dir = tempfile::tempdir().unwrap();
        let head = format!(
            "name: rollback\nimage: img:latest\nmin_dispatcher: {}\n",
            types::INPUTS_SCHEMA_EPOCH
        );
        let good = "  - name: sha\n    type: string\n    required: true\n    \
                    pattern: '^[0-9a-f]{7,40}$'\n  - name: service\n    type: enum\n    \
                    values: [web, worker]\n    default: web\n";
        assert_eq!(input_errors(dir.path(), &head, good), Vec::<String>::new());

        for (block, field) in [
            ("  - name: SHA\n    type: string\n", "inputs.name"),
            ("  - name: service\n    type: enum\n", "inputs.values"),
            (
                "  - name: sha\n    type: string\n    values: [a]\n",
                "inputs.values",
            ),
            (
                "  - name: service\n    type: enum\n    values: [web]\n    pattern: '^web$'\n",
                "inputs.pattern",
            ),
            (
                "  - name: sha\n    type: string\n    required: true\n    default: abc1234\n",
                "inputs.default",
            ),
            (
                "  - name: sha\n    type: string\n    default: 'a b'\n",
                "inputs.default",
            ),
        ] {
            let errs = input_errors(dir.path(), &head, block);
            assert!(
                errs.iter().any(|e| e.contains(field)),
                "expected a {field} error for {block:?}, got {errs:?}"
            );
        }

        let ungated = input_errors(
            dir.path(),
            "name: rollback\nimage: img:latest\n",
            "  - name: sha\n    type: string\n",
        );
        assert!(
            ungated.iter().any(|e| e.contains("min_dispatcher")),
            "{ungated:?}"
        );
    }

    #[test]
    fn config_requiring_a_newer_dispatcher_fails_the_merge_gate() {
        let dir = tempfile::tempdir().unwrap();
        let ahead = dir.path().join("ahead.yaml");
        std::fs::write(
            &ahead,
            "name: web\nimage: img:latest\nmin_dispatcher: 3\nwork:\n  type: agent\n  prompt: p.md\n",
        )
        .unwrap();
        let errs = errors(&ahead, 1);
        assert!(
            errs.iter()
                .any(|e| e.contains("requires dispatcher schema epoch >= 3")),
            "{errs:?}"
        );
        assert_eq!(errors(&ahead, 3), Vec::<String>::new());
    }
}

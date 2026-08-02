//! `chuggernaut validate` — offline validation of repo-authored YAML files,
//! the same parse + §1.1 field rules the platform applies at release. For
//! contributors and CI: catch a broken `jobs/*.yaml` before a job trips over
//! it. Repo-dependent checks (prompt-file existence, secret/var presence)
//! still run at release — this is the static slice only. The file kind follows
//! the path: a file under a `schedules/` directory is a `.chug/schedules/*.yaml`
//! schedule (design #310), anything else a job type or its `_defaults.yaml`.
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
use types::{JobType, ProjectDefaults, Schedule};

#[derive(Parser)]
pub struct ValidateArgs {
    /// YAML files to validate: .chug/jobs/{type}.yaml, .chug/jobs/_defaults.yaml
    /// and/or .chug/schedules/{name}.yaml.
    /// A job type with a `_defaults.yaml` sibling is validated post-merge,
    /// exactly as the dispatcher loads it; a schedule is validated against the
    /// job type it names when that file sits in the same config root.
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

    if path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == types::SCHEDULES_DIR)
    {
        return validate_schedule(path, &content, deployed_epoch);
    }

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
        errors.push(skew_error(needed, deployed_epoch));
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

/// The §14 skew diagnostic, shared by every config kind that carries
/// `min_dispatcher`.
fn skew_error(needed: u32, deployed_epoch: u32) -> String {
    format!(
        "requires dispatcher schema epoch >= {needed} but the deployed dispatcher is at \
         {deployed_epoch}: deploy the dispatcher first, or land this behind a version gate \
         (coordinated deploy)"
    )
}

/// A `.chug/schedules/{name}.yaml` file (design #310): the §1.1 field rules,
/// the skew gate, and — when the target job type sits in the same config root —
/// the rules that need it, an agent target's `description` and the declaration
/// every supplied input is judged against.
fn validate_schedule(path: &Path, content: &str, deployed_epoch: u32) -> Outcome {
    let schedule = match Schedule::parse(content) {
        Ok(s) => s,
        Err(e) => return Outcome::err(format!("parse error: {e}")),
    };
    let Some(stem) = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
    else {
        return Outcome::err("cannot read a schedule name from this path".to_string());
    };
    let warnings = schedule
        .config_warnings()
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    let mut errors: Vec<String> = schedule
        .validate(&stem)
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    if let Some(needed) = schedule.requires_dispatcher(deployed_epoch) {
        errors.push(skew_error(needed, deployed_epoch));
    }
    if let Some(target) = sibling_job_type(path, &schedule.job_type) {
        errors.extend(
            schedule
                .validate_against_target(&target)
                .iter()
                .map(std::string::ToString::to_string),
        );
    }
    Outcome { errors, warnings }
}

/// The job type a schedule names, when its file sits in the same config root. A
/// missing or unparseable target is not reported here — the existence check is
/// release-time, like a prompt file's.
fn sibling_job_type(path: &Path, job_type: &str) -> Option<JobType> {
    let root = path.parent()?.parent()?;
    let target = root.join("jobs").join(format!("{job_type}.yaml"));
    let content = std::fs::read_to_string(target).ok()?;
    JobType::parse(&content).ok()
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

    /// A config root holding one agent job type and one schedule file, the
    /// layout `.chug/` gives CI.
    fn config_root(schedule: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("jobs")).unwrap();
        std::fs::create_dir(dir.path().join("schedules")).unwrap();
        std::fs::write(
            dir.path().join("jobs/code.yaml"),
            "name: code\nimage: img:latest\nwork:\n  type: agent\n  prompt: p.md\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("schedules/nightly.yaml"), schedule).unwrap();
        dir
    }

    fn schedule_errors(dir: &tempfile::TempDir, epoch: u32) -> Vec<String> {
        errors(&dir.path().join("schedules/nightly.yaml"), epoch)
    }

    #[test]
    fn validate_accepts_a_schedule_file() {
        let dir = config_root(
            "name: nightly\njob_type: code\ncron: '0 2 * * *'\ndescription: Nightly suite.\n",
        );
        assert_eq!(
            schedule_errors(&dir, types::CONFIG_SCHEMA_EPOCH),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_schedule_reports_its_field_rules_and_skew() {
        let dir = config_root("name: other\njob_type: code\ncron: '0 2 * *'\n");
        let errs = schedule_errors(&dir, types::CONFIG_SCHEMA_EPOCH);
        for expected in ["'name'", "'cron'", "'description' is required"] {
            assert!(errs.iter().any(|e| e.contains(expected)), "{errs:?}");
        }

        let ahead = config_root(
            "name: nightly\njob_type: code\ncron: '0 2 * * *'\ndescription: d\nmin_dispatcher: 9\n",
        );
        assert!(
            schedule_errors(&ahead, 1)
                .iter()
                .any(|e| e.contains("requires dispatcher schema epoch >= 9")),
        );
    }

    /// The merge-time half of design #311 slice C: the target's declaration
    /// sits in the same config root, so a schedule supplying an input the type
    /// does not declare — or omitting one it requires — fails CI rather than
    /// firing at 3am.
    #[test]
    fn a_schedule_supplying_inputs_is_validated_against_its_target() {
        let dir = config_root(
            "name: nightly\njob_type: rollback\ncron: '0 2 * * *'\nmin_dispatcher: 3\n\
             inputs:\n  region: eu\n",
        );
        std::fs::write(
            dir.path().join("jobs/rollback.yaml"),
            "name: rollback\nimage: img:latest\nmin_dispatcher: 2\n\
             work:\n  type: command\n  run: ./r.sh\n\
             inputs:\n  - name: sha\n    type: string\n    required: true\n",
        )
        .unwrap();
        let errs = schedule_errors(&dir, types::CONFIG_SCHEMA_EPOCH);
        for expected in [
            "input 'region' is not declared by this job type",
            "input 'sha' is required but the schedule supplies no value",
        ] {
            assert!(errs.iter().any(|e| e.contains(expected)), "{errs:?}");
        }
    }

    #[test]
    fn a_schedule_unknown_field_is_a_warning_and_a_missing_target_is_neither() {
        let dir = config_root(
            "name: nightly\njob_type: absent\ncron: '0 2 * * *'\ntimezone: Europe/Berlin\n",
        );
        let outcome = validate_file(
            &dir.path().join("schedules/nightly.yaml"),
            types::CONFIG_SCHEMA_EPOCH,
        );
        assert_eq!(outcome.errors, Vec::<String>::new());
        assert!(
            outcome.warnings.iter().any(|w| w.contains("timezone")),
            "{:?}",
            outcome.warnings
        );
    }
}

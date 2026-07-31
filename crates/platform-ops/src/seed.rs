//! Platform starter template embedded in the binary (§12.2): the files a
//! fresh project is seeded with at creation. Chuggernaut-level, versioned
//! with the platform; everything project-specific stays editable in the
//! project repo afterwards.
//!
//! - **Accepts:** nothing at runtime — the template is embedded in the binary.
//! - **Emits:** the file set a fresh project is seeded with at creation.
//! - **Guarantees:** chuggernaut-level and versioned with the platform; seeded
//!   files stay editable in the project repo afterwards.
//! - **Spec:** §12.2.

/// The "Code" starter: a coding agent implements the job ticket, a second
/// coding agent reviews it; `.chug/tasks/` shows the reusable-task convention
/// (command task = script, agent task = markdown).
pub const CODE_TEMPLATE: &[(&str, &str)] = &[
    ("README.md", include_str!("../templates/code/README.md")),
    (
        ".chug/jobs/code.yaml",
        include_str!("../templates/code/.chug/jobs/code.yaml"),
    ),
    (
        ".chug/prompts/work/code.md",
        include_str!("../templates/code/.chug/prompts/work/code.md"),
    ),
    (
        ".chug/tasks/review-code.md",
        include_str!("../templates/code/.chug/tasks/review-code.md"),
    ),
    (
        ".chug/tasks/ci.sh",
        include_str!("../templates/code/.chug/tasks/ci.sh"),
    ),
];

/// The subset of [`CODE_TEMPLATE`] seeded into a linked-origin project: the
/// chuggernaut config surface (job types, prompts, tasks) without the README —
/// the existing repo already has its own identity. Seeded with skip-existing,
/// so a repo that already carries chuggernaut config keeps its own files.
pub const CONFIG_TEMPLATE: &[(&str, &str)] = &[
    (
        ".chug/jobs/code.yaml",
        include_str!("../templates/code/.chug/jobs/code.yaml"),
    ),
    (
        ".chug/prompts/work/code.md",
        include_str!("../templates/code/.chug/prompts/work/code.md"),
    ),
    (
        ".chug/tasks/review-code.md",
        include_str!("../templates/code/.chug/tasks/review-code.md"),
    ),
    (
        ".chug/tasks/ci.sh",
        include_str!("../templates/code/.chug/tasks/ci.sh"),
    ),
];

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The embedded job type must always parse and validate — a broken
    /// template would poison every new project.
    #[test]
    fn code_template_job_type_is_valid() {
        let yaml = CODE_TEMPLATE
            .iter()
            .find(|(p, _)| *p == ".chug/jobs/code.yaml")
            .unwrap()
            .1;
        let jt = types::JobType::parse(yaml).unwrap();
        assert_eq!(jt.name, "code");
        assert_eq!(jt.display_name.as_deref(), Some("Code"));
        assert_eq!(jt.validate(), vec![]);
        for path in [".chug/prompts/work/code.md", ".chug/tasks/review-code.md"] {
            assert!(
                CODE_TEMPLATE.iter().any(|(p, _)| *p == path),
                "missing {path}"
            );
        }
    }

    /// Every seeded config file lands under the `.chug/` config root (§1.1) —
    /// the platform reads job types, prompts and tasks from there, so a
    /// repo-root path would seed a file nothing loads. Only the README, which
    /// is the project's own front page rather than platform config, sits
    /// outside it.
    #[test]
    fn seeded_config_lives_under_the_config_root() {
        let prefix = format!("{}/", types::CONFIG_DIR);
        for (path, _) in CODE_TEMPLATE.iter().chain(CONFIG_TEMPLATE) {
            assert!(
                *path == "README.md" || path.starts_with(&prefix),
                "seeded path '{path}' is outside {}",
                types::CONFIG_DIR
            );
        }
        assert!(
            !CONFIG_TEMPLATE.iter().any(|(p, _)| *p == "README.md"),
            "the linked-origin subset seeds config only, never a README"
        );
    }
}

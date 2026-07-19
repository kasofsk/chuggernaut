//! Platform starter template embedded in the binary (§12.2): the files a
//! fresh project is seeded with at creation. Chuggernaut-level, versioned
//! with the platform; everything project-specific stays editable in the
//! project repo afterwards.

/// The "Code" starter: a coding agent implements the job ticket, a second
/// coding agent reviews it; `tasks/` shows the reusable-task convention
/// (command task = script, agent task = markdown).
pub const CODE_TEMPLATE: &[(&str, &str)] = &[
    ("README.md", include_str!("../templates/code/README.md")),
    ("jobs/code.yaml", include_str!("../templates/code/jobs/code.yaml")),
    (
        "prompts/work/code.md",
        include_str!("../templates/code/prompts/work/code.md"),
    ),
    (
        "tasks/review-code.md",
        include_str!("../templates/code/tasks/review-code.md"),
    ),
    ("tasks/ci.sh", include_str!("../templates/code/tasks/ci.sh")),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded job type must always parse and validate — a broken
    /// template would poison every new project.
    #[test]
    fn code_template_job_type_is_valid() {
        let yaml = CODE_TEMPLATE
            .iter()
            .find(|(p, _)| *p == "jobs/code.yaml")
            .unwrap()
            .1;
        let jt = types::JobType::parse(yaml).unwrap();
        assert_eq!(jt.name, "code");
        assert_eq!(jt.display_name.as_deref(), Some("Code"));
        assert_eq!(jt.validate(), vec![]);
        // Every repo path the type references ships in the template.
        for path in ["prompts/work/code.md", "tasks/review-code.md"] {
            assert!(CODE_TEMPLATE.iter().any(|(p, _)| *p == path), "missing {path}");
        }
    }
}

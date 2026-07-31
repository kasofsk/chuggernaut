//! Record fixtures: one blank [`types::Job`] every test builds its own case from.
//!
//! A `Job` has ~25 fields and no `Default`, so each test site that needs one
//! used to spell the whole record out. Two such literals in `src/` are a
//! copy-paste clone (STYLE.md Tier 1, `.chug/tasks/check-duplication.sh`), and
//! more importantly they drift: a field added to the record has to be added to
//! each of them, and the one that was written last is the one that stops
//! matching what production writes.
//!
//! - **Accepts:** the project slug and seq a fixture record should carry.
//! - **Emits:** a [`types::Job`] with every other field at its empty/None value,
//!   for a call site to override with struct-update syntax.
//! - **Guarantees:** pure — no I/O, no async; the returned record is Frozen,
//!   holds no branch history, and carries nothing a test did not ask for.

/// A blank job record for `{project}#{id}`: Frozen, no deps, no members, no
/// options set. Override what the case is about and leave the rest:
///
/// ```ignore
/// let ready = types::Job { state: JobState::Ready, ..test_utils::fixture::job("acme/api", 1) };
/// ```
#[must_use]
pub fn job(project: &str, id: u64) -> types::Job {
    types::Job {
        id,
        project: project.to_string(),
        r#type: "code".into(),
        title: String::new(),
        description: String::new(),
        cover_html: None,
        deps: vec![],
        members: vec![],
        batch_id: None,
        state: types::JobState::Frozen,
        branch: format!("job/{id}"),
        base_ref: None,
        knowledge_tags: vec![],
        eval: vec![],
        timeout: None,
        model: None,
        inputs: Default::default(),
        groups: vec![],
        claim_next: false,
        escalation: None,
        factory: None,
        schedule: None,
        created_at: chrono::Utc::now(),
        ready_at: None,
        completed_at: None,
        task_time_ms: None,
    }
}

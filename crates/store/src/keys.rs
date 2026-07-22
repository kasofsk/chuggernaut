//! KV key construction and encoding (spec §1.4).
//!
//! Key segments that may contain characters outside the NATS key alphabet are
//! base64url-encoded: user emails and knowledge subjects/predicates. Secret and
//! var names are validated to `[A-Za-z0-9_]+` and stored unencoded. The owner
//! name `global` is reserved.

use crate::StoreError;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use types::KnowledgeScope;

pub const RESERVED_OWNER: &str = "global";

pub fn b64(segment: &str) -> String {
    URL_SAFE_NO_PAD.encode(segment.as_bytes())
}

pub fn b64_decode(segment: &str) -> Result<String, StoreError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment.as_bytes())
        .map_err(|e| StoreError::InvalidKey(e.to_string()))?;
    String::from_utf8(bytes).map_err(|e| StoreError::InvalidKey(e.to_string()))
}

/// Validate a var/secret name: `[A-Za-z0-9_]+` (they become env var names).
pub fn validate_name(name: &str) -> Result<(), StoreError> {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(())
    } else {
        Err(StoreError::InvalidKey(format!("invalid name: {name:?}")))
    }
}

/// `[A-Za-z0-9_-]+` — ingest source, factory name, and similar subject components.
pub fn validate_subject_component(s: &str) -> Result<(), StoreError> {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Ok(())
    } else {
        Err(StoreError::InvalidKey(format!(
            "invalid subject component: {s:?}"
        )))
    }
}

pub fn job_key(owner: &str, project: &str, seq: u64) -> String {
    format!("{owner}.{project}.{seq}")
}

pub fn task_key(owner: &str, project: &str, job_seq: u64, task_id: u64) -> String {
    format!("{owner}.{project}.{job_seq}.{task_id}")
}

/// Inline review step log — one key per work task (spec §1.2).
pub fn step_key(owner: &str, project: &str, job_seq: u64, task_id: u64) -> String {
    format!("{owner}.{project}.{job_seq}.{task_id}")
}

pub fn user_key(email: &str) -> String {
    b64(email)
}

pub fn channel_key(owner: &str, project: &str, seq: u64) -> String {
    format!("{owner}.{project}.jobs.{seq}")
}

/// Object name within the `artifacts` object store: one blob per (task, kind).
/// `kind` carries a dot (`session.jsonl`), so it is always the trailing
/// segment — parse from the left.
pub fn artifact_key(owner: &str, project: &str, job_seq: u64, task_id: u64, kind: &str) -> String {
    format!("{owner}.{project}.{job_seq}.{task_id}.{kind}")
}

/// Prefix matching every artifact of one task, for listing.
pub fn artifact_task_prefix(owner: &str, project: &str, job_seq: u64, task_id: u64) -> String {
    format!("{owner}.{project}.{job_seq}.{task_id}.")
}

/// Object name for a job attachment — an operator-uploaded file (a screenshot
/// on a bug report, a reference document). Job-scoped, so the literal
/// `attachments` segment sits where a task artifact's numeric task id would;
/// since a task id is always numeric it can never collide. The filename is the
/// trailing remainder and may itself contain dots.
pub fn job_attachment_key(owner: &str, project: &str, job_seq: u64, name: &str) -> String {
    format!("{owner}.{project}.{job_seq}.attachments.{name}")
}

/// Prefix matching every attachment of one job, for listing.
pub fn job_attachment_prefix(owner: &str, project: &str, job_seq: u64) -> String {
    format!("{owner}.{project}.{job_seq}.attachments.")
}

/// Key within the `knowledge` bucket: `{scope-prefix}.{b64(subject)}.{b64(predicate)}`.
pub fn knowledge_key(scope: &KnowledgeScope, subject: &str, predicate: &str) -> String {
    let prefix = knowledge_scope_prefix(scope);
    format!("{prefix}.{}.{}", b64(subject), b64(predicate))
}

/// Prefix for list-by-subject scans: `{scope-prefix}.{b64(subject)}.`.
pub fn knowledge_subject_prefix(scope: &KnowledgeScope, subject: &str) -> String {
    format!("{}.{}.", knowledge_scope_prefix(scope), b64(subject))
}

fn knowledge_scope_prefix(scope: &KnowledgeScope) -> String {
    match scope {
        KnowledgeScope::Global => "global".to_string(),
        KnowledgeScope::Team { owner } => owner.clone(),
        KnowledgeScope::Project { owner, project } => format!("{owner}.{project}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_round_trips() {
        let email = "david@kasofsk.xyz";
        let key = user_key(email);
        assert!(!key.contains('@'));
        assert!(!key.contains('.'));
        assert_eq!(b64_decode(&key).unwrap(), email);
    }

    #[test]
    fn knowledge_key_handles_dots_and_slashes() {
        let scope = KnowledgeScope::Project {
            owner: "acme".into(),
            project: "api".into(),
        };
        let key = knowledge_key(
            &scope,
            "payments/stripe-integration",
            "webhook.retry.policy",
        );
        // owner + project + two encoded segments = exactly 4 dot-separated parts
        assert_eq!(key.split('.').count(), 4);
        let parts: Vec<&str> = key.split('.').collect();
        assert_eq!(b64_decode(parts[2]).unwrap(), "payments/stripe-integration");
        assert_eq!(b64_decode(parts[3]).unwrap(), "webhook.retry.policy");
    }

    /// A job attachment is job-scoped; its `attachments` segment must never be
    /// mistaken for a task id, and a filename with dots must survive listing.
    #[test]
    fn job_attachment_key_layout() {
        let key = job_attachment_key("acme", "api", 42, "mobile-bug.png");
        assert_eq!(key, "acme.api.42.attachments.mobile-bug.png");
        let prefix = job_attachment_prefix("acme", "api", 42);
        assert_eq!(key.strip_prefix(&prefix), Some("mobile-bug.png"));
        // A task artifact of the same job must not match the attachment prefix.
        let task_artifact = artifact_key("acme", "api", 42, 7, "stdout.log");
        assert!(task_artifact.strip_prefix(&prefix).is_none());
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("RUST_EDITION").is_ok());
        assert!(validate_name("GITHUB_TOKEN").is_ok());
        assert!(validate_name("bad-name").is_err());
        assert!(validate_name("bad.name").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_subject_component("sentry-prod").is_ok());
        assert!(validate_subject_component("no.dots").is_err());
    }
}

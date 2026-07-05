//! Identity and access (spec Part 7).

use async_trait::async_trait;
use thiserror::Error;
use types::{Identity, ProjectRole};

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("forbidden")]
    Forbidden,
    #[error("internal: {0}")]
    Internal(String),
}

/// Actions gated by the §7.5 permission rules table.
#[derive(Debug, Clone)]
pub enum Action {
    ReadProject { project: String },
    ResolveTask { project: String },
    ManageProjectConfig { project: String }, // vars, secrets, knowledge
    PlatformConfig,
    IssueSshCert,
}

/// Pure §7.5 permission check — no I/O.
pub fn authorize(identity: &Identity, action: &Action) -> Result<(), AuthError> {
    if identity.platform_admin {
        return Ok(());
    }
    let role = |project: &str| identity.project_roles.get(project).copied();
    let ok = match action {
        Action::ReadProject { project } => role(project) >= Some(ProjectRole::Viewer),
        Action::ResolveTask { project } => role(project) >= Some(ProjectRole::Member),
        Action::ManageProjectConfig { project } => role(project) >= Some(ProjectRole::Admin),
        Action::PlatformConfig => false,
        Action::IssueSshCert => true,
    };
    if ok {
        Ok(())
    } else {
        Err(AuthError::Forbidden)
    }
}

/// Swappable authentication provider (spec §7.1) — replaceable with Zitadel,
/// Keycloak, or Ory without touching business logic.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, jwt_cookie: &str) -> Result<Identity, AuthError>;
}

/// SSH principal for per-job certificates (spec §5.2, §7.4). Embeds the project
/// because job seqs are only unique per project.
pub fn job_ssh_principal(owner: &str, project: &str, seq: u64) -> String {
    format!("job:{owner}/{project}:{seq}")
}

// TODO (spec §7.1, §7.3, §7.4, §12.1):
// - JWT issue/verify (RS256), httpOnly cookie construction
// - argon2id password hashing/verification
// - SSH CA: user cert signing (24h, email principal), per-job cert issuance
// - per-job NATS JWT minting with the §7.4 allow-lists (+ triage create_job perm)
// - sshd AuthorizedPrincipalsCommand ref-authorization helper

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use types::IdentityKind;

    fn identity(role: Option<ProjectRole>) -> Identity {
        let mut project_roles = HashMap::new();
        if let Some(r) = role {
            project_roles.insert("acme/api".to_string(), r);
        }
        Identity {
            sub: "u1".into(),
            kind: IdentityKind::User,
            project_roles,
            platform_admin: false,
        }
    }

    #[test]
    fn permission_rules_table() {
        let read = Action::ReadProject {
            project: "acme/api".into(),
        };
        let resolve = Action::ResolveTask {
            project: "acme/api".into(),
        };
        let manage = Action::ManageProjectConfig {
            project: "acme/api".into(),
        };

        assert!(authorize(&identity(Some(ProjectRole::Viewer)), &read).is_ok());
        assert!(authorize(&identity(Some(ProjectRole::Viewer)), &resolve).is_err());
        assert!(authorize(&identity(Some(ProjectRole::Member)), &resolve).is_ok());
        assert!(authorize(&identity(Some(ProjectRole::Member)), &manage).is_err());
        assert!(authorize(&identity(Some(ProjectRole::Admin)), &manage).is_ok());
        assert!(authorize(&identity(None), &read).is_err());
        assert!(authorize(&identity(None), &Action::PlatformConfig).is_err());
        assert!(authorize(&identity(None), &Action::IssueSshCert).is_ok());
    }

    #[test]
    fn ssh_principal_format() {
        assert_eq!(job_ssh_principal("acme", "api", 42), "job:acme/api:42");
    }
}

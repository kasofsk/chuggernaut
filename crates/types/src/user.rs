//! Users and identities (spec §1.3, §7.1).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Stored in NATS KV at `users.{b64url(email)}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    /// `"owner/project"` → role.
    pub project_roles: HashMap<String, ProjectRole>,
    pub platform_admin: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub sub: String,
    pub kind: IdentityKind,
    pub project_roles: HashMap<String, ProjectRole>,
    pub platform_admin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityKind {
    User,
    Dispatcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectRole {
    Viewer,
    Member,
    Admin,
}

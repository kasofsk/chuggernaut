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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Identity {
    pub sub: String,
    pub kind: IdentityKind,
    pub project_roles: HashMap<String, ProjectRole>,
    pub platform_admin: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum IdentityKind {
    User,
    Dispatcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ProjectRole {
    Viewer,
    Member,
    Admin,
}

impl ProjectRole {
    /// Parse an operator-facing role name (the admin CLI `--role` flag and the
    /// members API body). `owner` is accepted as an alias for `admin` — the top
    /// project role — so the operator-facing vocabulary can say "owner" while the
    /// stored role stays the spec's `admin` (§7.5). Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "viewer" => Some(Self::Viewer),
            "member" => Some(Self::Member),
            "admin" | "owner" => Some(Self::Admin),
            _ => None,
        }
    }
}

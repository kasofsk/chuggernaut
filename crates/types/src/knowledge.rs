//! Knowledge Objects (spec Part 9).

use serde::{Deserialize, Serialize};

/// A discrete `(subject, predicate) → object` fact. Subjects and predicates may
/// contain any characters; storage keys base64url-encode them (spec §1.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeObject {
    pub subject: String,
    pub predicate: String,
    pub value: String,
}

/// Scope within the `knowledge` bucket. Narrower scopes win on dedup by
/// `(subject, predicate)`: Project > Team > Global.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeScope {
    Global,
    Team { owner: String },
    Project { owner: String, project: String },
}

//! Variables — project-scoped plaintext key-value pairs (spec §8.1).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Var {
    pub name: String,
    pub value: String,
}

#[async_trait]
pub trait VarStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> crate::Result<()>;
    async fn get(&self, owner: &str, project: &str, name: &str) -> crate::Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> crate::Result<()>;
    /// Names and values — vars are not sensitive.
    async fn list(&self, owner: &str, project: &str) -> crate::Result<Vec<Var>>;
}

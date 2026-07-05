//! Secrets — age-encrypted at rest in NATS KV (spec §8.2).
//!
//! The API layer constructs a [`SecretStore`] with the age *public* key only
//! (encrypt on write); the dispatcher holds the private key (decrypt at launch).

use async_trait::async_trait;

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> crate::Result<()>;
    /// Decrypts; only callable on a store constructed with the age private key.
    async fn get(&self, owner: &str, project: &str, name: &str) -> crate::Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> crate::Result<()>;
    /// Names only — values are never returned to callers outside the dispatcher.
    async fn list(&self, owner: &str, project: &str) -> crate::Result<Vec<String>>;
}

//! Secrets — age-encrypted at rest in NATS KV (spec §8.2).
//!
//! The API layer constructs a [`SecretStore`] with the age *public* key only
//! (encrypt on write); the dispatcher holds the private key (decrypt at launch).

use crate::{Bucket, StoreError, keys};
use async_trait::async_trait;
use std::io::{Read, Write};
use std::str::FromStr;

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> crate::Result<()>;
    /// Decrypts; only callable on a store constructed with the age private key.
    async fn get(&self, owner: &str, project: &str, name: &str) -> crate::Result<Option<String>>;
    async fn delete(&self, owner: &str, project: &str, name: &str) -> crate::Result<()>;
    /// Names only — values are never returned to callers outside the dispatcher.
    async fn list(&self, owner: &str, project: &str) -> crate::Result<Vec<String>>;
}

/// age-encrypted NATS KV secrets (spec §8.2). The API side is constructed
/// with the public key only (encrypt on write); the dispatcher side holds the
/// identity (decrypt at launch). Values are stored as base64 age ciphertext.
pub struct AgeSecretStore {
    bucket: Bucket,
    recipient: age::x25519::Recipient,
    identity: Option<age::x25519::Identity>,
}

impl AgeSecretStore {
    /// Encrypt-only: the API layer's construction (public key string, `age1...`).
    pub fn for_api(bucket: Bucket, public_key: &str) -> crate::Result<Self> {
        let recipient = age::x25519::Recipient::from_str(public_key)
            .map_err(|e| StoreError::Nats(format!("invalid age public key: {e}")))?;
        Ok(Self { bucket, recipient, identity: None })
    }

    /// Encrypt + decrypt: the dispatcher's construction (identity string,
    /// `AGE-SECRET-KEY-1...`).
    pub fn for_dispatcher(bucket: Bucket, identity: &str) -> crate::Result<Self> {
        let identity = age::x25519::Identity::from_str(identity)
            .map_err(|e| StoreError::Nats(format!("invalid age identity: {e}")))?;
        Ok(Self {
            bucket,
            recipient: identity.to_public(),
            identity: Some(identity),
        })
    }
}

#[async_trait]
impl SecretStore for AgeSecretStore {
    async fn set(&self, owner: &str, project: &str, name: &str, value: &str) -> crate::Result<()> {
        keys::validate_name(name)?;
        let crypt = |v: &str| -> Result<Vec<u8>, String> {
            let encryptor = age::Encryptor::with_recipients(std::iter::once(
                &self.recipient as &dyn age::Recipient,
            ))
            .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            let mut writer = encryptor.wrap_output(&mut out).map_err(|e| e.to_string())?;
            writer.write_all(v.as_bytes()).map_err(|e| e.to_string())?;
            writer.finish().map_err(|e| e.to_string())?;
            Ok(out)
        };
        let ciphertext = crypt(value).map_err(|e| StoreError::Nats(format!("age encrypt: {e}")))?;
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(ciphertext);
        self.bucket.put_json(&format!("{owner}.{project}.{name}"), &encoded).await
    }

    async fn get(&self, owner: &str, project: &str, name: &str) -> crate::Result<Option<String>> {
        let Some(identity) = &self.identity else {
            return Err(StoreError::Nats(
                "secret decryption requires the age identity (dispatcher-only)".into(),
            ));
        };
        let Some(encoded) = self
            .bucket
            .get_json::<String>(&format!("{owner}.{project}.{name}"))
            .await?
        else {
            return Ok(None);
        };
        use base64::Engine;
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .map_err(|e| StoreError::Nats(format!("secret is not base64: {e}")))?;
        let decrypt = || -> Result<String, String> {
            let decryptor =
                age::Decryptor::new_buffered(ciphertext.as_slice()).map_err(|e| e.to_string())?;
            let mut reader = decryptor
                .decrypt(std::iter::once(identity as &dyn age::Identity))
                .map_err(|e| e.to_string())?;
            let mut plaintext = String::new();
            reader.read_to_string(&mut plaintext).map_err(|e| e.to_string())?;
            Ok(plaintext)
        };
        decrypt()
            .map(Some)
            .map_err(|e| StoreError::Nats(format!("age decrypt: {e}")))
    }

    async fn delete(&self, owner: &str, project: &str, name: &str) -> crate::Result<()> {
        self.bucket.delete(&format!("{owner}.{project}.{name}")).await
    }

    async fn list(&self, owner: &str, project: &str) -> crate::Result<Vec<String>> {
        let prefix = format!("{owner}.{project}.");
        Ok(self
            .bucket
            .keys_with_prefix(&prefix)
            .await?
            .iter()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(String::from)
            .collect())
    }
}

/// Generate a fresh age keypair: `(identity, public_key)`. Used by platform
/// init (§12.1) and tests.
pub fn generate_age_keypair() -> (String, String) {
    use age::secrecy::ExposeSecret;
    let identity = age::x25519::Identity::generate();
    (
        identity.to_string().expose_secret().to_string(),
        identity.to_public().to_string(),
    )
}

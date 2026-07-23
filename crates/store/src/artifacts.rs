//! Blob artifacts: per-task Claude session transcripts and container logs, plus
//! per-job operator-uploaded attachments (screenshots, reference files; §1.6).
//!
//! Stored in a JetStream **Object** Store (chunked internally, so blobs are not
//! bound by `max_payload`), gzipped, then age-encrypted at rest.
//!
//! **This uses its own keypair, not the secrets key.** §10.2 keeps the secrets
//! identity dispatcher-only, and [`crate::secrets::AgeSecretStore::for_api`] is
//! deliberately encrypt-only. But the API must *decrypt* to serve a transcript
//! to the UI, and proxying blobs through the dispatcher would reintroduce the
//! `max_payload` cap this store exists to avoid. So `age_artifacts` is a
//! separate identity, held by both the dispatcher (writes) and the API (reads).
//! It protects artifacts at rest from anyone holding NATS creds or a disk
//! backup — not from the API, which is authorized to display them anyway.
//!
//! Transcripts are treated as **opaque bytes**. Anthropic documents the
//! `.jsonl` entry format as internal to Claude Code and subject to change on any
//! release, so nothing here parses it; structured data comes from the CLI's
//! `--output-format stream-json` event stream instead.

use crate::{StoreError, keys};
use async_nats::jetstream::object_store::ObjectStore;
use std::io::{Read, Write};
use std::str::FromStr;
use tokio::io::AsyncReadExt as _;

/// What a blob is. The string form is the trailing segment of the object name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// The Claude Code session transcript (`--session-id` names the file).
    SessionTranscript,
    /// Captured container stdout+stderr.
    Stdout,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::SessionTranscript => "session.jsonl",
            ArtifactKind::Stdout => "stdout.log",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session.jsonl" => Some(ArtifactKind::SessionTranscript),
            "stdout.log" => Some(ArtifactKind::Stdout),
            _ => None,
        }
    }
}

/// Fallback content type for an attachment whose type is unknown or whose
/// stored metadata is unreadable.
pub const DEFAULT_ATTACHMENT_CONTENT_TYPE: &str = "application/octet-stream";

/// Metadata for one operator-uploaded job attachment (spec §1.6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    /// Filename as uploaded (the object-name suffix after the job prefix).
    pub name: String,
    /// Client-supplied MIME type, echoed back on download.
    pub content_type: String,
    /// Original (plaintext) size in bytes.
    pub size: u64,
}

/// Pack an attachment's content type and original size into the object
/// description field, so a listing can report them without opening the blob.
fn attachment_desc(content_type: &str, size: u64) -> String {
    serde_json::json!({ "content_type": content_type, "size": size }).to_string()
}

/// Inverse of [`attachment_desc`], tolerant of a missing or malformed
/// description (falls back to the default content type and a zero size).
fn parse_attachment_desc(desc: Option<&str>) -> (String, u64) {
    desc.and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .map(|v| {
            let content_type = v
                .get("content_type")
                .and_then(|c| c.as_str())
                .unwrap_or(DEFAULT_ATTACHMENT_CONTENT_TYPE)
                .to_string();
            let size = v.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
            (content_type, size)
        })
        .unwrap_or_else(|| (DEFAULT_ATTACHMENT_CONTENT_TYPE.to_string(), 0))
}

/// gzip + age. Encrypt-only when built from a public key; encrypt+decrypt from
/// an identity.
pub struct ArtifactCrypto {
    recipient: age::x25519::Recipient,
    identity: Option<age::x25519::Identity>,
}

impl ArtifactCrypto {
    /// Encrypt-only (`age1...`).
    pub fn encrypt_only(public_key: &str) -> crate::Result<Self> {
        let recipient = age::x25519::Recipient::from_str(public_key.trim())
            .map_err(|e| StoreError::Nats(format!("invalid artifacts public key: {e}")))?;
        Ok(Self {
            recipient,
            identity: None,
        })
    }

    /// Encrypt + decrypt (`AGE-SECRET-KEY-1...`).
    pub fn with_identity(identity: &str) -> crate::Result<Self> {
        let identity = age::x25519::Identity::from_str(identity.trim())
            .map_err(|e| StoreError::Nats(format!("invalid artifacts identity: {e}")))?;
        Ok(Self {
            recipient: identity.to_public(),
            identity: Some(identity),
        })
    }

    /// gzip then encrypt.
    pub fn seal(&self, plaintext: &[u8]) -> crate::Result<Vec<u8>> {
        let gz = || -> std::io::Result<Vec<u8>> {
            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(plaintext)?;
            enc.finish()
        };
        let compressed = gz().map_err(|e| StoreError::Nats(format!("gzip: {e}")))?;

        let seal = || -> Result<Vec<u8>, String> {
            let encryptor = age::Encryptor::with_recipients(std::iter::once(
                &self.recipient as &dyn age::Recipient,
            ))
            .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            let mut writer = encryptor.wrap_output(&mut out).map_err(|e| e.to_string())?;
            writer.write_all(&compressed).map_err(|e| e.to_string())?;
            writer.finish().map_err(|e| e.to_string())?;
            Ok(out)
        };
        seal().map_err(|e| StoreError::Nats(format!("age encrypt: {e}")))
    }

    /// Decrypt then gunzip.
    pub fn open(&self, ciphertext: &[u8]) -> crate::Result<Vec<u8>> {
        let Some(identity) = &self.identity else {
            return Err(StoreError::Nats(
                "artifact decryption requires the age_artifacts identity".into(),
            ));
        };
        let unseal = || -> Result<Vec<u8>, String> {
            let decryptor = age::Decryptor::new_buffered(ciphertext).map_err(|e| e.to_string())?;
            let mut reader = decryptor
                .decrypt(std::iter::once(identity as &dyn age::Identity))
                .map_err(|e| e.to_string())?;
            let mut compressed = Vec::new();
            reader
                .read_to_end(&mut compressed)
                .map_err(|e| e.to_string())?;
            Ok(compressed)
        };
        let compressed = unseal().map_err(|e| StoreError::Nats(format!("age decrypt: {e}")))?;

        let mut out = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .read_to_end(&mut out)
            .map_err(|e| StoreError::Nats(format!("gunzip: {e}")))?;
        Ok(out)
    }
}

pub struct ArtifactStore {
    obj: ObjectStore,
    crypto: ArtifactCrypto,
}

impl ArtifactStore {
    pub fn new(obj: ObjectStore, crypto: ArtifactCrypto) -> Self {
        Self { obj, crypto }
    }

    pub async fn put(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
        kind: ArtifactKind,
        plaintext: &[u8],
    ) -> crate::Result<()> {
        let sealed = self.crypto.seal(plaintext)?;
        let name = keys::artifact_key(owner, project, job_seq, task_id, kind.as_str());
        self.obj
            .put(name.as_str(), &mut sealed.as_slice())
            .await
            .map_err(|e| StoreError::Nats(format!("artifact put: {e}")))?;
        Ok(())
    }

    /// `None` when the artifact was never captured (e.g. a human task, or a run
    /// that died before the copy).
    pub async fn get(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
        kind: ArtifactKind,
    ) -> crate::Result<Option<Vec<u8>>> {
        let name = keys::artifact_key(owner, project, job_seq, task_id, kind.as_str());
        let mut object = match self.obj.get(name.as_str()).await {
            Ok(o) => o,
            // The object-store API reports a missing object as an error, not None.
            Err(_) => return Ok(None),
        };
        let mut sealed = Vec::new();
        object
            .read_to_end(&mut sealed)
            .await
            .map_err(|e| StoreError::Nats(format!("artifact read: {e}")))?;
        self.crypto.open(&sealed).map(Some)
    }

    // ── Job attachments (operator-uploaded files) ──────────────────────────
    //
    // A job attachment is an operator-uploaded file carried alongside a job —
    // a screenshot on a bug report, a reference document. Unlike task
    // artifacts, it is per-*job*, has an arbitrary filename, and carries a
    // client-supplied content type. It reuses this store's object bucket and
    // crypto: the bytes are gzipped + age-encrypted at rest, dodging the 1MB
    // `max_payload` cap exactly as transcripts do. Like a transcript, an
    // attachment is presentational reference material — served to the UI, never
    // injected into an agent prompt.

    /// Store (or replace) an attachment. `content_type` and the original byte
    /// length ride in the object's description so a listing need not open the
    /// blob to report them.
    pub async fn put_attachment(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        name: &str,
        content_type: &str,
        plaintext: &[u8],
    ) -> crate::Result<()> {
        use async_nats::jetstream::object_store::ObjectMetadata;
        let sealed = self.crypto.seal(plaintext)?;
        let meta = ObjectMetadata {
            name: keys::job_attachment_key(owner, project, job_seq, name),
            description: Some(attachment_desc(content_type, plaintext.len() as u64)),
            chunk_size: None,
        };
        self.obj
            .put(meta, &mut sealed.as_slice())
            .await
            .map_err(|e| StoreError::Nats(format!("attachment put: {e}")))?;
        Ok(())
    }

    /// One attachment, decrypted, with its metadata. `None` when absent.
    pub async fn get_attachment(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        name: &str,
    ) -> crate::Result<Option<(Attachment, Vec<u8>)>> {
        let key = keys::job_attachment_key(owner, project, job_seq, name);
        let mut object = match self.obj.get(key.as_str()).await {
            Ok(o) => o,
            // The object-store API reports a missing object as an error, not None.
            Err(_) => return Ok(None),
        };
        // A DELETEd object is a tombstone: `get` succeeds but the reader has no
        // chunks and `read_to_end` awaits forever (reproduced 2026-07-23 —
        // GET-after-DELETE hung http_bridge_end_to_end and a prod CI task).
        // Deleted ⇒ absent.
        if object.info.deleted {
            return Ok(None);
        }
        let (content_type, _) = parse_attachment_desc(object.info.description.as_deref());
        let mut sealed = Vec::new();
        object
            .read_to_end(&mut sealed)
            .await
            .map_err(|e| StoreError::Nats(format!("attachment read: {e}")))?;
        let plaintext = self.crypto.open(&sealed)?;
        let meta = Attachment {
            name: name.to_string(),
            content_type,
            size: plaintext.len() as u64,
        };
        Ok(Some((meta, plaintext)))
    }

    /// Attachments present on a job, by filename. Reads metadata only — the
    /// blobs are not opened.
    pub async fn list_attachments(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
    ) -> crate::Result<Vec<Attachment>> {
        use futures::TryStreamExt as _;
        let prefix = keys::job_attachment_prefix(owner, project, job_seq);
        let list = self
            .obj
            .list()
            .await
            .map_err(|e| StoreError::Nats(format!("attachment list: {e}")))?;
        let infos: Vec<_> = list
            .try_collect()
            .await
            .map_err(|e| StoreError::Nats(format!("attachment list: {e}")))?;
        let mut out: Vec<Attachment> = infos
            .iter()
            .filter(|i| !i.deleted)
            .filter_map(|i| {
                let name = i.name.strip_prefix(&prefix)?;
                let (content_type, size) = parse_attachment_desc(i.description.as_deref());
                Some(Attachment {
                    name: name.to_string(),
                    content_type,
                    size,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Remove an attachment. Returns `false` when it did not exist.
    pub async fn delete_attachment(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        name: &str,
    ) -> crate::Result<bool> {
        let key = keys::job_attachment_key(owner, project, job_seq, name);
        // `info` on a DELETEd object still succeeds (tombstone) — treat it as
        // already-gone, same as missing, so double-delete reports false.
        match self.obj.info(key.as_str()).await {
            Err(_) => return Ok(false),
            Ok(info) if info.deleted => return Ok(false),
            Ok(_) => {}
        }
        self.obj
            .delete(key.as_str())
            .await
            .map_err(|e| StoreError::Nats(format!("attachment delete: {e}")))?;
        Ok(true)
    }

    /// Kinds present for a task.
    pub async fn list_for_task(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
    ) -> crate::Result<Vec<ArtifactKind>> {
        use futures::TryStreamExt as _;
        let prefix = keys::artifact_task_prefix(owner, project, job_seq, task_id);
        let list = self
            .obj
            .list()
            .await
            .map_err(|e| StoreError::Nats(format!("artifact list: {e}")))?;
        let infos: Vec<_> = list
            .try_collect()
            .await
            .map_err(|e| StoreError::Nats(format!("artifact list: {e}")))?;
        Ok(infos
            .iter()
            .filter_map(|i| i.name.strip_prefix(&prefix))
            .filter_map(ArtifactKind::parse)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips() {
        for k in [ArtifactKind::SessionTranscript, ArtifactKind::Stdout] {
            assert_eq!(ArtifactKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ArtifactKind::parse("passwd"), None);
    }

    /// The kind carries a dot, so it must remain the trailing segment and the
    /// task prefix must not swallow it.
    #[test]
    fn artifact_key_layout() {
        let key = keys::artifact_key(
            "acme",
            "api",
            42,
            7,
            ArtifactKind::SessionTranscript.as_str(),
        );
        assert_eq!(key, "acme.api.42.7.session.jsonl");
        let prefix = keys::artifact_task_prefix("acme", "api", 42, 7);
        assert_eq!(key.strip_prefix(&prefix), Some("session.jsonl"));
        // A sibling task's artifacts must not match this task's prefix.
        let other = keys::artifact_key("acme", "api", 42, 71, "stdout.log");
        assert!(other.strip_prefix(&prefix).is_none());
    }

    #[test]
    fn attachment_desc_round_trips_and_tolerates_junk() {
        let (ct, size) = parse_attachment_desc(Some(&attachment_desc("image/png", 4096)));
        assert_eq!(ct, "image/png");
        assert_eq!(size, 4096);
        // Missing or malformed descriptions fall back gracefully.
        let (ct, size) = parse_attachment_desc(None);
        assert_eq!(ct, DEFAULT_ATTACHMENT_CONTENT_TYPE);
        assert_eq!(size, 0);
        let (ct, size) = parse_attachment_desc(Some("not json"));
        assert_eq!(ct, DEFAULT_ATTACHMENT_CONTENT_TYPE);
        assert_eq!(size, 0);
    }

    #[test]
    fn seal_open_round_trips_and_compresses() {
        let (identity, public) = crate::secrets::generate_age_keypair();
        let writer = ArtifactCrypto::encrypt_only(&public).unwrap();
        let reader = ArtifactCrypto::with_identity(&identity).unwrap();

        // Transcript-shaped: repetitive JSONL, which is why gzip precedes age.
        let plaintext = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#
            .repeat(500)
            .into_bytes();
        let sealed = writer.seal(&plaintext).unwrap();
        assert!(
            sealed.len() < plaintext.len() / 2,
            "expected compression: {} -> {}",
            plaintext.len(),
            sealed.len()
        );
        assert_eq!(reader.open(&sealed).unwrap(), plaintext);
    }

    #[test]
    fn encrypt_only_cannot_decrypt() {
        let (_, public) = crate::secrets::generate_age_keypair();
        let writer = ArtifactCrypto::encrypt_only(&public).unwrap();
        let sealed = writer.seal(b"secret-bearing transcript").unwrap();
        assert!(writer.open(&sealed).is_err());
    }

    #[test]
    fn a_foreign_identity_cannot_decrypt() {
        let (_, public) = crate::secrets::generate_age_keypair();
        let (other_identity, _) = crate::secrets::generate_age_keypair();
        let sealed = ArtifactCrypto::encrypt_only(&public)
            .unwrap()
            .seal(b"transcript")
            .unwrap();
        let intruder = ArtifactCrypto::with_identity(&other_identity).unwrap();
        assert!(intruder.open(&sealed).is_err());
    }
}

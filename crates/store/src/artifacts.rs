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
    /// The archive a work container left at `/workspace/chug-output.tar.gz`
    /// (design #362 Decision 2). Lives in its own bucket, on its own clock.
    Output,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::SessionTranscript => "session.jsonl",
            ArtifactKind::Stdout => "stdout.log",
            ArtifactKind::Output => "output.tar.gz",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session.jsonl" => Some(ArtifactKind::SessionTranscript),
            "stdout.log" => Some(ArtifactKind::Stdout),
            "output.tar.gz" => Some(ArtifactKind::Output),
            _ => None,
        }
    }
}

/// The largest blob the platform accepts anywhere: an operator attachment
/// (§1.6) and a harvested output archive (design #362) share it, so the size
/// band has one number rather than two.
pub const MAX_BLOB_BYTES: usize = 16 * 1024 * 1024;

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

/// #196 belt-and-braces: no object-store await may park its caller forever.
/// async-nats 0.38's object store has watch-backed list streams and chunk
/// readers whose termination has raced in prod (the http_bridge / CI hang,
/// 2026-07-23). The construction-level fixes (tombstone guards) close the
/// known cases; this bound turns any unknown one into a loud `StoreError`
/// instead of an eternally-hung CI task.
const OBJ_OP_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Await `fut` under [`OBJ_OP_BOUND`], mapping both the op's own error and a
/// timeout into a `StoreError` tagged `what`.
async fn bound<T, E: std::fmt::Display>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> crate::Result<T> {
    match tokio::time::timeout(OBJ_OP_BOUND, fut).await {
        Ok(r) => r.map_err(|e| StoreError::Nats(format!("{what}: {e}"))),
        Err(_) => Err(StoreError::Nats(format!(
            "{what}: exceeded {}s object-store bound",
            OBJ_OP_BOUND.as_secs()
        ))),
    }
}

pub struct ArtifactStore {
    obj: ObjectStore,
    /// The `outputs` bucket (design #362 R1) — its own retention and its own
    /// byte ceiling, so a build byproduct can never displace a transcript.
    outputs: ObjectStore,
    crypto: ArtifactCrypto,
}

impl ArtifactStore {
    pub fn new(obj: ObjectStore, outputs: ObjectStore, crypto: ArtifactCrypto) -> Self {
        Self {
            obj,
            outputs,
            crypto,
        }
    }

    /// Which bucket a kind lives in. Outputs are isolated from the audit record
    /// (transcripts, stdout, attachments) precisely so their pressure stays
    /// theirs (design #362 R1).
    fn bucket_for(&self, kind: ArtifactKind) -> &ObjectStore {
        match kind {
            ArtifactKind::Output => &self.outputs,
            ArtifactKind::SessionTranscript | ArtifactKind::Stdout => &self.obj,
        }
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
        bound(
            "artifact put",
            self.bucket_for(kind)
                .put(name.as_str(), &mut sealed.as_slice()),
        )
        .await?;
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
        let obj = self.bucket_for(kind);
        let mut object = match tokio::time::timeout(OBJ_OP_BOUND, obj.get(name.as_str())).await {
            Ok(Err(_)) => return Ok(None),
            Ok(Ok(o)) => o,
            Err(_) => {
                return Err(StoreError::Nats("artifact get: exceeded bound".into()));
            }
        };
        if object.info.deleted {
            return Ok(None);
        }
        let mut sealed = Vec::new();
        bound("artifact read", object.read_to_end(&mut sealed)).await?;
        self.crypto.open(&sealed).map(Some)
    }

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
        bound("attachment put", self.obj.put(meta, &mut sealed.as_slice())).await?;
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
        let mut object = match tokio::time::timeout(OBJ_OP_BOUND, self.obj.get(key.as_str())).await
        {
            Ok(Err(_)) => return Ok(None),
            Ok(Ok(o)) => o,
            Err(_) => {
                return Err(StoreError::Nats("attachment get: exceeded bound".into()));
            }
        };
        if object.info.deleted {
            return Ok(None);
        }
        let (content_type, _) = parse_attachment_desc(object.info.description.as_deref());
        let mut sealed = Vec::new();
        bound("attachment read", object.read_to_end(&mut sealed)).await?;
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
        let list = bound("attachment list", self.obj.list()).await?;
        let infos: Vec<_> = bound("attachment list collect", list.try_collect()).await?;
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
        match tokio::time::timeout(OBJ_OP_BOUND, self.obj.info(key.as_str())).await {
            Ok(Err(_)) => return Ok(false),
            Ok(Ok(info)) if info.deleted => return Ok(false),
            Ok(Ok(_)) => {}
            Err(_) => {
                return Err(StoreError::Nats("attachment info: exceeded bound".into()));
            }
        }
        bound("attachment delete", self.obj.delete(key.as_str())).await?;
        Ok(true)
    }

    /// Kinds present for a task, across both buckets.
    pub async fn list_for_task(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
        task_id: u64,
    ) -> crate::Result<Vec<ArtifactKind>> {
        let prefix = keys::artifact_task_prefix(owner, project, job_seq, task_id);
        let mut kinds = Vec::new();
        for obj in [&self.obj, &self.outputs] {
            for name in list_names(obj).await? {
                if let Some(kind) = name.strip_prefix(&prefix).and_then(ArtifactKind::parse) {
                    kinds.push(kind);
                }
            }
        }
        Ok(kinds)
    }

    /// Drop every output a job produced and nothing else (spec §3.2,
    /// revoke-time GC) — a revoked job is still an audit record, so its
    /// transcripts, stdout and attachments stay.
    ///
    /// Returns how many were removed.
    pub async fn delete_outputs_for_job(
        &self,
        owner: &str,
        project: &str,
        job_seq: u64,
    ) -> crate::Result<usize> {
        let prefix = format!("{}.", keys::job_key(owner, project, job_seq));
        let suffix = format!(".{}", ArtifactKind::Output.as_str());
        let mut removed = 0;
        for name in list_names(&self.outputs).await? {
            if !name.starts_with(&prefix) || !name.ends_with(&suffix) {
                continue;
            }
            bound("output delete", self.outputs.delete(name.as_str())).await?;
            removed += 1;
        }
        Ok(removed)
    }
}

/// Live object names in a bucket. Both listings are metadata-only reads — no
/// blob is opened — and both are bounded by [`OBJ_OP_BOUND`].
async fn list_names(obj: &ObjectStore) -> crate::Result<Vec<String>> {
    use futures::TryStreamExt as _;
    let list = bound("artifact list", obj.list()).await?;
    let infos: Vec<async_nats::jetstream::object_store::ObjectInfo> =
        bound("artifact list collect", list.try_collect()).await?;
    Ok(infos
        .into_iter()
        .filter(|i| !i.deleted)
        .map(|i| i.name)
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn kind_round_trips() {
        for k in [
            ArtifactKind::SessionTranscript,
            ArtifactKind::Stdout,
            ArtifactKind::Output,
        ] {
            assert_eq!(ArtifactKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(ArtifactKind::parse("passwd"), None);
        assert_eq!(ArtifactKind::parse("output.tar"), None);
    }

    /// The output kind carries two dots, so the key layout has to survive a
    /// multi-dot trailing segment the same way `session.jsonl` does — the
    /// listing and the revoke-time GC both key off exactly this shape.
    #[test]
    fn output_key_layout() {
        let key = keys::artifact_key("acme", "api", 42, 7, ArtifactKind::Output.as_str());
        assert_eq!(key, "acme.api.42.7.output.tar.gz");
        let prefix = keys::artifact_task_prefix("acme", "api", 42, 7);
        assert_eq!(
            key.strip_prefix(&prefix).and_then(ArtifactKind::parse),
            Some(ArtifactKind::Output)
        );
        assert!(key.starts_with(&format!("{}.", keys::job_key("acme", "api", 42))));
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
        let other = keys::artifact_key("acme", "api", 42, 71, "stdout.log");
        assert!(other.strip_prefix(&prefix).is_none());
    }

    #[test]
    fn attachment_desc_round_trips_and_tolerates_junk() {
        let (ct, size) = parse_attachment_desc(Some(&attachment_desc("image/png", 4096)));
        assert_eq!(ct, "image/png");
        assert_eq!(size, 4096);
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

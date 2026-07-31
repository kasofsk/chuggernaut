//! Per-job NATS credentials (spec §7.4) via decentralized NATS JWT auth.
//!
//! One operator key and one platform account key ("CHUG") are generated at
//! init (§12.1); the server runs with a memory resolver preloaded with the
//! account JWTs. At each container launch the dispatcher mints a fresh user
//! nkey + JWT scoped to the §7.4 allow-list and valid for `task_timeout`,
//! rendered as a standard `.creds` file.
//!
//! JWT encoding is hand-rolled (`alg: ed25519-nkey`, claims v2) — the wire
//! format is three base64url segments like any JWT, signed with the issuer's
//! nkey; no NATS jwt library exists for Rust with a dependency list we want.

use crate::AuthError;
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use nkeys::KeyPair;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

fn internal(e: impl std::fmt::Display) -> AuthError {
    AuthError::Internal(e.to_string())
}

/// Encode and sign a NATS JWT: claims are finalized with `iat`/`iss`/`jti`
/// and signed by `issuer`.
fn encode_jwt(
    subject: &str,
    name: &str,
    nats: Value,
    expires: Option<i64>,
    issuer: &KeyPair,
) -> Result<String, AuthError> {
    let mut claims = Map::new();
    claims.insert("iat".into(), json!(chrono::Utc::now().timestamp()));
    if let Some(exp) = expires {
        claims.insert("exp".into(), json!(exp));
    }
    claims.insert("iss".into(), json!(issuer.public_key()));
    claims.insert("name".into(), json!(name));
    claims.insert("sub".into(), json!(subject));
    claims.insert("nats".into(), nats);

    let hash = Sha256::digest(serde_json::to_vec(&claims).map_err(internal)?);
    claims.insert("jti".into(), json!(BASE32_NOPAD.encode(&hash)));

    let header = BASE64URL_NOPAD.encode(br#"{"typ":"JWT","alg":"ed25519-nkey"}"#);
    let payload = BASE64URL_NOPAD.encode(&serde_json::to_vec(&claims).map_err(internal)?);
    let signing_input = format!("{header}.{payload}");
    let sig = issuer.sign(signing_input.as_bytes()).map_err(internal)?;
    Ok(format!("{signing_input}.{}", BASE64URL_NOPAD.encode(&sig)))
}

/// Self-signed operator JWT.
pub fn operator_jwt(operator: &KeyPair, name: &str) -> Result<String, AuthError> {
    encode_jwt(
        &operator.public_key(),
        name,
        json!({ "type": "operator", "version": 2 }),
        None,
        operator,
    )
}

/// Operator-signed account JWT with unlimited limits and JetStream enabled.
pub fn account_jwt(
    operator: &KeyPair,
    account_public: &str,
    name: &str,
) -> Result<String, AuthError> {
    encode_jwt(
        account_public,
        name,
        json!({
            "limits": {
                "subs": -1, "data": -1, "payload": -1,
                "imports": -1, "exports": -1, "wildcards": true,
                "conn": -1, "leaf": -1,
                "mem_storage": -1, "disk_storage": -1,
                "streams": -1, "consumer": -1,
            },
            "type": "account",
            "version": 2,
        }),
        None,
        operator,
    )
}

/// The system account: JetStream must stay disabled on it (the server
/// refuses to start otherwise), so no storage limits.
pub fn system_account_jwt(operator: &KeyPair, account_public: &str) -> Result<String, AuthError> {
    encode_jwt(
        account_public,
        "SYS",
        json!({
            "limits": {
                "subs": -1, "data": -1, "payload": -1,
                "imports": -1, "exports": -1, "wildcards": true,
                "conn": -1, "leaf": -1,
            },
            "type": "account",
            "version": 2,
        }),
        None,
        operator,
    )
}

/// Publish/subscribe allow-lists for a user JWT. Empty lists mean
/// unrestricted (the NATS default when no permissions are present).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Permissions {
    pub publish: Vec<String>,
    pub subscribe: Vec<String>,
}

/// Standard `.creds` file: user JWT + nkey seed.
pub fn format_creds(jwt: &str, seed: &str) -> String {
    format!(
        "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
         ************************* IMPORTANT *************************\n\
         NKEY Seed printed below can be used to sign and prove identity.\n\
         NKEYs are sensitive and should be treated as secrets.\n\n\
         -----BEGIN USER NKEY SEED-----\n{seed}\n------END USER NKEY SEED------\n\n\
         *************************************************************\n"
    )
}

/// Mints per-container user credentials, signed by the platform account key.
pub struct NatsUserSigner {
    account: KeyPair,
}

impl NatsUserSigner {
    pub fn from_account_seed(seed: &str) -> Result<Self, AuthError> {
        Ok(Self {
            account: KeyPair::from_seed(seed.trim()).map_err(internal)?,
        })
    }

    /// Mint a fresh user nkey + JWT and render the `.creds` content.
    /// `expires_in: None` for long-lived identities (dispatcher).
    pub fn mint_creds(
        &self,
        name: &str,
        permissions: &Permissions,
        expires_in: Option<chrono::Duration>,
    ) -> Result<String, AuthError> {
        let user = KeyPair::new_user();
        let mut nats = Map::new();
        if !permissions.publish.is_empty() {
            nats.insert("pub".into(), json!({ "allow": permissions.publish }));
        }
        if !permissions.subscribe.is_empty() {
            nats.insert("sub".into(), json!({ "allow": permissions.subscribe }));
        }
        nats.insert("subs".into(), json!(-1));
        nats.insert("data".into(), json!(-1));
        nats.insert("payload".into(), json!(-1));
        nats.insert("type".into(), json!("user"));
        nats.insert("version".into(), json!(2));
        let exp = expires_in.map(|d| (chrono::Utc::now() + d).timestamp());
        let jwt = encode_jwt(
            &user.public_key(),
            name,
            Value::Object(nats),
            exp,
            &self.account,
        )?;
        Ok(format_creds(&jwt, &user.seed().map_err(internal)?))
    }
}

/// `nats-server` config body for the memory resolver (§12.1 output): operator
/// JWT, system account, and both account JWTs preloaded. The caller appends
/// listen/jetstream settings.
pub fn resolver_config(
    operator_jwt: &str,
    sys_public: &str,
    sys_jwt: &str,
    chug_public: &str,
    chug_jwt: &str,
) -> String {
    format!(
        "operator: {operator_jwt}\n\
         system_account: {sys_public}\n\
         resolver: MEMORY\n\
         resolver_preload: {{\n  {sys_public}: {sys_jwt}\n  {chug_public}: {chug_jwt}\n}}\n"
    )
}

/// KV read of specific keys: direct-get subjects + the stream-info call
/// async-nats issues when binding the bucket.
fn kv_read(perms: &mut Permissions, bucket: &str, key_patterns: &[String]) {
    perms
        .publish
        .push(format!("$JS.API.STREAM.INFO.KV_{bucket}"));
    for pattern in key_patterns {
        perms.publish.push(format!(
            "$JS.API.DIRECT.GET.KV_{bucket}.$KV.{bucket}.{pattern}"
        ));
    }
}

/// Both container roles share: inbox replies, reading their own channel entry,
/// posting status/replies to the dispatcher, knowledge reads, and the
/// `channel-inbox` stream poll.
fn common_container(perms: &mut Permissions, owner: &str, project: &str, seq: u64) {
    perms.subscribe.push("_INBOX.>".into());
    perms
        .subscribe
        .push(store::subjects::channel_inbox(owner, project, seq));

    let channel_key = store::keys::channel_key(owner, project, seq);
    kv_read(
        perms,
        store::buckets::CHANNELS,
        std::slice::from_ref(&channel_key),
    );
    perms
        .publish
        .push(store::subjects::channel_update(owner, project, seq));
    perms
        .publish
        .push(store::subjects::channel_reply(owner, project, seq));

    kv_read(
        perms,
        store::buckets::KNOWLEDGE,
        &["global.>".into(), format!("{owner}.>")],
    );

    let stream = store::buckets::STREAM_CHANNEL_INBOX;
    perms.publish.push(format!("$JS.API.STREAM.INFO.{stream}"));
    perms
        .publish
        .push(format!("$JS.API.CONSUMER.CREATE.{stream}"));
    perms
        .publish
        .push(format!("$JS.API.CONSUMER.CREATE.{stream}.>"));
    perms
        .publish
        .push(format!("$JS.API.CONSUMER.INFO.{stream}.>"));
    perms
        .publish
        .push(format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.>"));
}

/// Worker-daemon allow-list (spec §3.1): serve its own node's op subjects and
/// answer request-reply inboxes, plus the announce heartbeat that dynamic fleet
/// registration rides on — nothing else. No KV, no JetStream: the
/// node-local-artifact design keeps bulk data off NATS entirely.
///
/// The `store::subjects::worker_announce()` grant is load-bearing: in an
/// operator-mode NATS server a non-empty publish list is a strict allow-list, so
/// without it the daemon's periodic announce is DENIED and the fleet never gains
/// capacity dynamically. Keep it sourced from the subject helper so it cannot
/// drift. Existing worker creds must be re-minted (`chuggernaut admin
/// worker-creds`) on deploy for this grant to take effect.
pub fn worker_permissions(node: &str) -> Permissions {
    Permissions {
        publish: vec!["_INBOX.>".into(), store::subjects::worker_announce()],
        subscribe: vec![format!("req.worker.{node}.>")],
    }
}

/// §7.4 work-container allow-list.
pub fn work_container_permissions(owner: &str, project: &str, seq: u64) -> Permissions {
    let mut perms = Permissions::default();
    common_container(&mut perms, owner, project, seq);
    kv_read(
        &mut perms,
        store::buckets::JOBS,
        &[store::keys::job_key(owner, project, seq)],
    );
    kv_read(
        &mut perms,
        store::buckets::TASKS,
        &[format!("{owner}.{project}.{seq}.*")],
    );
    perms
        .publish
        .push(store::subjects::work_submit(owner, project, seq));
    perms
        .publish
        .push(format!("req.step.report.{owner}.{project}.{seq}.*"));
    perms
}

/// §7.4 eval-container allow-list (more restricted).
pub fn eval_container_permissions(
    owner: &str,
    project: &str,
    seq: u64,
    task_id: u64,
) -> Permissions {
    let mut perms = Permissions::default();
    common_container(&mut perms, owner, project, seq);
    kv_read(
        &mut perms,
        store::buckets::TASKS,
        &[store::keys::task_key(owner, project, seq, task_id)],
    );
    perms
        .publish
        .push(store::subjects::eval_submit(owner, project, seq, task_id));
    perms
}

/// §13: factory triage jobs get the work allow-list plus `create_job`.
pub fn triage_container_permissions(owner: &str, project: &str, seq: u64) -> Permissions {
    let mut perms = work_container_permissions(owner, project, seq);
    perms
        .publish
        .push(format!("req.jobs.create.{owner}.{project}"));
    perms
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn decode_segment(seg: &str) -> Value {
        serde_json::from_slice(&BASE64URL_NOPAD.decode(seg.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn user_jwt_shape_and_signature() {
        let account = KeyPair::new_account();
        let signer = NatsUserSigner::from_account_seed(&account.seed().unwrap()).unwrap();
        let perms = Permissions {
            publish: vec!["req.work.submit.acme.api.42".into()],
            subscribe: vec!["_INBOX.>".into()],
        };
        let creds = signer
            .mint_creds("job-42-work", &perms, Some(chrono::Duration::hours(2)))
            .unwrap();

        let jwt = creds
            .lines()
            .skip_while(|l| !l.starts_with("-----BEGIN NATS USER JWT-----"))
            .nth(1)
            .unwrap();
        let [header, payload, sig]: [&str; 3] =
            jwt.split('.').collect::<Vec<_>>().try_into().unwrap();

        assert_eq!(decode_segment(header)["alg"], "ed25519-nkey");
        let claims = decode_segment(payload);
        assert_eq!(claims["iss"], account.public_key());
        assert!(claims["sub"].as_str().unwrap().starts_with('U'));
        assert_eq!(claims["nats"]["type"], "user");
        assert_eq!(
            claims["nats"]["pub"]["allow"][0],
            "req.work.submit.acme.api.42"
        );
        assert!(claims["exp"].as_i64().unwrap() > chrono::Utc::now().timestamp());

        let signing_input = format!("{header}.{payload}");
        let sig_bytes = BASE64URL_NOPAD.decode(sig.as_bytes()).unwrap();
        account
            .verify(signing_input.as_bytes(), &sig_bytes)
            .unwrap();

        let seed = creds
            .lines()
            .skip_while(|l| !l.starts_with("-----BEGIN USER NKEY SEED-----"))
            .nth(1)
            .unwrap();
        assert!(seed.starts_with("SU"));
        KeyPair::from_seed(seed).unwrap();
    }

    #[test]
    fn operator_and_account_jwts() {
        let operator = KeyPair::new_operator();
        let account = KeyPair::new_account();
        let op_jwt = operator_jwt(&operator, "chuggernaut").unwrap();
        let acc_jwt = account_jwt(&operator, &account.public_key(), "CHUG").unwrap();

        let op_claims = decode_segment(op_jwt.split('.').nth(1).unwrap());
        assert_eq!(op_claims["nats"]["type"], "operator");
        assert_eq!(op_claims["iss"], op_claims["sub"]);

        let acc_claims = decode_segment(acc_jwt.split('.').nth(1).unwrap());
        assert_eq!(acc_claims["nats"]["type"], "account");
        assert_eq!(acc_claims["iss"], operator.public_key());
        assert_eq!(acc_claims["sub"], account.public_key());
        assert_eq!(acc_claims["nats"]["limits"]["disk_storage"], -1);
    }

    #[test]
    fn work_allow_list_covers_the_spec_rows() {
        let p = work_container_permissions("acme", "api", 42);
        for needle in [
            "req.work.submit.acme.api.42",
            "req.step.report.acme.api.42.*",
            "req.channel.update.acme.api.42",
            "req.channel.reply.acme.api.42",
            "$JS.API.DIRECT.GET.KV_jobs.$KV.jobs.acme.api.42",
            "$JS.API.DIRECT.GET.KV_tasks.$KV.tasks.acme.api.42.*",
            "$JS.API.DIRECT.GET.KV_knowledge.$KV.knowledge.global.>",
            "$JS.API.DIRECT.GET.KV_knowledge.$KV.knowledge.acme.>",
            "$JS.API.CONSUMER.MSG.NEXT.channel-inbox.>",
        ] {
            assert!(p.publish.iter().any(|s| s == needle), "missing {needle}");
        }
        assert!(p.subscribe.contains(&"_INBOX.>".to_string()));
        assert!(
            p.subscribe
                .contains(&"channel.inbox.acme.api.42".to_string())
        );
        assert!(
            p.publish
                .iter()
                .any(|s| s == "$JS.API.DIRECT.GET.KV_channels.$KV.channels.acme.api.jobs.42")
        );
        assert!(
            !p.publish.iter().any(|s| s.starts_with("$KV.channels")),
            "container must not write channels KV directly: {:?}",
            p.publish
        );
        for forbidden in ["req.eval.submit", "req.jobs.create", "job.events"] {
            assert!(
                !p.publish.iter().any(|s| s.contains(forbidden)),
                "found {forbidden}"
            );
        }
    }

    #[test]
    fn eval_allow_list_is_narrower() {
        let p = eval_container_permissions("acme", "api", 42, 7);
        assert!(
            p.publish
                .iter()
                .any(|s| s == "req.eval.submit.acme.api.42.7")
        );
        assert!(
            p.publish
                .iter()
                .any(|s| s == "$JS.API.DIRECT.GET.KV_tasks.$KV.tasks.acme.api.42.7")
        );
        assert!(
            p.publish
                .iter()
                .any(|s| s == "req.channel.update.acme.api.42")
        );
        assert!(!p.publish.iter().any(|s| s.starts_with("$KV.channels")));
        for forbidden in ["req.work.submit", "req.step.report", "KV_jobs"] {
            assert!(
                !p.publish.iter().any(|s| s.contains(forbidden)),
                "found {forbidden}"
            );
        }
    }

    #[test]
    fn worker_allow_list_includes_the_announce_heartbeat() {
        let p = worker_permissions("gumbo-nuc-0");
        assert!(
            p.subscribe
                .contains(&"req.worker.gumbo-nuc-0.>".to_string())
        );
        assert!(p.publish.contains(&"_INBOX.>".to_string()));
        assert!(
            p.publish.contains(&store::subjects::worker_announce()),
            "worker must be allowed to publish its announce heartbeat: {:?}",
            p.publish
        );
    }

    #[test]
    fn triage_adds_job_creation_only() {
        let work = work_container_permissions("acme", "api", 42);
        let triage = triage_container_permissions("acme", "api", 42);
        let extra: Vec<_> = triage
            .publish
            .iter()
            .filter(|s| !work.publish.contains(s))
            .collect();
        assert_eq!(extra, vec!["req.jobs.create.acme.api"]);
    }
}

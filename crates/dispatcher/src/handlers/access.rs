//! Identity and access, dispatcher-side: user SSH cert minting (§7.3) and
//! project-role management (§7.5). The dispatcher is the single writer of
//! `users.*`, so the API — which has already authenticated and platform-admin-
//! gated the caller — forwards role mutations here rather than writing KV
//! itself.
//!
//! - **Accepts:** `req.ssh.sign-user-cert` (`{ public_key, email }`),
//!   `req.members.{set,remove,list}.{owner}.{project}` (`{ email, role? }`).
//! - **Emits:** a signed 24h certificate; reads/writes of `users.*` records.
//! - **Guarantees:** the roles baked into a cert are the user's current grants
//!   read from their record, never a client-supplied map. 503 when the CA key
//!   is not mounted, 404 when the user record is missing.
//! - **Spec:** §7.3, §7.5.

use super::reply::{NOT_FOUND, bad_request, error_reply, ok_reply, service_unavailable};
use crate::core::CoreError;
use store::NatsStore;

/// `req.ssh.sign-user-cert` — the API forwards the authenticated caller's email
/// plus the submitted public key; we sign a 24h cert with the CA key.
pub(super) async fn spawn_ssh_handler(
    store: &NatsStore,
    ssh_ca: Option<std::path::PathBuf>,
) -> store::Result<()> {
    let mut ssh_sub = store
        .subscribe_requests(&store::subjects::ssh_sign_user_cert())
        .await?;
    let ssh_store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = ssh_sub.next().await {
            #[derive(serde::Deserialize)]
            struct Body {
                public_key: String,
                email: String,
            }
            let body = match serde_json::from_slice::<Body>(&req.payload) {
                Err(e) => bad_request(&e.to_string()),
                Ok(b) => {
                    sign_user_cert(&ssh_store, ssh_ca.as_deref(), &b.email, &b.public_key).await
                }
            };
            req.respond(body).await;
        }
    });
    Ok(())
}

/// `req.members.{set,remove,list}.{owner}.{project}` — §7.5 role management.
pub(super) async fn spawn_members_handler(store: &NatsStore) -> store::Result<()> {
    let mut members_sub = store.subscribe_requests("req.members.>").await?;
    let members_store = store.clone();
    tokio::spawn(async move {
        while let Some(req) = members_sub.next().await {
            let Some((verb, owner, project)) = super::subject_target(&req.subject) else {
                req.respond(bad_request("malformed subject")).await;
                continue;
            };
            #[derive(serde::Deserialize)]
            struct Body {
                #[serde(default)]
                email: String,
                #[serde(default)]
                role: Option<String>,
            }
            let body = match serde_json::from_slice::<Body>(&req.payload) {
                Err(e) => bad_request(&e.to_string()),
                Ok(b) => {
                    manage_members(
                        &members_store,
                        verb,
                        owner,
                        project,
                        &b.email,
                        b.role.as_deref(),
                    )
                    .await
                }
            };
            req.respond(body).await;
        }
    });
    Ok(())
}

/// §7.5 project-role management, dispatcher-side (single writer of `users.*`).
/// `set` grants/updates a role, `remove` clears one, `list` returns the users
/// holding any role on the project. Role names accept `owner` as an alias for
/// `admin` (see `ProjectRole::parse`). 404 when the target user is missing.
async fn manage_members(
    store: &NatsStore,
    verb: &str,
    owner: &str,
    project: &str,
    email: &str,
    role: Option<&str>,
) -> Vec<u8> {
    if let Err(e) = store::keys::validate_subject_component(owner)
        .and_then(|()| store::keys::validate_subject_component(project))
    {
        return bad_request(&e.to_string());
    }
    let slug = format!("{owner}/{project}");
    let users = match store.raw_bucket(store::buckets::USERS).await {
        Ok(b) => b,
        Err(e) => return error_reply(&e.into()),
    };

    if verb == "list" {
        let keys = match users.keys_with_prefix("").await {
            Ok(k) => k,
            Err(e) => return error_reply(&e.into()),
        };
        let mut members = Vec::new();
        for key in keys {
            match users.get_json::<types::User>(&key).await {
                Ok(Some(user)) => {
                    if let Some(role) = user.project_roles.get(&slug) {
                        members.push(serde_json::json!({ "email": user.email, "role": role }));
                    }
                }
                Ok(None) => {}
                Err(e) => return error_reply(&e.into()),
            }
        }
        members.sort_by(|a, b| a["email"].as_str().cmp(&b["email"].as_str()));
        return ok_reply(&serde_json::json!({ "members": members }));
    }

    if email.is_empty() {
        return bad_request("payload must carry { email }");
    }
    let key = store::keys::user_key(email);
    let mut user = match users.get_json::<types::User>(&key).await {
        Ok(Some(u)) => u,
        Ok(None) => return NOT_FOUND.to_vec(),
        Err(e) => return error_reply(&e.into()),
    };
    match verb {
        "set" => {
            let Some(role) = role else {
                return bad_request("payload must carry { role }");
            };
            let Some(role) = types::ProjectRole::parse(role) else {
                return bad_request(&format!(
                    "invalid role {role:?} (expected owner|member|viewer)"
                ));
            };
            user.project_roles.insert(slug.clone(), role);
            if let Err(e) = users.put_json(&key, &user).await {
                return error_reply(&e.into());
            }
            ok_reply(&serde_json::json!({ "email": user.email, "project": slug, "role": role }))
        }
        "remove" => {
            user.project_roles.remove(&slug);
            if let Err(e) = users.put_json(&key, &user).await {
                return error_reply(&e.into());
            }
            ok_reply(&serde_json::json!({ "email": user.email, "project": slug }))
        }
        _ => bad_request("malformed subject"),
    }
}

/// §7.3 user SSH cert minting. Loads the caller's roles from their user record
/// — the roles map baked into the cert is the user's current grants, never a
/// client-supplied one — and signs a 24h cert with the CA key. 503 when the CA
/// key is not mounted; 404 when the user record is missing.
async fn sign_user_cert(
    store: &NatsStore,
    ssh_ca: Option<&std::path::Path>,
    email: &str,
    public_key: &str,
) -> Vec<u8> {
    let Some(ca_key) = ssh_ca else {
        return service_unavailable("ssh certificate authority not configured");
    };
    let users = match store.raw_bucket(store::buckets::USERS).await {
        Ok(b) => b,
        Err(e) => return error_reply(&e.into()),
    };
    let user: Option<types::User> = match users.get_json(&store::keys::user_key(email)).await {
        Ok(u) => u,
        Err(e) => return error_reply(&e.into()),
    };
    let Some(user) = user else {
        return NOT_FOUND.to_vec();
    };
    let ca = auth::ssh::SshCa::new(ca_key);
    match ca
        .sign_user_cert(
            public_key,
            &user.email,
            &user.project_roles,
            user.platform_admin,
            chrono::Duration::hours(24),
        )
        .await
    {
        Ok(certificate) => ok_reply(&serde_json::json!({ "certificate": certificate })),
        Err(e) => error_reply(&CoreError::Config(e.to_string())),
    }
}

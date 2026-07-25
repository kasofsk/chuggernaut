//! Platform JWTs (spec §7.1): RS256, claims carry the full `Identity`.
//! Issued on login (users) and at deploy time (dispatcher); verified by the
//! API middleware on every request with no external service call.

use crate::AuthError;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use types::{Identity, IdentityKind, ProjectRole};

pub const SESSION_COOKIE: &str = "session";

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    kind: IdentityKind,
    #[serde(default)]
    project_roles: HashMap<String, ProjectRole>,
    #[serde(default)]
    platform_admin: bool,
    iat: i64,
    exp: i64,
}

/// Signs platform JWTs with the RS256 private key (`jwt_private.pem`, §12.1).
pub struct JwtSigner {
    key: EncodingKey,
}

impl JwtSigner {
    pub fn from_pem(private_pem: &[u8]) -> Result<Self, AuthError> {
        let key = EncodingKey::from_rsa_pem(private_pem)
            .map_err(|e| AuthError::Internal(format!("jwt private key: {e}")))?;
        Ok(Self { key })
    }

    /// Issue a token for `identity`, valid for `ttl` from now.
    pub fn issue(&self, identity: &Identity, ttl: chrono::Duration) -> Result<String, AuthError> {
        let now = chrono::Utc::now();
        let claims = Claims {
            sub: identity.sub.clone(),
            kind: identity.kind,
            project_roles: identity.project_roles.clone(),
            platform_admin: identity.platform_admin,
            iat: now.timestamp(),
            exp: (now + ttl).timestamp(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.key)
            .map_err(|e| AuthError::Internal(format!("jwt encode: {e}")))
    }
}

/// Verifies platform JWTs with the RS256 public key (`jwt_public.pem`).
pub struct JwtVerifier {
    key: DecodingKey,
}

impl JwtVerifier {
    pub fn from_pem(public_pem: &[u8]) -> Result<Self, AuthError> {
        let key = DecodingKey::from_rsa_pem(public_pem)
            .map_err(|e| AuthError::Internal(format!("jwt public key: {e}")))?;
        Ok(Self { key })
    }

    pub fn verify(&self, token: &str) -> Result<Identity, AuthError> {
        let data =
            jsonwebtoken::decode::<Claims>(token, &self.key, &Validation::new(Algorithm::RS256))
                .map_err(|_| AuthError::Unauthenticated)?;
        let c = data.claims;
        Ok(Identity {
            sub: c.sub,
            kind: c.kind,
            project_roles: c.project_roles,
            platform_admin: c.platform_admin,
        })
    }
}

/// `Set-Cookie` value for a session token (§7.1: httpOnly, Secure,
/// SameSite=Strict).
pub fn session_cookie(token: &str, max_age: chrono::Duration) -> String {
    format!(
        "{SESSION_COOKIE}={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={}",
        max_age.num_seconds()
    )
}

/// Extract the session token from a `Cookie` request header.
pub fn token_from_cookie_header(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == SESSION_COOKIE).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    // Test-only RSA keypair generated once with:
    //   openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048
    const TEST_KEY: &str = include_str!("../testdata/jwt_test_key.pem");
    const TEST_PUB: &str = include_str!("../testdata/jwt_test_key.pub.pem");

    fn identity() -> Identity {
        Identity {
            sub: "david@example.com".into(),
            kind: IdentityKind::User,
            project_roles: HashMap::from([("acme/api".to_string(), ProjectRole::Member)]),
            platform_admin: false,
        }
    }

    #[test]
    fn issue_verify_round_trip() {
        let signer = JwtSigner::from_pem(TEST_KEY.as_bytes()).unwrap();
        let verifier = JwtVerifier::from_pem(TEST_PUB.as_bytes()).unwrap();
        let token = signer
            .issue(&identity(), chrono::Duration::hours(1))
            .unwrap();
        let got = verifier.verify(&token).unwrap();
        assert_eq!(got, identity());
    }

    #[test]
    fn expired_token_rejected() {
        let signer = JwtSigner::from_pem(TEST_KEY.as_bytes()).unwrap();
        let verifier = JwtVerifier::from_pem(TEST_PUB.as_bytes()).unwrap();
        let token = signer
            .issue(&identity(), chrono::Duration::hours(-2))
            .unwrap();
        assert!(matches!(
            verifier.verify(&token),
            Err(AuthError::Unauthenticated)
        ));
    }

    #[test]
    fn tampered_token_rejected() {
        let signer = JwtSigner::from_pem(TEST_KEY.as_bytes()).unwrap();
        let verifier = JwtVerifier::from_pem(TEST_PUB.as_bytes()).unwrap();
        let mut token = signer
            .issue(&identity(), chrono::Duration::hours(1))
            .unwrap();
        token.truncate(token.len() - 4);
        token.push_str("AAAA");
        assert!(verifier.verify(&token).is_err());
    }

    #[test]
    fn cookie_round_trip() {
        let cookie = session_cookie("tok123", chrono::Duration::hours(24));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        let header = format!("theme=dark; {}", cookie.split(';').next().unwrap());
        assert_eq!(token_from_cookie_header(&header), Some("tok123"));
        assert_eq!(token_from_cookie_header("theme=dark"), None);
    }
}

//! The OIDC issuer's identity: its key, its identifier, and the two documents
//! it publishes (design #313 A2/A4, spec §6.7, §12.1).
//!
//! Accepts the issuer's RSA public key (`oidc_public.pem`) as SPKI PEM and the
//! `OIDC_ISSUER` setting; emits the `kid` — the RFC 7638 JWK thumbprint that a
//! minted workload token and a published JWK both carry — plus the RFC 7517
//! JWK set and the discovery document served at §6.7's `.well-known` paths.
//! Guarantees: every derivation is pure and a function of the key bytes alone,
//! so the same key yields the same `kid` on every host and after every restart
//! and a JWKS consumer can recompute the id from the published JWK; the only
//! environment read is [`issuer_from_env`]. The thumbprint form is chosen over
//! a digest of the raw SubjectPublicKeyInfo because it is the one every JWKS
//! consumer already knows how to reproduce.

use crate::AuthError;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const B64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

const DER_INTEGER: u8 = 0x02;
const DER_BIT_STRING: u8 = 0x03;
const DER_SEQUENCE: u8 = 0x30;

const DER_LENGTH_BYTES_MAX: usize = 4;

const ISSUER_LEN_MAX: usize = 256;

/// The environment variable naming the issuer, read by every process that
/// mints or publishes over the issuer key.
pub const ISSUER_ENV: &str = "OIDC_ISSUER";

/// The platform's issuer identifier when `OIDC_ISSUER` is unset (design #313
/// D1) — an identifier a provider is registered with, not a URL anyone fetches.
pub const ISSUER_DEFAULT: &str = "https://chug.kasofsk.xyz";

/// The §6.7 path of the discovery document, relative to the issuer.
pub const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";

/// The §6.7 path of the JWK set, relative to the issuer.
pub const JWKS_PATH: &str = "/.well-known/jwks.json";

/// The issuer identifier from `OIDC_ISSUER`, else [`ISSUER_DEFAULT`]. This is
/// the one place the setting is read, so a token's `iss` and the published
/// document cannot disagree.
pub fn issuer_from_env() -> Result<String, AuthError> {
    resolve_issuer(std::env::var(ISSUER_ENV).ok())
}

/// The issuer for a configured value, validated: it is compared byte-for-byte
/// with a token's `iss` at a cloud STS, so a sloppy one fails there and not here.
fn resolve_issuer(configured: Option<String>) -> Result<String, AuthError> {
    let issuer = configured
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ISSUER_DEFAULT.to_string());
    let bad = |why: &str| {
        Err(AuthError::Internal(format!(
            "{ISSUER_ENV} {issuer:?}: {why}"
        )))
    };
    if !issuer.starts_with("https://") {
        return bad("must be an absolute https identifier");
    }
    if issuer.ends_with('/') {
        return bad("must not end in a slash");
    }
    if issuer.len() > ISSUER_LEN_MAX {
        return bad(&format!("longer than the {ISSUER_LEN_MAX}-byte bound"));
    }
    Ok(issuer)
}

/// One RFC 7517 JWK: the issuer's public key as a JWKS consumer receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub alg: String,
    #[serde(rename = "use")]
    pub key_use: String,
    pub kid: String,
    pub n: String,
    pub e: String,
}

/// An RFC 7517 JWK set — the document uploaded to a provider or served at
/// [`JWKS_PATH`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

/// The OIDC discovery document served at [`DISCOVERY_PATH`], naming the issuer
/// and where its keys live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryDocument {
    pub issuer: String,
    pub jwks_uri: String,
    pub response_types_supported: Vec<String>,
    pub subject_types_supported: Vec<String>,
    pub id_token_signing_alg_values_supported: Vec<String>,
}

/// The single-key JWK set for an SPKI PEM RSA public key, carrying the `kid`
/// [`kid_from_public_pem`] derives for that same key.
pub fn jwk_set_from_public_pem(public_pem: &str) -> Result<JwkSet, AuthError> {
    let (modulus, exponent) = rsa_public_parts(public_pem)?;
    let key = Jwk {
        kty: "RSA".into(),
        alg: "RS256".into(),
        key_use: "sig".into(),
        kid: kid_from_public_pem(public_pem)?,
        n: B64URL.encode(&modulus),
        e: B64URL.encode(&exponent),
    };
    assert!(!key.n.is_empty() && !key.e.is_empty(), "a JWK has n and e");
    Ok(JwkSet { keys: vec![key] })
}

/// The discovery document for a validated issuer; its `jwks_uri` is that
/// issuer's [`JWKS_PATH`], so the two documents cannot name different keys.
pub fn discovery_document(issuer: &str) -> DiscoveryDocument {
    assert!(
        issuer.starts_with("https://") && !issuer.ends_with('/'),
        "issuer is validated before a document names it"
    );
    DiscoveryDocument {
        issuer: issuer.to_string(),
        jwks_uri: format!("{issuer}{JWKS_PATH}"),
        response_types_supported: vec!["id_token".into()],
        subject_types_supported: vec!["public".into()],
        id_token_signing_alg_values_supported: vec!["RS256".into()],
    }
}

/// The `kid` (RFC 7517 §4.5) of an RSA public key in SPKI PEM form: its
/// RFC 7638 JWK thumbprint, base64url-encoded without padding.
pub fn kid_from_public_pem(public_pem: &str) -> Result<String, AuthError> {
    let (modulus, exponent) = rsa_public_parts(public_pem)?;
    let canonical = format!(
        r#"{{"e":"{}","kty":"RSA","n":"{}"}}"#,
        B64URL.encode(&exponent),
        B64URL.encode(&modulus)
    );
    let kid = B64URL.encode(Sha256::digest(canonical.as_bytes()));
    assert_eq!(kid.len(), 43, "a sha-256 thumbprint is 43 base64url chars");
    Ok(kid)
}

/// The modulus and exponent of an SPKI PEM RSA public key, each as the minimal
/// big-endian unsigned integer a JWK's `n` and `e` members encode.
fn rsa_public_parts(public_pem: &str) -> Result<(Vec<u8>, Vec<u8>), AuthError> {
    let der = pem_body(public_pem)?;
    let spki = der_field(&mut der.as_slice(), DER_SEQUENCE)?;
    let mut spki = spki.as_slice();
    der_field(&mut spki, DER_SEQUENCE)?;
    let bits = der_field(&mut spki, DER_BIT_STRING)?;
    let Some((0, key)) = bits.split_first().map(|(unused, rest)| (*unused, rest)) else {
        return Err(malformed("public key bit string"));
    };
    let rsa = der_field(&mut &key[..], DER_SEQUENCE)?;
    let mut rsa = rsa.as_slice();
    let modulus = der_field(&mut rsa, DER_INTEGER)?;
    let exponent = der_field(&mut rsa, DER_INTEGER)?;
    let (modulus, exponent) = (unsigned(&modulus), unsigned(&exponent));
    if modulus.is_empty() || exponent.is_empty() {
        return Err(malformed("empty RSA modulus or exponent"));
    }
    Ok((modulus.to_vec(), exponent.to_vec()))
}

/// The decoded body of a `PUBLIC KEY` PEM, armour and line breaks removed.
fn pem_body(public_pem: &str) -> Result<Vec<u8>, AuthError> {
    if !public_pem.contains("-----BEGIN PUBLIC KEY-----") {
        return Err(malformed("not a PUBLIC KEY PEM"));
    }
    let body: String = public_pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|e| malformed(&format!("base64 body: {e}")))
}

/// One DER tag-length-value of the expected tag, consumed from `input`.
fn der_field(input: &mut &[u8], tag: u8) -> Result<Vec<u8>, AuthError> {
    let (&found, rest) = input.split_first().ok_or_else(|| malformed("truncated"))?;
    if found != tag {
        return Err(malformed(&format!(
            "want tag {tag:#04x}, found {found:#04x}"
        )));
    }
    let (&first, rest) = rest
        .split_first()
        .ok_or_else(|| malformed("truncated length"))?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > DER_LENGTH_BYTES_MAX || rest.len() < count {
            return Err(malformed("unsupported length"));
        }
        let (raw, rest) = rest.split_at(count);
        let len = raw.iter().fold(0, |acc, b| (acc << 8) | usize::from(*b));
        (len, rest)
    };
    if rest.len() < len {
        return Err(malformed("truncated value"));
    }
    let (value, rest) = rest.split_at(len);
    *input = rest;
    Ok(value.to_vec())
}

/// A DER INTEGER's value with its sign padding stripped, as JWK integer
/// members are minimal and unsigned.
fn unsigned(der_integer: &[u8]) -> &[u8] {
    let start = der_integer
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(der_integer.len());
    &der_integer[start..]
}

fn malformed(what: &str) -> AuthError {
    AuthError::Internal(format!("oidc public key: {what}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    const RFC_7638_EXAMPLE: &str = include_str!("../testdata/rfc7638_example.pub.pem");
    const RFC_7638_EXAMPLE_KID: &str = "NzbLsXh8uDCcd-6MNwXF4W_7noWXFZAfHkxZsRGC9Xs";
    const OTHER_KEY: &str = include_str!("../testdata/jwt_test_key.pub.pem");

    #[test]
    fn kid_matches_the_rfc_7638_published_answer() {
        assert_eq!(
            kid_from_public_pem(RFC_7638_EXAMPLE).unwrap(),
            RFC_7638_EXAMPLE_KID
        );
    }

    #[test]
    fn kid_is_stable_and_separates_keys() {
        let kid = kid_from_public_pem(RFC_7638_EXAMPLE).unwrap();
        assert_eq!(kid, kid_from_public_pem(RFC_7638_EXAMPLE).unwrap());
        assert_eq!(
            kid,
            kid_from_public_pem(&RFC_7638_EXAMPLE.replace('\n', "\r\n")).unwrap()
        );
        assert_ne!(kid, kid_from_public_pem(OTHER_KEY).unwrap());
    }

    #[test]
    fn jwk_set_is_one_rs256_signing_key_under_s1s_kid() {
        let set = jwk_set_from_public_pem(RFC_7638_EXAMPLE).unwrap();
        assert_eq!(set.keys.len(), 1);
        let key = &set.keys[0];
        assert_eq!(key.kty, "RSA");
        assert_eq!(key.alg, "RS256");
        assert_eq!(key.key_use, "sig");
        assert_eq!(key.e, "AQAB");
        assert_eq!(key.kid, kid_from_public_pem(RFC_7638_EXAMPLE).unwrap());
        assert_eq!(B64URL.decode(&key.n).unwrap().len(), 256);
        assert_ne!(
            key.kid,
            jwk_set_from_public_pem(OTHER_KEY).unwrap().keys[0].kid
        );
    }

    #[test]
    fn jwk_set_serializes_under_rfc_7517_member_names() {
        let set = jwk_set_from_public_pem(RFC_7638_EXAMPLE).unwrap();
        let json = serde_json::to_value(&set).unwrap();
        assert_eq!(json["keys"][0]["use"], "sig");
        assert_eq!(json["keys"][0]["kty"], "RSA");
        assert!(json["keys"][0].get("key_use").is_none());
    }

    #[test]
    fn discovery_names_the_issuer_and_its_jwks_path() {
        let document = discovery_document("https://issuer.example");
        assert_eq!(document.issuer, "https://issuer.example");
        assert_eq!(
            document.jwks_uri,
            format!("https://issuer.example{JWKS_PATH}")
        );
        assert_eq!(document.id_token_signing_alg_values_supported, ["RS256"]);
    }

    #[test]
    fn issuer_is_configured_and_validated() {
        assert_eq!(resolve_issuer(None).unwrap(), ISSUER_DEFAULT);
        assert_eq!(resolve_issuer(Some(String::new())).unwrap(), ISSUER_DEFAULT);
        assert_eq!(
            resolve_issuer(Some("https://other.example".into())).unwrap(),
            "https://other.example"
        );
        assert!(resolve_issuer(Some("http://insecure.example".into())).is_err());
        assert!(resolve_issuer(Some("https://issuer.example/".into())).is_err());
        assert!(resolve_issuer(Some(format!("https://{}", "x".repeat(300)))).is_err());
    }

    #[test]
    fn malformed_input_is_an_error_not_a_kid() {
        assert!(kid_from_public_pem("not a pem at all").is_err());
        assert!(
            kid_from_public_pem("-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n")
                .is_err()
        );
        assert!(kid_from_public_pem(include_str!("../testdata/jwt_test_key.pem")).is_err());
    }
}

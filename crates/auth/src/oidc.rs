//! The OIDC issuer's key identity (design #313 A2, spec §12.1).
//!
//! Accepts the issuer's RSA public key (`oidc_public.pem`) as SPKI PEM; emits
//! its `kid` — the RFC 7638 JWK thumbprint that a minted workload token and a
//! published JWK both carry. Guarantees: pure, no I/O, and a function of the
//! key bytes alone, so the same key yields the same `kid` on every host and
//! after every restart and a JWKS consumer can recompute the id from the
//! published JWK. The thumbprint form is chosen over a digest of the raw
//! SubjectPublicKeyInfo because it is the one every JWKS consumer already
//! knows how to reproduce.

use crate::AuthError;
use base64::Engine;
use sha2::{Digest, Sha256};

const B64URL: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

const DER_INTEGER: u8 = 0x02;
const DER_BIT_STRING: u8 = 0x03;
const DER_SEQUENCE: u8 = 0x30;

const DER_LENGTH_BYTES_MAX: usize = 4;

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
    fn malformed_input_is_an_error_not_a_kid() {
        assert!(kid_from_public_pem("not a pem at all").is_err());
        assert!(
            kid_from_public_pem("-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----\n")
                .is_err()
        );
        assert!(kid_from_public_pem(include_str!("../testdata/jwt_test_key.pem")).is_err());
    }
}

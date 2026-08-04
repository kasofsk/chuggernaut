//! Workload-token claim assembly and minting (design #313 A1/A3/A6, spec §7.4).
//!
//! - **Accepts:** a [`WorkloadTokenRequest`] — the typed identity of one
//!   container launch — plus the issuer keypair (`oidc_private.pem` /
//!   `oidc_public.pem`, §12.1) and the instant the token is issued at.
//! - **Emits:** a [`MintedWorkloadToken`]: the RS256 token a container presents
//!   to a cloud STS, beside the [`WorkloadTokenAudit`] §10.3 records *in place
//!   of* it. Refusals are [`WorkloadTokenError`], one variant per rule.
//! - **Guarantees:** pure and synchronous — no I/O, no clock read (`now` is an
//!   argument), no logging path, and no `Debug`/`Display`/`Serialize` route to
//!   the token value (§10.2). Exactly one audience is representable, the
//!   `sub` and TTL bounds are hard errors rather than truncations, and the
//!   claim set is assembled from typed fields only, so free text cannot enter a
//!   claim by accident (#311 Decision 5).
//! - **Spec:** §7.4 (per-task credential lifetime), §10.2/§10.3 (the token is
//!   never recorded, its identity is), §4.1 (the `container` claim mirrors the
//!   `CHUG_EVALUATOR` stamp).
//!
//! It lives in `auth` rather than `chuggernaut-domain` because a mint is a
//! credential construction, not a lifecycle decision: it needs the issuer key
//! and `jsonwebtoken`, which the pure-core crate resolves neither of by
//! construction, and the `kid` it stamps comes from [`crate::oidc`] next door.
//! The decision this belongs beside — *which* identities a container is granted
//! — stays on the decider side (design #313 slices S3/S4).

use crate::oidc::kid_from_public_pem;
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;
use thiserror::Error;
use types::inputs::{INPUT_VALUE_LEN_MAX, InputValueError, check_value_charset};

/// The provider's cap on the mapped `google.subject`, verified in #313 A1.
/// Bytes, which is also characters — every component clears an ASCII-only
/// charset first.
pub const SUBJECT_BYTES_MAX: usize = 127;

/// The provider's cap on one audience (#313 A3), which is exactly the
/// [`INPUT_VALUE_LEN_MAX`] floor every claim value already clears.
pub const AUDIENCE_CHARS_MAX: usize = INPUT_VALUE_LEN_MAX;

/// The default `oidc_token_ttl_secs_max` (#313 A3): one hour, an order of
/// magnitude under the provider's ceiling.
pub const TOKEN_TTL_SECS_MAX_DEFAULT: u64 = 3600;

/// The provider's hard `exp − iat` ceiling of 24 hours (#313 A3). A cap above
/// it is a misconfiguration, so it is refused rather than clamped.
pub const PROVIDER_TTL_SECS_MAX: u64 = 24 * 60 * 60;

/// The characters the composite `sub` and `workload` claims join on, and
/// therefore the ones no component of either may contain.
const COMPOSITE_SEPARATORS: [char; 2] = [':', '/'];

/// The container a token is minted for — the per-container scoping boundary
/// (#313 A5), mirroring the `CHUG_EVALUATOR` stamp (§4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkloadContainer {
    Work,
    Evaluator { name: String },
    WrapUp,
}

impl WorkloadContainer {
    /// The `container` claim: `work`, `eval:{name}` or `wrap_up`.
    #[must_use]
    pub fn claim(&self) -> String {
        match self {
            Self::Work => "work".to_string(),
            Self::Evaluator { name } => format!("eval:{name}"),
            Self::WrapUp => "wrap_up".to_string(),
        }
    }

    /// The `phase` claim — diagnostic, and redundant with `container`.
    #[must_use]
    pub fn phase(&self) -> &'static str {
        match self {
            Self::Work => "Work",
            Self::Evaluator { .. } => "Evaluation",
            Self::WrapUp => "WrapUp",
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Self::Evaluator { name } => Some(name),
            Self::Work | Self::WrapUp => None,
        }
    }
}

/// Everything one workload token identifies, as typed fields. Nothing mutable
/// within a job and nothing free-text is representable here — no branch, no
/// sha, no description (#313 A1).
#[derive(Debug, Clone)]
pub struct WorkloadTokenRequest {
    pub owner: String,
    pub project: String,
    pub job_type: String,
    pub container: WorkloadContainer,
    pub job_seq: u64,
    pub task_id: u64,
    /// The one audience this token is valid at — the full provider resource
    /// name. One per token, never a list (#313 A3).
    pub audience: String,
    /// The job's resolved `task_timeout`, the same value §7.4's two existing
    /// per-task credentials are minted for.
    pub task_timeout_secs: u64,
    /// The identity's own `token_ttl_secs`, capping the above; defaults to
    /// [`TOKEN_TTL_SECS_MAX_DEFAULT`].
    pub token_ttl_secs_max: Option<u64>,
}

/// Why a workload token was refused. One variant per rule, so a caller's
/// diagnostic names the rule rather than restating the request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkloadTokenError {
    #[error("workload token {field}: {source}")]
    Field {
        field: &'static str,
        source: InputValueError,
    },
    #[error(
        "workload token {field} {value:?} contains ':' or '/', which the composite claims join on"
    )]
    FieldSeparator { field: &'static str, value: String },
    #[error(
        "workload token subject {subject:?} is {len} bytes, over the provider's {max}-byte google.subject limit",
        max = SUBJECT_BYTES_MAX
    )]
    SubjectTooLong { subject: String, len: usize },
    #[error(
        "workload token ttl cap of {secs}s is over the provider's {max}s exp-iat ceiling",
        max = PROVIDER_TTL_SECS_MAX
    )]
    TtlCapOverCeiling { secs: u64 },
    #[error("workload token ttl resolves to zero seconds")]
    TtlZero,
    #[error("the workload issuer keypair is unusable: {reason}")]
    Key { reason: String },
    #[error("signing the workload token failed: {reason}")]
    Sign { reason: String },
}

/// A token's `jti`: the audit join key that makes a replay attributable
/// (#313 A6), one per mint and never reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenId(String);

impl TokenId {
    /// A fresh UUIDv4, kept in its own type so claim assembly stays a pure
    /// function of its arguments.
    #[must_use]
    pub fn random() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a task record and `task-started` carry in the token's place (#313 A6).
/// The `identity` field — which declaration was honored — is the caller's, since
/// a declared name resolves *to* a request rather than out of one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkloadTokenAudit {
    pub audience: String,
    pub sub: String,
    pub workload: String,
    pub jti: String,
    pub expires_at: DateTime<Utc>,
}

/// The claim set one workload token carries (#313 A1). `aud` is one string, so
/// a multi-audience token is unrepresentable rather than merely unbuilt.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadClaims {
    iss: String,
    sub: String,
    aud: String,
    iat: i64,
    exp: i64,
    jti: String,
    project: String,
    job_type: String,
    container: String,
    workload: String,
    job_seq: u64,
    task_id: u64,
    phase: &'static str,
    /// `exp` as the instant the audit row records; not a claim, so it is never
    /// serialized into the token.
    #[serde(skip)]
    expires_at: DateTime<Utc>,
}

impl WorkloadClaims {
    /// Assemble one token's claims — pure and deterministic in its arguments,
    /// so both the clock and the `jti` are the caller's to supply.
    pub fn assemble(
        issuer: &str,
        request: &WorkloadTokenRequest,
        jti: &TokenId,
        now: DateTime<Utc>,
    ) -> Result<Self, WorkloadTokenError> {
        check_field("issuer", issuer)?;
        check_field("audience", &request.audience)?;
        check_component("owner", &request.owner)?;
        check_component("project", &request.project)?;
        check_component("job_type", &request.job_type)?;
        if let Some(name) = request.container.name() {
            check_component("evaluator", name)?;
        }
        let project = format!("{}/{}", request.owner, request.project);
        let sub = format!("project:{project}:type:{}", request.job_type);
        if sub.len() > SUBJECT_BYTES_MAX {
            return Err(WorkloadTokenError::SubjectTooLong {
                len: sub.len(),
                subject: sub,
            });
        }
        let container = request.container.claim();
        let workload = format!("{project}:{}:{container}", request.job_type);
        let ttl_secs = resolve_ttl_secs(request)?;
        let expires_at = now + chrono::TimeDelta::seconds(ttl_secs as i64);
        let (iat, exp) = (now.timestamp(), expires_at.timestamp());
        assert!(exp > iat, "a workload token expires after it is issued");
        assert!(
            exp.saturating_sub(iat) <= PROVIDER_TTL_SECS_MAX as i64,
            "a workload token lives at most {PROVIDER_TTL_SECS_MAX}s"
        );
        Ok(Self {
            iss: issuer.to_string(),
            sub,
            aud: request.audience.clone(),
            iat,
            exp,
            jti: jti.as_str().to_string(),
            project,
            job_type: request.job_type.clone(),
            container,
            workload,
            job_seq: request.job_seq,
            task_id: request.task_id,
            phase: request.container.phase(),
            expires_at,
        })
    }

    /// The audit row this claim set answers for (#313 A6).
    #[must_use]
    pub fn audit(&self) -> WorkloadTokenAudit {
        WorkloadTokenAudit {
            audience: self.aud.clone(),
            sub: self.sub.clone(),
            workload: self.workload.clone(),
            jti: self.jti.clone(),
            expires_at: self.expires_at,
        }
    }
}

/// The TTL one token is minted with: `min(resolved task_timeout, cap)`, the
/// same rule §7.4's two existing per-task credentials follow (#313 A3).
fn resolve_ttl_secs(request: &WorkloadTokenRequest) -> Result<u64, WorkloadTokenError> {
    let cap = request
        .token_ttl_secs_max
        .unwrap_or(TOKEN_TTL_SECS_MAX_DEFAULT);
    if cap > PROVIDER_TTL_SECS_MAX {
        return Err(WorkloadTokenError::TtlCapOverCeiling { secs: cap });
    }
    let ttl_secs = request.task_timeout_secs.min(cap);
    if ttl_secs == 0 {
        return Err(WorkloadTokenError::TtlZero);
    }
    assert!(ttl_secs <= cap, "a minted ttl never exceeds its cap");
    assert!(
        ttl_secs <= PROVIDER_TTL_SECS_MAX,
        "a minted ttl stays inside the provider's ceiling"
    );
    Ok(ttl_secs)
}

/// The floor every claim value clears: #311 Decision 5's identifier charset, so
/// no whitespace, quote or shell metacharacter reaches a CEL expression.
fn check_field(field: &'static str, value: &str) -> Result<(), WorkloadTokenError> {
    check_value_charset(value).map_err(|source| WorkloadTokenError::Field { field, source })
}

/// A component of the composite `sub` and `workload` claims: the identifier
/// floor, plus the separators those composites join on.
fn check_component(field: &'static str, value: &str) -> Result<(), WorkloadTokenError> {
    check_field(field, value)?;
    if value.contains(COMPOSITE_SEPARATORS) {
        return Err(WorkloadTokenError::FieldSeparator {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// One minted token, beside the audit row recorded in its place. The token has
/// no `Debug`, `Display` or `Serialize` route out — only [`Self::token`].
pub struct MintedWorkloadToken {
    token: String,
    audit: WorkloadTokenAudit,
}

impl MintedWorkloadToken {
    /// The signed token, borrowed — the sole path to the credential (§10.2).
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub fn audit(&self) -> &WorkloadTokenAudit {
        &self.audit
    }
}

impl std::fmt::Debug for MintedWorkloadToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedWorkloadToken")
            .field("audit", &self.audit)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Signs workload tokens with the issuer keypair, stamping the S1 `kid`
/// (#313 A2) in every header. RS256, per §7.1 and every WIF implementation.
pub struct WorkloadTokenSigner {
    issuer: String,
    kid: String,
    header: Header,
    key: EncodingKey,
}

impl WorkloadTokenSigner {
    /// Build a signer from the issuer keypair and the `iss` every token
    /// carries; the `kid` is the public half's RFC 7638 thumbprint.
    pub fn new(
        private_pem: &[u8],
        public_pem: &str,
        issuer: impl Into<String>,
    ) -> Result<Self, WorkloadTokenError> {
        let kid = kid_from_public_pem(public_pem).map_err(|e| WorkloadTokenError::Key {
            reason: e.to_string(),
        })?;
        let key = EncodingKey::from_rsa_pem(private_pem).map_err(|e| WorkloadTokenError::Key {
            reason: format!("oidc private key: {e}"),
        })?;
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.clone());
        Ok(Self {
            issuer: issuer.into(),
            kid,
            header,
            key,
        })
    }

    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// Mint one token for one container launch: a fresh `jti`, the assembled
    /// claims, and the RS256 signature over them.
    pub fn mint(
        &self,
        request: &WorkloadTokenRequest,
        now: DateTime<Utc>,
    ) -> Result<MintedWorkloadToken, WorkloadTokenError> {
        let jti = TokenId::random();
        let claims = WorkloadClaims::assemble(&self.issuer, request, &jti, now)?;
        let audit = claims.audit();
        assert_eq!(audit.jti, jti.as_str(), "a mint records the jti it signed");
        let token = jsonwebtoken::encode(&self.header, &claims, &self.key).map_err(|e| {
            WorkloadTokenError::Sign {
                reason: e.to_string(),
            }
        })?;
        assert!(!token.is_empty(), "a minted token is never empty");
        Ok(MintedWorkloadToken { token, audit })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use jsonwebtoken::{DecodingKey, Validation};
    use serde_json::Value;

    const TEST_KEY: &str = include_str!("../testdata/jwt_test_key.pem");
    const TEST_PUB: &str = include_str!("../testdata/jwt_test_key.pub.pem");
    const ISSUER: &str = "https://chug.kasofsk.xyz";
    const AUDIENCE: &str = "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/chug/providers/chuggernaut";

    fn request(container: WorkloadContainer) -> WorkloadTokenRequest {
        WorkloadTokenRequest {
            owner: "kasofsk".into(),
            project: "beacon".into(),
            job_type: "deploy".into(),
            container,
            job_seq: 4211,
            task_id: 4213,
            audience: AUDIENCE.into(),
            task_timeout_secs: 1800,
            token_ttl_secs_max: None,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_775_000_000, 0).unwrap()
    }

    fn claims(container: WorkloadContainer) -> WorkloadClaims {
        WorkloadClaims::assemble(ISSUER, &request(container), &TokenId("j".into()), now()).unwrap()
    }

    fn signer() -> WorkloadTokenSigner {
        WorkloadTokenSigner::new(TEST_KEY.as_bytes(), TEST_PUB, ISSUER).unwrap()
    }

    fn json(claims: &WorkloadClaims) -> Value {
        serde_json::to_value(claims).unwrap()
    }

    #[test]
    fn subject_is_the_short_stable_policy_identity() {
        let c = claims(WorkloadContainer::Work);
        assert_eq!(c.sub, "project:kasofsk/beacon:type:deploy");
        assert_eq!(c.sub.len(), 34);
        assert_eq!(c.iss, ISSUER);
        assert_eq!(c.project, "kasofsk/beacon");
        assert_eq!(c.job_seq, 4211);
        assert_eq!(c.task_id, 4213);
    }

    #[test]
    fn claims_per_container_kind_with_a_matching_composite() {
        let cases = [
            (WorkloadContainer::Work, "work", "Work"),
            (
                WorkloadContainer::Evaluator {
                    name: "health".into(),
                },
                "eval:health",
                "Evaluation",
            ),
            (WorkloadContainer::WrapUp, "wrap_up", "WrapUp"),
        ];
        for (container, expected, phase) in cases {
            let c = claims(container);
            assert_eq!(c.container, expected);
            assert_eq!(c.phase, phase);
            assert_eq!(c.workload, format!("kasofsk/beacon:deploy:{expected}"));
            assert_eq!(
                c.workload,
                format!("{}:{}:{}", c.project, c.job_type, c.container),
                "the composite is exactly its three components"
            );
        }
    }

    #[test]
    fn a_subject_over_the_provider_bound_is_refused_not_truncated() {
        let mut request = request(WorkloadContainer::Work);
        request.owner = "o".repeat(60);
        request.project = "p".repeat(60);
        let err =
            WorkloadClaims::assemble(ISSUER, &request, &TokenId("j".into()), now()).unwrap_err();
        let WorkloadTokenError::SubjectTooLong { subject, len } = &err else {
            panic!("want SubjectTooLong, got {err:?}");
        };
        assert_eq!(*len, subject.len());
        assert!(*len > SUBJECT_BYTES_MAX);
        assert!(err.to_string().contains("127"));
    }

    #[test]
    fn a_subject_at_the_bound_is_accepted_and_one_byte_over_is_not() {
        let fixed = "project:/:type:deploy".len() + "p".len();
        let mut request = request(WorkloadContainer::Work);
        request.owner = "o".repeat(SUBJECT_BYTES_MAX - fixed);
        request.project = "p".into();
        let c = WorkloadClaims::assemble(ISSUER, &request, &TokenId("j".into()), now()).unwrap();
        assert_eq!(c.sub.len(), SUBJECT_BYTES_MAX);
        request.owner.push('o');
        let err =
            WorkloadClaims::assemble(ISSUER, &request, &TokenId("j".into()), now()).unwrap_err();
        assert!(matches!(err, WorkloadTokenError::SubjectTooLong { len, .. } if len == 128));
    }

    #[test]
    fn ttl_is_the_task_timeout_under_the_cap_and_the_cap_over_it() {
        let cases = [
            (60, None, 60),
            (TOKEN_TTL_SECS_MAX_DEFAULT, None, TOKEN_TTL_SECS_MAX_DEFAULT),
            (12 * 3600, None, TOKEN_TTL_SECS_MAX_DEFAULT),
            (u64::MAX, None, TOKEN_TTL_SECS_MAX_DEFAULT),
            (1800, Some(300), 300),
            (120, Some(300), 120),
            (u64::MAX, Some(PROVIDER_TTL_SECS_MAX), PROVIDER_TTL_SECS_MAX),
        ];
        for (task_timeout_secs, token_ttl_secs_max, expected) in cases {
            let request = WorkloadTokenRequest {
                task_timeout_secs,
                token_ttl_secs_max,
                ..request(WorkloadContainer::Work)
            };
            let c =
                WorkloadClaims::assemble(ISSUER, &request, &TokenId("j".into()), now()).unwrap();
            assert_eq!(c.exp - c.iat, expected as i64, "{task_timeout_secs:?}");
            assert!(
                c.exp - c.iat <= PROVIDER_TTL_SECS_MAX as i64,
                "never over the provider's 24h ceiling"
            );
        }
    }

    #[test]
    fn a_ttl_cap_over_the_provider_ceiling_or_of_zero_is_refused() {
        let over = WorkloadTokenRequest {
            token_ttl_secs_max: Some(PROVIDER_TTL_SECS_MAX + 1),
            ..request(WorkloadContainer::Work)
        };
        let err = WorkloadClaims::assemble(ISSUER, &over, &TokenId("j".into()), now()).unwrap_err();
        assert_eq!(
            err,
            WorkloadTokenError::TtlCapOverCeiling {
                secs: PROVIDER_TTL_SECS_MAX + 1
            }
        );
        for zero in [
            WorkloadTokenRequest {
                task_timeout_secs: 0,
                ..request(WorkloadContainer::Work)
            },
            WorkloadTokenRequest {
                token_ttl_secs_max: Some(0),
                ..request(WorkloadContainer::Work)
            },
        ] {
            let err =
                WorkloadClaims::assemble(ISSUER, &zero, &TokenId("j".into()), now()).unwrap_err();
            assert_eq!(err, WorkloadTokenError::TtlZero);
        }
    }

    #[test]
    fn free_text_never_enters_a_claim() {
        let cases = [
            ("job_type", "deploy && rm -rf /"),
            ("owner", "kas ofsk"),
            ("project", "beacon'"),
            ("evaluator", "health\"x"),
            ("audience", "//iam.googleapis.com/ providers"),
        ];
        for (field, value) in cases {
            let mut request = request(WorkloadContainer::Evaluator {
                name: "health".into(),
            });
            match field {
                "job_type" => request.job_type = value.into(),
                "owner" => request.owner = value.into(),
                "project" => request.project = value.into(),
                "audience" => request.audience = value.into(),
                _ => {
                    request.container = WorkloadContainer::Evaluator { name: value.into() };
                }
            }
            let err = WorkloadClaims::assemble(ISSUER, &request, &TokenId("j".into()), now())
                .unwrap_err();
            assert!(
                matches!(&err, WorkloadTokenError::Field { field: f, .. } if *f == field),
                "want a charset refusal for {field}, got {err:?}"
            );
        }
    }

    #[test]
    fn a_component_may_not_carry_a_composite_separator() {
        let mut request = request(WorkloadContainer::Work);
        request.job_type = "deploy:work".into();
        let err =
            WorkloadClaims::assemble(ISSUER, &request, &TokenId("j".into()), now()).unwrap_err();
        assert!(
            matches!(
                err,
                WorkloadTokenError::FieldSeparator {
                    field: "job_type",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn the_claim_key_set_is_golden() {
        let value = json(&claims(WorkloadContainer::Work));
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "aud",
                "container",
                "exp",
                "iat",
                "iss",
                "job_seq",
                "job_type",
                "jti",
                "phase",
                "project",
                "sub",
                "task_id",
                "workload",
            ],
            "a new claim is a policy surface — add it deliberately"
        );
    }

    #[test]
    fn exactly_one_audience_per_token() {
        let value = json(&claims(WorkloadContainer::Work));
        assert_eq!(value["aud"], Value::String(AUDIENCE.to_string()));
        assert!(!value["aud"].is_array(), "aud is never a list");
    }

    #[test]
    fn jti_differs_between_two_mints_of_identical_inputs() {
        let signer = signer();
        let request = request(WorkloadContainer::Work);
        let first = signer.mint(&request, now()).unwrap();
        let second = signer.mint(&request, now()).unwrap();
        assert_ne!(first.audit().jti, second.audit().jti);
        assert_ne!(first.token(), second.token());
        assert_eq!(first.audit().sub, second.audit().sub);
        assert_eq!(first.audit().expires_at, second.audit().expires_at);
    }

    #[test]
    fn a_minted_token_verifies_against_the_public_key_and_carries_the_kid() {
        let signer = signer();
        let minted = signer
            .mint(&request(WorkloadContainer::Work), Utc::now())
            .unwrap();
        assert_eq!(
            jsonwebtoken::decode_header(minted.token()).unwrap().kid,
            Some(kid_from_public_pem(TEST_PUB).unwrap())
        );
        assert_eq!(signer.kid(), kid_from_public_pem(TEST_PUB).unwrap());
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[AUDIENCE]);
        validation.set_issuer(&[ISSUER]);
        let decoded = jsonwebtoken::decode::<Value>(
            minted.token(),
            &DecodingKey::from_rsa_pem(TEST_PUB.as_bytes()).unwrap(),
            &validation,
        )
        .unwrap();
        assert_eq!(decoded.header.alg, Algorithm::RS256);
        assert_eq!(decoded.claims["workload"], "kasofsk/beacon:deploy:work");
        assert_eq!(decoded.claims["jti"], minted.audit().jti);
    }

    #[test]
    fn a_token_signed_by_another_key_does_not_verify() {
        let minted = signer()
            .mint(&request(WorkloadContainer::Work), Utc::now())
            .unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[AUDIENCE]);
        assert!(
            jsonwebtoken::decode::<Value>(
                minted.token(),
                &DecodingKey::from_rsa_pem(
                    include_str!("../testdata/rfc7638_example.pub.pem").as_bytes()
                )
                .unwrap(),
                &validation,
            )
            .is_err()
        );
    }

    #[test]
    fn the_token_value_has_no_debug_or_serialize_route_out() {
        let minted = signer()
            .mint(&request(WorkloadContainer::Work), now())
            .unwrap();
        let rendered = format!("{minted:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(minted.token()));
        let audit = serde_json::to_string(minted.audit()).unwrap();
        assert!(!audit.contains(minted.token()));
        assert!(audit.contains("kasofsk/beacon:deploy:work"));
    }

    #[test]
    fn the_audience_bound_is_the_provider_bound() {
        assert_eq!(AUDIENCE_CHARS_MAX, 256);
        let mut request = request(WorkloadContainer::Work);
        request.audience = "a".repeat(AUDIENCE_CHARS_MAX + 1);
        let err =
            WorkloadClaims::assemble(ISSUER, &request, &TokenId("j".into()), now()).unwrap_err();
        assert!(
            matches!(
                err,
                WorkloadTokenError::Field {
                    field: "audience",
                    source: InputValueError::TooLong { .. }
                }
            ),
            "{err:?}"
        );
    }
}

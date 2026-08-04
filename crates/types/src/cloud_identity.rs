//! Cloud identity records — the coordinates a `workload_identities:` name
//! resolves to (spec §8.3, design #313 A5).
//!
//! Operator data, not secret data: an audience and the service account a
//! minted workload token impersonates. Stored plaintext beside vars, in its
//! own KV namespace, so a cloud identity is never expressible as a secret and
//! can never ride the reserved `global/agents` grant (§8.2).

use serde::{Deserialize, Serialize};

/// One `cloud-identities.{owner}.{project}.{name}` record (design #313 A5).
/// `service_account` is populated rather than optional — the exchanged token
/// impersonates a service account (#313 D4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudIdentity {
    /// The provider audience the token is valid at, e.g.
    /// `//iam.googleapis.com/projects/{n}/locations/global/workloadIdentityPools/{pool}/providers/{p}`.
    pub audience: String,
    /// The service account the exchanged token impersonates.
    pub service_account: String,
    /// Optional per-identity cap on the minted token's lifetime, below the
    /// platform default (design #313 A3). Unset means the platform bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_ttl_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// A record round-trips through JSON as stored, and an unknown key is a
    /// hard error — the record is a contract, not a bag.
    #[test]
    fn record_round_trips_and_refuses_unknown_keys() {
        let record = CloudIdentity {
            audience: "//iam.googleapis.com/projects/1/providers/chuggernaut".into(),
            service_account: "deployer@example.iam.gserviceaccount.com".into(),
            token_ttl_secs: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("token_ttl_secs"), "{json}");
        assert_eq!(
            serde_json::from_str::<CloudIdentity>(&json).unwrap(),
            record
        );
        assert!(
            serde_json::from_str::<CloudIdentity>(
                r#"{"audience":"a","service_account":"b","secret":"c"}"#
            )
            .is_err()
        );
    }
}

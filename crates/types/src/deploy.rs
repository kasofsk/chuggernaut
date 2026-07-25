//! Structured deploy legs (ticket #187).
//!
//! A deploy is a checklist, not a conversation: the same legs run every time, so
//! a deploy job should carry a typed record of each leg instead of one opaque
//! log. `deploy/prod/update.sh` emits one machine-readable line per leg to
//! stdout — `@chug:leg {"name":"build-dispatcher","status":"ok","secs":41}` —
//! plus a single `@chug:report {…}` envelope; `tasks/deploy.sh` passes stdout
//! through unchanged, and the dispatcher harvests those lines (crate
//! `dispatcher`'s `harvest`) from the command work task's captured logs into a
//! [`DeployReport`] on the task's structured result.
//!
//! Pure data: the harvest itself reads container logs and so lives in the
//! dispatcher; the shape and the marker strings live here so every consumer —
//! the harvest, the api, the UI — shares one definition.

use serde::{Deserialize, Serialize};

/// Stdout marker prefixing one leg line: `@chug:leg {json}`.
pub const LEG_MARKER: &str = "@chug:leg";

/// Stdout marker prefixing the single deploy-envelope line:
/// `@chug:report {json}` — carrying `from_sha`/`to_sha`/`rollback`/`health`.
pub const REPORT_MARKER: &str = "@chug:report";

/// Status of one deploy leg. An unknown status fails to deserialize, so a
/// malformed leg line is dropped by the harvest rather than corrupting the
/// report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LegStatus {
    /// The leg completed successfully.
    Ok,
    /// The leg ran and failed; [`DeployLeg::error`] carries a short reason, and
    /// every subsequent leg reports [`LegStatus::Skipped`].
    Failed,
    /// The leg never ran because an earlier leg failed.
    Skipped,
}

/// One deploy leg's typed record (ticket #187). The legs are the fixed steps of
/// `update.sh`: `build-dispatcher`, `build-images`, `web-publish`,
/// `worker-refresh:{node}` (one per node), `init`, `ssh-front`,
/// `restart-verify`, `sha-advance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployLeg {
    /// Leg name — one of the fixed step names above.
    pub name: String,
    /// Whether the leg succeeded, failed, or was skipped after an earlier
    /// failure.
    pub status: LegStatus,
    /// Wall-clock seconds the leg took, when measured. Absent for a skipped leg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secs: Option<u64>,
    /// A short failure reason, present only on [`LegStatus::Failed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A bounded tail of the underlying failure output (e.g. the worker
    /// `worker-refresh.sh` stderr tail), present only on [`LegStatus::Failed`]
    /// when the leg could capture it. `error` stays the one-line summary;
    /// `detail` carries the real text so the structured result and the
    /// escalation prompt show what actually broke (deploy #212). Bounded by the
    /// emitter (`update.sh` caps it) so a huge build log cannot bloat the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The structured result the dispatcher builds from a command work task's
/// harvested `@chug:leg`/`@chug:report` lines (ticket #187). Generic to command
/// work — any job type could emit legs — but a deploy is the consumer that
/// matters: the envelope (`from_sha`/`to_sha`/`rollback`/`health`) frames the
/// leg list. Every field defaults, so a report built from legs alone round-trips
/// and a record written before a given field existed still deserializes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployReport {
    /// SHA the deploy started from (the previously-deployed SHA), if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_sha: Option<String>,
    /// SHA the deploy targeted, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_sha: Option<String>,
    /// Whether the deploy rolled back to the previous binary — restart-verify's
    /// health check failed and prod was restored to `from_sha`.
    #[serde(default)]
    pub rollback: bool,
    /// Post-restart health verdict, if reported (e.g. `"ok"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    /// The legs, in emission order.
    #[serde(default)]
    pub legs: Vec<DeployLeg>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// A full report — envelope plus a mix of ok/failed/skipped legs —
    /// round-trips, and a `secs`-less skipped leg omits the field.
    #[test]
    fn deploy_report_round_trips() {
        let report = DeployReport {
            from_sha: Some("aaa111".into()),
            to_sha: Some("bbb222".into()),
            rollback: true,
            health: Some("ok".into()),
            legs: vec![
                DeployLeg {
                    name: "build-dispatcher".into(),
                    status: LegStatus::Ok,
                    secs: Some(41),
                    error: None,
                    detail: None,
                },
                DeployLeg {
                    name: "worker-refresh:air".into(),
                    status: LegStatus::Failed,
                    secs: Some(31),
                    error: Some("refresh not confirmed".into()),
                    detail: Some("docker: no space left on device".into()),
                },
                DeployLeg {
                    name: "restart-verify".into(),
                    status: LegStatus::Failed,
                    secs: Some(12),
                    error: Some("health check timed out".into()),
                    detail: None,
                },
                DeployLeg {
                    name: "sha-advance".into(),
                    status: LegStatus::Skipped,
                    secs: None,
                    error: None,
                    detail: None,
                },
            ],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert_eq!(serde_json::from_str::<DeployReport>(&json).unwrap(), report);
        // A skipped leg carries neither secs nor error on the wire.
        assert!(json.contains(r#""status":"skipped"#));
        assert!(!json.contains(r#""name":"sha-advance","status":"skipped","secs"#));
        // A failed leg carries its detail tail alongside the one-line error.
        assert!(json.contains(r#""detail":"docker: no space left on device""#));
    }

    /// A legs-only report (the generic harvest's output) round-trips, and the
    /// absent envelope fields deserialize to their defaults.
    #[test]
    fn legs_only_report_defaults_the_envelope() {
        let report = DeployReport {
            legs: vec![DeployLeg {
                name: "build-images".into(),
                status: LegStatus::Ok,
                secs: Some(7),
                error: None,
                detail: None,
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: DeployReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
        assert_eq!(back.from_sha, None);
        assert!(!back.rollback);
    }

    /// A leg record written before `error`/`secs`/`detail` existed still decodes
    /// — `detail` defaults to `None`, so an old deploy record stays readable.
    #[test]
    fn minimal_leg_deserializes() {
        let leg: DeployLeg = serde_json::from_str(r#"{"name":"init","status":"ok"}"#).unwrap();
        assert_eq!(leg.status, LegStatus::Ok);
        assert_eq!(leg.secs, None);
        assert_eq!(leg.error, None);
        assert_eq!(leg.detail, None);
    }

    /// A failed leg carrying both a one-line `error` and a longer `detail` tail
    /// round-trips (the deploy #212 shape: summary + real failure text).
    #[test]
    fn failed_leg_with_detail_round_trips() {
        let leg = DeployLeg {
            name: "worker-refresh:air".into(),
            status: LegStatus::Failed,
            secs: Some(31),
            error: Some("refresh not confirmed".into()),
            detail: Some("build: docker: no space left on device (~11G free)".into()),
        };
        let json = serde_json::to_string(&leg).unwrap();
        assert_eq!(serde_json::from_str::<DeployLeg>(&json).unwrap(), leg);
    }
}

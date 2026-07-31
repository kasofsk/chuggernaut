//! Step reporting over `req.step.report.*` (spec §4.5): bounded retry,
//! non-fatal on failure — a lost report degrades observability, never the loop.

use crate::config::HarnessConfig;
use types::StepRecord;

/// Report a step transition. TODO: publish `record` via
/// `store::NatsStore::request_with_retry` on
/// `store::subjects::step_report(...)`; log and swallow errors.
pub async fn report(_cfg: &HarnessConfig, _record: &StepRecord) {}

//! Ingest event envelope (spec §13.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Envelope the API layer wraps around every accepted ingest payload before
/// publishing to `ingest.{owner}.{project}.{source}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestEvent {
    pub source: String,
    pub received_at: DateTime<Utc>,
    /// The POST body, verbatim.
    pub payload: serde_json::Value,
}

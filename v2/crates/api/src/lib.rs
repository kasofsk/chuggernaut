//! HTTP↔NATS bridge (spec Part 6, §10.4, Part 11, §13.2).
//!
//! A bridge, not a service: translate authenticated HTTP into NATS request-reply,
//! bridge streams to SSE, encrypt secrets on write, validate ingest tokens.
//! No orchestration logic. Never depends on the `dispatcher` crate.

pub mod ingest;
pub mod push;
pub mod routes;
pub mod sse;

/// Build the axum router for the full §6.2 surface. TODO.
pub fn router() -> axum::Router {
    axum::Router::new()
}

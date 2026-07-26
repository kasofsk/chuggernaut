//! The §6.5 reply envelope — the single place a handler turns a result into
//! wire bytes. Success is the resource JSON verbatim; failure is
//! `{"error": {"status": u16, "message": string, "errors": [...]?}}`, so the
//! HTTP bridge maps a reply straight onto a §6.5 response without re-deciding
//! the status.
//!
//! - **Accepts:** a `CoreError`, a serializable resource, or a status + message
//!   from any `req.*` handler in this directory.
//! - **Emits:** reply bodies (`Vec<u8>`).
//! - **Guarantees:** total — a serializer failure degrades to a bare 500
//!   envelope rather than panicking, so a handler can always answer.
//! - **Spec:** §6.5.

use crate::core::CoreError;

/// Map a core error to the §6.5 envelope with an HTTP status hint.
pub(super) fn error_reply(e: &CoreError) -> Vec<u8> {
    let status = match e {
        CoreError::NotFound(_) => 404,
        CoreError::Validation(_) => 422,
        CoreError::Transition(_) | CoreError::InvalidResolution(_) | CoreError::Conflict(_) => 409,
        _ => 500,
    };
    let mut body = serde_json::json!({
        "error": { "status": status, "message": e.to_string() }
    });
    if let CoreError::Validation(errs) = e {
        body["error"]["errors"] = serde_json::json!(errs);
    }
    serde_json::to_vec(&body).unwrap_or_else(|_| FALLBACK_500.to_vec())
}

pub(super) fn ok_reply<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| FALLBACK_500.to_vec())
}

/// The envelope every status helper below shares: they differ only in the
/// number they carry, so the body is written once here.
fn status_reply(status: u16, message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "error": { "status": status, "message": message }
    }))
    .unwrap_or_else(|_| FALLBACK_500.to_vec())
}

/// A malformed or unparseable request: subject, payload, or a missing field.
pub(super) fn bad_request(message: &str) -> Vec<u8> {
    status_reply(400, message)
}

/// 409 for a request that conflicts with existing state (e.g. a project name
/// already taken) — the same status `CoreError::Conflict` maps to.
pub(super) fn conflict(message: &str) -> Vec<u8> {
    status_reply(409, message)
}

/// 422 for a well-formed body that fails a semantic bound (e.g. an oversized
/// `cover_html`) — distinct from the 400 a malformed/unparseable body gets.
pub(super) fn unprocessable(message: &str) -> Vec<u8> {
    status_reply(422, message)
}

/// 503 when a dependency the handler needs is absent or wedged (no CA key
/// mounted, a core actor that will not answer).
pub(super) fn service_unavailable(message: &str) -> Vec<u8> {
    status_reply(503, message)
}

/// 502 when a node behind the dispatcher is unreachable — an error envelope
/// rather than a stall.
pub(super) fn bad_gateway(message: &str) -> Vec<u8> {
    status_reply(502, message)
}

pub(super) const NOT_FOUND: &[u8] = br#"{"error":{"status":404,"message":"not found"}}"#;

/// The last-resort body when even the envelope will not serialize.
const FALLBACK_500: &[u8] = br#"{"error":{"status":500}}"#;

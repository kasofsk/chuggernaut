//! Resource-limit string parsing (spec §1.1: `resources.memory`).
//!
//! Format: a positive integer, optionally suffixed with a binary unit
//! (`Ki`/`Mi`/`Gi`), or plain bytes ("512Mi", "4Gi", "1048576"). This is the
//! single shared grammar: the YAML layer keeps the limit as a string, and both
//! `types` field-rules validation and the container backend's launch-time parse
//! go through here — so a malformed limit is rejected offline at `validate`
//! time instead of wedging a job when the container fails to launch.

use thiserror::Error;

/// The regex describing an accepted memory-limit string. Kept alongside the
/// parser so the JSON Schema (`chuggernaut schema job-type`) documents exactly
/// what [`parse_memory`] accepts.
pub const MEMORY_PATTERN: &str = r"^[0-9]+(Ki|Mi|Gi)?$";

#[derive(Debug, Clone, PartialEq, Error)]
pub enum MemoryParseError {
    #[error("empty memory limit")]
    Empty,
    #[error(
        "invalid memory limit {input:?}: {reason} \
         (expected a positive integer optionally suffixed with Ki/Mi/Gi, \
         e.g. 512Mi, 4Gi, or plain bytes)"
    )]
    Invalid { input: String, reason: String },
}

/// Parse a memory-limit string like "512Mi", "4Gi", or plain bytes ("1048576")
/// into a byte count. Rejects unknown suffixes (e.g. "5g", "4GB"), non-integer,
/// zero, and negative values — matching what the container backend accepts at
/// launch.
pub fn parse_memory(input: &str) -> Result<i64, MemoryParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(MemoryParseError::Empty);
    }
    let invalid = |reason: &str| MemoryParseError::Invalid {
        input: input.to_string(),
        reason: reason.to_string(),
    };

    let (num, mult) = if let Some(n) = s.strip_suffix("Ki") {
        (n, 1024i64)
    } else if let Some(n) = s.strip_suffix("Mi") {
        (n, 1024 * 1024)
    } else if let Some(n) = s.strip_suffix("Gi") {
        (n, 1024 * 1024 * 1024)
    } else {
        (s, 1)
    };
    // Only a bare non-negative integer is a legal numeric part; this rejects
    // signs, decimals, and stray unit letters (the "g" in "5g", "GB" in "4GB").
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("expected a positive integer"));
    }
    let value: i64 = num.parse().map_err(|_| invalid("number out of range"))?;
    let bytes = value
        .checked_mul(mult)
        .ok_or_else(|| invalid("memory limit overflows"))?;
    if bytes <= 0 {
        return Err(invalid("memory limit must be positive"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn accepts_spec_forms() {
        assert_eq!(parse_memory("5Gi").unwrap(), 5 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory("512Mi").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory("4Ki").unwrap(), 4 * 1024);
        assert_eq!(parse_memory("1048576").unwrap(), 1_048_576);
    }

    #[test]
    fn rejects_malformed() {
        // The exact cases from the dogfood bug and the acceptance criteria.
        for bad in [
            "5g", "4GB", "", "  ", "-5", "-5Gi", "0", "1.5Gi", "Gi", "5 Gi", "5gi",
        ] {
            assert!(parse_memory(bad).is_err(), "should reject {bad:?}");
        }
    }
}

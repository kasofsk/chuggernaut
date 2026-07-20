//! Duration string parsing (spec §1.1: `task_timeout`, `job_deadline`, §13: `batch_window`).
//!
//! Format: one or more `{integer}{unit}` segments, units `s`/`m`/`h`/`d`,
//! descending and non-repeating ("1h30m", "2h", "45s"). This is the single
//! shared parser — the YAML layer keeps durations as strings and every
//! consumer (validation, timeout scans, credential TTLs) parses through here.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum DurationParseError {
    #[error("empty duration string")]
    Empty,
    #[error("invalid duration {input:?}: {reason}")]
    Invalid { input: String, reason: String },
}

/// Parse a duration string like "2h", "30m", "1h30m", "45s", "7d".
pub fn parse_duration(input: &str) -> Result<Duration, DurationParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(DurationParseError::Empty);
    }
    let invalid = |reason: &str| DurationParseError::Invalid {
        input: input.to_string(),
        reason: reason.to_string(),
    };

    const UNITS: &[(char, u64)] = &[('d', 86_400), ('h', 3_600), ('m', 60), ('s', 1)];
    let mut total: u64 = 0;
    let mut last_unit_idx: Option<usize> = None;
    let mut chars = s.chars().peekable();

    while chars.peek().is_some() {
        let mut digits = String::new();
        while let Some(c) = chars.peek() {
            if c.is_ascii_digit() {
                digits.push(*c);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            return Err(invalid("expected a number"));
        }
        let value: u64 = digits.parse().map_err(|_| invalid("number out of range"))?;
        let unit = chars.next().ok_or_else(|| invalid("missing unit"))?;
        let idx = UNITS
            .iter()
            .position(|(u, _)| *u == unit)
            .ok_or_else(|| invalid(&format!("unknown unit {unit:?} (expected s/m/h/d)")))?;
        if let Some(last) = last_unit_idx
            && idx <= last
        {
            return Err(invalid("units must be descending and non-repeating"));
        }
        last_unit_idx = Some(idx);
        total = value
            .checked_mul(UNITS[idx].1)
            .and_then(|v| total.checked_add(v))
            .ok_or_else(|| invalid("duration overflows"))?;
    }

    if total == 0 {
        return Err(invalid("duration must be positive"));
    }
    Ok(Duration::from_secs(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_examples() {
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7_200));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1_800));
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5_400));
        assert_eq!(
            parse_duration("1d2h3m4s").unwrap(),
            Duration::from_secs(93_784)
        );
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "", "  ", "h", "30", "30x", "1m1h", "1h1h", "0s", "-5m", "1.5h",
        ] {
            assert!(parse_duration(bad).is_err(), "should reject {bad:?}");
        }
    }
}

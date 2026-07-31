//! Five-field UTC cron expressions (spec §1.1, design #310 Decision 3).
//!
//! The accepted grammar is a deliberate subset of Vixie cron: five
//! whitespace-separated fields (`minute hour day-of-month month day-of-week`),
//! each `*`, `N`, `N-M`, `*/S`, or a comma-list of the last three. No `@daily`
//! aliases, no `L`/`W`/`#`, no month or weekday names, no seconds and no year
//! field — a `.chug/schedules/*.yaml` expression is a copy of a GitHub Actions
//! `schedule:` string, so anything the two sides would read differently is
//! rejected rather than reinterpreted.
//!
//! Every expression is evaluated in **UTC**. A schedule whose meaning depended
//! on where the dispatcher happens to run would silently reschedule itself when
//! the deployment moved hosts, and UTC is also the answer to DST: the ambiguous
//! and non-existent local times never arise.
//!
//! - **Accepts:** a cron string, the instant to test it against, and the
//!   half-open window a backward search may look through.
//! - **Emits:** [`CronExpr`], or [`CronParseError`] naming the field and the
//!   term that broke a rule.
//! - **Guarantees:** pure and total — no I/O, no async, no panics; parsing is
//!   bounded by the expression's own length, matching is constant time, and the
//!   backward search is bounded by [`CRON_LOOKBACK_DAYS_MAX`].
//! - **Spec:** §1.1 (`.chug/schedules/{name}.yaml`).

use chrono::{DateTime, Datelike, Days, NaiveDate, TimeDelta, Timelike, Utc};
use thiserror::Error;

/// How many whitespace-separated fields an accepted expression carries.
pub const CRON_FIELD_COUNT: usize = 5;

/// How far back [`CronExpr::latest_occurrence`] looks (STYLE.md Tier 2 #3:
/// everything is bounded). A dispatcher down longer than this does not catch up
/// on the occurrences it missed — it arms for the next one.
pub const CRON_LOOKBACK_DAYS_MAX: u64 = 366;

/// Minutes in a day — the span of one day's backward minute scan.
const MINUTES_PER_DAY: u32 = 24 * 60;

const MINUTE: usize = 0;
const HOUR: usize = 1;
const DAY_OF_MONTH: usize = 2;
const MONTH: usize = 3;
const DAY_OF_WEEK: usize = 4;

/// One field's name and closed value range, in expression order.
struct CronField {
    name: &'static str,
    min: u32,
    max: u32,
}

/// Day-of-week is `0`–`6`, Sunday first — the numbering `chrono`'s
/// `num_days_from_sunday` and GitHub Actions both use, with no `7` alias so an
/// expression reads the same on either side.
const FIELDS: [CronField; CRON_FIELD_COUNT] = [
    CronField {
        name: "minute",
        min: 0,
        max: 59,
    },
    CronField {
        name: "hour",
        min: 0,
        max: 23,
    },
    CronField {
        name: "day-of-month",
        min: 1,
        max: 31,
    },
    CronField {
        name: "month",
        min: 1,
        max: 12,
    },
    CronField {
        name: "day-of-week",
        min: 0,
        max: 6,
    },
];

/// Why a cron string was refused, naming the field and term rather than
/// restating the whole expression.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CronParseError {
    #[error(
        "expected {expected} space-separated fields \
         (minute hour day-of-month month day-of-week), found {found}"
    )]
    FieldCount { expected: usize, found: usize },
    #[error("{field} field {term:?} is invalid: {reason}")]
    Term {
        field: &'static str,
        term: String,
        reason: String,
    },
}

/// A parsed expression: one bitmask per field, plus whether the field was
/// restricted (anything other than a bare `*`), which is what the day-of-month
/// / day-of-week OR rule turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CronExpr {
    fields: [u64; CRON_FIELD_COUNT],
    restricted: [bool; CRON_FIELD_COUNT],
}

impl CronExpr {
    /// Parse a five-field UTC expression, rejecting everything outside the
    /// documented subset.
    pub fn parse(expr: &str) -> Result<Self, CronParseError> {
        let terms: Vec<&str> = expr.split_whitespace().collect();
        if terms.len() != CRON_FIELD_COUNT {
            return Err(CronParseError::FieldCount {
                expected: CRON_FIELD_COUNT,
                found: terms.len(),
            });
        }
        let mut fields = [0u64; CRON_FIELD_COUNT];
        let mut restricted = [false; CRON_FIELD_COUNT];
        for (index, term) in terms.iter().enumerate() {
            let (mask, is_restricted) = parse_field(&FIELDS[index], term)?;
            assert!(
                mask != 0,
                "an accepted {} field matches at least one value",
                FIELDS[index].name
            );
            fields[index] = mask;
            restricted[index] = is_restricted;
        }
        assert!(
            fields.iter().all(|mask| *mask != 0),
            "every accepted field matches at least one value"
        );
        Ok(CronExpr { fields, restricted })
    }

    /// Whether `at` is an occurrence of this expression, in UTC.
    ///
    /// When day-of-month and day-of-week are **both** restricted the day
    /// matches if *either* matches (the POSIX OR rule); otherwise both must,
    /// which the unrestricted `*` satisfies for free.
    #[must_use]
    pub fn matches(&self, at: DateTime<Utc>) -> bool {
        self.matches_day(at.date_naive())
            && self.field_matches(MINUTE, at.minute())
            && self.field_matches(HOUR, at.hour())
    }

    /// The newest occurrence in `(after, at]`, or None when the expression has
    /// none there — the half-open bound is what makes an occurrence consumable,
    /// and asking for the newest is what coalesces missed ones into a single
    /// answer (spec §1.1 schedules).
    ///
    /// Bounded by [`CRON_LOOKBACK_DAYS_MAX`]: an older `after` is raised to it.
    #[must_use]
    pub fn latest_occurrence(
        &self,
        at: DateTime<Utc>,
        after: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let floor = after.max(at - TimeDelta::days(i64::try_from(CRON_LOOKBACK_DAYS_MAX).ok()?));
        if at <= floor {
            return None;
        }
        let mut day = at.date_naive();
        let mut days_scanned = 0u64;
        while day >= floor.date_naive() && days_scanned <= CRON_LOOKBACK_DAYS_MAX {
            days_scanned += 1;
            if self.matches_day(day)
                && let Some(found) = self.latest_minute_of_day(day, at, floor)
            {
                assert!(
                    found > floor && found <= at,
                    "occurrence is inside (after, at]"
                );
                return Some(found);
            }
            day = day.checked_sub_days(Days::new(1))?;
        }
        None
    }

    /// The newest matching minute of `day` inside `(floor, at]`, scanning the
    /// day backwards. A day the date fields match still yields nothing when the
    /// window covers only part of it.
    fn latest_minute_of_day(
        &self,
        day: NaiveDate,
        at: DateTime<Utc>,
        floor: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        for minute_of_day in (0..MINUTES_PER_DAY).rev() {
            let candidate = day
                .and_hms_opt(minute_of_day / 60, minute_of_day % 60, 0)?
                .and_utc();
            if candidate > at || candidate <= floor {
                continue;
            }
            if self.field_matches(MINUTE, candidate.minute())
                && self.field_matches(HOUR, candidate.hour())
            {
                return Some(candidate);
            }
        }
        None
    }

    /// Whether `date` is a day this expression can occur on: the month field,
    /// and the day-of-month / day-of-week pair under the POSIX OR rule.
    fn matches_day(&self, date: NaiveDate) -> bool {
        let by_month_day = self.field_matches(DAY_OF_MONTH, date.day());
        let by_week_day = self.field_matches(DAY_OF_WEEK, date.weekday().num_days_from_sunday());
        let day_matches = if self.restricted[DAY_OF_MONTH] && self.restricted[DAY_OF_WEEK] {
            by_month_day || by_week_day
        } else {
            by_month_day && by_week_day
        };
        day_matches && self.field_matches(MONTH, date.month())
    }

    fn field_matches(&self, index: usize, value: u32) -> bool {
        assert!(index < CRON_FIELD_COUNT, "cron field index is in range");
        assert!(
            value <= FIELDS[index].max,
            "a calendar {} is inside the field's range",
            FIELDS[index].name
        );
        self.fields[index] & (1u64 << value) != 0
    }
}

/// One whole field: a bare `*`, or a comma-list of terms that each restrict it.
fn parse_field(field: &CronField, term: &str) -> Result<(u64, bool), CronParseError> {
    if term == "*" {
        return Ok((mask_range(field.min, field.max), false));
    }
    let mut mask = 0u64;
    for part in term.split(',') {
        mask |= parse_term(field, part).map_err(|reason| CronParseError::Term {
            field: field.name,
            term: term.to_string(),
            reason,
        })?;
    }
    assert!(mask != 0, "a restricted field matches at least one value");
    Ok((mask, true))
}

/// One comma-separated term — `N`, `N-M` or `*/S` — as a value mask, or the
/// reason it is outside the accepted grammar.
fn parse_term(field: &CronField, term: &str) -> Result<u64, String> {
    if let Some(step) = term.strip_prefix("*/") {
        let step = parse_number(step)?;
        if step == 0 || step > field.max {
            return Err(format!("step must be between 1 and {}", field.max));
        }
        return Ok(mask_stepped(field, step));
    }
    if term == "*" {
        return Err("'*' matches every value, so it must stand alone".to_string());
    }
    if let Some((start, end)) = term.split_once('-') {
        let start = parse_value(field, start)?;
        let end = parse_value(field, end)?;
        if start > end {
            return Err(format!("range {start}-{end} runs backwards"));
        }
        return Ok(mask_range(start, end));
    }
    Ok(1u64 << parse_value(field, term)?)
}

fn parse_value(field: &CronField, text: &str) -> Result<u32, String> {
    let value = parse_number(text)?;
    if value < field.min || value > field.max {
        return Err(format!("{value} is outside {}..={}", field.min, field.max));
    }
    Ok(value)
}

/// A bare decimal number, rejecting the sign and whitespace `str::parse` would
/// otherwise accept.
fn parse_number(text: &str) -> Result<u32, String> {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("{text:?} is not a decimal number"));
    }
    text.parse::<u32>()
        .map_err(|_| format!("{text:?} is out of range"))
}

fn mask_range(start: u32, end: u32) -> u64 {
    assert!(start <= end, "a cron range is ascending");
    let width = end - start + 1;
    assert!(width < 64, "a cron field spans fewer than 64 values");
    ((1u64 << width) - 1) << start
}

fn mask_stepped(field: &CronField, step: u32) -> u64 {
    assert!(step >= 1, "a cron step is positive");
    let mut mask = 0u64;
    let mut value = field.min;
    while value <= field.max {
        mask |= 1u64 << value;
        value += step;
    }
    assert!(mask != 0, "a stepped field matches at least its minimum");
    mask
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use chrono::TimeZone;

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    fn parse(expr: &str) -> CronExpr {
        CronExpr::parse(expr).unwrap()
    }

    #[test]
    fn nightly_expression_matches_only_its_minute() {
        let nightly = parse("0 2 * * *");
        assert!(nightly.matches(at(2026, 7, 31, 2, 0)));
        assert!(!nightly.matches(at(2026, 7, 31, 2, 1)));
        assert!(!nightly.matches(at(2026, 7, 31, 3, 0)));
        assert!(nightly.matches(at(2027, 1, 1, 2, 0)));
    }

    #[test]
    fn steps_lists_and_ranges_expand_from_the_field_minimum() {
        let quarter_hour = parse("*/15 * * * *");
        for minute in [0, 15, 30, 45] {
            assert!(quarter_hour.matches(at(2026, 7, 31, 9, minute)), "{minute}");
        }
        assert!(!quarter_hour.matches(at(2026, 7, 31, 9, 14)));

        let listed = parse("0,30 9-11 * * *");
        assert!(listed.matches(at(2026, 7, 31, 11, 30)));
        assert!(!listed.matches(at(2026, 7, 31, 12, 0)));
        assert!(!listed.matches(at(2026, 7, 31, 9, 15)));

        let every_seventh_day = parse("0 0 */7 * *");
        for day in [1, 8, 15, 22, 29] {
            assert!(every_seventh_day.matches(at(2026, 7, day, 0, 0)), "{day}");
        }
        assert!(!every_seventh_day.matches(at(2026, 7, 7, 0, 0)));
    }

    #[test]
    fn months_and_weekdays_use_the_documented_numbering() {
        let new_year = parse("0 0 1 1 *");
        assert!(new_year.matches(at(2027, 1, 1, 0, 0)));
        assert!(!new_year.matches(at(2026, 12, 1, 0, 0)));

        let sunday = parse("0 0 * * 0");
        assert!(sunday.matches(at(2026, 8, 2, 0, 0)));
        assert!(!sunday.matches(at(2026, 8, 1, 0, 0)));

        let saturday = parse("0 0 * * 6");
        assert!(saturday.matches(at(2026, 8, 1, 0, 0)));
    }

    /// Design #310 Decision 3: with day-of-month **and** day-of-week both
    /// restricted, an occurrence matches when *either* does.
    #[test]
    fn restricted_day_fields_are_ored_not_anded() {
        let first_or_monday = parse("0 0 1 * 1");
        assert!(first_or_monday.matches(at(2026, 7, 1, 0, 0)));
        assert!(first_or_monday.matches(at(2026, 7, 6, 0, 0)));
        assert!(!first_or_monday.matches(at(2026, 7, 2, 0, 0)));
        assert!(first_or_monday.matches(at(2026, 6, 1, 0, 0)));
    }

    /// The other half of the same rule: one unrestricted day field makes the
    /// pair an AND, so `* * 1 * *` is the 1st and nothing else.
    #[test]
    fn one_unrestricted_day_field_leaves_the_other_alone() {
        let first_of_month = parse("0 0 1 * *");
        assert!(first_of_month.matches(at(2026, 7, 1, 0, 0)));
        assert!(!first_of_month.matches(at(2026, 7, 6, 0, 0)));

        let weekdays = parse("0 9 * * 1-5");
        assert!(weekdays.matches(at(2026, 7, 31, 9, 0)));
        assert!(!weekdays.matches(at(2026, 8, 1, 9, 0)));
        assert!(!weekdays.matches(at(2026, 8, 2, 9, 0)));

        let every_minute = parse("* * * * *");
        assert!(every_minute.matches(at(2026, 8, 2, 13, 37)));
    }

    /// A `*/S` day-of-week is restricted too, so it still triggers the OR rule.
    #[test]
    fn a_stepped_day_field_counts_as_restricted() {
        let stepped = parse("0 0 15 * */3");
        assert!(stepped.matches(at(2026, 7, 15, 0, 0)));
        assert!(stepped.matches(at(2026, 7, 5, 0, 0)));
        assert!(!stepped.matches(at(2026, 7, 7, 0, 0)));
    }

    /// The search reports the NEWEST occurrence in `(after, at]`, which is what
    /// turns a run of missed occurrences into one fire (design #310 Decision 5).
    #[test]
    fn latest_occurrence_coalesces_a_window_to_its_newest_match() {
        let hourly = parse("0 * * * *");
        assert_eq!(
            hourly.latest_occurrence(at(2026, 7, 31, 8, 30), at(2026, 7, 31, 2, 20)),
            Some(at(2026, 7, 31, 8, 0)),
            "six missed hours collapse to the most recent one"
        );
        assert_eq!(
            hourly.latest_occurrence(at(2026, 7, 31, 8, 30), at(2026, 7, 31, 8, 0)),
            None,
            "the bound is exclusive, so an honored occurrence is consumed"
        );
        assert_eq!(
            hourly.latest_occurrence(at(2026, 7, 31, 8, 0), at(2026, 7, 31, 7, 59)),
            Some(at(2026, 7, 31, 8, 0)),
            "`at` itself is inside the window"
        );
    }

    /// A window with no occurrence in it, and a window that crosses days,
    /// months and a year boundary — the day scan, not a minute walk.
    #[test]
    fn latest_occurrence_spans_days_or_finds_nothing() {
        let nightly = parse("0 2 * * *");
        assert_eq!(
            nightly.latest_occurrence(at(2026, 7, 31, 1, 59), at(2026, 7, 31, 0, 0)),
            None
        );
        assert_eq!(
            nightly.latest_occurrence(at(2026, 7, 31, 1, 59), at(2026, 7, 20, 0, 0)),
            Some(at(2026, 7, 30, 2, 0))
        );

        let new_year = parse("0 0 1 1 *");
        assert_eq!(
            new_year.latest_occurrence(at(2026, 7, 31, 12, 0), at(2025, 12, 31, 0, 0)),
            Some(at(2026, 1, 1, 0, 0))
        );

        let weekdays = parse("0 9 * * 1-5");
        assert_eq!(
            weekdays.latest_occurrence(at(2026, 8, 2, 23, 0), at(2026, 8, 1, 0, 0)),
            None,
            "a weekend window matches no weekday occurrence"
        );
    }

    /// The bound is a real bound: an `after` older than
    /// [`CRON_LOOKBACK_DAYS_MAX`] is raised to it, so an ancient anchor costs
    /// the same search as a recent one and skips what it cannot reach.
    #[test]
    fn latest_occurrence_is_bounded_by_the_lookback() {
        let leap_day = parse("0 0 29 2 *");
        let now = at(2026, 7, 31, 12, 0);
        assert_eq!(
            leap_day.latest_occurrence(at(2024, 3, 1, 0, 0), at(2020, 1, 1, 0, 0)),
            Some(at(2024, 2, 29, 0, 0)),
            "within the lookback, the newest match still wins"
        );
        assert_eq!(
            leap_day.latest_occurrence(now, at(2020, 1, 1, 0, 0)),
            None,
            "the last leap day is years back, past the bound"
        );
        let yearly = parse("0 0 1 1 *");
        assert_eq!(yearly.latest_occurrence(now, now), None, "an empty window");
        assert_eq!(
            yearly.latest_occurrence(now, now + chrono::Duration::hours(1)),
            None,
            "a backwards window is empty, not a panic"
        );
    }

    #[test]
    fn field_count_is_exactly_five() {
        for bad in ["", "   ", "* * * *", "* * * * * *", "0"] {
            assert!(
                matches!(
                    CronExpr::parse(bad),
                    Err(CronParseError::FieldCount { expected: 5, .. })
                ),
                "should reject {bad:?}"
            );
        }
        assert!(CronExpr::parse("  0   2  *  *  *  ").is_ok());
    }

    #[test]
    fn values_outside_a_field_are_rejected() {
        for bad in [
            "60 * * * *",
            "* 24 * * *",
            "0 0 0 * *",
            "0 0 32 * *",
            "0 0 * 0 *",
            "0 0 * 13 *",
            "0 0 * * 7",
            "4294967296 * * * *",
        ] {
            let err = CronExpr::parse(bad).unwrap_err();
            assert!(
                matches!(err, CronParseError::Term { .. }),
                "should reject {bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn syntax_outside_the_subset_is_rejected() {
        for bad in [
            "a * * * *",
            "+5 * * * *",
            "-5 * * * *",
            "5- * * * *",
            "1-2-3 * * * *",
            "1,,2 * * * *",
            "*,5 * * * *",
            "* * * * mon",
            "@daily * * * *",
            "*/ * * * *",
            "*/0 * * * *",
            "*/60 * * * *",
            "5-1 * * * *",
            "0 0 L * *",
            "5.0 * * * *",
        ] {
            assert!(CronExpr::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn errors_name_the_field_and_the_term() {
        let err = CronExpr::parse("0 0 * * 9").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("day-of-week"), "{message}");
        assert!(message.contains("\"9\""), "{message}");
        assert!(message.contains("0..=6"), "{message}");

        let count = CronExpr::parse("0 2 * *").unwrap_err().to_string();
        assert!(count.contains("found 4"), "{count}");
    }
}

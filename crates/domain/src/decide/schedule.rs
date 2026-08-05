//! Schedule decisions (spec §1.1 schedules, design #310 Decisions 4, 5 and 7):
//! whether a `.chug/schedules/{name}.yaml` occurrence is due, and whether it
//! fires or is skipped.
//!
//! The whole rule is one value per schedule — the **anchor**, the instant an
//! occurrence must be strictly after to fire. A schedule that has never fired
//! anchors on `first_seen_at` (no backfill); one whose last job is terminal
//! anchors on that job's completion (catch-up across restarts, and skipped
//! occurrences consumed); one whose last job is still live cannot fire at all
//! (at most one job in flight per schedule). Because the decider asks "is there
//! an occurrence in `(anchor, now]`" rather than "does `now` match", a run of
//! missed occurrences coalesces to exactly one fire.
//!
//! Creating a job is not an [`Effect`]: allocating a job seq is I/O, and
//! pre-allocating one per tick would burn ids for the overwhelmingly common
//! "nothing is due". So a fire comes back as a **decision** the shell performs
//! through `Core::create_job`, while a skip — which has a blocking job to
//! publish on — comes back as its event effect.
//!
//! - **Accepts:** the project, one [`ScheduleView`] per loaded schedule, and
//!   the tick's instant.
//! - **Emits:** the [`ScheduleDecision`]s this turn (fire and skip), plus the
//!   `schedule-skipped` [`Effect::PublishEvent`]s that report the skips.
//! - **Guarantees:** pure and total — no I/O, no clock, no id allocation;
//!   performs no effect (docs/reference/style.md Tier 2 #1), and the per-turn work is bounded
//!   by [`SCHEDULES_MAX`].
//! - **Spec:** §1.1 (schedules), §2.1 (`is_terminal`); design #310.

use crate::effects::Effect;
use chrono::{DateTime, Utc};
use types::{JobState, SCHEDULES_MAX, Schedule};

/// The read-only inputs one schedule's decision consumes: its loaded config,
/// the job that carries its provenance, and the two pieces of in-memory table
/// state (both safe to lose on restart).
pub struct ScheduleView<'a> {
    /// The loaded, already-validated schedule file.
    pub schedule: &'a Schedule,
    /// The most recent job carrying this schedule's provenance, by
    /// `created_at`. None until the schedule has ever fired.
    pub latest: Option<ScheduleLatest>,
    /// When the dispatcher first loaded this file. The anchor while `latest` is
    /// None, which is what makes a newly-merged schedule not backfill.
    pub first_seen_at: DateTime<Utc>,
    /// The occurrence the last `schedule-skipped` reported, so a blocked
    /// schedule reports each occurrence once instead of once per tick.
    pub last_skipped_occurrence: Option<DateTime<Utc>>,
}

/// The latest job of a schedule, projected to the three values the anchor rule
/// needs — never the whole record, so the decider cannot reach for anything
/// else.
#[derive(Debug, Clone, Copy)]
pub struct ScheduleLatest {
    pub seq: u64,
    pub state: JobState,
    /// Bounds the skip-report interval while this job blocks the schedule, and
    /// stands in as the anchor for a terminal record written before
    /// `completed_at` existed.
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// What one occurrence's turn decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleVerdict {
    /// The occurrence is due and nothing is in flight: create one job.
    Fire,
    /// The occurrence came due while a prior run was non-terminal. It is
    /// **consumed**, never deferred — the shell only records it as reported.
    Skip,
}

/// One schedule's verdict for one occurrence.
#[derive(Debug, Clone)]
pub struct ScheduleDecision {
    pub schedule: String,
    pub occurrence_at: DateTime<Utc>,
    pub verdict: ScheduleVerdict,
}

/// Decide every loaded schedule of one project (design #310 Decision 8).
///
/// Returns the turn's decisions — at most one per schedule, because an
/// occurrence run coalesces — and the `schedule-skipped` events, published on
/// the job that did the blocking.
pub fn decide(
    owner: &str,
    project: &str,
    views: &[ScheduleView<'_>],
    now: DateTime<Utc>,
) -> (Vec<ScheduleDecision>, Vec<Effect>) {
    assert!(
        views.len() <= SCHEDULES_MAX,
        "{owner}/{project} decided {} schedules, past the {SCHEDULES_MAX} cap",
        views.len()
    );

    let mut decisions = Vec::new();
    let mut effects = Vec::new();
    for view in views {
        let Some(decision) = decide_one(view, now) else {
            continue;
        };
        if decision.verdict == ScheduleVerdict::Skip {
            let seq = view
                .latest
                .map(|latest| latest.seq)
                .unwrap_or_else(|| panic!("a skip needs the job that blocked it"));
            effects.push(Effect::PublishEvent {
                owner: owner.to_string(),
                project: project.to_string(),
                seq,
                event_type: "schedule-skipped".to_string(),
                extra: serde_json::json!({
                    "schedule": decision.schedule,
                    "occurrence_at": decision.occurrence_at,
                }),
            });
        }
        decisions.push(decision);
    }
    debug_assert!(
        decisions.len() <= views.len(),
        "a schedule decides at most one occurrence per turn"
    );
    (decisions, effects)
}

/// One schedule's turn: the anchor rule, or — while a prior run blocks it — the
/// once-per-occurrence skip report.
fn decide_one(view: &ScheduleView<'_>, now: DateTime<Utc>) -> Option<ScheduleDecision> {
    if !view.schedule.enabled {
        return None;
    }
    let cron = view.schedule.cron_expr();
    debug_assert!(
        cron.is_ok(),
        "schedule '{}' reached the decider with an unparseable cron",
        view.schedule.name
    );
    let cron = cron.ok()?;

    let (verdict, after) = match view.latest {
        None => (ScheduleVerdict::Fire, view.first_seen_at),
        Some(latest) if latest.state.is_terminal() => (
            ScheduleVerdict::Fire,
            latest.completed_at.unwrap_or(latest.created_at),
        ),
        Some(latest) => (ScheduleVerdict::Skip, latest.created_at),
    };

    let occurrence_at = cron.latest_occurrence(now, after)?;
    assert!(
        occurrence_at > after && occurrence_at <= now,
        "occurrence {occurrence_at} for '{}' is outside ({after}, {now}]",
        view.schedule.name
    );
    if verdict == ScheduleVerdict::Skip && view.last_skipped_occurrence == Some(occurrence_at) {
        return None;
    }
    Some(ScheduleDecision {
        schedule: view.schedule.name.clone(),
        occurrence_at,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Tier-1 coverage of the anchor rule, coalescing and the skip rule — pure
    //! values in, pure values out. The three traces design #310 Decision 5 pins
    //! are the first three tests.
    use super::*;
    use chrono::TimeZone;

    fn at(month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, month, day, hour, minute, 0)
            .unwrap()
    }

    fn schedule(cron: &str) -> Schedule {
        Schedule::parse(&format!(
            "name: nightly\njob_type: code\ncron: '{cron}'\ndescription: run it\n"
        ))
        .unwrap()
    }

    fn view<'a>(schedule: &'a Schedule, first_seen_at: DateTime<Utc>) -> ScheduleView<'a> {
        ScheduleView {
            schedule,
            latest: None,
            first_seen_at,
            last_skipped_occurrence: None,
        }
    }

    fn latest(state: JobState, created_at: DateTime<Utc>) -> ScheduleLatest {
        ScheduleLatest {
            seq: 7,
            state,
            created_at,
            completed_at: None,
        }
    }

    fn turn(view: ScheduleView<'_>, now: DateTime<Utc>) -> (Vec<ScheduleDecision>, Vec<Effect>) {
        decide("acme", "api", &[view], now)
    }

    /// Decision 5 trace 1 — **catch-up**: hourly, last job created 02:00 and
    /// completed 02:20, dispatcher down 02:30–08:30. Six occurrences passed and
    /// exactly ONE job fires, for the newest of them.
    #[test]
    fn missed_occurrences_coalesce_to_one_fire() {
        let hourly = schedule("0 * * * *");
        let mut v = view(&hourly, at(7, 31, 0, 0));
        v.latest = Some(ScheduleLatest {
            completed_at: Some(at(7, 31, 2, 20)),
            ..latest(JobState::Done, at(7, 31, 2, 0))
        });

        let (decisions, effects) = turn(v, at(7, 31, 8, 30));
        assert_eq!(decisions.len(), 1, "one job, not six and not zero");
        assert_eq!(decisions[0].verdict, ScheduleVerdict::Fire);
        assert_eq!(decisions[0].occurrence_at, at(7, 31, 8, 0));
        assert_eq!(decisions[0].schedule, "nightly");
        assert!(effects.is_empty(), "a fire publishes nothing from here");
    }

    /// Decision 5 trace 2 — **no backfill**: a schedule merged at 08:30 anchors
    /// on `first_seen_at`, so its first fire is the next occurrence and no epoch
    /// appears anywhere in the computation.
    #[test]
    fn a_new_schedule_never_backfills() {
        let hourly = schedule("0 * * * *");
        let merged_at = at(7, 31, 8, 30);
        assert!(
            turn(view(&hourly, merged_at), at(7, 31, 8, 45))
                .0
                .is_empty(),
            "08:00 is before the schedule existed"
        );
        let (decisions, _) = turn(view(&hourly, merged_at), at(7, 31, 9, 0));
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].occurrence_at, at(7, 31, 9, 0));
    }

    /// Decision 5 trace 3 — **skips are consumed, not deferred**: Monday's
    /// nightly escalates, Tue and Wed 02:00 are skipped, an operator revokes it
    /// Wednesday 10:00. The next fire is Thursday 02:00, not an off-schedule run
    /// minutes after the escalation cleared.
    #[test]
    fn a_skipped_occurrence_is_consumed_by_the_blocking_job() {
        let nightly = schedule("0 2 * * *");
        let created = at(7, 27, 2, 0);
        let mut blocked = view(&nightly, at(7, 1, 0, 0));
        blocked.latest = Some(latest(JobState::Escalated, created));
        let (decisions, effects) = turn(blocked, at(7, 29, 3, 0));
        assert_eq!(decisions[0].verdict, ScheduleVerdict::Skip);
        assert_eq!(decisions[0].occurrence_at, at(7, 29, 2, 0));
        assert_eq!(effects.len(), 1);

        let mut cleared = view(&nightly, at(7, 1, 0, 0));
        cleared.latest = Some(ScheduleLatest {
            completed_at: Some(at(7, 29, 10, 0)),
            ..latest(JobState::Revoked, created)
        });
        assert!(
            turn(cleared, at(7, 29, 10, 1)).0.is_empty(),
            "Tue and Wed 02:00 are behind the revoke, and gone"
        );

        let mut next = view(&nightly, at(7, 1, 0, 0));
        next.latest = Some(ScheduleLatest {
            completed_at: Some(at(7, 29, 10, 0)),
            ..latest(JobState::Revoked, created)
        });
        let (decisions, _) = turn(next, at(7, 30, 2, 0));
        assert_eq!(decisions[0].verdict, ScheduleVerdict::Fire);
        assert_eq!(decisions[0].occurrence_at, at(7, 30, 2, 0));
    }

    /// Every non-terminal state blocks — Escalated, Stalled and Frozen included
    /// — and the skip is published on the job doing the blocking.
    #[test]
    fn any_non_terminal_job_blocks_the_next_occurrence() {
        let nightly = schedule("0 2 * * *");
        for state in [
            JobState::Frozen,
            JobState::Ready,
            JobState::Work,
            JobState::Evaluation,
            JobState::Escalated,
            JobState::Stalled,
        ] {
            let mut v = view(&nightly, at(7, 1, 0, 0));
            v.latest = Some(latest(state, at(7, 30, 2, 0)));
            let (decisions, effects) = turn(v, at(7, 31, 2, 0));
            assert_eq!(decisions.len(), 1, "{state:?}");
            assert_eq!(decisions[0].verdict, ScheduleVerdict::Skip, "{state:?}");
            match &effects[0] {
                Effect::PublishEvent {
                    owner,
                    project,
                    seq,
                    event_type,
                    extra,
                } => {
                    assert_eq!((owner.as_str(), project.as_str(), *seq), ("acme", "api", 7));
                    assert_eq!(event_type, "schedule-skipped");
                    assert_eq!(extra["schedule"], "nightly");
                    assert_eq!(extra["occurrence_at"], serde_json::json!(at(7, 31, 2, 0)));
                }
                other => panic!("expected a schedule-skipped publish, got {other:?}"),
            }
        }
    }

    /// The skip report is bounded to one per occurrence, and its interval
    /// starts at the blocking job's creation — so the fire that just happened
    /// never reports itself as a skip on the very next tick.
    #[test]
    fn a_skip_is_reported_once_per_occurrence() {
        let nightly = schedule("0 2 * * *");
        let fired_at = at(7, 31, 2, 0) + chrono::Duration::seconds(15);

        let mut fresh = view(&nightly, at(7, 1, 0, 0));
        fresh.latest = Some(latest(JobState::Work, fired_at));
        assert!(
            turn(fresh, fired_at + chrono::Duration::seconds(30))
                .0
                .is_empty(),
            "the occurrence that just fired is behind created_at"
        );

        let mut repeat = view(&nightly, at(7, 1, 0, 0));
        repeat.latest = Some(latest(JobState::Work, fired_at));
        repeat.last_skipped_occurrence = Some(at(8, 1, 2, 0));
        let (decisions, effects) = turn(repeat, at(8, 1, 2, 30));
        assert!(decisions.is_empty(), "{decisions:?}");
        assert!(effects.is_empty(), "the same occurrence reports once");
    }

    /// A disabled schedule loads and validates but never fires or skips.
    #[test]
    fn a_disabled_schedule_decides_nothing() {
        let disabled = Schedule::parse(
            "name: nightly\njob_type: code\ncron: '0 * * * *'\nenabled: false\ndescription: x\n",
        )
        .unwrap();
        let mut v = view(&disabled, at(7, 31, 0, 0));
        v.latest = Some(latest(JobState::Done, at(7, 31, 2, 0)));
        assert!(turn(v, at(7, 31, 9, 0)).0.is_empty());
    }

    /// A terminal record written before `completed_at` existed anchors on its
    /// creation instead — never on the epoch.
    #[test]
    fn a_terminal_job_without_a_completion_anchors_on_its_creation() {
        let hourly = schedule("0 * * * *");
        let mut v = view(&hourly, at(7, 1, 0, 0));
        v.latest = Some(latest(JobState::Done, at(7, 31, 8, 0)));
        let (decisions, _) = turn(v, at(7, 31, 9, 30));
        assert_eq!(decisions[0].occurrence_at, at(7, 31, 9, 0));

        let mut same = view(&hourly, at(7, 1, 0, 0));
        same.latest = Some(latest(JobState::Done, at(7, 31, 8, 0)));
        assert!(
            turn(same, at(7, 31, 8, 30)).0.is_empty(),
            "the occurrence it was created for is consumed"
        );
    }

    /// Negative space: nothing is decided for a project with no schedules, and
    /// the per-turn work is capped.
    #[test]
    fn an_empty_project_decides_nothing() {
        let (decisions, effects) = decide("acme", "api", &[], at(7, 31, 2, 0));
        assert!(decisions.is_empty() && effects.is_empty());
    }

    #[test]
    #[should_panic(expected = "past the")]
    fn deciding_more_schedules_than_the_cap_is_a_loader_bug() {
        let hourly = schedule("0 * * * *");
        let views: Vec<ScheduleView<'_>> = (0..=SCHEDULES_MAX)
            .map(|_| view(&hourly, at(7, 31, 0, 0)))
            .collect();
        decide("acme", "api", &views, at(7, 31, 2, 0));
    }
}

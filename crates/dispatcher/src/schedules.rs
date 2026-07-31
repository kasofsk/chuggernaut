//! The in-memory schedule table (spec §1.1 schedules, design #310 Decision 8):
//! every project's `.chug/schedules/*.yaml` as the tick sees them.
//!
//! Reading the config at default-branch HEAD is a repo tree read plus a file
//! read per schedule — subprocess work that must not run on the single-writer
//! loop every 30 seconds. So the files are loaded into this table and refreshed
//! at startup, after every squash-merge to a default branch, and on a bounded
//! periodic backstop; a stale table delays a schedule *change* by at most one
//! refresh interval and never misfires.
//!
//! An invalid file is **skipped and logged**, never fatal: an unparseable
//! schedule, a `cron` that does not parse, a name disagreeing with the file
//! stem, a `job_type` naming a file absent at HEAD, or a missing `description`
//! for an agent target all leave the project's other schedules loading
//! normally. `schedule-invalid` has no home — the event stream is job-scoped
//! and an invalid file has no job — so `chuggernaut validate` in CI is the
//! primary defense and this is the fallback.
//!
//! - **Accepts:** a project and the `vcs` port.
//! - **Emits:** the project's valid schedules, each paired with the two pieces
//!   of in-memory state the decider reads (`first_seen_at`,
//!   `last_skipped_occurrence`).
//! - **Guarantees:** reads only — no job or task record is written here; at most
//!   [`SCHEDULES_MAX`] entries per project, refused rather than truncated; a
//!   refresh preserves the in-memory state of a schedule that is still present,
//!   so re-reading HEAD never re-arms a schedule that was about to fire.
//! - **Spec:** §1.1 (schedules), §14 (skew); design #310 Decisions 2, 6 and 8.

use crate::core::Core;
use crate::project_config;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use types::{CONFIG_SCHEMA_EPOCH, JobType, SCHEDULES_DIR, SCHEDULES_MAX, Schedule};
use vcs::RepoManager;

/// How many scan ticks pass between periodic refreshes — the backstop for a
/// default branch that moved without a Chuggernaut merge (an origin sync, an
/// operator push). 20 ticks is ~10 minutes at the §3.5 scan interval.
pub(crate) const SCHEDULE_REFRESH_TICKS: u64 = 20;

/// One loaded schedule plus the in-memory state its decision reads. Both
/// timestamps are deliberately *not* persisted (design #310 Decision 5): losing
/// them on restart costs at most a re-armed never-fired schedule and one
/// re-reported skip.
pub(crate) struct ScheduleEntry {
    pub schedule: Schedule,
    /// When this dispatcher first loaded the file — the anchor until the
    /// schedule has ever fired, which is what makes a merge not backfill.
    pub first_seen_at: DateTime<Utc>,
    /// The occurrence the last `schedule-skipped` reported, bounding that event
    /// to one per occurrence rather than one per tick.
    pub last_skipped_occurrence: Option<DateTime<Utc>>,
}

/// One project's schedules by name, in a deterministic order.
pub(crate) type ScheduleTable = BTreeMap<String, ScheduleEntry>;

impl Core {
    /// Reload every known project's schedules from default-branch HEAD. Called
    /// at startup, after a squash-merge lands, and on the periodic backstop.
    pub async fn refresh_schedules(&mut self) {
        let mut slugs: Vec<String> = self.graphs.keys().cloned().collect();
        match self.projects.list_all().await {
            Ok(records) => slugs.extend(
                records
                    .into_iter()
                    .filter_map(|(key, _)| key.split_once('.').map(|(o, p)| format!("{o}/{p}"))),
            ),
            Err(e) => tracing::warn!("schedule refresh: listing projects failed: {e}"),
        }
        slugs.sort();
        slugs.dedup();
        for slug in slugs {
            let Some((owner, project)) = slug.split_once('/') else {
                continue;
            };
            let (owner, project) = (owner.to_string(), project.to_string());
            self.refresh_project_schedules(&owner, &project).await;
        }
    }

    /// Reload one project's schedules, preserving the in-memory state of every
    /// schedule still present at HEAD. A project with none holds no table at
    /// all, so the map never outgrows the projects that actually schedule.
    pub(crate) async fn refresh_project_schedules(&mut self, owner: &str, project: &str) {
        let slug = format!("{owner}/{project}");
        let loaded = load(&self.repos, owner, project).await;
        let mut table = self.schedules.remove(&slug).unwrap_or_default();
        merge(&mut table, loaded, Utc::now());
        if !table.is_empty() {
            self.schedules.insert(slug, table);
        }
    }
}

/// Fold a fresh read of HEAD into an existing table: entries that disappeared
/// are dropped, new ones start their `first_seen_at` now, and a schedule still
/// present keeps the state it had — including when its file changed, because
/// re-arming on every edit would let a frequently-edited schedule never fire.
pub(crate) fn merge(table: &mut ScheduleTable, loaded: Vec<Schedule>, now: DateTime<Utc>) {
    assert!(
        loaded.len() <= SCHEDULES_MAX,
        "the loader admitted {} schedules, past the {SCHEDULES_MAX} cap",
        loaded.len()
    );
    let names: Vec<String> = loaded.iter().map(|s| s.name.clone()).collect();
    table.retain(|name, _| names.contains(name));
    for schedule in loaded {
        match table.get_mut(&schedule.name) {
            Some(entry) => entry.schedule = schedule,
            None => {
                table.insert(
                    schedule.name.clone(),
                    ScheduleEntry {
                        schedule,
                        first_seen_at: now,
                        last_skipped_occurrence: None,
                    },
                );
            }
        }
    }
    debug_assert!(
        table.len() <= SCHEDULES_MAX,
        "a merged table stays inside the cap"
    );
}

/// Every valid schedule of one project at default-branch HEAD. A project with
/// no repo, no schedules directory, or nothing valid in it loads to an empty
/// list — never an error, because an invalid trigger file must not block
/// dispatch.
pub async fn load(repos: &RepoManager, owner: &str, project: &str) -> Vec<Schedule> {
    let head = match head_of_default_branch(repos, owner, project).await {
        Ok(head) => head,
        Err(e) => {
            tracing::debug!("schedules for {owner}/{project}: no readable HEAD ({e})");
            return Vec::new();
        }
    };
    let tree = match repos.tree(owner, project, &head).await {
        Ok(tree) => tree,
        Err(e) => {
            tracing::warn!("schedules for {owner}/{project}: reading the tree failed: {e}");
            return Vec::new();
        }
    };
    let mut entries = project_config::entries(&tree, SCHEDULES_DIR, ".yaml");
    if entries.len() > SCHEDULES_MAX {
        let refused: Vec<String> = entries
            .split_off(SCHEDULES_MAX)
            .into_iter()
            .map(|e| e.stem)
            .collect();
        tracing::warn!(
            "{owner}/{project} declares more than {SCHEDULES_MAX} schedules; refusing {refused:?}"
        );
    }

    let mut loaded = Vec::with_capacity(entries.len());
    for entry in entries {
        match read_one(repos, owner, project, &head, &entry.stem).await {
            Ok(schedule) => loaded.push(schedule),
            Err(reason) => tracing::warn!(
                "{owner}/{project} schedule '{}' is invalid and will not fire: {reason}",
                entry.stem
            ),
        }
    }
    loaded
}

async fn head_of_default_branch(
    repos: &RepoManager,
    owner: &str,
    project: &str,
) -> vcs::Result<String> {
    let branch = repos.default_branch(owner, project).await?;
    repos.resolve_ref(owner, project, &branch).await
}

/// One schedule file at `head`, or the reason it is skipped: the §1.1 field
/// rules, the §14 skew gate, and the target job type's existence and work type.
async fn read_one(
    repos: &RepoManager,
    owner: &str,
    project: &str,
    head: &str,
    stem: &str,
) -> Result<Schedule, String> {
    let relative = format!("{SCHEDULES_DIR}/{stem}.yaml");
    let file = project_config::read_file(repos, owner, project, head, &relative)
        .await
        .map_err(|e| format!("read failed: {e}"))?
        .ok_or_else(|| format!("{relative} vanished between listing and reading"))?;
    let schedule = Schedule::parse(&file.content).map_err(|e| format!("parse error: {e}"))?;

    let errors = schedule.validate(stem);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "));
    }
    if let Some(needed) = schedule.requires_dispatcher(CONFIG_SCHEMA_EPOCH) {
        return Err(format!(
            "requires dispatcher schema epoch >= {needed}, this one is at {CONFIG_SCHEMA_EPOCH}"
        ));
    }
    for warning in schedule.config_warnings() {
        tracing::warn!("{owner}/{project} schedule '{stem}': {warning}");
    }

    let target = target_work_type(repos, owner, project, head, &schedule.job_type).await?;
    let errors = schedule.validate_against_target(target);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "));
    }
    Ok(schedule)
}

/// The work type of the job type a schedule fires, checked at the same HEAD
/// (design #310 Decision 6): a schedule naming a type that does not exist there
/// is invalid at load and stops firing.
async fn target_work_type(
    repos: &RepoManager,
    owner: &str,
    project: &str,
    head: &str,
    job_type: &str,
) -> Result<types::WorkType, String> {
    let relative = format!("jobs/{job_type}.yaml");
    let file = project_config::read_file(repos, owner, project, head, &relative)
        .await
        .map_err(|e| format!("reading {relative} failed: {e}"))?
        .ok_or_else(|| format!("job_type '{job_type}' has no {relative} at HEAD"))?;
    JobType::parse(&file.content)
        .map(|jt| jt.work.r#type)
        .map_err(|e| format!("job_type '{job_type}' does not parse: {e}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn schedule(name: &str) -> Schedule {
        Schedule::parse(&format!(
            "name: {name}\njob_type: code\ncron: '0 2 * * *'\ndescription: run it\n"
        ))
        .unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    /// A refresh keeps what the decider reads: `first_seen_at` must not move
    /// under a schedule that has never fired (it would re-arm forever), and a
    /// reported skip must stay reported.
    #[test]
    fn a_refresh_preserves_in_memory_state_and_drops_deleted_schedules() {
        let mut table = ScheduleTable::new();
        let first = now() - chrono::Duration::hours(3);
        merge(&mut table, vec![schedule("nightly")], first);
        table.get_mut("nightly").unwrap().last_skipped_occurrence = Some(first);

        merge(
            &mut table,
            vec![schedule("nightly"), schedule("weekly")],
            now(),
        );
        let nightly = &table["nightly"];
        assert_eq!(nightly.first_seen_at, first);
        assert_eq!(nightly.last_skipped_occurrence, Some(first));
        assert!(table["weekly"].first_seen_at > first);

        merge(&mut table, vec![schedule("weekly")], now());
        assert!(
            !table.contains_key("nightly"),
            "a deleted file stops firing"
        );
        assert_eq!(table.len(), 1);
    }

    /// An edited file keeps its state: re-arming on every edit would let a
    /// schedule someone tunes daily never reach an occurrence.
    #[test]
    fn an_edited_schedule_keeps_its_anchor() {
        let mut table = ScheduleTable::new();
        let first = now() - chrono::Duration::days(1);
        merge(&mut table, vec![schedule("nightly")], first);
        let edited = Schedule::parse(
            "name: nightly\njob_type: code\ncron: '0 3 * * *'\ndescription: run it\n",
        )
        .unwrap();

        merge(&mut table, vec![edited], now());
        assert_eq!(table["nightly"].schedule.cron, "0 3 * * *");
        assert_eq!(table["nightly"].first_seen_at, first);
    }
}

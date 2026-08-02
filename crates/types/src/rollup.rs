//! The **derived** reads over [`Job::groups`](crate::job::Job::groups) and the
//! `docs/design/` registry (design #321 Decision 4 and 7, slice B).
//!
//! Nothing here is ever stored. A group exists because a job says so, and every
//! count, member list and enumeration is one pass over the project's job
//! records at read time — no bucket, no reverse index, no startup rebuild, and
//! so nothing that can disagree with the records because there is nothing else
//! to disagree. The types live in `types` rather than being assembled with
//! `serde_json::json!` in the dispatcher so the §6.2 contract is generated from
//! them (`chuggernaut schema api`) instead of hand-mirrored in TypeScript.
//!
//! The two shapes answer two different questions and are deliberately not one
//! endpoint: [`GroupEntry`] is member-derived ("what groups exist, and how are
//! their members doing"), [`DesignEntry`] is repo-derived ("what designs exist,
//! and how are they doing"). A design nobody has filed a job against yet is a
//! row only the second can carry.
//!
//! - **Accepts:** a project's [`Job`] records, and the head of a design
//!   document's text.
//! - **Emits:** [`GroupRollup`] per distinct group name, [`DesignDocHead`] per
//!   document, and the two reply rows built from them.
//! - **Guarantees:** pure and total — no I/O, no async, no panics. Derivation
//!   only: no aggregate is stored, `open` is computed with
//!   [`JobState::is_terminal`] so it cannot drift, and a zero-count state is
//!   omitted rather than emitted as `0`. Document parsing reads at most
//!   [`DOC_HEAD_LINES_MAX`] lines and keeps at most [`DOC_STATUS_LEN_MAX`]
//!   characters of the status line, **verbatim and unparsed**.
//! - **Spec:** §1.1 (`groups`), §6.2 (`GET .../groups`, `GET .../designs`).

use crate::groups::{DESIGN_DOC_DIR, DESIGN_GROUP_PREFIX};
use crate::job::{Job, JobState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Most design documents one `GET .../designs` enumerates (STYLE.md Tier 2 #3).
/// Eight exist today; the bound is what stops a repo that grows a
/// `docs/design/` of thousands from turning one read into thousands of blob
/// reads. Documents past it are dropped from the reply and logged, never
/// silently — a truncated listing that reads as complete is the failure worth
/// avoiding.
pub const DESIGNS_MAX: usize = 128;

/// How far into a document [`design_doc_head`] looks for the title and the
/// status line. Both live in the opening lines by convention; scanning a 60 KB
/// body to discover a `Status:` that is not there is cost with no answer.
pub const DOC_HEAD_LINES_MAX: usize = 32;

/// Longest `doc_status` served, in characters. The status is display text the
/// platform compares to nothing, so a long line is truncated rather than
/// refused — unlike a group *name*, where truncation would invent a second
/// group (see [`crate::groups`]).
pub const DOC_STATUS_LEN_MAX: usize = 120;

/// The line prefix a design document's status is written behind (design #321
/// Decision 8). Matched at the start of a line, case-sensitively, exactly as
/// all eight documents in the tree write it.
pub const DOC_STATUS_PREFIX: &str = "Status:";

/// One member of a group, as the group views render it: a state badge and a
/// title, with the job page one click away. The same four fields the jobs list
/// projection leads with — deliberately not the whole record, which the roll-up
/// has no use for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GroupJob {
    pub id: u64,
    pub r#type: String,
    pub title: String,
    pub state: JobState,
}

/// A group and how its members are doing — the shape both derived reads carry,
/// so a design's roll-up and a group's roll-up are the same thing rendered
/// twice rather than two shapes to keep in step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GroupRollup {
    /// The label the members carry, verbatim.
    pub name: String,
    /// The members, in ascending job seq.
    pub jobs: Vec<GroupJob>,
    /// Per-state histogram keyed by the state name serde writes, zero states
    /// omitted. Not a percentage: "5 Done, 1 Frozen" is the operator's actual
    /// question, and a percentage discards *which* one is not done.
    pub counts: BTreeMap<String, usize>,
    /// Members that are not terminal, via [`JobState::is_terminal`] — the same
    /// definition batches and the roll-up's staleness flag already use.
    pub open: usize,
}

/// A row of `GET .../groups`: the roll-up, plus the design document the name
/// conventionally refers to when one is there.
///
/// `doc_path`/`doc_status` are best-effort and present only for a
/// `design/`-namespaced name that resolves to a document at default-branch
/// HEAD — the knowledge-tag posture (spec §4.4: a tag with no file is skipped).
/// A group whose document is absent still lists; it just renders without a
/// status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GroupEntry {
    #[serde(flatten)]
    pub group: GroupRollup,
    /// Where the document was found, so a reader fetching it back through
    /// `GET .../file` never re-derives the path (the `req.tags.list` posture).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_path: Option<String>,
    /// The document's status line, verbatim and unparsed (design #321
    /// Decision 8). The platform compares it to nothing and infers nothing
    /// from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_status: Option<String>,
}

/// A row of `GET .../designs`: a document under `docs/design/`, joined to the
/// group its jobs carry. Repo-derived, so a design with **no** jobs is a row —
/// which is exactly the row `GET .../groups` cannot represent, and the one the
/// operator most needs to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DesignEntry {
    /// Repo-relative path at default-branch HEAD, e.g.
    /// `docs/design/321-job-groups.md`.
    pub path: String,
    /// The basename without `.md` — the stem a `design/` group name embeds.
    pub slug: String,
    /// The leading `<seq>-` of the slug when the document follows the naming
    /// convention, `None` when it does not. A convention, not a rule: the path
    /// is the identity (design #321 Decision 2, correction 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The `# …` heading, falling back to the slug.
    pub title: String,
    /// The document's status line, verbatim and unparsed. Absent when the
    /// document has none — six of the eight in the tree read `PROPOSED`, one
    /// `DRAFT`, one `FINDING`, under no schema and no enforcement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The design has a member that did not write it, **every** member is
    /// terminal, and the status line is non-empty — so the text beside this
    /// flag may no longer describe the design. The design's own authoring job
    /// is excluded from the first half: a design belongs to its own group
    /// (design #321 Decision 4), so counting it would call every design stale
    /// the moment the job that wrote it landed, with no implementation work
    /// having happened at all.
    ///
    /// Reported, never acted on: the repo stays the source of truth for a
    /// design's status and the operator resolves a discrepancy with an
    /// ordinary `design` amendment job (design #321 Decision 8). Deliberately
    /// not a machine-checked `implemented` — that needs the front-matter
    /// vocabulary, which is #86's to define.
    pub status_stale: bool,
    /// The group this design's jobs carry (`design/{slug}`), rolled up
    /// identically to a `GET .../groups` row. Empty for a design nobody has
    /// filed a job against.
    #[serde(flatten)]
    pub group: GroupRollup,
}

/// What [`design_doc_head`] reads out of a document's opening lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesignDocHead {
    /// The first `# …` heading, without its marker.
    pub title: Option<String>,
    /// The first `Status:` line's remainder, trimmed and bounded.
    pub status: Option<String>,
}

impl GroupRollup {
    /// A group with no members — the shape a design nobody has ticketed rolls
    /// up to. Never produced by [`group_rollups`], which derives names *from*
    /// the jobs, so an empty group is unrepresentable there by construction.
    #[must_use]
    pub fn empty(name: String) -> Self {
        Self {
            name,
            jobs: Vec::new(),
            counts: BTreeMap::new(),
            open: 0,
        }
    }

    /// Record one member: the projection, its state's tally, and the open
    /// count. Revoked members are members — no cascade and no cleanup touches a
    /// group (design #321 Decision 5), so a revoked job still lists and is
    /// still counted, under `Revoked`.
    fn add(&mut self, job: &Job) {
        self.jobs.push(GroupJob {
            id: job.id,
            r#type: job.r#type.clone(),
            title: job.title.clone(),
            state: job.state,
        });
        *self
            .counts
            .entry(job.state.as_str().to_string())
            .or_insert(0) += 1;
        if !job.state.is_terminal() {
            self.open += 1;
        }
    }
}

/// Every group the project's jobs name, keyed by name, each with its roll-up.
///
/// The set is `distinct(job.groups)` over the project — a group exists because
/// a job says so — so this is the whole enumeration and there is no registry to
/// consult (design #321 Decision 7). One pass over the values the dispatcher
/// already holds; the map is ordered, so the reply's group order is a property
/// of the names rather than of hash iteration.
#[must_use]
pub fn group_rollups<'a>(jobs: impl IntoIterator<Item = &'a Job>) -> BTreeMap<String, GroupRollup> {
    let mut rollups: BTreeMap<String, GroupRollup> = BTreeMap::new();
    let mut jobs: Vec<&Job> = jobs.into_iter().collect();
    jobs.sort_by_key(|job| job.id);
    for job in jobs {
        for name in &job.groups {
            rollups
                .entry(name.clone())
                .or_insert_with(|| GroupRollup::empty(name.clone()))
                .add(job);
        }
    }
    debug_assert!(
        rollups.values().all(|g| !g.jobs.is_empty()),
        "a derived group always has the member that named it"
    );
    rollups
}

/// The design document's slug for a `docs/design/*.md` path, or `None` for
/// anything else — the inverse of
/// [`design_doc_path`](crate::groups::design_doc_path), and pinned to it by
/// `slug_and_path_round_trip`. Nested and extension-less paths are not designs:
/// the directory is flat, exactly as the group namespace it mirrors is.
#[must_use]
pub fn design_slug(path: &str) -> Option<&str> {
    let slug = path.strip_prefix(DESIGN_DOC_DIR)?.strip_suffix(".md")?;
    (!slug.is_empty() && !slug.contains('/') && !slug.starts_with('.')).then_some(slug)
}

/// The group name a design document's jobs carry: `docs/design/321-job-groups.md`
/// → `design/321-job-groups`. The join key of the two derived reads, in the one
/// place the convention is implemented on this side.
#[must_use]
pub fn design_group_name(slug: &str) -> String {
    format!("{DESIGN_GROUP_PREFIX}{slug}")
}

/// The `<seq>` a slug leads with (`321-job-groups` → `321`), or `None` when it
/// does not follow the convention. Not an identity — the path is (design #321
/// Decision 2) — but it is what the Designs view sorts and labels by.
#[must_use]
pub fn design_seq(slug: &str) -> Option<u64> {
    let digits = slug.split('-').next()?;
    (!digits.is_empty() && digits.len() <= 19)
        .then(|| digits.parse::<u64>().ok())
        .flatten()
}

/// The title and status line out of a document's opening
/// [`DOC_HEAD_LINES_MAX`] lines.
///
/// The status is the remainder of the **first** line starting with
/// [`DOC_STATUS_PREFIX`], trimmed and truncated to [`DOC_STATUS_LEN_MAX`]
/// characters, surfaced verbatim: no vocabulary is parsed out of it and none is
/// defined here (design #321 Decision 8). An empty remainder is no status.
#[must_use]
pub fn design_doc_head(text: &str) -> DesignDocHead {
    let mut head = DesignDocHead::default();
    for line in text.lines().take(DOC_HEAD_LINES_MAX) {
        if head.title.is_none()
            && let Some(title) = line.strip_prefix("# ")
            && !title.trim().is_empty()
        {
            head.title = Some(title.trim().to_string());
        }
        if head.status.is_none()
            && let Some(status) = line.strip_prefix(DOC_STATUS_PREFIX)
        {
            let status: String = status.trim().chars().take(DOC_STATUS_LEN_MAX).collect();
            if !status.is_empty() {
                head.status = Some(status);
            }
        }
        if head.title.is_some() && head.status.is_some() {
            break;
        }
    }
    head
}

impl DesignEntry {
    /// Join one document to its group. The title falls back to the slug (a
    /// document with no heading still needs a label), and `status_stale` is the
    /// discrepancy the platform reports and never resolves: a member other than
    /// the design's own authoring job exists, none of the members is open, and
    /// the document still says something.
    ///
    /// The authoring job is the member whose seq **is** the design's number —
    /// `#321`'s doc is written by job #321, which then joins `design/321-…`
    /// like any other member. Excluding it is why this lives here rather than
    /// on [`GroupRollup`], which must stay design-agnostic: an ordinary group
    /// has no authoring job to exclude. A design with no seq in its slug, or
    /// one whose authoring job predates groups and is simply absent, has no
    /// member to exclude either — every member counts, and the group can go
    /// stale on the strength of its implementation jobs alone.
    ///
    /// The open half deliberately still counts the authoring job: a design
    /// whose own job is in flight is work in flight, not a stale document.
    #[must_use]
    pub fn new(path: String, slug: &str, head: DesignDocHead, group: GroupRollup) -> Self {
        debug_assert_eq!(
            design_slug(&path),
            Some(slug),
            "a design entry's slug is its path's"
        );
        let seq = design_seq(slug);
        let implemented = group.jobs.iter().any(|job| Some(job.id) != seq);
        debug_assert!(
            !implemented || !group.jobs.is_empty(),
            "an implemented design has members"
        );
        let status_stale = implemented && group.open == 0 && head.status.is_some();
        Self {
            path,
            seq,
            title: head.title.unwrap_or_else(|| slug.to_string()),
            status: head.status,
            status_stale,
            slug: slug.to_string(),
            group,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::groups::design_doc_path;

    /// A blank record carrying only what a roll-up reads: seq, type, title,
    /// state and groups.
    fn job(id: u64, state: JobState, groups: &[&str]) -> Job {
        Job {
            id,
            project: "acme/api".into(),
            r#type: "code".into(),
            title: format!("job {id}"),
            description: String::new(),
            cover_html: None,
            deps: vec![],
            members: vec![],
            batch_id: None,
            state,
            branch: format!("job/{id}"),
            base_ref: None,
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            inputs: BTreeMap::new(),
            groups: groups.iter().map(|g| (*g).to_string()).collect(),
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: "2026-07-30T09:00:00Z".parse().unwrap(),
            ready_at: None,
            completed_at: None,
            task_time_ms: None,
        }
    }

    fn rollup(jobs: &[Job], name: &str) -> GroupRollup {
        group_rollups(jobs)
            .remove(name)
            .unwrap_or_else(|| panic!("no group {name}"))
    }

    /// The whole enumeration: names come from the jobs, a job in two groups
    /// appears under both, and the histogram omits the states nobody is in.
    #[test]
    fn groups_are_derived_from_their_members() {
        let jobs = vec![
            job(1, JobState::Done, &["design/311-job-inputs"]),
            job(2, JobState::Frozen, &["design/311-job-inputs", "beacon"]),
            job(3, JobState::Work, &[]),
        ];
        let rollups = group_rollups(&jobs);
        assert_eq!(
            rollups.keys().collect::<Vec<_>>(),
            vec!["beacon", "design/311-job-inputs"],
            "the group set is distinct(job.groups), in name order"
        );

        let inputs = &rollups["design/311-job-inputs"];
        assert_eq!(
            inputs.jobs.iter().map(|j| j.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            inputs.counts,
            BTreeMap::from([("Done".into(), 1), ("Frozen".into(), 1)]),
            "zero states are omitted, not emitted as 0"
        );
        assert_eq!(inputs.open, 1);
        assert_eq!(rollups["beacon"].jobs.len(), 1);
        assert_eq!(rollups["beacon"].open, 1);
    }

    /// A finished group: every member terminal, so nothing is open — the input
    /// to the staleness flag.
    #[test]
    fn a_group_whose_members_are_all_terminal_has_nothing_open() {
        let jobs = vec![
            job(1, JobState::Done, &["design/293-worker-capacity"]),
            job(2, JobState::Done, &["design/293-worker-capacity"]),
        ];
        let group = rollup(&jobs, "design/293-worker-capacity");
        assert_eq!(group.open, 0);
        assert_eq!(group.counts, BTreeMap::from([("Done".into(), 2)]));
    }

    /// A revoked member still lists and is still counted: revoking a job does
    /// not touch the group (design #321 Decision 5 — no cascade, no cleanup).
    #[test]
    fn a_revoked_member_still_lists_under_its_group() {
        let jobs = vec![
            job(1, JobState::Revoked, &["beacon-import"]),
            job(2, JobState::Ready, &["beacon-import"]),
        ];
        let group = rollup(&jobs, "beacon-import");
        assert_eq!(group.jobs.len(), 2);
        assert_eq!(
            group.counts,
            BTreeMap::from([("Revoked".into(), 1), ("Ready".into(), 1)])
        );
        assert_eq!(group.open, 1, "Revoked is terminal, so it is not open");
    }

    /// A group with no document is an ordinary group — the name resolves to no
    /// design, and the roll-up is unaffected.
    #[test]
    fn a_group_name_that_resolves_to_no_doc_still_rolls_up() {
        let jobs = vec![job(1, JobState::Work, &["ops/fleet-refresh"])];
        let group = rollup(&jobs, "ops/fleet-refresh");
        assert_eq!(group.open, 1);
        assert_eq!(design_doc_path("ops/fleet-refresh"), None);
    }

    /// The path→slug→name→path round trip, which is what lets the two derived
    /// reads join on a name derived from opposite directions.
    #[test]
    fn slug_and_path_round_trip() {
        for slug in ["321-job-groups", "169-knowledge", "no-seq"] {
            let path = format!("{DESIGN_DOC_DIR}{slug}.md");
            assert_eq!(design_slug(&path), Some(slug));
            assert_eq!(
                design_doc_path(&design_group_name(slug)).as_deref(),
                Some(path.as_str())
            );
        }
        for other in [
            "docs/design/nested/deep.md",
            "docs/design/.hidden.md",
            "docs/design/README",
            "docs/runbooks/deploy.md",
            "docs/design/.md",
        ] {
            assert_eq!(design_slug(other), None, "{other} names no design");
        }
    }

    /// The seq is read off the convention when it is followed and is `None`
    /// when it is not — never an error, and never invented.
    #[test]
    fn seq_is_read_off_the_convention_or_absent() {
        assert_eq!(design_seq("321-job-groups"), Some(321));
        assert_eq!(design_seq("86"), Some(86));
        for slug in [
            "job-groups",
            "v2-plan",
            "-leading",
            "99999999999999999999-x",
        ] {
            assert_eq!(design_seq(slug), None, "{slug} carries no seq");
        }
    }

    /// The title and the verbatim status line, and the bounds on both: only
    /// the head is scanned, and a long status is truncated rather than refused.
    #[test]
    fn doc_head_reads_the_title_and_the_verbatim_status() {
        let head = design_doc_head("# Design #321 — Job groups\n\nStatus: PROPOSED. Written…\n");
        assert_eq!(head.title.as_deref(), Some("Design #321 — Job groups"));
        assert_eq!(
            head.status.as_deref(),
            Some("PROPOSED. Written…"),
            "the remainder is surfaced verbatim, not parsed into a vocabulary"
        );

        let deep = format!(
            "# T\n{}Status: LATE\n",
            "filler\n".repeat(DOC_HEAD_LINES_MAX)
        );
        assert_eq!(
            design_doc_head(&deep).status,
            None,
            "only the head is scanned"
        );

        let long = format!("Status: {}", "x".repeat(DOC_STATUS_LEN_MAX * 2));
        assert_eq!(
            design_doc_head(&long).status.map(|s| s.chars().count()),
            Some(DOC_STATUS_LEN_MAX)
        );
    }

    /// A document with no `Status:` line, and one with an empty remainder: both
    /// are simply status-less, and a status-less design is never stale.
    #[test]
    fn a_doc_with_no_status_line_is_never_stale() {
        for text in ["# Just a title\n\nbody\n", "# T\nStatus:   \n", ""] {
            let head = design_doc_head(text);
            assert_eq!(head.status, None, "{text:?} carries no status");
            let jobs = vec![job(1, JobState::Done, &["design/x"])];
            let entry = DesignEntry::new(
                "docs/design/x.md".into(),
                "x",
                head,
                rollup(&jobs, "design/x"),
            );
            assert!(!entry.status_stale, "no status is nothing to be stale");
        }
    }

    /// A design nobody has filed a job against is a row with an empty roll-up —
    /// the row `GET .../groups` cannot represent — and it is not stale, because
    /// "every member is terminal" is vacuous with no members.
    #[test]
    fn a_design_with_no_jobs_is_a_row_with_an_empty_rollup() {
        let rollups = group_rollups(&[job(1, JobState::Done, &["beacon"])]);
        assert!(!rollups.contains_key("design/313-workload-identity"));

        let entry = DesignEntry::new(
            "docs/design/313-workload-identity.md".into(),
            "313-workload-identity",
            design_doc_head("# Design #313\n\nStatus: PROPOSED\n"),
            GroupRollup::empty(design_group_name("313-workload-identity")),
        );
        assert_eq!(entry.seq, Some(313));
        assert_eq!(entry.title, "Design #313");
        assert_eq!(entry.status.as_deref(), Some("PROPOSED"));
        assert!(entry.group.jobs.is_empty());
        assert_eq!(entry.group.counts, BTreeMap::new());
        assert!(
            !entry.status_stale,
            "a design with no members is not a stale one"
        );
    }

    /// The flag the whole staleness feature is: a member that did not write the
    /// document, all terminal, and a status line that still says something. One
    /// open member clears it. (Neither #1 nor #2 is design #311's own job, so
    /// both count as implementation.)
    #[test]
    fn status_is_stale_when_every_member_is_terminal() {
        let done = vec![
            job(1, JobState::Done, &["design/311-job-inputs"]),
            job(2, JobState::Revoked, &["design/311-job-inputs"]),
        ];
        let entry = DesignEntry::new(
            "docs/design/311-job-inputs.md".into(),
            "311-job-inputs",
            design_doc_head("# Design #311\nStatus: PROPOSED\n"),
            rollup(&done, "design/311-job-inputs"),
        );
        assert!(entry.status_stale);
        assert_eq!(entry.status.as_deref(), Some("PROPOSED"), "raw text beside");

        let mut open = done;
        open.push(job(3, JobState::Work, &["design/311-job-inputs"]));
        let entry = DesignEntry::new(
            "docs/design/311-job-inputs.md".into(),
            "311-job-inputs",
            design_doc_head("# Design #311\nStatus: PROPOSED\n"),
            rollup(&open, "design/311-job-inputs"),
        );
        assert!(!entry.status_stale, "an open member is work in flight");
    }

    /// The design entry for `slug` against the group its jobs derive, with a
    /// document that always carries a status — so the only thing under test is
    /// which members count.
    fn design_entry(slug: &str, jobs: &[Job]) -> DesignEntry {
        let name = design_group_name(slug);
        let group = group_rollups(jobs)
            .remove(&name)
            .unwrap_or_else(|| GroupRollup::empty(name));
        DesignEntry::new(
            format!("{DESIGN_DOC_DIR}{slug}.md"),
            slug,
            design_doc_head("# A design\nStatus: PROPOSED\n"),
            group,
        )
    }

    /// A design's own authoring job is not implementation of it. #333 grouped
    /// every design job under its own design (Decision 4), so a design with
    /// only that member is the shape of one the day it merges — and counting it
    /// called every design in the repo stale with no work having happened.
    #[test]
    fn a_design_whose_only_member_is_its_authoring_job_is_not_stale() {
        let authored = design_entry(
            "310-scheduled-jobs",
            &[job(310, JobState::Done, &["design/310-scheduled-jobs"])],
        );
        assert_eq!(
            authored.group.jobs.len(),
            1,
            "the authoring job is a member"
        );
        assert_eq!(authored.group.open, 0);
        assert!(
            !authored.status_stale,
            "the job that wrote the doc did not implement it"
        );
    }

    /// …and one non-authoring member is all it takes, once every member —
    /// authoring job included — is terminal.
    #[test]
    fn a_design_goes_stale_on_its_first_terminal_non_authoring_member() {
        let mut jobs = vec![job(293, JobState::Done, &["design/293-worker-capacity"])];
        jobs.push(job(298, JobState::Done, &["design/293-worker-capacity"]));
        assert!(
            design_entry("293-worker-capacity", &jobs).status_stale,
            "seven non-authoring members Done is the case #337 is about"
        );

        jobs.push(job(304, JobState::Work, &["design/293-worker-capacity"]));
        assert!(
            !design_entry("293-worker-capacity", &jobs).status_stale,
            "an open member is work in flight"
        );
    }

    /// The two boundaries of the exclusion: a group whose authoring job is
    /// absent entirely still goes stale on its implementation jobs, and the
    /// `open` half still counts the authoring job.
    #[test]
    fn only_a_member_whose_seq_is_the_designs_own_is_excluded() {
        assert!(
            design_entry(
                "293-worker-capacity",
                &[job(298, JobState::Done, &["design/293-worker-capacity"])]
            )
            .status_stale,
            "a design whose own job predates groups has no member to exclude"
        );
        assert!(
            design_entry("scratch", &[job(1, JobState::Done, &["design/scratch"])]).status_stale,
            "a slug with no seq names no authoring job either"
        );
        assert!(
            !design_entry(
                "321-job-groups",
                &[
                    job(321, JobState::Work, &["design/321-job-groups"]),
                    job(325, JobState::Done, &["design/321-job-groups"]),
                ]
            )
            .status_stale,
            "the authoring job still counts as open: an amendment in flight"
        );
    }

    /// Both reply rows flatten the roll-up, so a design's group and a group row
    /// carry `name`/`jobs`/`counts`/`open` at the same level — one shape for
    /// the UI, whichever read it came from.
    #[test]
    fn both_rows_serialize_the_rollup_flat() {
        let jobs = vec![job(7, JobState::Done, &["design/321-job-groups"])];
        let group = rollup(&jobs, "design/321-job-groups");
        let entry = GroupEntry {
            group: group.clone(),
            doc_path: design_doc_path("design/321-job-groups"),
            doc_status: Some("PROPOSED".into()),
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["name"], "design/321-job-groups");
        assert_eq!(v["counts"]["Done"], 1);
        assert_eq!(v["open"], 0);
        assert_eq!(v["jobs"][0]["state"], "Done");
        assert_eq!(v["doc_path"], "docs/design/321-job-groups.md");
        assert_eq!(
            serde_json::from_value::<GroupEntry>(v).unwrap(),
            entry,
            "the flattened row round-trips"
        );

        let design = DesignEntry::new(
            "docs/design/321-job-groups.md".into(),
            "321-job-groups",
            design_doc_head("# Design #321\nStatus: PROPOSED\n"),
            group,
        );
        let v = serde_json::to_value(&design).unwrap();
        assert_eq!(v["name"], "design/321-job-groups");
        assert_eq!(v["counts"]["Done"], 1);
        assert_eq!(v["status_stale"], true);
        assert_eq!(serde_json::from_value::<DesignEntry>(v).unwrap(), design);
    }

    /// An ungrouped project derives nothing at all — the common case, and the
    /// one that must not invent a group.
    #[test]
    fn an_ungrouped_project_derives_no_groups() {
        let jobs = vec![job(1, JobState::Done, &[]), job(2, JobState::Work, &[])];
        assert!(group_rollups(&jobs).is_empty());
    }
}

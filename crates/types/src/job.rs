//! Job record and state machine states (spec §1.1, §2.1).

use crate::channel::ChannelUpdate;
use crate::job_type::Evaluator;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A node in the project DAG. Stored in NATS KV at `jobs.{owner}.{project}.{seq}`;
/// the dispatcher is its sole writer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Job {
    /// Sequential per project; maintained via counter in NATS KV.
    pub id: u64,
    /// `"{owner}/{repo}"` slug.
    pub project: String,
    /// Job type name; references `.chug/jobs/{type}.yaml` at `base_ref`.
    pub r#type: String,
    /// Ticket-style instance identity: what this particular run is for.
    /// The type carries the *how* (prompts, evaluators); title/description
    /// carry the *what*, and are injected into work and eval prompts as the
    /// job brief (§4.3). Empty for jobs whose prompt is self-contained.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Optional rich, richly-formatted cover page for the operator UI (spec
    /// §1.1, §4.3). Purely presentational: unlike [`Job::description`], it is
    /// **never** injected into any agent prompt — the job brief consumes only
    /// title/description, so the cover can carry HTML the UI sandboxes without
    /// polluting what agents read. Authors should ship self-contained styling
    /// (no external scripts/network). None for records that predate it and for
    /// jobs with a plain-text brief; defaulted so older records deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_html: Option<String>,
    /// Upstream job ids this job depends on. Edges are ordering: upstreams
    /// must be Done first (their work is in this job's base, their structured
    /// results are available to it). Plain ids, no named roles — picked at
    /// creation, validated (existence, no cycles) at release.
    #[serde(default)]
    pub deps: Vec<u64>,
    /// Member job ids absorbed into this batch (design-lifecycle.md, spec §2.1
    /// batches). Empty for an ordinary job; non-empty marks this job as a
    /// **batch** — one branch implementing all members, evaluated under the
    /// union of their criteria, whose single merge completes every member.
    /// Serde-defaulted so records written before batches deserialize.
    #[serde(default)]
    pub members: Vec<u64>,
    /// Set on a member job absorbed into a batch: the batch job's id. `Some`
    /// implies the job is (or was) [`JobState::Batched`] under that batch;
    /// cleared when the batch is revoked/fails and the member returns to Frozen.
    /// None for ordinary jobs and batches themselves; defaulted for old records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<u64>,
    pub state: JobState,
    /// `"job/{id}"`; set at creation; actual git branch created when job enters Work.
    pub branch: String,
    /// Exact HEAD of default branch; set/updated at every Ready-transition and on
    /// squash-merge conflict; None until job first enters Ready.
    pub base_ref: Option<String>,
    /// Union of job type defaults and operator-supplied tags at creation.
    pub knowledge_tags: Vec<String>,
    /// Additive per-job evaluators (design-lifecycle.md): layered on top of the
    /// type's `eval:` list at execution. The type's evaluators are a floor —
    /// creation can add criteria, never remove or override them. Name
    /// collisions with the type's evaluators are a release-time error.
    #[serde(default)]
    pub eval: Vec<Evaluator>,
    /// Optional per-job work-task timeout override (duration string, §1.1).
    /// Layers over the job type's `resources.task_timeout` exactly like [`eval`]
    /// layers over the type's evaluators — but for Work tasks only; evaluators
    /// keep the type default. Any valid duration (shorter or longer). Parseability
    /// is validated at release, consistent with "wiring validated at release, not
    /// creation". None means the type default applies.
    #[serde(default)]
    pub timeout: Option<String>,
    /// Optional per-job model override for the Work agent (spec §1.1, §12.4).
    /// The most specific choice an operator can make, so it wins over every
    /// other layer: the job type's `work.model`, the project default
    /// (`.chug/jobs/_defaults.yaml`), and the platform default (`AGENT_MODEL_DEFAULT`).
    /// Applies to Work-phase agent tasks only — evaluators keep the
    /// type/project/platform resolution, exactly as [`Job::timeout`] scopes to
    /// Work. None → the resolution chain applies. Defaulted for records written
    /// before per-job model selection existed.
    #[serde(default)]
    pub model: Option<String>,
    /// The job's **effective** input values (spec §1.1 `inputs:`, design #311).
    /// Empty for every job whose type declares no inputs, which is every job
    /// that predates the feature — defaulted so old records deserialize, and
    /// skipped on the wire when empty so such a record is byte-identical to what
    /// it is today.
    ///
    /// A `BTreeMap` for deterministic ordering (like [`crate::JobType::unknown`]):
    /// the map is an audit surface, and a stable order is what makes two records
    /// comparable.
    ///
    /// **Written by exactly two paths**, both on the single-writer dispatcher:
    /// creation (and the Draft edit, which is the same act repeated), and the
    /// Ready-transition that *first* records [`Job::base_ref`], which fills in a
    /// declared `default` for every input the creator did not supply — add-only,
    /// never overwriting a supplied value. From that moment this is the complete
    /// effective set: what the run acted on, beside the config version it acted
    /// under. **Immutable thereafter** — not on rework, not on a work retry, not
    /// on a claim, and not across a later `base_ref` update (a re-resolved
    /// default would make the target mutable mid-flight). Getting a different
    /// target is getting a different job.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    /// What this job is **part of** (spec §1.1 `groups:`, design #321): the
    /// operator's own labels — `design/311-job-inputs`, `beacon-import` — so a
    /// group of jobs can be enumerated and rolled up. Many per job; shape-checked
    /// by [`crate::groups::check_groups`] and nothing else. Empty for every job
    /// nobody grouped, which is every job that predates the field — defaulted so
    /// old records deserialize, skipped on the wire when empty so such a record
    /// is byte-identical to what it is today.
    ///
    /// **Inert to execution** (design #321 Decision 3), and that is a property
    /// rather than an intention: no container env, no agent prompt, no job-type
    /// resolution and no state transition reads it, pinned by
    /// `groups_never_reach_the_job_brief` (`dispatcher::exec`) and the tier-2
    /// byte-identical-env trace. That is what makes the third write path safe —
    /// unlike every other field here, `groups` is **mutable in every state,
    /// including `Done` and `Revoked`** (`req.jobs.groups.*`): annotating a
    /// finished ticket with what it was part of does not change what it did.
    ///
    /// Deliberately *not* a knowledge tag, whose resolution at `base_ref` changes
    /// what the agent is told; deliberately not a dep, which is ordering; and
    /// deliberately carrying no aggregate — every count and every enumeration is
    /// derived from the job records at read time (Decision 4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// A human has claimed the job's NEXT work attempt (spec §1.2 claims):
    /// instead of launching a container, the dispatcher parks that attempt as
    /// a Pending task with the declared kind and `performed_by: human`, then
    /// clears this flag — a claim covers exactly one attempt. Defaults false
    /// so records that predate claims deserialize.
    #[serde(default)]
    pub claim_next: bool,
    /// Structured record of the job's most recent escalation or stall (spec
    /// §1.2, §3.4): the reason code, a human-readable detail, the failing task
    /// (when one exists), and when it happened — so operators see WHY on the
    /// record instead of reconstructing it from dispatcher logs. Written at
    /// every escalate/stall call site; advisory, no transition consults it.
    /// None until the job first escalates; defaulted so older records
    /// deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<Escalation>,
    /// Factory name when created by a factory triage agent (spec §13); None for
    /// operator-created jobs.
    pub factory: Option<String>,
    /// Schedule name when an occurrence of `.chug/schedules/{name}.yaml`
    /// created this job (spec §1.1 schedules); None for every other origin,
    /// written only by the origination path, immutable after creation.
    ///
    /// It is also the key the at-most-one-in-flight rule reads — the most
    /// recent job carrying a schedule's name IS that schedule's anchor, so no
    /// last-fired state is stored anywhere (spec §1.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set once (immutably) when job first enters Ready; anchor for `job_deadline`.
    pub ready_at: Option<DateTime<Utc>>,
    /// When the job reached a terminal state (Done or Revoked). Set once by the
    /// dispatcher's single state-write path at the terminal transition and never
    /// cleared — so the jobs list can show completion time and duration without
    /// opening the job. None while the job is still live; defaulted so records
    /// written before completion stamping existed deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// How long the job spent **working**: the sum of its own tasks' spans, per
    /// [`crate::task::task_time_ms`]. Carried on the record — not derived by the
    /// UI — so the jobs list can show a job's duration without a per-row task
    /// fetch, and recomputed from that one job's tasks whenever one of them is
    /// written back, so a missed write self-heals instead of drifting.
    ///
    /// Distinct from `completed_at - created_at`, which is dominated by the
    /// waiting a job does while Frozen and Blocked. None while no task of the
    /// job carries a usable span, and on records written before the field
    /// existed — a `Some(0)` genuinely means "took no measurable time".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_time_ms: Option<u64>,
}

/// The jobs-**list** projection (spec §6.1): every [`Job`] field except the two
/// heavy prose ones, `description` and `cover_html`.
///
/// Those two dominate the payload — on the dogfood project they are 78% of a
/// 578 KB list reply — and no list consumer reads either: the operator table
/// renders title/type/state, and its search matches id/title/type only. They
/// stay on the single-job reply, which is where the UI renders them.
///
/// Serialize-only by design: this is a wire shape, never a stored record, and
/// nothing should be able to round-trip a summary back into a [`Job`] with an
/// empty description. `job_summary_mirrors_job_fields` pins the field set, so
/// a field added to [`Job`] fails the build until someone decides whether the
/// list should carry it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct JobSummary<'a> {
    pub id: u64,
    pub project: &'a str,
    pub r#type: &'a str,
    pub title: &'a str,
    pub deps: &'a [u64],
    pub members: &'a [u64],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<u64>,
    pub state: JobState,
    pub branch: &'a str,
    pub base_ref: Option<&'a str>,
    pub knowledge_tags: &'a [String],
    pub eval: &'a [Evaluator],
    pub timeout: Option<&'a str>,
    pub model: Option<&'a str>,
    /// The job's effective inputs ([`Job::inputs`]). Carried by the list because
    /// they are what a parameterized job *is* — a `deploy` row that does not say
    /// which service it deploys is unreadable — and bounded small by
    /// construction (at most `INPUTS_COUNT_MAX` short values), unlike the prose
    /// fields the projection drops.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: &'a BTreeMap<String, String>,
    /// What the job is part of ([`Job::groups`]). Carried by the list because
    /// the list is where filtering and the group chips happen (design #321
    /// Decision 7), and bounded small by construction (at most
    /// `GROUPS_COUNT_MAX` × `GROUP_NAME_LEN_MAX` bytes), unlike the prose fields
    /// the projection drops.
    ///
    /// Skipped when empty, matching the record rather than the projection's
    /// other list fields: the two shapes must stay *assignable* in the generated
    /// client (`web/src/api/types.gen.ts`), where the job page hands a full
    /// `Job` to code typed on `JobSummary`. A required field here against an
    /// optional one there is exactly the mismatch that breaks.
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pub groups: &'a [String],
    pub claim_next: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalation: Option<&'a Escalation>,
    pub factory: Option<&'a str>,
    /// The schedule that created the job ([`Job::schedule`]). Carried by the
    /// list because trigger provenance is what tells a scheduled run apart from
    /// an operator's, and it is one short name rather than prose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<&'a str>,
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_time_ms: Option<u64>,
    /// The job's latest channel post, for the muted progress line the operator
    /// table shows under a live job's title. Not a [`Job`] field — it lives in
    /// the `channels` bucket — and the one deliberate *addition* the projection
    /// makes over the record.
    ///
    /// Carrying it here is what lets a cold page load stop replaying the
    /// project's entire event history just to learn what a handful of live jobs
    /// are doing. Only populated for non-terminal jobs; None otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<&'a ChannelUpdate>,
}

/// Fields [`JobSummary`] adds that [`Job`] does not have, pinned by
/// `job_summary_mirrors_job_fields` so the mirror check stays honest as the
/// projection grows.
pub const JOB_SUMMARY_EXTRA_FIELDS: &[&str] = &["channel"];

impl<'a> From<&'a Job> for JobSummary<'a> {
    fn from(job: &'a Job) -> Self {
        Self {
            id: job.id,
            project: &job.project,
            r#type: &job.r#type,
            title: &job.title,
            deps: &job.deps,
            members: &job.members,
            batch_id: job.batch_id,
            state: job.state,
            branch: &job.branch,
            base_ref: job.base_ref.as_deref(),
            knowledge_tags: &job.knowledge_tags,
            eval: &job.eval,
            timeout: job.timeout.as_deref(),
            model: job.model.as_deref(),
            inputs: &job.inputs,
            groups: &job.groups,
            claim_next: job.claim_next,
            escalation: job.escalation.as_ref(),
            factory: job.factory.as_deref(),
            schedule: job.schedule.as_deref(),
            created_at: job.created_at,
            ready_at: job.ready_at,
            completed_at: job.completed_at,
            task_time_ms: job.task_time_ms,
            channel: None,
        }
    }
}

impl<'a> JobSummary<'a> {
    /// Attach the job's latest channel post (see [`JobSummary::channel`]).
    /// Terminal jobs keep None — nothing is making progress to report.
    pub fn with_channel(mut self, update: Option<&'a ChannelUpdate>) -> Self {
        if !self.state.is_terminal() {
            self.channel = update;
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum JobState {
    /// Editable pre-release draft (spec §2.1): the job's definition can be
    /// iterated on (PATCH .../jobs/{seq}) before it enters the DAG for real.
    /// Invisible to scheduling, holds no branch, cannot be claimed. Leaves via
    /// release (→ Ready/Blocked) or revoke; a Frozen never-released job may be
    /// moved back here (`POST .../draft`). Once released, never editable again.
    Draft,
    Frozen,
    /// Absorbed into a batch (spec §2.1 batches): the member's changes will be
    /// produced on the batch's single branch and its completion fans out from
    /// the batch merge. Invisible to scheduling, holds no branch, cannot be
    /// claimed or released — like Draft. Leaves via Batched→Done (batch merged),
    /// Batched→Frozen (batch revoked/failed; re-batchable), or Revoked.
    Batched,
    Blocked,
    Ready,
    Work,
    Evaluation,
    /// Eval passed; the job is landing (merge queue → merge gate → squash).
    /// Only `wrap_up: merge` jobs enter here; `wrap_up: none` goes
    /// Evaluation→Done directly (spec §2.1, §3.3).
    WrapUp,
    /// Post-work human intervention: automation ran out after work executed.
    /// Resolved Retry/Resolve/Revoke.
    Escalated,
    /// Pre-work human intervention: the job could not start or become ready
    /// (config re-validation failed, or its deadline elapsed while still
    /// Ready). No work task exists. Resolved Retry/Revoke only — Resolve is
    /// rejected (spec §1.2 pre-Work escalations).
    Stalled,
    Done,
    Revoked,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(self, JobState::Done | JobState::Revoked)
    }

    /// The state's name on the wire — the same string serde writes, pinned to
    /// it by `state_names_match_serde`. For the places a state is a *key*
    /// rather than a value (the group roll-up's histogram, `crate::rollup`),
    /// where a second spelling would be a second vocabulary.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Draft => "Draft",
            JobState::Frozen => "Frozen",
            JobState::Batched => "Batched",
            JobState::Blocked => "Blocked",
            JobState::Ready => "Ready",
            JobState::Work => "Work",
            JobState::Evaluation => "Evaluation",
            JobState::WrapUp => "WrapUp",
            JobState::Escalated => "Escalated",
            JobState::Stalled => "Stalled",
            JobState::Done => "Done",
            JobState::Revoked => "Revoked",
        }
    }
}

impl Job {
    /// True when this job is a batch: it absorbs [`members`](Job::members) and
    /// produces one branch for all of them (spec §2.1 batches).
    pub fn is_batch(&self) -> bool {
        !self.members.is_empty()
    }
}

/// What a `POST jobs` asks for (spec §3.1 step 1, §6.2): the definition a job
/// is born with, before any state the dispatcher stamps on it. It lives here
/// rather than in the dispatcher because the pure authoring decider
/// (`chuggernaut_domain::decide::authoring`) reads it and cannot see
/// `dispatcher::core`, and because `types` is the crate that lets `api`
/// eventually type the create body it forwards opaquely today.
///
/// The owner/project the request targets are *not* fields here for long: the
/// authoring view carries them (refactor-plan F1b).
pub struct CreateSpec {
    pub owner: String,
    pub project: String,
    pub r#type: String,
    /// Ticket-style identity: what this run is for (optional, empty = none).
    pub title: String,
    pub description: String,
    /// Optional rich cover page for the UI (spec §1.1, §4.3). Purely
    /// presentational — never enters any agent prompt. None = no cover.
    pub cover_html: Option<String>,
    pub deps: Vec<u64>,
    /// Member job seqs to absorb into a **batch** (spec §2.1 batches). Empty for
    /// an ordinary job; when non-empty this request creates a batch that pulls
    /// each member Frozen→Batched, unions their deps and evaluators, and lands
    /// one branch for all of them. Validated at creation (unlike per-job wiring,
    /// which defers to release) — a batch is a structural act over existing
    /// jobs, not a definition to iterate on.
    pub members: Vec<u64>,
    pub knowledge_tags: Vec<String>,
    /// Additive per-job evaluators; validated (field rules + name collisions
    /// against the type's list) at release, not creation.
    pub eval: Vec<Evaluator>,
    /// Optional per-job work-task timeout override (duration string, §1.1);
    /// parseability validated at release. None → the type default applies.
    pub timeout: Option<String>,
    /// Optional per-job Work agent model override (§12.4); wins over the job
    /// type, project, and platform defaults. None → the resolution chain applies.
    pub model: Option<String>,
    /// The values this job supplies for its type's declared `inputs:` (spec §1.1,
    /// design #311). Shape-checked before it gets here (the 422 at the wire edge);
    /// whether each name is *declared* and each value satisfies its declaration is
    /// release-time, like every other wiring question. Empty for every job type
    /// that declares no inputs.
    pub inputs: BTreeMap<String, String>,
    /// What this job is part of ([`Job::groups`], design #321). Shape-checked
    /// before it gets here (the 422 at the wire edge); there is no release-time
    /// pass, because a group has no declaration to be checked against. Empty for
    /// every job the creator did not group — most of them, and the label can be
    /// added later in any state.
    pub groups: Vec<String>,
    pub factory: Option<String>,
    /// The schedule whose occurrence is creating this job ([`Job::schedule`],
    /// design #310). Set only by the dispatcher's own origination path — an
    /// operator's `POST jobs` never carries it.
    pub schedule: Option<String>,
    /// Land the job in [`JobState::Draft`] instead of Frozen (spec §2.1): its
    /// definition can be edited (the dispatcher's `update_job`) before release.
    /// Default false preserves today's behavior (created jobs land Frozen).
    pub draft: bool,
}

/// The composition of a batch derived from its member list (spec §2.1): the
/// union of the members' external deps and their additive evaluators. Computed
/// by `chuggernaut_domain::decide::authoring::plan_batch` and committed onto the
/// batch record at create (non-draft) or at finalize/release (draft).
#[derive(Debug)]
pub struct BatchComposition {
    pub deps: Vec<u64>,
    pub eval: Vec<Evaluator>,
}

/// Why the dispatcher escalated (→Escalated) or stalled (→Stalled) a job,
/// carried on the job record so operators diagnose from what the API serves
/// rather than from dispatcher logs (spec §1.2, §3.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Escalation {
    /// Machine reason code, matching the `job-escalated`/`job-stalled` event
    /// reason (e.g. `launch_validation_failed`, `work_retries_exhausted`,
    /// `eval_abort`, `job_deadline_exceeded`).
    pub reason: String,
    /// Human-readable explanation — the same text shown in the operator's
    /// intervention task prompt.
    pub detail: String,
    /// The task whose failure triggered the escalation, when one exists. None
    /// for pre-work escalations that fail before any task runs (launch
    /// validation, a deadline that elapsed while still Ready) and for
    /// evaluation-phase escalations with no single culprit task. Defaulted so
    /// records without it deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_task: Option<u64>,
    /// When the escalation was recorded.
    pub at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn job_round_trips_spec_example() {
        let json = r#"{
          "id": 42,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [11, 22],
          "state": "Frozen",
          "branch": "job/42",
          "base_ref": null,
          "knowledge_tags": ["rust", "rest-api", "payments/stripe-integration"],
          "factory": null,
          "created_at": "2026-04-05T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.id, 42);
        assert_eq!(job.state, JobState::Frozen);
        assert_eq!(job.deps, vec![11, 22]);
        assert_eq!(job.timeout, None);
        assert_eq!(job.model, None);
        assert!(!job.claim_next);
        let back = serde_json::to_string(&job).unwrap();
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }

    /// [`JobState::as_str`] and serde are two statements of one vocabulary;
    /// this is what keeps them one. A histogram key that drifted from the
    /// `state` field beside it would have the UI render a state twice under two
    /// spellings.
    #[test]
    fn state_names_match_serde() {
        for state in [
            JobState::Draft,
            JobState::Frozen,
            JobState::Batched,
            JobState::Blocked,
            JobState::Ready,
            JobState::Work,
            JobState::Evaluation,
            JobState::WrapUp,
            JobState::Escalated,
            JobState::Stalled,
            JobState::Done,
            JobState::Revoked,
        ] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(state.as_str()),
                "{state:?} spells itself two ways"
            );
            assert_eq!(
                serde_json::from_value::<JobState>(serde_json::json!(state.as_str())).unwrap(),
                state
            );
        }
    }

    #[test]
    fn job_round_trips_with_timeout_override() {
        let json = r#"{
          "id": 7,
          "project": "acme/api",
          "type": "deploy",
          "deps": [],
          "state": "Frozen",
          "branch": "job/7",
          "base_ref": null,
          "knowledge_tags": [],
          "timeout": "45m",
          "factory": null,
          "created_at": "2026-07-20T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.timeout.as_deref(), Some("45m"));
        let back = serde_json::to_string(&job).unwrap();
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }

    #[test]
    fn job_round_trips_in_draft_state() {
        let json = r#"{
          "id": 9,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Draft",
          "branch": "job/9",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.state, JobState::Draft);
        assert!(!job.state.is_terminal());
        let back = serde_json::to_string(&job).unwrap();
        assert!(back.contains(r#""state":"Draft""#));
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }

    #[test]
    fn job_round_trips_with_escalation_and_stays_backward_compat() {
        let json = r#"{
          "id": 9,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Frozen",
          "branch": "job/9",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.escalation, None);
        assert!(!serde_json::to_string(&job).unwrap().contains("escalation"));

        let at = "2026-07-22T11:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let mut escalated = job.clone();
        escalated.escalation = Some(Escalation {
            reason: "launch_validation_failed".into(),
            detail: "Job 9 failed launch-time validation: missing secret".into(),
            failing_task: None,
            at,
        });
        let back = serde_json::to_string(&escalated).unwrap();
        assert!(back.contains("launch_validation_failed"));
        assert!(!back.contains("failing_task"));
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), escalated);

        escalated.escalation.as_mut().unwrap().failing_task = Some(4);
        let back = serde_json::to_string(&escalated).unwrap();
        assert!(back.contains("\"failing_task\":4"));
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), escalated);
    }

    #[test]
    fn job_round_trips_with_completed_at_and_stays_backward_compat() {
        let json = r#"{
          "id": 5,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Done",
          "branch": "job/5",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.completed_at, None);
        assert!(
            !serde_json::to_string(&job)
                .unwrap()
                .contains("completed_at")
        );

        let at = "2026-07-22T12:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let mut finished = job.clone();
        finished.completed_at = Some(at);
        let back = serde_json::to_string(&finished).unwrap();
        assert!(back.contains("\"completed_at\":\"2026-07-22T12:30:00Z\""));
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), finished);
    }

    #[test]
    fn job_round_trips_with_task_time_and_stays_backward_compat() {
        let json = r#"{
          "id": 6,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Done",
          "branch": "job/6",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.task_time_ms, None);
        assert!(
            !serde_json::to_string(&job)
                .unwrap()
                .contains("task_time_ms")
        );

        for ms in [0u64, 18 * 60 * 1000] {
            let mut timed = job.clone();
            timed.task_time_ms = Some(ms);
            let back = serde_json::to_string(&timed).unwrap();
            assert!(back.contains(&format!("\"task_time_ms\":{ms}")));
            assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), timed);
        }
    }

    #[test]
    fn job_round_trips_as_batch_and_member() {
        let json = r#"{
          "id": 20,
          "project": "acme/api",
          "type": "web",
          "deps": [3],
          "members": [11, 12, 13],
          "state": "Frozen",
          "branch": "job/20",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let batch: Job = serde_json::from_str(json).unwrap();
        assert!(batch.is_batch());
        assert_eq!(batch.members, vec![11, 12, 13]);
        assert_eq!(batch.batch_id, None);
        let back = serde_json::to_string(&batch).unwrap();
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), batch);

        let member_json = r#"{
          "id": 11,
          "project": "acme/api",
          "type": "web",
          "deps": [],
          "batch_id": 20,
          "state": "Batched",
          "branch": "job/11",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let member: Job = serde_json::from_str(member_json).unwrap();
        assert_eq!(member.state, JobState::Batched);
        assert!(!member.state.is_terminal());
        assert!(!member.is_batch());
        assert_eq!(member.batch_id, Some(20));
        let back = serde_json::to_string(&member).unwrap();
        assert!(back.contains(r#""state":"Batched""#));
        assert!(back.contains(r#""batch_id":20"#));
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), member);
    }

    #[test]
    fn old_record_without_batch_fields_stays_compat() {
        let json = r#"{
          "id": 1,
          "project": "acme/api",
          "type": "code",
          "deps": [],
          "state": "Frozen",
          "branch": "job/1",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert!(job.members.is_empty());
        assert!(!job.is_batch());
        assert_eq!(job.batch_id, None);
        assert!(!serde_json::to_string(&job).unwrap().contains("batch_id"));
    }

    #[test]
    fn job_round_trips_with_cover_html_and_stays_backward_compat() {
        let json = r#"{
          "id": 3,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Draft",
          "branch": "job/3",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-22T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.cover_html, None);
        assert!(!serde_json::to_string(&job).unwrap().contains("cover_html"));

        let mut rich = job.clone();
        rich.cover_html = Some("<h1>Ship it</h1>".into());
        let back = serde_json::to_string(&rich).unwrap();
        assert!(back.contains("\"cover_html\":\"<h1>Ship it</h1>\""));
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), rich);
    }

    /// The §1.1 `inputs` field is additive (design #311 Skew surface 2): a record
    /// written before it deserializes to an empty map and stays keyless on the
    /// wire, so no epoch moves for the *record* and no old job's bytes change.
    #[test]
    fn job_round_trips_with_inputs_and_stays_backward_compat() {
        let json = r#"{
          "id": 12,
          "project": "acme/api",
          "type": "rollback",
          "deps": [],
          "state": "Frozen",
          "branch": "job/12",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-29T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert!(job.inputs.is_empty());
        assert!(!serde_json::to_string(&job).unwrap().contains("inputs"));

        let mut parameterized = job.clone();
        parameterized.inputs = BTreeMap::from([
            ("sha".to_string(), "4f9c1ab".to_string()),
            ("service".to_string(), "web".to_string()),
        ]);
        let back = serde_json::to_string(&parameterized).unwrap();
        assert!(
            back.contains(r#""inputs":{"service":"web","sha":"4f9c1ab"}"#),
            "{back}"
        );
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), parameterized);
    }

    /// The §1.1 `groups` field is additive in both directions (design #321
    /// Skew surface 1): a record written before it deserializes to an empty list
    /// and stays keyless on the wire, so no old job's bytes change and no epoch
    /// moves. The populated case round-trips in the operator's own order —
    /// `groups` is a set the write paths keep unique, not a sorted list.
    #[test]
    fn job_round_trips_with_groups_and_stays_backward_compat() {
        let json = r#"{
          "id": 14,
          "project": "acme/api",
          "type": "code",
          "deps": [],
          "state": "Done",
          "branch": "job/14",
          "base_ref": null,
          "knowledge_tags": [],
          "factory": null,
          "created_at": "2026-07-30T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert!(job.groups.is_empty());
        assert!(!serde_json::to_string(&job).unwrap().contains("groups"));

        let mut grouped = job.clone();
        grouped.groups = vec!["design/311-job-inputs".into(), "beacon-import".into()];
        let back = serde_json::to_string(&grouped).unwrap();
        assert!(
            back.contains(r#""groups":["design/311-job-inputs","beacon-import"]"#),
            "{back}"
        );
        assert_eq!(serde_json::from_str::<Job>(&back).unwrap(), grouped);
        assert!(grouped.state.is_terminal());
    }

    #[test]
    fn job_round_trips_with_model_override() {
        let json = r#"{
          "id": 8,
          "project": "acme/api",
          "type": "implement-endpoint",
          "deps": [],
          "state": "Frozen",
          "branch": "job/8",
          "base_ref": null,
          "knowledge_tags": [],
          "model": "claude-fable-5",
          "factory": null,
          "created_at": "2026-07-21T10:00:00Z",
          "ready_at": null
        }"#;
        let job: Job = serde_json::from_str(json).unwrap();
        assert_eq!(job.model.as_deref(), Some("claude-fable-5"));
        let back = serde_json::to_string(&job).unwrap();
        let again: Job = serde_json::from_str(&back).unwrap();
        assert_eq!(job, again);
    }

    /// A [`Job`] with every optional field populated, so nothing is dropped by a
    /// `skip_serializing_if` and the key sets below compare the *declared*
    /// fields rather than whichever happened to be `Some`.
    fn job_with_every_field_set() -> Job {
        Job {
            id: 7,
            project: "acme/api".into(),
            r#type: "code".into(),
            title: "title".into(),
            description: "the ticket body".into(),
            cover_html: Some("<p>cover</p>".into()),
            deps: vec![1, 2],
            members: vec![3],
            batch_id: Some(4),
            state: JobState::Work,
            branch: "job/7".into(),
            base_ref: Some("abc123".into()),
            knowledge_tags: vec!["rust".into()],
            eval: Vec::new(),
            timeout: Some("30m".into()),
            model: Some("claude-opus-5".into()),
            inputs: BTreeMap::from([("service".to_string(), "web".to_string())]),
            groups: vec!["design/321-job-groups".into()],
            claim_next: true,
            escalation: Some(Escalation {
                reason: "work_retries_exhausted".into(),
                detail: "three attempts failed".into(),
                failing_task: Some(2),
                at: "2026-07-24T10:00:00Z".parse().unwrap(),
            }),
            factory: Some("triage".into()),
            schedule: Some("nightly-integration".into()),
            created_at: "2026-07-24T09:00:00Z".parse().unwrap(),
            ready_at: Some("2026-07-24T09:30:00Z".parse().unwrap()),
            completed_at: Some("2026-07-24T11:00:00Z".parse().unwrap()),
            task_time_ms: Some(18 * 60 * 1000),
        }
    }

    /// The list projection must stay a *complete* mirror of [`Job`] minus the
    /// two prose fields. This is the guard that makes that true over time: add a
    /// field to `Job` and this fails until someone decides whether the jobs list
    /// should carry it. Without it the omission would be invisible — the field
    /// would just quietly stop reaching the operator UI's table.
    #[test]
    fn job_summary_mirrors_job_fields() {
        use std::collections::BTreeSet;

        let job = job_with_every_field_set();
        let keys = |v: &serde_json::Value| -> BTreeSet<String> {
            v.as_object()
                .expect("job shapes serialize as objects")
                .keys()
                .cloned()
                .collect()
        };

        let update = ChannelUpdate {
            message: "running the gate".into(),
            percent: Some(40),
            at: Some("2026-07-24T10:30:00Z".parse().unwrap()),
            origin: Default::default(),
        };
        let full = keys(&serde_json::to_value(&job).unwrap());
        let summary = keys(
            &serde_json::to_value(JobSummary::from(&job).with_channel(Some(&update))).unwrap(),
        );

        let mut expected = full.clone();
        assert!(expected.remove("description"), "Job must have description");
        assert!(expected.remove("cover_html"), "Job must have cover_html");
        for extra in JOB_SUMMARY_EXTRA_FIELDS {
            expected.insert((*extra).to_string());
        }
        assert_eq!(
            summary, expected,
            "JobSummary must carry every Job field except description/cover_html \
             (plus JOB_SUMMARY_EXTRA_FIELDS) — a new Job field needs a deliberate \
             decision about whether the jobs list should carry it"
        );
    }

    /// The projection exists to drop the payload's bulk; pin that it actually
    /// does, so a future refactor can't reintroduce the prose fields by name.
    #[test]
    fn job_summary_omits_the_prose_fields() {
        let job = job_with_every_field_set();
        let json = serde_json::to_string(&JobSummary::from(&job)).unwrap();
        assert!(
            !json.contains("the ticket body"),
            "description leaked: {json}"
        );
        assert!(!json.contains("<p>cover</p>"), "cover_html leaked: {json}");
        assert!(json.contains("\"title\":\"title\""));
        assert!(json.contains("\"state\":\"Work\""));
    }
}

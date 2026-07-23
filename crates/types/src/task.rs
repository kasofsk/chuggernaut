//! Task log entries (spec §1.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unit of execution within a job's Work and Evaluation phases. Chronological log,
/// no task graph. Stored at `tasks.{owner}.{project}.{job_seq}.{task_id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Sequential within job, 1-indexed.
    pub id: u64,
    pub job_seq: u64,
    pub project: String,
    pub phase: TaskPhase,
    pub cycle: u32,
    pub kind: TaskKind,
    pub state: TaskState,
    /// 1-indexed; each retry is a new task record with attempt+1.
    pub attempt: u32,
    /// Evaluator name for Evaluation/MergeGate tasks; None for work and
    /// escalation tasks. Ties the task to its `eval:` declaration — restart
    /// reconciliation and the UI both need the mapping.
    #[serde(default)]
    pub evaluator: Option<String>,
    /// Human-facing label for the task, so every task kind is as
    /// self-describing as an evaluator (job #146). Set from the job-type config:
    /// a wrap-up task carries its `wrap_up.name` (or a derived default), and an
    /// evaluator task mirrors its `evaluator` name here so the UI reads one
    /// label field for both. None for work/escalation/triage tasks and for
    /// records written before labels existed (they fall back to `evaluator`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Evaluation stage this task belongs to (spec §3.3 staged evaluation).
    /// Carries the evaluator's `stage:` for Evaluation/MergeGate tasks so the
    /// UI can group a cycle's tasks by stage; 0 for work/escalation/triage
    /// tasks, which have no stage. Defaulted for records written before staging.
    #[serde(default)]
    pub stage: u32,
    /// Who actually performed this attempt when it differs from what `kind`
    /// declares (spec §1.2 claims): a claimed attempt keeps its declared kind
    /// — the job type's immutable requirement — and records the human
    /// performer here. Absent (None) for every normally-executed attempt and
    /// for records written before claims existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performed_by: Option<Performer>,
    /// Backend-assigned container ID (Docker or k8s); None for Human tasks.
    /// Persisted the instant the container launches — while the task is still
    /// Running — and kept after exit, so operators and artifact tooling can name
    /// a live container, not just a finished one.
    pub container_id: Option<String>,
    /// Why this task is `Pending`, when the reason is worth surfacing (spec
    /// §3.5). Set to [`PendingReason::QueuedForCapacity`] when the capacity
    /// launch queue defers a container launch (no free fleet slot); cleared the
    /// instant the launch succeeds. Absent for a task Pending for any other
    /// reason — a parked human/claimed attempt, or a just-created task awaiting
    /// its first launch — so the UI can distinguish a queued launch from an
    /// idle Pending. Defaulted + skipped so records written before it existed
    /// still deserialize and non-queued tasks carry no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<PendingReason>,
    /// When this launch first joined the capacity queue (spec §3.5), stamped
    /// alongside [`Self::pending_reason`] and cleared on launch. Persisted so
    /// the queue survives a dispatcher restart *faithfully*: reconciliation
    /// re-queues Pending launches sorted by this timestamp (stable FIFO across
    /// restarts, not reconcile iteration order), and the max-queue-wait backstop
    /// measures the total wait from it rather than from process-local time —
    /// under frequent auto-deploys the in-memory clock would otherwise reset
    /// every restart and never fire. None for non-queued tasks. Defaulted so
    /// pre-existing records still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<DateTime<Utc>>,
    /// Why a rework cycle created this Work task (spec §3.3): set at rework
    /// re-entry so a Work task appearing after passed evaluations is
    /// self-explaining. None for cycle-1 work, evaluation/gate/wrap-up tasks,
    /// and every non-Work task. Defaulted so records written before rework
    /// causes were recorded still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rework_reason: Option<ReworkReason>,
    /// Set when reconciliation retired this attempt because its container was
    /// gone at restart (spec §3.6): docker pruned it, the node rebooted, colima
    /// restarted. That is an infrastructure loss, NOT a real nonzero exit — the
    /// relaunch does not consume a `work_retries`/`eval_retries` budget, and
    /// these markers are counted to cap infra relaunches per task before
    /// escalating (`infra_loss`). Defaulted so records written before it existed
    /// still deserialize; false for every real failure and completion.
    #[serde(default)]
    pub infra_loss: bool,
    /// Agent tasks only: the session id handed to the agent CLI, which names
    /// its transcript. Recorded at task creation so the artifact stays
    /// addressable across a dispatcher restart, and so a later cycle can resume
    /// the conversation.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Evaluation/MergeGate tasks only: the branch tip SHA this evaluator round
    /// judged, resolved at launch (spec §3.3, job #155). Persisted so a later
    /// cycle's re-review can show the reviewer "what you reviewed last time" and
    /// compute the `last_reviewed_tip..HEAD` delta — and so that context
    /// survives a dispatcher restart (rebuilt from the task log, not memory).
    /// None for work/escalation/triage tasks and pre-#155 records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_tip: Option<String>,
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskPhase {
    Work,
    Evaluation,
    /// Merge-gate re-run of required command evaluators against the candidate
    /// squash commit (spec §3.3 Merge Gate). Only present when the default
    /// branch HEAD moved past `base_ref` while the job was in flight.
    MergeGate,
    /// Post-merge wrap-up command (spec §3.2, design-lifecycle.md wrap-up hook):
    /// a `wrap_up.run` command task launched *after* the squash lands on the
    /// default branch, run against the merged main content. Its existence in the
    /// task log is the restart-reconciliation marker that the merge already
    /// landed and only the publish remains (§3.6). A non-zero exit escalates —
    /// the merge is already final.
    WrapUp,
    /// Operator-dispatched triage (spec §1.2): an advisory agent run over the
    /// whole job state that produces a written assessment + recommendation.
    /// Purely advisory — it never drives a job transition. Only created while
    /// the job is Escalated or Stalled, and may be repeated.
    Triage,
    /// A Human escalation task (spec §1.2, §3.4): the operator-facing decision
    /// item created when automation exhausts a phase's budget. Stamped with its
    /// own phase — not the phase that failed — so the UI never reads an
    /// escalation resolution as a `Work · pass` row (job #141). Records written
    /// before this existed carry escalations under `Work`; the UI tolerates
    /// both (a Human result with an `action` is an escalation regardless).
    Escalation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TaskKind {
    Command {
        run: String,
    },
    Agent {
        provider: String,
        model: Option<String>,
        prompt: String,
    },
    Human {
        prompt: String,
    },
}

/// The actual performer of a claimed attempt (spec §1.2). Only `Human` exists:
/// normal execution is implied by absence, so the field never restates `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Performer {
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Pending,
    Running,
    Done,
    Failed,
}

/// Why a task sits `Pending` when the reason is worth showing an operator
/// (spec §3.5). Kept a distinct enum rather than a bool so later parked-reasons
/// (e.g. awaiting a claim) can join without another field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingReason {
    /// The capacity launch queue deferred this container launch: the fleet had
    /// no free slot, so the launch waits rather than failing (§3.5). No retry
    /// budget is consumed while it waits.
    QueuedForCapacity,
}

/// Cause of a rework-created Work task (spec §3.3). Mirrors the
/// `job-rework-started` event's `reason`, but persisted on the task record so
/// the tasks list explains itself — a Work task after passed evaluations is no
/// longer a mystery — without event-stream archaeology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReworkReason {
    /// Evaluation failed and rework budget remained (§3.3 product failure).
    EvalFailure,
    /// A squash-merge conflict against a default branch that moved while the
    /// job was in flight (§3.2 step 12).
    MergeConflict,
    /// A required command evaluator failed at the merge gate (§3.3 merge gate).
    GateCiFailure,
    /// The merge gate failed on a **compile/build** stage only (spec §3.3
    /// gate-fix fast path, job #154): an already-approved branch that no longer
    /// compiles after rebasing onto moved main. A narrowly-scoped fix task that
    /// returns straight to the merge gate — no re-review, no eval-phase CI.
    GateCompileFix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TaskResult {
    Work {
        summary: Option<String>,
        structured: Option<serde_json::Value>,
        token_usage: Option<TokenUsage>,
        /// Optional agent-authored HTML cover page for this result (spec §1.1,
        /// §4.3, job #143). Purely presentational — a visual changelog/before-
        /// after the operator UI renders in a sandboxed frame beside the text
        /// `summary`. Sanitized (size-capped) at ingest; **never** enters the
        /// squash body or any downstream prompt, and its absence is never
        /// penalized. Defaults None so pre-#143 records still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cover_html: Option<String>,
    },
    Command {
        pass: bool,
        exit_code: i32,
        output: String,
        structured: Option<serde_json::Value>,
    },
    Agent {
        pass: bool,
        /// Eval verdict "not satisfiable by rework" (design-lifecycle.md):
        /// implies `pass: false`; a required evaluator's abort skips the
        /// remaining rework budget and escalates.
        #[serde(default)]
        abort: bool,
        structured: Option<serde_json::Value>,
        token_usage: Option<TokenUsage>,
        /// Optional agent-authored HTML cover page for an evaluator's verdict
        /// summary (job #143), same semantics as [`TaskResult::Work::cover_html`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cover_html: Option<String>,
    },
    Human {
        pass: bool,
        /// Same abort semantics as `Agent`; set via `TaskResolution::Fail`.
        #[serde(default)]
        abort: bool,
        structured: Option<serde_json::Value>,
        action: Option<EscalationAction>,
        operator: String,
        resolved_at: DateTime<Utc>,
        /// Work-task Pass only: the operator's completion summary, carried from
        /// `TaskResolution::Pass::summary` so the Reports thread renders
        /// human-completed work like an agent's closing summary. Defaults None
        /// so pre-summary records still deserialize.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Result of an operator-dispatched triage task (spec §1.2). The agent's
    /// written assessment + recommendation, captured from the CLI JSON result
    /// text (triage runs without the channel MCP, so there is no `submit_result`).
    /// Advisory only — never consulted by any state transition.
    Triage {
        assessment: String,
        token_usage: Option<TokenUsage>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscalationAction {
    Retry,
    Resolve,
    Revoke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

/// Operator submission for Human tasks (spec §1.2). Adjacent tagging: `kind`
/// discriminates. `structured` is required (non-null) on `Fail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TaskResolution {
    Pass {
        structured: Option<serde_json::Value>,
        /// Work-task Pass only: the human's completion summary, flowing into
        /// the squash-merge commit body exactly like an agent's
        /// `submit_result` summary (spec §1.2 claims). Ignored on evaluator
        /// and escalation resolutions.
        #[serde(default)]
        summary: Option<String>,
    },
    Fail {
        structured: serde_json::Value,
        /// Human evaluator's "not satisfiable by rework" (design-lifecycle.md);
        /// only meaningful on evaluator tasks, ignored elsewhere.
        #[serde(default)]
        abort: bool,
    },
    Escalation {
        action: EscalationAction,
        structured: Option<serde_json::Value>,
    },
}

/// Rework context passed to the next work cycle (spec §3.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    pub evaluator: String,
    pub pass: bool,
    pub structured: Option<serde_json::Value>,
    /// A command evaluator's captured output tail (#167): the failure evidence
    /// (compiler/test stderr) threaded into the rework brief and #155's
    /// re-review, since a `command` result carries no structured findings.
    /// `None` for agent evaluators (which report through structured findings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_resolution_matches_spec_wire_format() {
        let cases = [
            r#"{ "kind": "Pass", "structured": null }"#,
            r#"{ "kind": "Fail", "structured": { "notes": "auth check failed" } }"#,
            r#"{ "kind": "Escalation", "action": "Retry", "structured": null }"#,
        ];
        for c in cases {
            let _: TaskResolution = serde_json::from_str(c).unwrap();
        }
        // abort defaults false on the wire; explicit abort round-trips.
        let r: TaskResolution =
            serde_json::from_str(r#"{ "kind": "Fail", "structured": {} }"#).unwrap();
        assert_eq!(
            r,
            TaskResolution::Fail {
                structured: serde_json::json!({}),
                abort: false
            }
        );
        let r: TaskResolution =
            serde_json::from_str(r#"{ "kind": "Fail", "structured": {}, "abort": true }"#).unwrap();
        assert_eq!(
            r,
            TaskResolution::Fail {
                structured: serde_json::json!({}),
                abort: true
            }
        );

        // summary defaults None on Pass; explicit summary round-trips.
        let r: TaskResolution =
            serde_json::from_str(r#"{ "kind": "Pass", "structured": null }"#).unwrap();
        assert_eq!(
            r,
            TaskResolution::Pass {
                structured: None,
                summary: None
            }
        );
        let r: TaskResolution = serde_json::from_str(
            r#"{ "kind": "Pass", "structured": null, "summary": "did the thing" }"#,
        )
        .unwrap();
        assert_eq!(
            r,
            TaskResolution::Pass {
                structured: None,
                summary: Some("did the thing".into())
            }
        );

        let r: TaskResolution = serde_json::from_str(
            r#"{ "kind": "Escalation", "action": "Revoke", "structured": null }"#,
        )
        .unwrap();
        assert_eq!(
            r,
            TaskResolution::Escalation {
                action: EscalationAction::Revoke,
                structured: None
            }
        );
    }

    #[test]
    fn performed_by_defaults_absent_and_round_trips() {
        // Old task records (no performed_by key) deserialize to None.
        let json = r#"{
          "id": 1, "job_seq": 7, "project": "acme/api",
          "phase": "Work", "cycle": 1,
          "kind": { "kind": "Agent", "provider": "claude", "model": null, "prompt": "p.md" },
          "state": "Pending", "attempt": 1,
          "container_id": null, "result": null,
          "created_at": "2026-07-21T10:00:00Z", "started_at": null, "completed_at": null
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.performed_by, None);
        // Absent stays absent on the wire (skip_serializing_if).
        assert!(
            !serde_json::to_string(&task)
                .unwrap()
                .contains("performed_by")
        );

        // A claimed attempt round-trips as "human".
        let mut claimed = task.clone();
        claimed.performed_by = Some(Performer::Human);
        let json = serde_json::to_string(&claimed).unwrap();
        assert!(json.contains(r#""performed_by":"human""#));
        assert_eq!(serde_json::from_str::<Task>(&json).unwrap(), claimed);
    }

    #[test]
    fn rework_reason_defaults_absent_and_round_trips() {
        // Old task records (no rework_reason key) deserialize to None.
        let json = r#"{
          "id": 3, "job_seq": 7, "project": "acme/api",
          "phase": "Work", "cycle": 2,
          "kind": { "kind": "Agent", "provider": "claude", "model": null, "prompt": "p.md" },
          "state": "Running", "attempt": 1,
          "container_id": "fake/c1", "result": null,
          "created_at": "2026-07-21T10:00:00Z", "started_at": null, "completed_at": null
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.rework_reason, None);
        // Absent stays absent on the wire (skip_serializing_if).
        assert!(
            !serde_json::to_string(&task)
                .unwrap()
                .contains("rework_reason")
        );

        // Each cause round-trips through JSON.
        for reason in [
            ReworkReason::EvalFailure,
            ReworkReason::MergeConflict,
            ReworkReason::GateCiFailure,
        ] {
            let mut reworked = task.clone();
            reworked.rework_reason = Some(reason);
            let json = serde_json::to_string(&reworked).unwrap();
            assert!(json.contains("\"rework_reason\""));
            assert_eq!(serde_json::from_str::<Task>(&json).unwrap(), reworked);
        }
    }

    #[test]
    fn infra_loss_defaults_absent_and_round_trips() {
        // Old task records (no infra_loss key) deserialize to false.
        let json = r#"{
          "id": 1, "job_seq": 7, "project": "acme/api",
          "phase": "Work", "cycle": 1,
          "kind": { "kind": "Agent", "provider": "claude", "model": null, "prompt": "p.md" },
          "state": "Failed", "attempt": 1,
          "container_id": "gone", "result": null,
          "created_at": "2026-07-21T10:00:00Z", "started_at": null, "completed_at": null
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert!(!task.infra_loss);

        // A retired infra loss round-trips as true.
        let mut lost = task.clone();
        lost.infra_loss = true;
        let json = serde_json::to_string(&lost).unwrap();
        assert!(json.contains(r#""infra_loss":true"#));
        assert_eq!(serde_json::from_str::<Task>(&json).unwrap(), lost);
    }

    #[test]
    fn pending_reason_and_queued_at_default_absent_and_round_trip() {
        // Old task records (no pending_reason / queued_at keys) deserialize to
        // None — an idle Pending stays indistinguishable, as before.
        let json = r#"{
          "id": 1, "job_seq": 7, "project": "acme/api",
          "phase": "Work", "cycle": 1,
          "kind": { "kind": "Command", "run": "cargo test" },
          "state": "Pending", "attempt": 1,
          "container_id": null, "result": null,
          "created_at": "2026-07-21T10:00:00Z", "started_at": null, "completed_at": null
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.pending_reason, None);
        assert_eq!(task.queued_at, None);
        // Absent stays absent on the wire (skip_serializing_if).
        let out = serde_json::to_string(&task).unwrap();
        assert!(!out.contains("pending_reason"));
        assert!(!out.contains("queued_at"));

        // A capacity-queued launch round-trips both fields.
        let queued_at = "2026-07-22T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let mut queued = task.clone();
        queued.pending_reason = Some(PendingReason::QueuedForCapacity);
        queued.queued_at = Some(queued_at);
        let json = serde_json::to_string(&queued).unwrap();
        assert!(json.contains(r#""pending_reason":"QueuedForCapacity""#));
        assert!(json.contains(r#""queued_at":"2026-07-22T09:00:00Z""#));
        assert_eq!(serde_json::from_str::<Task>(&json).unwrap(), queued);
    }

    #[test]
    fn triage_phase_and_result_round_trip() {
        // TaskPhase::Triage round-trips.
        let p: TaskPhase = serde_json::from_str(r#""Triage""#).unwrap();
        assert_eq!(p, TaskPhase::Triage);
        assert_eq!(serde_json::to_string(&p).unwrap(), r#""Triage""#);

        // TaskPhase::WrapUp round-trips.
        let p: TaskPhase = serde_json::from_str(r#""WrapUp""#).unwrap();
        assert_eq!(p, TaskPhase::WrapUp);
        assert_eq!(serde_json::to_string(&p).unwrap(), r#""WrapUp""#);

        // TaskResult::Triage round-trips, with and without token usage.
        let with_usage = TaskResult::Triage {
            assessment: "Work failed on a missing migration; recommend Revoke.".into(),
            token_usage: Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
        };
        let json = serde_json::to_string(&with_usage).unwrap();
        assert!(json.contains(r#""kind":"Triage""#));
        assert_eq!(
            serde_json::from_str::<TaskResult>(&json).unwrap(),
            with_usage
        );

        let no_usage: TaskResult = serde_json::from_str(
            r#"{ "kind": "Triage", "assessment": "insufficient signal", "token_usage": null }"#,
        )
        .unwrap();
        assert_eq!(
            no_usage,
            TaskResult::Triage {
                assessment: "insufficient signal".into(),
                token_usage: None
            }
        );
    }

    #[test]
    fn human_result_summary_round_trips_and_stays_backward_compat() {
        // A resolved Human work-task result carries the operator's summary and
        // round-trips it.
        let resolved_at = "2026-07-22T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let with_summary = TaskResult::Human {
            pass: true,
            abort: false,
            structured: None,
            action: None,
            operator: "david".into(),
            resolved_at,
            summary: Some("Reworked the auth check and verified the fix.".into()),
        };
        let json = serde_json::to_string(&with_summary).unwrap();
        assert!(json.contains(r#""summary":"Reworked the auth check and verified the fix.""#));
        assert_eq!(
            serde_json::from_str::<TaskResult>(&json).unwrap(),
            with_summary
        );

        // Old stored results (no `summary` key) still deserialize to None, and
        // None is omitted on the wire (skip_serializing_if).
        let legacy: TaskResult = serde_json::from_str(
            r#"{ "kind": "Human", "pass": true, "structured": null, "action": null,
                 "operator": "david", "resolved_at": "2026-07-22T09:00:00Z" }"#,
        )
        .unwrap();
        assert_eq!(
            legacy,
            TaskResult::Human {
                pass: true,
                abort: false,
                structured: None,
                action: None,
                operator: "david".into(),
                resolved_at,
                summary: None,
            }
        );
        assert!(!serde_json::to_string(&legacy).unwrap().contains("summary"));
    }

    #[test]
    fn work_result_cover_html_round_trips_and_back_compat() {
        // A pre-#143 Work result (no cover_html key) deserializes to None, and
        // None is omitted on the wire (skip_serializing_if).
        let legacy = r#"{"kind":"Work","summary":"did it","structured":null,"token_usage":null}"#;
        let r: TaskResult = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            &r,
            TaskResult::Work {
                cover_html: None,
                ..
            }
        ));
        assert!(!serde_json::to_string(&r).unwrap().contains("cover_html"));

        // A cover survives the round trip verbatim (sanitized at ingest, stored
        // and served as-is — job #143).
        let with_cover = TaskResult::Work {
            summary: Some("did it".into()),
            structured: None,
            token_usage: None,
            cover_html: Some("<h1>before/after</h1>".into()),
        };
        let json = serde_json::to_string(&with_cover).unwrap();
        assert!(json.contains(r#""cover_html":"<h1>before/after</h1>""#));
        assert_eq!(
            serde_json::from_str::<TaskResult>(&json).unwrap(),
            with_cover
        );

        // Same for an evaluator verdict (Agent) cover.
        let agent = TaskResult::Agent {
            pass: true,
            abort: false,
            structured: None,
            token_usage: None,
            cover_html: Some("<p>ok</p>".into()),
        };
        let back: TaskResult =
            serde_json::from_str(&serde_json::to_string(&agent).unwrap()).unwrap();
        assert_eq!(back, agent);
    }

    #[test]
    fn label_defaults_absent_and_round_trips() {
        // Old task records (no `label` key) deserialize to None, and None is
        // omitted on the wire (skip_serializing_if) — job #146 back-compat.
        let json = r#"{
          "id": 4, "job_seq": 7, "project": "acme/api",
          "phase": "WrapUp", "cycle": 1,
          "kind": { "kind": "Command", "run": "./tasks/web-publish.sh" },
          "state": "Running", "attempt": 1,
          "container_id": "fake/c1", "result": null,
          "created_at": "2026-07-21T10:00:00Z", "started_at": null, "completed_at": null
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.label, None);
        assert!(!serde_json::to_string(&task).unwrap().contains("label"));

        // A labelled wrap-up task round-trips.
        let mut labelled = task.clone();
        labelled.label = Some("publish".into());
        let json = serde_json::to_string(&labelled).unwrap();
        assert!(json.contains(r#""label":"publish""#));
        assert_eq!(serde_json::from_str::<Task>(&json).unwrap(), labelled);
    }

    #[test]
    fn escalation_phase_round_trips() {
        // The dedicated escalation phase (job #141) round-trips, and the legacy
        // `Work`-stamped escalation record still deserializes (back-compat).
        let p: TaskPhase = serde_json::from_str(r#""Escalation""#).unwrap();
        assert_eq!(p, TaskPhase::Escalation);
        assert_eq!(serde_json::to_string(&p).unwrap(), r#""Escalation""#);
        assert_eq!(
            serde_json::from_str::<TaskPhase>(r#""Work""#).unwrap(),
            TaskPhase::Work
        );
    }
}

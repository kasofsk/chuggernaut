//! The effect vocabulary (refactor-plan B2, `contracts.md` §2).
//!
//! An [`Effect`] is one thing the dispatcher does *about* a decision — a write
//! to the world through a port, never a decision itself. The enum names each of
//! those actions as a value so that deciders ([`crate::decide`]) return
//! `(transitions, Vec<Effect>)` and one interpreter (the dispatcher's
//! `interpret` module — deliberately *not* in this crate, since executing an
//! effect is I/O) performs them. Most `.await` sites in `eval`/`exec`/`core`
//! still reach the ports inline; Track C migrates them one decider at a time.
//!
//! - **Accepts:** nothing — `Effect` is a plain data type, constructed by
//!   deciders and by tests.
//! - **Emits:** nothing itself; `Core::interpret` (dispatcher `interpret`
//!   module) is what turns an `Effect` into a port call.
//! - **Guarantees:** every variant is `serde`-serializable (the vocabulary
//!   emits JSON Schema later, `NORTH-STAR.md` §2) and carries exactly the data
//!   its execution needs — no `&Core` handles, no live futures. Reads
//!   (`jobs.get`, `tasks.list_for_job`, `counters.next`, …) are deliberately
//!   *not* effects: they feed the decider's view of state, not its output.
//! - **Spec:** §3.1–3.4 (the actions), `contracts.md` §2 (why).
//!
//! ## Variant → port method
//!
//! Each variant maps to exactly one port method (or the thin `Core` helper that
//! funnels to it); [`Effect::port`] returns that name, and the interpreter's
//! match arm dispatches to it. Keep this table, `Effect::port`, and the
//! interpreter arms in lock-step.
//!
//! | Variant | Port method |
//! | --- | --- |
//! | `SetJobState` | `Core::set_state` → `jobs.put` |
//! | `PutJob` | `jobs.put` |
//! | `AppendRdep` | `rdeps.append` |
//! | `RemoveRdep` | `rdeps.remove` |
//! | `PutTask` | `tasks.put` |
//! | `PutProject` | `projects.put` |
//! | `PublishEvent` | `Core::publish` → `store.publish_event` |
//! | `PublishStatus` | `store.publish` |
//! | `WriteKv` | `store.raw_bucket().put_json` |
//! | `KillContainer` | `backend.kill` |
//! | `RemoveContainer` | `backend.remove` |
//! | `LaunchWorkTask` | `Core::launch_work_task` → `provider.run` |
//! | `LaunchWrapupTask` | `Core::launch_wrapup_task` → `provider.run` |
//! | `LaunchGateFix` | `Core::launch_gate_fix` → `provider.run` |
//! | `DeferLaunch` | `Core::defer_launch` |
//! | `SquashMerge` | `repos.squash_merge` |
//! | `DeleteBranch` | `repos.delete_branch` |
//! | `IssueCredentials` | `SshCa::issue_job_credential` |
//! | `Escalate` | `Core::escalate` |
//! | `Stall` | `Core::stall` |

use serde::{Deserialize, Serialize};
use types::{EvalResult, Job, JobState, ProjectRecord, Task};

/// Whether a §7.4 per-job credential may push. A self-contained mirror of the
/// `auth` crate's `CertAccess` so the vocabulary stays `serde`-serializable and
/// this crate stays free of the async `auth` dependency; the dispatcher's
/// interpreter maps it back at execution time (and owns the mapping — the
/// orphan rule keeps a `From` impl between two foreign types out of here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialAccess {
    /// Work-phase certificate — may push (§7.4).
    ReadWrite,
    /// Eval-phase certificate — read-only (§7.4).
    ReadOnly,
}

/// One action the dispatcher performs through a port. See the module header for
/// the full variant → port-method table; every variant is `serde`-serializable
/// and self-contained.
///
/// `Job`/`Task` payloads are boxed to keep the enum small (a `Job` dwarfs the
/// scalar variants; an unboxed variant would bloat every `Effect` value).
#[derive(Debug, Serialize, Deserialize)]
pub enum Effect {
    // --- Job records & graph (JobStore / RdepsStore) ---
    /// Transition a job through the §2.1 funnel (`assert_transition`, then
    /// `jobs.put`, then the in-memory graph). Maps to `Core::set_state`.
    ///
    /// Example call site: `core.rs::escalate` moving a job to
    /// [`JobState::Escalated`].
    ///
    SetJobState { job: Box<Job>, to: JobState },
    /// Persist a job record without a state change (definition edits, cover
    /// stamps). Maps to `jobs.put`.
    ///
    /// Example call site: `core.rs::update_job` writing back a Draft edit.
    PutJob { job: Box<Job> },
    /// Add `dependent_seq` to `dep_seq`'s reverse-dependency set. Maps to
    /// `rdeps.append`.
    ///
    /// Example call site: `core.rs::create_job` wiring a new job's deps.
    AppendRdep {
        owner: String,
        project: String,
        dep_seq: u64,
        dependent_seq: u64,
    },
    /// Drop `dependent_seq` from `dep_seq`'s reverse-dependency set. Maps to
    /// `rdeps.remove`.
    ///
    /// Example call site: `core.rs` revoke cascade unwiring a dependent.
    RemoveRdep {
        owner: String,
        project: String,
        dep_seq: u64,
        dependent_seq: u64,
    },

    // --- Task records (TaskStore) ---
    /// Persist a task record. Maps to `tasks.put`.
    ///
    /// Example call site: `exec.rs::launch_work_task` writing the Running task.
    PutTask { task: Box<Task> },

    // --- Project records (ProjectStore) ---
    /// Persist a linked-origin project record. Maps to `projects.put`.
    ///
    /// Example call site: `origin.rs` recording an opened origin release.
    PutProject {
        owner: String,
        project: String,
        record: Box<ProjectRecord>,
    },

    // --- Events & status snapshots ---
    /// Append a `job-events` trail entry (durable JetStream publish). Maps to
    /// `Core::publish` → `store.publish_event`.
    ///
    /// Example call site: `core.rs::escalate` emitting `job-escalated`.
    ///
    PublishEvent {
        owner: String,
        project: String,
        seq: u64,
        event_type: String,
        extra: serde_json::Value,
    },
    /// Fire-and-forget publish on a plain core-NATS subject (no JetStream, no
    /// reply). Maps to `store.publish`.
    ///
    /// Example call site: worker-announce heartbeat re-publish.
    PublishStatus { subject: String, payload: Vec<u8> },
    /// Write a JSON value into a raw KV bucket (the `platform` fleet/config
    /// snapshot). Maps to `store.raw_bucket().put_json`.
    ///
    /// Example call site: `fleet.rs::refresh_fleet_status` writing
    /// `fleet.status`.
    WriteKv {
        bucket: String,
        key: String,
        value: serde_json::Value,
    },

    // --- Container lifecycle (ContainerBackend) ---
    /// SIGKILL a running container. Maps to `backend.kill`.
    ///
    /// Example call site: `core.rs::kill_running_containers` on revoke.
    KillContainer { container_id: String },
    /// Remove an exited container, reclaiming its overlay. Maps to
    /// `backend.remove`.
    ///
    /// Example call site: `harvest.rs` overlay reclaim after artifact pull.
    RemoveContainer { container_id: String },

    // --- Task launches (AgentProvider + ContainerBackend, via exec/eval) ---
    /// Launch (or resume) a Work-phase task. Maps to `Core::launch_work_task`
    /// → `provider.run`.
    ///
    /// Example call site: `exec.rs` Ready→Work entry.
    ///
    LaunchWorkTask {
        owner: String,
        project: String,
        seq: u64,
        cycle: u32,
        attempt: u32,
        resume: bool,
    },
    /// Launch a WrapUp-phase task. Maps to `Core::launch_wrapup_task` →
    /// `provider.run`.
    ///
    /// Example call site: `eval.rs` post-eval WrapUp entry.
    ///
    LaunchWrapupTask {
        owner: String,
        project: String,
        seq: u64,
        attempt: u32,
    },
    /// Launch a merge-gate fix task after a gate-level eval failure. Maps to
    /// `Core::launch_gate_fix` → `provider.run`.
    ///
    /// Example call site: `eval.rs` merge-gate re-entry with the new base.
    ///
    LaunchGateFix {
        owner: String,
        project: String,
        seq: u64,
        new_base: String,
        failures: Vec<EvalResult>,
        compiler_output: String,
    },
    /// Park a launch that hit `NoCapacity` on the §3.5 launch queue. Maps to
    /// `Core::defer_launch`.
    ///
    /// Example call site: `exec.rs` launch path on a full fleet.
    ///
    DeferLaunch {
        owner: String,
        project: String,
        seq: u64,
        task: Box<Task>,
        reason: String,
    },

    // --- Repository mutations (RepoManager) ---
    /// Squash-merge a job's branch onto the default branch. Maps to
    /// `repos.squash_merge`.
    ///
    /// Example call site: `eval.rs::finalize` fast-path landing.
    SquashMerge {
        owner: String,
        project: String,
        seq: u64,
        base_ref: String,
        job_type: String,
        summary: Option<String>,
    },
    /// Delete a branch (job branch or `merge-gate/{seq}` scratch). Maps to
    /// `repos.delete_branch`.
    ///
    /// Example call site: `eval.rs::finish_landing` cleaning the gate branch.
    DeleteBranch {
        owner: String,
        project: String,
        branch: String,
    },

    // --- Credentials (auth SSH CA, §7.4) ---
    /// Mint a per-job SSH certificate scoped to `access` for `ttl_secs`. Maps
    /// to `SshCa::issue_job_credential`.
    ///
    /// Example call site: `exec.rs::ssh_credential_files` before a Work launch.
    IssueCredentials {
        owner: String,
        project: String,
        seq: u64,
        access: CredentialAccess,
        ttl_secs: u64,
    },

    // --- Escalation composites (§1.2) ---
    /// Post-work escalation: create a Human task, move the job to
    /// [`JobState::Escalated`], publish `job-escalated`. Maps to
    /// `Core::escalate`.
    ///
    /// Example call site: `exec.rs::retry_or_escalate_work` on exhausted retries.
    ///
    Escalate {
        owner: String,
        project: String,
        seq: u64,
        reason: String,
        detail: String,
        failing_task: Option<u64>,
    },
    /// Pre-work escalation: create a Human task, move the job to
    /// [`JobState::Stalled`], publish `job-stalled`. Maps to `Core::stall`.
    ///
    /// Example call site: `core.rs` Blocked→Ready re-validation failure.
    ///
    Stall {
        owner: String,
        project: String,
        seq: u64,
        reason: String,
        detail: String,
        failing_task: Option<u64>,
    },
}

impl Effect {
    /// The port method this effect maps to — the executable form of the
    /// module-header table. Kept in lock-step with the interpreter's match arms
    /// so a new variant that forgets its arm cannot silently claim a port.
    pub fn port(&self) -> &'static str {
        match self {
            Effect::SetJobState { .. } => "Core::set_state",
            Effect::PutJob { .. } => "jobs.put",
            Effect::AppendRdep { .. } => "rdeps.append",
            Effect::RemoveRdep { .. } => "rdeps.remove",
            Effect::PutTask { .. } => "tasks.put",
            Effect::PutProject { .. } => "projects.put",
            Effect::PublishEvent { .. } => "Core::publish",
            Effect::PublishStatus { .. } => "store.publish",
            Effect::WriteKv { .. } => "store.raw_bucket.put_json",
            Effect::KillContainer { .. } => "backend.kill",
            Effect::RemoveContainer { .. } => "backend.remove",
            Effect::LaunchWorkTask { .. } => "Core::launch_work_task",
            Effect::LaunchWrapupTask { .. } => "Core::launch_wrapup_task",
            Effect::LaunchGateFix { .. } => "Core::launch_gate_fix",
            Effect::DeferLaunch { .. } => "Core::defer_launch",
            Effect::SquashMerge { .. } => "repos.squash_merge",
            Effect::DeleteBranch { .. } => "repos.delete_branch",
            Effect::IssueCredentials { .. } => "SshCa::issue_job_credential",
            Effect::Escalate { .. } => "Core::escalate",
            Effect::Stall { .. } => "Core::stall",
        }
    }
}

#[cfg(test)]
mod tests {
    //! Narrow unit coverage per variant group: every `Effect` round-trips
    //! through `serde` (the JSON-Schema-bound vocabulary must serialize) and
    //! reports the port method the module-header table promises. These are pure
    //! — no NATS/Docker — because `Effect` is plain data; the interpreter's
    //! port dispatch is exercised end-to-end by the Tier-2 tests that stay
    //! green.
    use super::*;

    /// A minimal valid job — `Job` has no `Default`, and every field beyond
    /// these deserializes from its `serde(default)`.
    fn sample_job() -> Box<Job> {
        Box::new(
            serde_json::from_str(
                r#"{ "id": 1, "project": "acme/api", "type": "build",
                     "deps": [], "state": "Ready", "branch": "job/1",
                     "base_ref": null, "knowledge_tags": [], "factory": null,
                     "created_at": "2026-07-24T10:00:00Z", "ready_at": null }"#,
            )
            .expect("sample job"),
        )
    }

    /// A minimal valid task, same reasoning as [`sample_job`].
    fn sample_task() -> Box<Task> {
        Box::new(
            serde_json::from_str(
                r#"{ "id": 1, "job_seq": 1, "project": "acme/api",
                     "phase": "Work", "cycle": 1,
                     "kind": { "kind": "Agent", "provider": "claude", "model": null, "prompt": "p.md" },
                     "state": "Pending", "attempt": 1,
                     "container_id": null, "result": null,
                     "created_at": "2026-07-24T10:00:00Z", "started_at": null, "completed_at": null }"#,
            )
            .expect("sample task"),
        )
    }

    /// Serialize → deserialize an effect and assert its `port()` name. JSON is
    /// the wire the schema will describe, so a variant that cannot round-trip is
    /// a vocabulary bug.
    fn assert_roundtrip(effect: Effect, port: &str) {
        assert_eq!(effect.port(), port, "port mapping");
        let json = serde_json::to_string(&effect).expect("serialize");
        let back: Effect = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.port(), port, "port survives round-trip");
    }

    #[test]
    fn job_and_graph_group() {
        assert_roundtrip(
            Effect::SetJobState {
                job: sample_job(),
                to: JobState::Escalated,
            },
            "Core::set_state",
        );
        assert_roundtrip(Effect::PutJob { job: sample_job() }, "jobs.put");
        assert_roundtrip(
            Effect::AppendRdep {
                owner: "acme".into(),
                project: "api".into(),
                dep_seq: 1,
                dependent_seq: 2,
            },
            "rdeps.append",
        );
        assert_roundtrip(
            Effect::RemoveRdep {
                owner: "acme".into(),
                project: "api".into(),
                dep_seq: 1,
                dependent_seq: 2,
            },
            "rdeps.remove",
        );
    }

    #[test]
    fn task_and_project_group() {
        assert_roundtrip(
            Effect::PutTask {
                task: sample_task(),
            },
            "tasks.put",
        );
        assert_roundtrip(
            Effect::PutProject {
                owner: "acme".into(),
                project: "api".into(),
                record: Box::new(ProjectRecord::default()),
            },
            "projects.put",
        );
    }

    #[test]
    fn event_group() {
        assert_roundtrip(
            Effect::PublishEvent {
                owner: "acme".into(),
                project: "api".into(),
                seq: 7,
                event_type: "job-escalated".into(),
                extra: serde_json::json!({ "reason": "timeout" }),
            },
            "Core::publish",
        );
        assert_roundtrip(
            Effect::PublishStatus {
                subject: "fleet.announce".into(),
                payload: vec![1, 2, 3],
            },
            "store.publish",
        );
        assert_roundtrip(
            Effect::WriteKv {
                bucket: "platform".into(),
                key: "fleet.status".into(),
                value: serde_json::json!({ "nodes": [] }),
            },
            "store.raw_bucket.put_json",
        );
    }

    #[test]
    fn container_group() {
        assert_roundtrip(
            Effect::KillContainer {
                container_id: "c1".into(),
            },
            "backend.kill",
        );
        assert_roundtrip(
            Effect::RemoveContainer {
                container_id: "c1".into(),
            },
            "backend.remove",
        );
    }

    #[test]
    fn task_launch_group() {
        assert_roundtrip(
            Effect::LaunchWorkTask {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                cycle: 1,
                attempt: 1,
                resume: false,
            },
            "Core::launch_work_task",
        );
        assert_roundtrip(
            Effect::LaunchWrapupTask {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                attempt: 1,
            },
            "Core::launch_wrapup_task",
        );
        assert_roundtrip(
            Effect::LaunchGateFix {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                new_base: "abc123".into(),
                failures: vec![],
                compiler_output: String::new(),
            },
            "Core::launch_gate_fix",
        );
        assert_roundtrip(
            Effect::DeferLaunch {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                task: sample_task(),
                reason: "NoCapacity".into(),
            },
            "Core::defer_launch",
        );
    }

    #[test]
    fn vcs_group() {
        assert_roundtrip(
            Effect::SquashMerge {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                base_ref: "abc123".into(),
                job_type: "build".into(),
                summary: Some("landed".into()),
            },
            "repos.squash_merge",
        );
        assert_roundtrip(
            Effect::DeleteBranch {
                owner: "acme".into(),
                project: "api".into(),
                branch: "merge-gate/3".into(),
            },
            "repos.delete_branch",
        );
    }

    #[test]
    fn credential_group() {
        assert_roundtrip(
            Effect::IssueCredentials {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                access: CredentialAccess::ReadWrite,
                ttl_secs: 3600,
            },
            "SshCa::issue_job_credential",
        );
        // The mapping back onto the auth crate's `CertAccess` lives with the
        // dispatcher's interpreter (orphan rule + purity), tested there.
    }

    #[test]
    fn escalation_group() {
        assert_roundtrip(
            Effect::Escalate {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                reason: "retries exhausted".into(),
                detail: "3 failed cycles".into(),
                failing_task: Some(4),
            },
            "Core::escalate",
        );
        assert_roundtrip(
            Effect::Stall {
                owner: "acme".into(),
                project: "api".into(),
                seq: 3,
                reason: "revalidation failed".into(),
                detail: "missing dep".into(),
                failing_task: None,
            },
            "Core::stall",
        );
    }
}

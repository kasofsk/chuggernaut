//! Work-phase decisions (spec §3.2) — refactor-plan C6, the last phase carve
//! and the one that dismantles `exec.rs`'s decision half.
//!
//! The phase opens when a job takes a launch slot and closes when its work is
//! Done (or its budget is spent). Every decision on that road is here, told
//! apart by [`WorkEvent`]:
//!
//! - **Entered** — the launch-time fork (§3.2 steps 1–6, §14.2): the contract
//!   and the §2.2 launch-time KV pass either hold — the job moves to `Work` and
//!   a first cycle announces itself — or they do not, and the job parks. Which
//!   park is the decision: a job still `Ready` has no work task, so it parks
//!   `Stalled` (pre-work, Retry/Revoke only, spec §575); a rework re-entry is
//!   post-work and parks `Escalated`.
//! - **Attempt** — one attempt's task record, and whether it runs at all: a
//!   declared human work task and a *claimed* attempt (§1.2) are parked for the
//!   operator instead of launched, and the claim is consumed by the same
//!   decision that honours it — so an attempt is either launched or parked,
//!   never both.
//! - **Exited** — the container's verdict (§3.2 step 8): exit 0 completes the
//!   attempt (unless the finish-line guard has a question, below), any other
//!   exit fails it and spends the retry policy.
//! - **OutputChecked** — the finish-line guard's continuation (docs/reference/contracts.md §2):
//!   "did the branch move?" is a ref read, so the decider asks for it
//!   ([`WorkStep::CheckOutput`]) and the answer re-enters here.
//! - **Declined** — an operator failed a Pending human-performed attempt
//!   (§1.2 claims): the same retry policy, but the branch is PRESERVED and the
//!   operator's notes ride into the next attempt's §4.3 context.
//! - **InfraLost** — restart reconciliation found the attempt's container gone
//!   (§3.6): relaunch the SAME attempt without spending budget, bounded by
//!   [`WorkView::infra_relaunch_cap`].
//!
//! One retry policy serves all three failure paths (`work_retries` spent, then
//! `work_retries_exhausted`); the two axes the callers differ on — whether the
//! branch is recovered and what context carries forward — are values on
//! [`WorkStep::Retry`], not separate policies.
//!
//! The `Msg` contracts this decider owns (docs/reference/contracts.md §1):
//!
//! - `Msg::TaskExited` for a `TaskPhase::Work` task — **pre:** the job has a
//!   live execution slice (the shim ignores a late exit from a revoked job);
//!   **post:** the attempt's record is terminal (`Done`/`Failed`) exactly once —
//!   a stale monitor's exit for an already-resolved task is [`WorkStep::Idle`] —
//!   and the job either advanced (Evaluation, or the merge gate for a gate-fix),
//!   relaunched at `attempt + 1`, or `Escalated` with the slice released. Never
//!   two live attempts, and never an advance on an attempt that produced
//!   nothing (§3.2 finish-line guard).
//! - `Msg::ResolveTask` with `Fail` on a `Work` job (§1.2 claims) — **pre:** the
//!   task is a Pending human-performed attempt (the shim rejects otherwise);
//!   **post:** as above, with the branch preserved rather than recovered.
//! - The launch path (`Core::launch_work_task`, and the `LaunchWorkTask`
//!   effect) — **pre:** the job is in `Work` with a live slice; **post:** exactly
//!   one new task record exists for the attempt, `Running` with a container
//!   coming or `Pending` in the operator inbox, and a consumed claim covers
//!   exactly that one attempt.
//!
//! - **Accepts:** a [`WorkView`] (the job, its contract, the slice's cycle and
//!   submission, the pre-minted task id and session id, the §4.3 brief, the
//!   clock) and a [`WorkEvent`].
//! - **Emits:** `(Vec<Transition>, Vec<Effect>, WorkStep)` — values only. The
//!   owned effect set (docs/reference/contracts.md §2): `CreateTask` for the attempt record,
//!   `PutJob` for the consumed claim, `PutTask` for a retired attempt,
//!   `PublishEvent` for `job-started`, `config-warning`, `task-completed` and
//!   `task-failed`, and `Escalate`/`Stall` for the two parks. The [`WorkStep`]
//!   names the shell work that follows — the container launch, the branch
//!   recover-or-reset, the finish-line ref read, the Evaluation hand-off — all
//!   of which are I/O the pure crate cannot do.
//! - **Guarantees:** pure and synchronous; every branch exhaustively matched and
//!   unit-tested; asserts negative space (docs/reference/style.md Tier 2 #2) — never launches
//!   for a terminal job, never resolves an attempt twice, never spends budget
//!   for an infrastructure loss. Performs no effect, holds no `&mut Core`.
//! - **Spec:** §3.2; §1.2 (claims, human work); §2.2 and §14.2 (the launch-time
//!   pass and the skew park); §3.6 (drain, infra loss); docs/reference/contracts.md §2;
//!   refactor-plan C6.
//!
//! **Boundary.** Retiring an infrastructure-lost attempt (the `Failed` +
//! `infra_loss` stamp and its `task-failed` announcement) stays shell-side: the
//! Evaluation phase loses containers the same way and shares that retirement, so
//! only the *relaunch-or-escalate* half — which is Work-phase policy — is here.
//! The launch itself (prompt assembly, credentials, container env) is I/O and
//! stays in `exec.rs`; this decider stops at the task record.

use crate::decide::Transition;
use crate::effects::Effect;
use crate::release::ValidationError;
use chrono::{DateTime, Utc};
use types::{
    EvalResult, Job, JobState, JobType, Performer, ReworkReason, Task, TaskKind, TaskPhase,
    TaskResult, TaskState, TokenUsage, WorkType, job_type::Provider,
};

/// Machine code for an infrastructure-loss failure/escalation (§3.6): a task
/// whose container was gone at restart, relaunched without spending retry
/// budget. The `task-failed`/`job-escalated` event `reason`, and the marker on
/// the retired task record (pairs with #76 self-reporting).
pub const INFRA_LOSS_REASON: &str = "infra_loss";

/// Max infrastructure relaunches for one task lineage (this cycle, this
/// evaluator) before escalating with reason [`INFRA_LOSS_REASON`] (§3.6). Bounds
/// a genuinely-vanishing environment so it escalates instead of looping forever
/// (STYLE.md Tier 2 #3). Shared with the Evaluation phase, which loses
/// containers the same way.
pub const INFRA_RELAUNCH_CAP: u32 = 3;

/// Machine code for a work attempt that exited 0 but left nothing behind (§3.2
/// finish-line guard): no commits on `job/{seq}` beyond `base_ref` and no
/// summary. A headless CLI ends its turn (and its container) before committing,
/// so the exit is 0 yet the branch is empty. Unlike [`INFRA_LOSS_REASON`] this
/// is a genuine agent failure and DOES spend a `work_retries` budget. Surfaced
/// on the retired task result and the `task-failed` event `reason`.
pub const NO_OUTPUT_REASON: &str = "no_output_produced";

/// The agent's `submit_result` payload as the exit verdict reads it — the
/// dispatcher's `WorkSubmission` narrowed to the four fields that become the
/// attempt's [`TaskResult::Work`]. Mirrored rather than imported for the same
/// reason `EvalExit` is: the dispatcher's form is a §4.2 request body that also
/// carries transport concerns the pure crate has no use for.
#[derive(Debug, Clone, Default)]
pub struct WorkSubmissionView {
    pub summary: Option<String>,
    pub structured: Option<serde_json::Value>,
    pub token_usage: Option<TokenUsage>,
    pub cover_html: Option<String>,
}

/// What a work container reported at exit — the dispatcher's `TaskExit` narrowed
/// to the fields the Work verdict reads (the eval-only fields and the port types
/// stay behind).
#[derive(Debug, Default)]
pub struct WorkExit {
    pub exit_code: i32,
    /// Usage measured from the agent CLI's own JSON result — preferred over the
    /// agent's self-report, which it may omit or invent.
    pub usage: Option<TokenUsage>,
    /// Set when the container never launched: the reason is the attempt's only
    /// record of why it failed, since there are no logs to harvest.
    pub launch_error: Option<String>,
    /// A command work task's harvested report (#187 `@chug:leg` lines) — its
    /// structured result, kept on the record whichever way the exit fell.
    pub structured: Option<serde_json::Value>,
}

/// The read-only inputs one Work-phase decision consumes. The shim re-gathers it
/// before every [`decide`] call — including the re-entry after a
/// [`WorkStep::CheckOutput`] hop — so a decision never runs on a view the world
/// moved under. Reads feed the view; they are not effects.
pub struct WorkView<'a> {
    /// The job whose work is being decided.
    pub job: &'a Job,
    /// The contract this cycle runs under, loaded at `base_ref` (§2.2). `None`
    /// only on the entry hop where loading it is what failed — no other branch
    /// reads it.
    pub job_type: Option<&'a JobType>,
    /// The execution slice's cycle: 1 for a first launch, higher for each
    /// rework re-entry (§3.3).
    pub cycle: u32,
    /// Why this cycle's Work tasks exist, when the cycle is a rework re-entry
    /// (§3.3). Stamped onto every task the cycle launches — retries included —
    /// so the record self-explains. `None` for cycle 1.
    pub rework_reason: Option<ReworkReason>,
    /// The id the next task record takes (§1.2, sequential within the job).
    pub next_task_id: u64,
    /// A freshly minted session id (§4.2). Used only when the attempt actually
    /// runs an agent — whether it does is this decider's call.
    pub session_id: &'a str,
    /// The §4.3 job brief (batch-aware), appended to a *human* work task's
    /// prompt so the operator's inbox item carries the ticket.
    pub human_brief: &'a str,
    /// §12.4 platform provider default, the last link of the fallback chain.
    pub agent_provider_default: Option<&'a str>,
    /// §12.4 platform model default, the last link of the fallback chain.
    pub agent_model_default: Option<&'a str>,
    /// The latest `submit_result` payload cached on the execution slice — what
    /// an exit-0 attempt's result is built from when the agent exited without
    /// the record being written.
    pub submission: Option<&'a WorkSubmissionView>,
    /// Bound on infrastructure relaunches for one attempt lineage (§3.6);
    /// [`INFRA_RELAUNCH_CAP`] in production.
    pub infra_relaunch_cap: u32,
    /// §3.6 graceful drain: initiate no new work container. The job stays in
    /// Work with its prior task record and restart reconciliation re-launches.
    pub draining: bool,
    /// The decision moment — stamped on the records this decision writes.
    pub now: DateTime<Utc>,
}

impl<'a> WorkView<'a> {
    /// The entry hop's narrow view (§3.2 steps 1–6): the job, the contract the
    /// shim just loaded — `None` when loading it is what failed — and the cycle
    /// being entered. Every other input belongs to a later hop and is unread
    /// here, so the entry call site does not have to invent one.
    pub fn entry(
        job: &'a Job,
        job_type: Option<&'a JobType>,
        cycle: u32,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            job,
            job_type,
            cycle,
            rework_reason: None,
            next_task_id: 0,
            session_id: "",
            human_brief: "",
            agent_provider_default: None,
            agent_model_default: None,
            submission: None,
            infra_relaunch_cap: INFRA_RELAUNCH_CAP,
            draining: false,
            now,
        }
    }

    /// The contract this decision runs under. Absent only on the entry hop that
    /// failed to load one — and that hop's decision is made before any branch
    /// reads it, so reaching this without a contract is a shim bug.
    fn contract(&self) -> &JobType {
        self.job_type
            .unwrap_or_else(|| panic!("job #{}: this decision needs a job type", self.job.id))
    }
}

/// Why a Work entry cannot proceed (§2.2 launch-time pass, §14.2 skew). Both
/// arms park the job; which park is [`decide`]'s call.
#[derive(Debug)]
pub enum EntryFailure {
    /// The job-type contract failed to load or validate at `base_ref`. Carries
    /// the field errors verbatim — including the `min_dispatcher` version-skew
    /// verdict, which is the one error class that parks pre-work (§14.2).
    Contract(Vec<ValidationError>),
    /// The §2.2 launch-time pass found declared secrets/vars missing from KV,
    /// re-checked immediately before injection.
    MissingKv(Vec<String>),
    /// The §2.2 launch-time pass found a value on `Job::inputs` that no longer
    /// clears the shape floor (charset, length, name form, count) — the third
    /// and last pass, re-checked immediately before injection (design #311
    /// Decision 3). Parks like [`EntryFailure::MissingKv`] rather than launching:
    /// a value three passes rejected must not reach a `run:` script that crosses
    /// further shells. Carries the violation as `types::inputs` reported it.
    BadInput(String),
}

/// What drove this Work-phase decision. [`WorkEvent::OutputChecked`] carries the
/// result of the read [`WorkStep::CheckOutput`] asked for (docs/reference/contracts.md §2's
/// continuation contract).
#[derive(Debug)]
pub enum WorkEvent {
    /// The job is entering Work at [`WorkView::cycle`] — a first launch, or a
    /// rework re-entry. `failure` is the verdict of the shim's contract load and
    /// §2.2 launch-time pass.
    Entered { failure: Option<EntryFailure> },
    /// Create one attempt of `cycle`'s work task. `resume` means the branch
    /// already carries a predecessor's pushed commits (§3.2 crash recovery).
    /// The cycle is the caller's — a restart relaunches the cycle the task log
    /// names — not the view's, so a stale slice can never mislabel a record.
    Attempt {
        cycle: u32,
        attempt: u32,
        resume: bool,
    },
    /// A work container exited (§3.2 step 8). `task` is the record as persisted
    /// — for an agent that submitted, it already carries the result.
    Exited { task: Box<Task>, exit: WorkExit },
    /// The finish-line guard's ref read came back: did the branch move beyond
    /// `base_ref`?
    OutputChecked { task: Box<Task>, has_output: bool },
    /// An operator declined a Pending human-performed attempt (§1.2 claims).
    /// `task` already carries the operator's [`TaskResult::Human`]; `structured`
    /// is the handoff note that rides into the next attempt's §4.3 context.
    Declined {
        task: Box<Task>,
        operator: String,
        structured: serde_json::Value,
    },
    /// Restart reconciliation found the attempt's container gone (§3.6). The
    /// shim has already retired the lost attempt; `losses` counts this lineage's
    /// retired losses INCLUDING it.
    InfraLost { task: Box<Task>, losses: u32 },
}

/// The bookkeeping the shim owes after applying a decision. Everything here is
/// I/O — the container launch, the git branch, the phase hand-offs — that is
/// deliberately outside the pure crate.
#[derive(Debug)]
pub enum WorkStep {
    /// Nothing follows: the entry parked, or the event named an attempt that is
    /// no longer live (a stale monitor's exit, a duplicate).
    Idle,
    /// §3.6 drain: launch no container. The job holds in Work and restart
    /// reconciliation re-launches.
    Hold,
    /// The entry validated: the shim opens the execution slice and launches
    /// attempt 1 of the cycle.
    Begin,
    /// The attempt's record is created and its container is the shim's to
    /// launch (prompt, credentials, env — all I/O). The task is handed over so
    /// the launch half never re-derives what the decision already fixed.
    Launch { task: Box<Task>, resume: bool },
    /// The attempt is parked `Pending` for the operator inbox — declared human
    /// work, or a claimed attempt (§1.2). No container, no monitor.
    Park,
    /// Relaunch this cycle at `attempt`. `recover` asks for the §3.2
    /// recover-or-reset of the job branch first (a crash may have pushed
    /// commits); `false` preserves the branch as-is, which is what an operator's
    /// deliberate handoff gets (#121). `eval_context_add` is appended to the
    /// slice's §4.3 rework context before the launch.
    Retry {
        cycle: u32,
        attempt: u32,
        recover: bool,
        eval_context_add: Vec<EvalResult>,
    },
    /// The attempt exited 0 with no summary: the shim reads whether the branch
    /// carries commits beyond `base_ref` and re-enters with
    /// [`WorkEvent::OutputChecked`] (§3.2 finish-line guard).
    CheckOutput { task: Box<Task> },
    /// Work is Done: the shim hands the job to Evaluation (§3.2 steps 9–10).
    Evaluate,
    /// A gate-fix attempt is Done (job #154): straight back to the merge gate,
    /// where gate CI is the final authority — no re-review, no eval CI.
    ReenterGate,
    /// Escalated out of Work: the shim releases the execution slice before the
    /// `Escalate` effect runs, so the escalation task is not stamped with the
    /// cycle of a slice the decision just ended (the order C2/C3/C5 established).
    EscalatedDropExec,
}

impl WorkStep {
    /// True when the shim must release the job's execution slice — after the
    /// transitions, before the effects.
    pub fn drops_exec(&self) -> bool {
        matches!(self, WorkStep::EscalatedDropExec)
    }
}

/// §12.4 provider fallback chain: declaration → platform default → `claude`
/// (tests construct a config without a default; production always sets one).
/// Lives with the Work decider because resolving an agent task's provider is
/// part of building it; the evaluator launches reuse it.
pub fn provider_name(declared: Option<Provider>, platform_default: Option<&str>) -> String {
    declared
        .map(|p| format!("{p:?}").to_lowercase())
        .or_else(|| platform_default.map(String::from))
        .unwrap_or_else(|| "claude".into())
}

/// Decide one Work-phase step (spec §3.2). Pure: every await the old
/// `enter_work` / `launch_work_task` / `on_work_exited` / `retry_or_escalate_work`
/// chain interleaved is either a pre-read in the view or an [`Effect`] (or a
/// [`WorkStep`]) the shim performs afterwards.
pub fn decide(view: &WorkView<'_>, event: WorkEvent) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    let (owner, project) = view
        .job
        .project
        .split_once('/')
        .unwrap_or_else(|| panic!("malformed job project '{}'", view.job.project));

    match event {
        WorkEvent::Entered { failure } => decide_entered(view, owner, project, failure),
        WorkEvent::Attempt {
            cycle,
            attempt,
            resume,
        } => decide_attempt(view, owner, project, cycle, attempt, resume),
        WorkEvent::Exited { task, exit } => decide_exited(view, owner, project, *task, exit),
        WorkEvent::OutputChecked { task, has_output } => {
            decide_output_checked(view, owner, project, *task, has_output)
        }
        WorkEvent::Declined {
            task,
            operator,
            structured,
        } => decide_declined(view, owner, project, *task, &operator, structured),
        WorkEvent::InfraLost { task, losses } => {
            decide_infra_lost(view, owner, project, *task, losses)
        }
    }
}

/// Work entry (§3.2 steps 1–6): the launch-time fork. A validation failure parks
/// the job and nothing else happens; a clean entry moves the record to `Work`
/// (idempotent — a rework re-entry is already there) and, on the first cycle
/// only, announces the start plus any tolerated unknown-field warning (§14.2) so
/// a "feature quietly off" config is visible without grepping dispatcher logs.
fn decide_entered(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    failure: Option<EntryFailure>,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    let job = view.job;
    debug_assert!(
        !job.state.is_terminal(),
        "work entered for terminal job #{} in {:?}",
        job.id,
        job.state,
    );
    if let Some(failure) = failure {
        return (
            Vec::new(),
            vec![decide_entered_park(view, owner, project, failure)],
            WorkStep::Idle,
        );
    }

    let mut transitions = Vec::new();
    if job.state != JobState::Work {
        transitions.push(Transition {
            job: Box::new(job.clone()),
            to: JobState::Work,
        });
    }
    let mut effects = Vec::new();
    if view.cycle == 1 {
        effects.push(work_event(
            owner,
            project,
            job.id,
            "job-started",
            serde_json::json!({ "cycle": view.cycle }),
        ));
        for warning in view.contract().config_warnings() {
            effects.push(work_event(
                owner,
                project,
                job.id,
                "config-warning",
                serde_json::json!({ "field": warning.field, "message": warning.to_string() }),
            ));
        }
    }
    (transitions, effects, WorkStep::Begin)
}

/// Which park a failed launch-time validation earns. A job still `Ready` has no
/// work task yet, so every pre-work park is `Stalled` (§2.1's only pre-work park
/// edge, and spec §575) — config-ahead-of-binary (§14.2) says so with its own
/// reason; a rework re-entry is post-work and keeps the `Escalated` path.
fn decide_entered_park(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    failure: EntryFailure,
) -> Effect {
    let seq = view.job.id;
    let pre_work = view.job.state == JobState::Ready;
    let (reason, detail) = match failure {
        EntryFailure::Contract(errors) => {
            let detail = errors
                .iter()
                .map(|e| format!("- {}: {}", e.field, e.message))
                .collect::<Vec<_>>()
                .join("\n");
            if view.cycle == 1 && errors.iter().any(ValidationError::is_schema_skew) {
                return Effect::Stall {
                    owner: owner.to_string(),
                    project: project.to_string(),
                    seq,
                    reason: "config_schema_skew".to_string(),
                    detail: format!(
                        "Job {seq} parked: its job-type config requires a newer \
                         dispatcher than is deployed (config ahead of binary). Deploy \
                         the newer dispatcher, then Retry.\n{detail}"
                    ),
                    failing_task: None,
                };
            }
            (
                "launch_validation_failed",
                format!("Job {seq} failed launch-time validation:\n{detail}"),
            )
        }
        EntryFailure::MissingKv(missing) => (
            "launch_validation_failed",
            format!("Job {seq}: missing at launch: {}", missing.join(", ")),
        ),
        EntryFailure::BadInput(violation) => (
            "launch_validation_failed",
            format!("Job {seq}: input rejected at launch: {violation}"),
        ),
    };
    let (owner, project, reason) = (owner.to_string(), project.to_string(), reason.to_string());
    if pre_work {
        return Effect::Stall {
            owner,
            project,
            seq,
            reason,
            detail,
            failing_task: None,
        };
    }
    Effect::Escalate {
        owner,
        project,
        seq,
        reason,
        detail,
        failing_task: None,
    }
}

/// One attempt's task record (§1.2 creation rules), and whether it runs. The
/// claim check is here — inside the single serialized launch decision — so an
/// attempt is either launched or parked, never both, and the claim is consumed
/// by the same decision that honours it. A claimed attempt keeps its DECLARED
/// kind; the claim only records the performer.
fn decide_attempt(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    cycle: u32,
    attempt: u32,
    resume: bool,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    if view.draining {
        return (Vec::new(), Vec::new(), WorkStep::Hold);
    }
    let job = view.job;
    debug_assert!(
        !job.state.is_terminal(),
        "work attempt decided for terminal job #{} in {:?}",
        job.id,
        job.state,
    );
    let (kind, pending_human) = decide_attempt_kind(view);
    let claimed = job.claim_next;
    let parked = pending_human || claimed;
    let task = Task {
        id: view.next_task_id,
        job_seq: job.id,
        project: job.project.clone(),
        phase: TaskPhase::Work,
        cycle,
        kind,
        state: if parked {
            TaskState::Pending
        } else {
            TaskState::Running
        },
        attempt,
        evaluator: None,
        label: (view.rework_reason == Some(ReworkReason::GateCompileFix))
            .then(|| "gate-fix".to_string()),
        stage: 0,
        performed_by: claimed.then_some(Performer::Human),
        container_id: None,
        rework_reason: view.rework_reason,
        infra_loss: false,
        session_id: (matches!(view.contract().work.r#type, WorkType::Agent) && !claimed)
            .then(|| view.session_id.to_string()),
        pending_reason: None,
        queued_at: None,
        reviewed_tip: None,
        workload_identities: vec![],
        result: None,
        created_at: view.now,
        started_at: (!pending_human).then_some(view.now),
        completed_at: None,
    };
    let mut effects = vec![Effect::CreateTask {
        owner: owner.to_string(),
        project: project.to_string(),
        task: Box::new(task.clone()),
        extra: serde_json::json!({
            "attempt": attempt, "performed_by": claimed.then_some("human"),
        }),
    }];
    if claimed {
        let mut consumed = job.clone();
        consumed.claim_next = false;
        effects.push(Effect::PutJob {
            job: Box::new(consumed),
        });
    }
    let step = if parked {
        WorkStep::Park
    } else {
        WorkStep::Launch {
            task: Box::new(task),
            resume,
        }
    };
    (Vec::new(), effects, step)
}

/// The attempt's [`TaskKind`] and whether it waits on a human (§1.1 work types,
/// §12.4 provider/model resolution). A human work task's prompt carries the §4.3
/// brief, since the operator's inbox item is all they see.
fn decide_attempt_kind(view: &WorkView<'_>) -> (TaskKind, bool) {
    let work = &view.contract().work;
    let prompt = work.prompt.clone().unwrap_or_default();
    match work.r#type {
        WorkType::Agent => (
            TaskKind::Agent {
                provider: provider_name(work.provider, view.agent_provider_default),
                model: view
                    .job
                    .model
                    .clone()
                    .or_else(|| work.model.clone())
                    .or_else(|| view.agent_model_default.map(String::from)),
                prompt,
            },
            false,
        ),
        WorkType::Command => (
            TaskKind::Command {
                run: work.run.clone().unwrap_or_default(),
            },
            false,
        ),
        WorkType::Human => (
            TaskKind::Human {
                prompt: format!("{prompt}{}", view.human_brief),
            },
            true,
        ),
    }
}

/// A work container exited (§3.2 step 8). Exit 0 completes the attempt — unless
/// the finish-line guard has a question — and any other exit fails it and spends
/// the retry policy.
fn decide_exited(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    mut task: Task,
    exit: WorkExit,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    debug_assert_eq!(
        task.phase,
        TaskPhase::Work,
        "task {} is not a Work task",
        task.id,
    );
    if task.state != TaskState::Running {
        return (Vec::new(), Vec::new(), WorkStep::Idle);
    }
    let seq = view.job.id;
    task.completed_at = Some(view.now);
    if exit.exit_code == 0 {
        decide_exited_zero(view, owner, project, task, exit)
    } else {
        task.state = TaskState::Failed;
        if let Some(result) = decide_exited_failure_result(&exit) {
            task.result = Some(result);
        }
        let mut effects = vec![
            Effect::PutTask {
                task: Box::new(task.clone()),
            },
            work_event(
                owner,
                project,
                seq,
                "task-failed",
                serde_json::json!({
                    "task_id": task.id, "phase": "Work", "exit_code": exit.exit_code,
                    "launch_error": exit.launch_error,
                }),
            ),
        ];
        let (retry_effects, step) = decide_retry_or_escalate(
            view,
            owner,
            project,
            &task,
            true,
            Vec::new(),
            format!(
                "Job {seq}: work task failed (exit {}) with no retries left",
                exit.exit_code
            ),
        );
        effects.extend(retry_effects);
        (Vec::new(), effects, step)
    }
}

/// The record a failed attempt leaves behind. A container that never launched
/// has no logs to harvest, so its result is the only account of why it failed; a
/// FAILED run's harvested report (#187 `@chug:leg` lines) is likewise the whole
/// point of structured legs — a deploy that died mid-leg must record which leg
/// failed and which never ran. `None` keeps whatever the attempt already wrote.
fn decide_exited_failure_result(exit: &WorkExit) -> Option<TaskResult> {
    match (&exit.launch_error, &exit.structured) {
        (Some(reason), structured) => Some(TaskResult::Command {
            pass: false,
            exit_code: exit.exit_code,
            output: reason.clone(),
            structured: structured.clone(),
        }),
        (None, Some(structured)) => Some(TaskResult::Command {
            pass: false,
            exit_code: exit.exit_code,
            output: String::new(),
            structured: Some(structured.clone()),
        }),
        (None, None) => None,
    }
}

/// Exit 0 (§3.2): assemble the attempt's result, then apply the finish-line
/// guard. A headless work agent that ends its turn before committing dies with
/// its work in the container filesystem — the CLI exits 0, but `job/{seq}`
/// carries nothing beyond `base_ref`. Exit-0 + no summary is half that
/// signature; the branch read is the other half, so the decision pauses for it.
/// A non-empty summary — the agent declaring "no change is the correct outcome"
/// — proceeds. Scoped to AGENT work: a `command` task's effect is external (a
/// deploy produces no branch commits by design), so its exit code stays
/// authoritative.
fn decide_exited_zero(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    mut task: Task,
    exit: WorkExit,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    if task.result.is_none() {
        let sub = view.submission;
        task.result = Some(TaskResult::Work {
            summary: sub.and_then(|s| s.summary.clone()),
            structured: sub.and_then(|s| s.structured.clone()).or(exit.structured),
            cover_html: sub.and_then(|s| s.cover_html.clone()),
            token_usage: sub.and_then(|s| s.token_usage),
        });
    }
    if let (Some(measured), Some(TaskResult::Work { token_usage, .. })) =
        (exit.usage, task.result.as_mut())
    {
        *token_usage = Some(measured);
    }
    let summary_present = matches!(
        &task.result,
        Some(TaskResult::Work { summary: Some(s), .. }) if !s.trim().is_empty()
    );
    if !summary_present
        && view.job.state == JobState::Work
        && matches!(task.kind, TaskKind::Agent { .. })
    {
        return (
            Vec::new(),
            Vec::new(),
            WorkStep::CheckOutput {
                task: Box::new(task),
            },
        );
    }
    decide_completed(owner, project, task)
}

/// The finish-line guard's answer (§3.2). Commits beyond `base_ref` mean the
/// attempt did land work and completes normally; nothing beyond it is the
/// died-before-committing signature — retire the attempt `Failed` with a
/// machine-readable [`NO_OUTPUT_REASON`] (so the UI shows "exited without
/// producing changes" instead of a silent Done → review-fail cycle) and route it
/// through the SAME `work_retries` policy a nonzero exit uses. This is a genuine
/// agent failure and DOES spend a retry — contrast [`decide_infra_lost`].
fn decide_output_checked(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    mut task: Task,
    has_output: bool,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    if has_output {
        return decide_completed(owner, project, task);
    }
    let seq = view.job.id;
    task.state = TaskState::Failed;
    task.result = Some(TaskResult::Command {
        pass: false,
        exit_code: 0,
        output: "exited without producing changes".to_string(),
        structured: Some(serde_json::json!({ "reason": NO_OUTPUT_REASON })),
    });
    let mut effects = vec![
        Effect::PutTask {
            task: Box::new(task.clone()),
        },
        work_event(
            owner,
            project,
            seq,
            "task-failed",
            serde_json::json!({
                "task_id": task.id, "phase": "Work", "exit_code": 0,
                "reason": NO_OUTPUT_REASON,
            }),
        ),
    ];
    let (retry_effects, step) = decide_retry_or_escalate(
        view,
        owner,
        project,
        &task,
        true,
        Vec::new(),
        format!(
            "Job {seq}: work task exited 0 without producing changes \
             ({NO_OUTPUT_REASON}) and has no retries left"
        ),
    );
    effects.extend(retry_effects);
    (Vec::new(), effects, step)
}

/// A work attempt that finished the job's work: retire it `Done` and hand the
/// job on. A gate-fix task (job #154) returns straight to the merge gate — no
/// re-review, no eval CI — where gate CI is the final authority; everything else
/// goes to Evaluation (§3.2 steps 9–10).
fn decide_completed(
    owner: &str,
    project: &str,
    mut task: Task,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    task.state = TaskState::Done;
    let gate_fix = task.rework_reason == Some(ReworkReason::GateCompileFix);
    let effects = vec![
        Effect::PutTask {
            task: Box::new(task.clone()),
        },
        work_event(
            owner,
            project,
            task.job_seq,
            "task-completed",
            serde_json::json!({ "task_id": task.id, "phase": "Work" }),
        ),
    ];
    let step = if gate_fix {
        WorkStep::ReenterGate
    } else {
        WorkStep::Evaluate
    };
    (Vec::new(), effects, step)
}

/// An operator declined a Pending human-performed attempt (§1.2 claims). The
/// attempt is consumed through the normal work-failure path: retries remaining →
/// the next attempt launches per the DECLARED kind (an agent picks the work
/// right back up — no un-conversion), else escalation. Unlike a container crash
/// this is a deliberate handoff at a clean commit boundary, so the branch is
/// PRESERVED and the operator's notes carry forward as §4.3 context (#121).
fn decide_declined(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    mut task: Task,
    operator: &str,
    structured: serde_json::Value,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    let seq = view.job.id;
    task.state = TaskState::Failed;
    let mut effects = vec![
        Effect::PutTask {
            task: Box::new(task.clone()),
        },
        work_event(
            owner,
            project,
            seq,
            "task-failed",
            serde_json::json!({
                "task_id": task.id, "phase": "Work", "declined_by": operator,
            }),
        ),
    ];
    let handoff = vec![EvalResult {
        evaluator: format!("operator handoff ({operator})"),
        pass: false,
        structured: Some(structured),
        output: None,
    }];
    let (retry_effects, step) = decide_retry_or_escalate(
        view,
        owner,
        project,
        &task,
        false,
        handoff,
        format!("Job {seq}: work attempt failed (declined by operator) with no retries left"),
    );
    effects.extend(retry_effects);
    (Vec::new(), effects, step)
}

/// A Running work attempt whose container was GONE at restart (§3.6): an
/// infrastructure loss — docker pruned it, the node rebooted, colima restarted —
/// distinct from a real nonzero exit. Relaunch the SAME attempt WITHOUT spending
/// a `work_retries` budget (mirroring how a conflict rework does not spend
/// `rework_budget`), bounded by [`WorkView::infra_relaunch_cap`] so a genuinely
/// vanishing environment escalates instead of looping forever. The shim has
/// already retired the lost attempt — that retirement is shared with the
/// Evaluation phase and stays there.
fn decide_infra_lost(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    task: Task,
    losses: u32,
) -> (Vec<Transition>, Vec<Effect>, WorkStep) {
    let seq = view.job.id;
    debug_assert!(
        task.infra_loss,
        "task {} decided as an infrastructure loss without the stamp",
        task.id,
    );
    if losses > view.infra_relaunch_cap {
        let effects = vec![Effect::Escalate {
            owner: owner.to_string(),
            project: project.to_string(),
            seq,
            reason: INFRA_LOSS_REASON.to_string(),
            detail: format!(
                "Job {seq}: the work container was lost to infrastructure \
                 {losses} times without a real exit (docker prune, node reboot, \
                 colima restart). Escalating rather than relaunching forever."
            ),
            failing_task: Some(task.id),
        }];
        return (Vec::new(), effects, WorkStep::EscalatedDropExec);
    }
    (
        Vec::new(),
        Vec::new(),
        WorkStep::Retry {
            cycle: task.cycle,
            attempt: task.attempt,
            recover: true,
            eval_context_add: Vec::new(),
        },
    )
}

/// The one retry policy every Work failure path spends (§3.2): one
/// `work_retries` budget per attempt, then `work_retries_exhausted`. The callers
/// differ only in what the relaunch does with the branch (`recover`) and what
/// context it carries (`eval_context_add`) — those are values, not policies.
fn decide_retry_or_escalate(
    view: &WorkView<'_>,
    owner: &str,
    project: &str,
    task: &Task,
    recover: bool,
    eval_context_add: Vec<EvalResult>,
    exhausted_detail: String,
) -> (Vec<Effect>, WorkStep) {
    let work_retries = view.contract().work_retries.unwrap_or(0);
    if task.attempt <= work_retries {
        return (
            Vec::new(),
            WorkStep::Retry {
                cycle: task.cycle,
                attempt: task.attempt + 1,
                recover,
                eval_context_add,
            },
        );
    }
    let effects = vec![Effect::Escalate {
        owner: owner.to_string(),
        project: project.to_string(),
        seq: view.job.id,
        reason: "work_retries_exhausted".to_string(),
        detail: exhausted_detail,
        failing_task: Some(task.id),
    }];
    (effects, WorkStep::EscalatedDropExec)
}

/// One `job-events` announcement, the only publish shape this decider emits.
fn work_event(
    owner: &str,
    project: &str,
    seq: u64,
    event_type: &str,
    extra: serde_json::Value,
) -> Effect {
    Effect::PublishEvent {
        owner: owner.to_string(),
        project: project.to_string(),
        seq,
        event_type: event_type.to_string(),
        extra,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! Tier-1 coverage of every Work-phase branch: pure values in, pure values
    //! out, no NATS/Docker (`docs/reference/testing.md` tier 1). Before C6 the same branches
    //! needed a container backend and a live NATS to reach; the dispatcher's
    //! golden traces (`work_eval_merge_no_gate.yaml`, `gate_fix_fast_path.yaml`,
    //! `conflict_reentry.yaml`) pin the same decisions end-to-end.
    use super::*;

    fn sample_job(state: JobState) -> Job {
        let mut job: Job = serde_json::from_str(
            r#"{ "id": 7, "project": "acme/api", "type": "build",
                 "deps": [], "state": "Work", "branch": "job/7",
                 "base_ref": "abc123", "knowledge_tags": [], "factory": null,
                 "created_at": "2026-07-24T10:00:00Z", "ready_at": null }"#,
        )
        .expect("sample job");
        job.state = state;
        job
    }

    /// A job type from its JSON form — the same shape the YAML deserializes
    /// into, so every `serde(default)` applies exactly as in production.
    fn job_type(json: &str) -> JobType {
        serde_json::from_str(json).expect("job type")
    }

    fn agent_type() -> JobType {
        job_type(
            r#"{ "name": "build", "image": "img:latest",
                 "work": { "type": "agent", "prompt": "prompts/build.md" },
                 "work_retries": 1 }"#,
        )
    }

    fn view<'a>(job: &'a Job, jt: &'a JobType) -> WorkView<'a> {
        WorkView {
            job,
            job_type: Some(jt),
            cycle: 1,
            rework_reason: None,
            next_task_id: 3,
            session_id: "sess-1",
            human_brief: "\n\n---\n## Job Brief\n**Ship it**\n",
            agent_provider_default: Some("claude"),
            agent_model_default: Some("opus"),
            submission: None,
            infra_relaunch_cap: INFRA_RELAUNCH_CAP,
            draining: false,
            now: Utc::now(),
        }
    }

    fn work_task(state: TaskState, attempt: u32) -> Box<Task> {
        Box::new(Task {
            id: 2,
            job_seq: 7,
            project: "acme/api".into(),
            phase: TaskPhase::Work,
            cycle: 1,
            kind: TaskKind::Agent {
                provider: "claude".into(),
                model: None,
                prompt: "prompts/build.md".into(),
            },
            state,
            attempt,
            evaluator: None,
            label: None,
            stage: 0,
            performed_by: None,
            container_id: None,
            rework_reason: None,
            infra_loss: false,
            session_id: None,
            pending_reason: None,
            queued_at: None,
            reviewed_tip: None,
            workload_identities: vec![],
            result: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        })
    }

    fn labels(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .map(|e| match e {
                Effect::PublishEvent { event_type, .. } => format!("PublishEvent {event_type}"),
                other => other.port().to_string(),
            })
            .collect()
    }

    /// A launch-time park's payload plus the state it puts the job in —
    /// `Stalled` pre-work, `Escalated` after (§2.1, spec §575).
    fn park(effects: &[Effect]) -> (JobState, &str, &str, Option<u64>) {
        match effects.last().expect("an effect") {
            Effect::Stall {
                reason,
                detail,
                failing_task,
                ..
            } => (JobState::Stalled, reason, detail, *failing_task),
            Effect::Escalate {
                reason,
                detail,
                failing_task,
                ..
            } => (JobState::Escalated, reason, detail, *failing_task),
            other => panic!("expected a park, got {other:?}"),
        }
    }

    fn escalation(effects: &[Effect]) -> (&str, &str, Option<u64>) {
        match effects.last().expect("an effect") {
            Effect::Escalate {
                reason,
                detail,
                failing_task,
                ..
            } => (reason, detail, *failing_task),
            other => panic!("expected Escalate, got {other:?}"),
        }
    }

    fn created_task(effects: &[Effect]) -> &Task {
        match &effects[0] {
            Effect::CreateTask { task, .. } => task,
            other => panic!("expected CreateTask, got {other:?}"),
        }
    }

    fn retired_task(effects: &[Effect]) -> &Task {
        match &effects[0] {
            Effect::PutTask { task, .. } => task,
            other => panic!("expected PutTask, got {other:?}"),
        }
    }

    /// A clean first entry moves the record to Work and announces the start.
    #[test]
    fn entry_moves_to_work_and_announces_the_first_cycle() {
        let job = sample_job(JobState::Ready);
        let jt = agent_type();
        let (transitions, effects, step) = decide(
            &WorkView::entry(&job, Some(&jt), 1, Utc::now()),
            WorkEvent::Entered { failure: None },
        );
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to, JobState::Work);
        assert_eq!(labels(&effects), vec!["PublishEvent job-started"]);
        assert!(matches!(step, WorkStep::Begin));
    }

    /// A rework re-entry is already in Work: no transition, and no second
    /// `job-started` — the job started once.
    #[test]
    fn entry_on_a_rework_cycle_neither_transitions_nor_reannounces() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (transitions, effects, step) = decide(
            &WorkView::entry(&job, Some(&jt), 2, Utc::now()),
            WorkEvent::Entered { failure: None },
        );
        assert!(transitions.is_empty(), "already in Work");
        assert!(effects.is_empty());
        assert!(matches!(step, WorkStep::Begin));
    }

    /// Unknown config fields are tolerated (§14.2) but announced, once, at first
    /// launch — a "feature quietly off" config is visible without log grepping.
    #[test]
    fn entry_announces_tolerated_unknown_config_fields() {
        let job = sample_job(JobState::Ready);
        let jt = job_type(
            r#"{ "name": "build", "image": "img:latest",
                 "work": { "type": "agent", "prompt": "p.md" },
                 "sparkles": true }"#,
        );
        let (_, effects, _) = decide(
            &WorkView::entry(&job, Some(&jt), 1, Utc::now()),
            WorkEvent::Entered { failure: None },
        );
        assert_eq!(
            labels(&effects),
            vec!["PublishEvent job-started", "PublishEvent config-warning"]
        );
    }

    /// Config ahead of binary on a FIRST launch parks pre-work: Stalled, one
    /// park, Retry/Revoke only (§14.2 — the 2026-07-22 escalation storm).
    #[test]
    fn entry_schema_skew_on_a_first_launch_stalls() {
        let job = sample_job(JobState::Ready);
        let jt = agent_type();
        let (transitions, effects, step) = decide(
            &WorkView::entry(&job, Some(&jt), 1, Utc::now()),
            WorkEvent::Entered {
                failure: Some(EntryFailure::Contract(vec![ValidationError::new(
                    Some(7),
                    "min_dispatcher",
                    "config requires dispatcher >= 4",
                )])),
            },
        );
        assert!(transitions.is_empty(), "the Stall composite owns the flip");
        assert!(matches!(step, WorkStep::Idle));
        match &effects[0] {
            Effect::Stall { reason, detail, .. } => {
                assert_eq!(reason, "config_schema_skew");
                assert!(detail.contains("requires dispatcher >= 4"));
            }
            other => panic!("expected Stall, got {other:?}"),
        }
    }

    /// The same skew on a rework re-entry is post-work: Escalated, because
    /// Evaluation/WrapUp→Stalled is not in the §2.1 table.
    #[test]
    fn entry_schema_skew_after_the_first_cycle_escalates() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &WorkView::entry(&job, Some(&jt), 2, Utc::now()),
            WorkEvent::Entered {
                failure: Some(EntryFailure::Contract(vec![ValidationError::new(
                    Some(7),
                    "min_dispatcher",
                    "config requires dispatcher >= 4",
                )])),
            },
        );
        assert!(matches!(step, WorkStep::Idle));
        assert_eq!(escalation(&effects).0, "launch_validation_failed");
    }

    /// Any other contract error parks, with every field error verbatim.
    #[test]
    fn entry_contract_errors_park_with_the_reasons() {
        let job = sample_job(JobState::Ready);
        let (_, effects, _) = decide(
            &WorkView::entry(&job, None, 1, Utc::now()),
            WorkEvent::Entered {
                failure: Some(EntryFailure::Contract(vec![ValidationError::new(
                    Some(7),
                    "work.prompt",
                    "prompt file missing",
                )])),
            },
        );
        let (state, reason, detail, failing) = park(&effects);
        assert_eq!(state, JobState::Stalled);
        assert_eq!(reason, "launch_validation_failed");
        assert_eq!(
            detail,
            "Job 7 failed launch-time validation:\n- work.prompt: prompt file missing"
        );
        assert_eq!(failing, None, "no task exists at launch time");
    }

    /// The §2.2 launch-time KV pass names what is missing, in order.
    #[test]
    fn entry_missing_kv_parks_naming_every_name() {
        let job = sample_job(JobState::Ready);
        let (_, effects, _) = decide(
            &WorkView::entry(&job, None, 1, Utc::now()),
            WorkEvent::Entered {
                failure: Some(EntryFailure::MissingKv(vec![
                    "secret 'DEPLOY_KEY'".into(),
                    "var 'REGION'".into(),
                ])),
            },
        );
        assert_eq!(
            park(&effects).2,
            "Job 7: missing at launch: secret 'DEPLOY_KEY', var 'REGION'"
        );
    }

    /// **The §2.1 park-edge guard**: a job still `Ready` has no work task, so
    /// every launch-time park is `Stalled` — an `Escalate` here is a transition
    /// [`crate::state::assert_transition`] rejects, which silently strands the job.
    #[test]
    fn every_pre_work_park_stalls_a_ready_job() {
        let job = sample_job(JobState::Ready);
        for failure in [
            EntryFailure::Contract(vec![ValidationError::new(
                Some(7),
                "work.prompt",
                "missing",
            )]),
            EntryFailure::MissingKv(vec!["secret 'DEPLOY_KEY'".into()]),
            EntryFailure::BadInput("input 'sha': value contains ';'".into()),
        ] {
            let (_, effects, _) = decide(
                &WorkView::entry(&job, None, 1, Utc::now()),
                WorkEvent::Entered {
                    failure: Some(failure),
                },
            );
            assert_eq!(park(&effects).0, JobState::Stalled);
            assert!(
                crate::state::assert_transition(job.state, JobState::Stalled).is_ok(),
                "the park state must be an edge the §2.1 table admits"
            );
        }
    }

    /// The third launch-time pass (design #311 Decision 3): an input value that
    /// no longer clears the charset parks the job exactly like a missing secret —
    /// no container, and the violation named. Reaching this means an earlier pass
    /// was bypassed, which is why it parks rather than sanitizing.
    #[test]
    fn entry_bad_input_parks_naming_the_violation() {
        let job = sample_job(JobState::Ready);
        let (transitions, effects, step) = decide(
            &WorkView::entry(&job, None, 1, Utc::now()),
            WorkEvent::Entered {
                failure: Some(EntryFailure::BadInput(
                    "input 'sha': value contains ';'".into(),
                )),
            },
        );
        assert!(transitions.is_empty(), "a parked entry moves no record");
        assert!(matches!(step, WorkStep::Idle), "and launches nothing");
        let (state, reason, detail, failing) = park(&effects);
        assert_eq!(state, JobState::Stalled, "pre-work parks are Stalled");
        assert_eq!(reason, "launch_validation_failed");
        assert_eq!(
            detail,
            "Job 7: input rejected at launch: input 'sha': value contains ';'"
        );
        assert_eq!(failing, None, "no task exists at launch time");
    }

    /// An agent attempt: Running record, resolved provider/model, a minted
    /// session, and the launch handed to the shim.
    #[test]
    fn attempt_creates_a_running_agent_task_and_launches() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (transitions, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Attempt {
                cycle: 1,
                attempt: 1,
                resume: false,
            },
        );
        assert!(transitions.is_empty(), "Work entry owns the state flip");
        assert_eq!(labels(&effects), vec!["Core::task_create"]);
        let task = created_task(&effects);
        assert_eq!((task.id, task.attempt, task.cycle), (3, 1, 1));
        assert_eq!(task.state, TaskState::Running);
        assert_eq!(task.session_id.as_deref(), Some("sess-1"));
        assert!(task.started_at.is_some(), "an agent attempt starts now");
        match &task.kind {
            TaskKind::Agent {
                provider, model, ..
            } => {
                assert_eq!(provider, "claude");
                assert_eq!(model.as_deref(), Some("opus"), "platform default applies");
            }
            other => panic!("expected an agent task, got {other:?}"),
        }
        match step {
            WorkStep::Launch { task, resume } => {
                assert_eq!(task.id, 3);
                assert!(!resume);
            }
            other => panic!("expected Launch, got {other:?}"),
        }
    }

    /// §12.4: the per-job model override beats the type's and the platform's.
    #[test]
    fn attempt_model_resolution_prefers_the_job_override() {
        let mut job = sample_job(JobState::Work);
        job.model = Some("haiku".into());
        let jt = agent_type();
        let (_, effects, _) = decide(
            &view(&job, &jt),
            WorkEvent::Attempt {
                cycle: 1,
                attempt: 1,
                resume: false,
            },
        );
        match &created_task(&effects).kind {
            TaskKind::Agent { model, .. } => assert_eq!(model.as_deref(), Some("haiku")),
            other => panic!("expected an agent task, got {other:?}"),
        }
    }

    /// Declared command work runs its script: Running, launched — and no §4.2
    /// session, since no agent transcript exists to address.
    #[test]
    fn attempt_for_command_work_creates_a_running_command_task() {
        let job = sample_job(JobState::Work);
        let jt = job_type(
            r#"{ "name": "deploy", "image": "img:latest",
                 "work": { "type": "command", "run": "./deploy.sh" } }"#,
        );
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Attempt {
                cycle: 1,
                attempt: 1,
                resume: false,
            },
        );
        let task = created_task(&effects);
        assert_eq!(task.state, TaskState::Running);
        assert!(task.session_id.is_none(), "no agent runs, no session");
        assert!(task.started_at.is_some(), "a command attempt starts now");
        match &task.kind {
            TaskKind::Command { run } => assert_eq!(run, "./deploy.sh"),
            other => panic!("expected a command task, got {other:?}"),
        }
        assert!(matches!(step, WorkStep::Launch { resume: false, .. }));
    }

    /// Declared human work parks for the operator inbox: Pending, no session,
    /// no `started_at` (nobody has started), and the §4.3 brief in the prompt.
    #[test]
    fn attempt_for_human_work_parks_pending_with_the_brief() {
        let job = sample_job(JobState::Work);
        let jt = job_type(
            r#"{ "name": "signoff", "work": { "type": "human", "prompt": "Approve: " } }"#,
        );
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Attempt {
                cycle: 1,
                attempt: 1,
                resume: false,
            },
        );
        let task = created_task(&effects);
        assert_eq!(task.state, TaskState::Pending);
        assert!(task.session_id.is_none(), "no agent runs, no session");
        assert!(task.started_at.is_none(), "nobody has started it");
        match &task.kind {
            TaskKind::Human { prompt } => {
                assert!(prompt.starts_with("Approve: ") && prompt.contains("Ship it"))
            }
            other => panic!("expected a human task, got {other:?}"),
        }
        assert!(matches!(step, WorkStep::Park));
    }

    /// A claimed attempt (§1.2) keeps its DECLARED kind, parks for the human,
    /// records the performer, mints no session — and consumes the claim in the
    /// same decision, so the claim covers exactly this one attempt.
    #[test]
    fn attempt_with_a_claim_parks_and_consumes_the_claim() {
        let mut job = sample_job(JobState::Work);
        job.claim_next = true;
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Attempt {
                cycle: 1,
                attempt: 2,
                resume: false,
            },
        );
        assert_eq!(labels(&effects), vec!["Core::task_create", "jobs.put"]);
        let task = created_task(&effects);
        assert_eq!(task.state, TaskState::Pending);
        assert_eq!(task.performed_by, Some(Performer::Human));
        assert!(task.session_id.is_none(), "a claimed attempt runs no agent");
        assert!(
            task.started_at.is_some(),
            "the claim was the 'I'm starting' declaration"
        );
        assert!(
            matches!(task.kind, TaskKind::Agent { .. }),
            "kind is declared, not converted"
        );
        match &effects[1] {
            Effect::PutJob { job } => assert!(!job.claim_next, "the claim is consumed"),
            other => panic!("expected PutJob, got {other:?}"),
        }
        assert!(matches!(step, WorkStep::Park));
    }

    /// A gate-fix cycle labels its task so the story reads `gate-fix` rather
    /// than a bare Work row (job #154/#146).
    #[test]
    fn attempt_in_a_gate_fix_cycle_carries_the_label_and_reason() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut v = view(&job, &jt);
        v.rework_reason = Some(ReworkReason::GateCompileFix);
        let (_, effects, _) = decide(
            &v,
            WorkEvent::Attempt {
                cycle: 1,
                attempt: 1,
                resume: false,
            },
        );
        let task = created_task(&effects);
        assert_eq!(task.label.as_deref(), Some("gate-fix"));
        assert_eq!(task.rework_reason, Some(ReworkReason::GateCompileFix));
    }

    /// §3.6 drain: no record, no container, no state change — restart
    /// reconciliation relaunches.
    #[test]
    fn attempt_while_draining_creates_nothing() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut v = view(&job, &jt);
        v.draining = true;
        let (transitions, effects, step) = decide(
            &v,
            WorkEvent::Attempt {
                cycle: 1,
                attempt: 1,
                resume: false,
            },
        );
        assert!(transitions.is_empty() && effects.is_empty());
        assert!(matches!(step, WorkStep::Hold));
    }

    /// Exit 0 with a summary: the attempt is Done and the job goes to Evaluation.
    #[test]
    fn exit_zero_with_a_summary_completes_and_evaluates() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut v = view(&job, &jt);
        let submission = WorkSubmissionView {
            summary: Some("did the thing".into()),
            ..WorkSubmissionView::default()
        };
        v.submission = Some(&submission);
        let (_, effects, step) = decide(
            &v,
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit::default(),
            },
        );
        assert_eq!(
            labels(&effects),
            vec!["tasks.put", "PublishEvent task-completed"]
        );
        let task = retired_task(&effects);
        assert_eq!(task.state, TaskState::Done);
        assert!(task.completed_at.is_some());
        match task.result.as_ref().expect("result") {
            TaskResult::Work { summary, .. } => {
                assert_eq!(summary.as_deref(), Some("did the thing"));
            }
            other => panic!("expected a work result, got {other:?}"),
        }
        assert!(matches!(step, WorkStep::Evaluate));
    }

    /// A completed gate-fix attempt goes back to the merge gate, not Evaluation
    /// — gate CI is the final authority (job #154).
    #[test]
    fn exit_zero_on_a_gate_fix_task_reenters_the_gate() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut v = view(&job, &jt);
        let submission = WorkSubmissionView {
            summary: Some("fixed the build".into()),
            ..WorkSubmissionView::default()
        };
        v.submission = Some(&submission);
        let mut task = work_task(TaskState::Running, 1);
        task.rework_reason = Some(ReworkReason::GateCompileFix);
        let (_, _, step) = decide(
            &v,
            WorkEvent::Exited {
                task,
                exit: WorkExit::default(),
            },
        );
        assert!(matches!(step, WorkStep::ReenterGate));
    }

    /// Measured CLI usage wins over the agent's self-reported number.
    #[test]
    fn exit_zero_prefers_measured_token_usage() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut v = view(&job, &jt);
        let submission = WorkSubmissionView {
            summary: Some("done".into()),
            token_usage: Some(TokenUsage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }),
            ..WorkSubmissionView::default()
        };
        v.submission = Some(&submission);
        let measured = TokenUsage {
            input_tokens: 900,
            output_tokens: 100,
            cache_read_tokens: None,
            cache_write_tokens: None,
        };
        let (_, effects, _) = decide(
            &v,
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit {
                    usage: Some(measured),
                    ..WorkExit::default()
                },
            },
        );
        match retired_task(&effects).result.as_ref().expect("result") {
            TaskResult::Work { token_usage, .. } => {
                assert_eq!(token_usage.expect("usage").input_tokens, 900);
            }
            other => panic!("expected a work result, got {other:?}"),
        }
    }

    /// Exit 0 with no summary on an AGENT task is half the died-before-
    /// committing signature: the decision pauses for the branch read.
    #[test]
    fn exit_zero_without_a_summary_asks_for_the_branch_read() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit::default(),
            },
        );
        assert!(
            effects.is_empty(),
            "nothing is written until the read lands"
        );
        assert!(matches!(step, WorkStep::CheckOutput { .. }));
    }

    /// A COMMAND work task's effect is external, so its exit code stays
    /// authoritative — the finish-line guard never fires for it (§3.2).
    #[test]
    fn exit_zero_without_a_summary_completes_a_command_task() {
        let job = sample_job(JobState::Work);
        let jt = job_type(
            r#"{ "name": "deploy", "image": "img:latest",
                 "work": { "type": "command", "run": "./deploy.sh" } }"#,
        );
        let mut task = work_task(TaskState::Running, 1);
        task.kind = TaskKind::Command {
            run: "./deploy.sh".into(),
        };
        let (_, _, step) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task,
                exit: WorkExit::default(),
            },
        );
        assert!(matches!(step, WorkStep::Evaluate));
    }

    /// A revoked job's orphaned container exiting late is not guarded — it
    /// completes and no-ops on the invalid transition downstream, as before.
    #[test]
    fn exit_zero_for_a_job_that_left_work_is_not_guarded() {
        let job = sample_job(JobState::Revoked);
        let jt = agent_type();
        let (_, _, step) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit::default(),
            },
        );
        assert!(matches!(step, WorkStep::Evaluate));
    }

    /// The branch moved: the attempt did land work, so it completes normally.
    #[test]
    fn output_check_with_commits_completes_the_attempt() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::OutputChecked {
                task: work_task(TaskState::Running, 1),
                has_output: true,
            },
        );
        assert_eq!(retired_task(&effects).state, TaskState::Done);
        assert!(matches!(step, WorkStep::Evaluate));
    }

    /// Nothing beyond `base_ref`: the attempt is retired Failed with the
    /// machine-readable reason and spends a `work_retries` budget.
    #[test]
    fn output_check_without_commits_fails_and_retries() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::OutputChecked {
                task: work_task(TaskState::Running, 1),
                has_output: false,
            },
        );
        assert_eq!(
            labels(&effects),
            vec!["tasks.put", "PublishEvent task-failed"]
        );
        let task = retired_task(&effects);
        assert_eq!(task.state, TaskState::Failed);
        match task.result.as_ref().expect("result") {
            TaskResult::Command { structured, .. } => {
                assert_eq!(
                    structured.as_ref().expect("reason")["reason"],
                    serde_json::json!(NO_OUTPUT_REASON)
                );
            }
            other => panic!("expected a command result, got {other:?}"),
        }
        match step {
            WorkStep::Retry {
                attempt, recover, ..
            } => {
                assert_eq!(attempt, 2);
                assert!(recover, "a crashed attempt's branch is recovered or reset");
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    /// With the budget spent, an empty-handed attempt escalates naming the
    /// reason — not a silent Done → review-fail cycle.
    #[test]
    fn output_check_without_commits_escalates_when_the_budget_is_spent() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::OutputChecked {
                task: work_task(TaskState::Running, 2),
                has_output: false,
            },
        );
        let (reason, detail, failing) = escalation(&effects);
        assert_eq!(reason, "work_retries_exhausted");
        assert!(detail.contains(NO_OUTPUT_REASON));
        assert_eq!(failing, Some(2));
        assert!(step.drops_exec(), "the slice is released before the effect");
    }

    /// A nonzero exit fails the attempt and burns a retry.
    #[test]
    fn nonzero_exit_fails_the_attempt_and_retries() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit {
                    exit_code: 3,
                    ..WorkExit::default()
                },
            },
        );
        assert_eq!(
            labels(&effects),
            vec!["tasks.put", "PublishEvent task-failed"]
        );
        assert_eq!(retired_task(&effects).state, TaskState::Failed);
        assert!(matches!(step, WorkStep::Retry { attempt: 2, .. }));
    }

    /// §4.2 lets a live container submit its result before dying (an agent-run
    /// timeout exits -1 with nothing attached). The retired record keeps that
    /// summary — it is what `ensure_exec_state` rebuilds `work_submission` from,
    /// and the squash body downstream of it.
    #[test]
    fn nonzero_exit_preserves_an_already_submitted_result() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut task = work_task(TaskState::Running, 1);
        task.result = Some(TaskResult::Work {
            summary: Some("partial work".into()),
            structured: None,
            token_usage: None,
            cover_html: None,
        });
        let (_, effects, _) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task,
                exit: WorkExit {
                    exit_code: -1,
                    ..WorkExit::default()
                },
            },
        );
        match retired_task(&effects).result.as_ref().expect("result kept") {
            TaskResult::Work { summary, .. } => {
                assert_eq!(summary.as_deref(), Some("partial work"))
            }
            other => panic!("expected the submitted work result, got {other:?}"),
        }
    }

    /// A container that never launched has no logs, so the launch error is the
    /// attempt's only account of why it failed.
    #[test]
    fn launch_failure_records_the_reason_on_the_task() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, _) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit {
                    exit_code: -1,
                    launch_error: Some("container launch failed: bad image".into()),
                    ..WorkExit::default()
                },
            },
        );
        match retired_task(&effects).result.as_ref().expect("result") {
            TaskResult::Command { output, pass, .. } => {
                assert!(!pass);
                assert_eq!(output, "container launch failed: bad image");
            }
            other => panic!("expected a command result, got {other:?}"),
        }
    }

    /// #187: a FAILED run's harvested report is kept — which leg failed and
    /// which never ran is exactly what a mid-leg death must record.
    #[test]
    fn nonzero_exit_keeps_a_harvested_structured_report() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, _) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit {
                    exit_code: 1,
                    structured: Some(serde_json::json!({ "legs": ["build"] })),
                    ..WorkExit::default()
                },
            },
        );
        match retired_task(&effects).result.as_ref().expect("result") {
            TaskResult::Command { structured, .. } => {
                assert_eq!(
                    structured.as_ref().expect("report")["legs"][0],
                    serde_json::json!("build")
                );
            }
            other => panic!("expected a command result, got {other:?}"),
        }
    }

    /// Budget spent: escalate `work_retries_exhausted` naming the failing task,
    /// with the slice released before the effect runs.
    #[test]
    fn nonzero_exit_escalates_when_the_budget_is_spent() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 2),
                exit: WorkExit {
                    exit_code: 3,
                    ..WorkExit::default()
                },
            },
        );
        let (reason, detail, failing) = escalation(&effects);
        assert_eq!(reason, "work_retries_exhausted");
        assert_eq!(
            detail,
            "Job 7: work task failed (exit 3) with no retries left"
        );
        assert_eq!(failing, Some(2));
        assert!(step.drops_exec());
    }

    /// A type declaring no `work_retries` gets none: the first failure escalates.
    #[test]
    fn a_type_without_retries_escalates_on_the_first_failure() {
        let job = sample_job(JobState::Work);
        let jt = job_type(
            r#"{ "name": "build", "image": "img:latest",
                 "work": { "type": "agent", "prompt": "p.md" } }"#,
        );
        let (_, _, step) = decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task: work_task(TaskState::Running, 1),
                exit: WorkExit {
                    exit_code: 1,
                    ..WorkExit::default()
                },
            },
        );
        assert!(step.drops_exec());
    }

    /// A stale monitor's exit for an already-resolved attempt is noise: nothing
    /// is written, so an attempt is never resolved twice.
    #[test]
    fn an_exit_for_an_already_resolved_attempt_is_ignored() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        for state in [TaskState::Done, TaskState::Failed, TaskState::Pending] {
            let (transitions, effects, step) = decide(
                &view(&job, &jt),
                WorkEvent::Exited {
                    task: work_task(state, 1),
                    exit: WorkExit::default(),
                },
            );
            assert!(transitions.is_empty() && effects.is_empty(), "{state:?}");
            assert!(matches!(step, WorkStep::Idle), "{state:?} must be ignored");
        }
    }

    /// A declined attempt fails, keeps the branch, and carries the operator's
    /// notes into the next attempt's §4.3 context (#121).
    #[test]
    fn a_declined_attempt_retries_preserving_the_branch() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Declined {
                task: work_task(TaskState::Pending, 1),
                operator: "ana".into(),
                structured: serde_json::json!({ "notes": "handing back" }),
            },
        );
        assert_eq!(
            labels(&effects),
            vec!["tasks.put", "PublishEvent task-failed"]
        );
        assert_eq!(retired_task(&effects).state, TaskState::Failed);
        match step {
            WorkStep::Retry {
                attempt,
                recover,
                eval_context_add,
                ..
            } => {
                assert_eq!(attempt, 2);
                assert!(!recover, "a deliberate handoff never resets the branch");
                assert_eq!(eval_context_add[0].evaluator, "operator handoff (ana)");
                assert!(!eval_context_add[0].pass);
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    /// A decline with no retries left escalates like any exhausted attempt.
    #[test]
    fn a_declined_attempt_with_no_retries_escalates() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::Declined {
                task: work_task(TaskState::Pending, 2),
                operator: "ana".into(),
                structured: serde_json::json!({}),
            },
        );
        assert_eq!(
            escalation(&effects).1,
            "Job 7: work attempt failed (declined by operator) with no retries left"
        );
        assert!(step.drops_exec());
    }

    /// A lost container relaunches the SAME attempt: no budget spent, branch
    /// recovered in case the lost attempt pushed commits.
    #[test]
    fn an_infra_loss_relaunches_the_same_attempt_without_spending_budget() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut task = work_task(TaskState::Failed, 2);
        task.infra_loss = true;
        let (_, effects, step) = decide(&view(&job, &jt), WorkEvent::InfraLost { task, losses: 1 });
        assert!(effects.is_empty(), "the shim already retired the attempt");
        match step {
            WorkStep::Retry {
                attempt, recover, ..
            } => {
                assert_eq!(attempt, 2, "the same attempt, not the next one");
                assert!(recover);
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    /// A vanishing environment escalates rather than relaunching forever
    /// (docs/reference/style.md Tier 2 #3): the cap is on the lineage, not the budget.
    #[test]
    fn an_infra_loss_past_the_cap_escalates() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut task = work_task(TaskState::Failed, 1);
        task.infra_loss = true;
        let (_, effects, step) = decide(
            &view(&job, &jt),
            WorkEvent::InfraLost {
                task,
                losses: INFRA_RELAUNCH_CAP + 1,
            },
        );
        let (reason, detail, failing) = escalation(&effects);
        assert_eq!(reason, INFRA_LOSS_REASON);
        assert!(detail.contains("4 times without a real exit"));
        assert_eq!(failing, Some(2));
        assert!(step.drops_exec());
    }

    /// Exactly at the cap still relaunches — the bound is "more than", so the
    /// budget arithmetic can't be off by one.
    #[test]
    fn an_infra_loss_at_the_cap_still_relaunches() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut task = work_task(TaskState::Failed, 1);
        task.infra_loss = true;
        let (_, _, step) = decide(
            &view(&job, &jt),
            WorkEvent::InfraLost {
                task,
                losses: INFRA_RELAUNCH_CAP,
            },
        );
        assert!(matches!(step, WorkStep::Retry { .. }));
    }

    /// A terminal job never enters Work — `assert_transition` would reject the
    /// edge anyway, but the decision is the earlier bug.
    #[test]
    #[should_panic(expected = "terminal job")]
    #[cfg(debug_assertions)]
    fn entering_work_for_a_terminal_job_is_a_caller_bug() {
        let job = sample_job(JobState::Done);
        let jt = agent_type();
        decide(
            &WorkView::entry(&job, Some(&jt), 1, Utc::now()),
            WorkEvent::Entered { failure: None },
        );
    }

    /// An exit is decided for a Work task or not at all: an evaluator's exit
    /// reaching this decider is a routing bug.
    #[test]
    #[should_panic(expected = "not a Work task")]
    #[cfg(debug_assertions)]
    fn deciding_a_non_work_exit_is_a_caller_bug() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        let mut task = work_task(TaskState::Running, 1);
        task.phase = TaskPhase::Evaluation;
        decide(
            &view(&job, &jt),
            WorkEvent::Exited {
                task,
                exit: WorkExit::default(),
            },
        );
    }

    /// An infrastructure relaunch is decided only for an attempt the shim
    /// actually stamped as lost — otherwise it would silently skip the budget.
    #[test]
    #[should_panic(expected = "without the stamp")]
    #[cfg(debug_assertions)]
    fn an_unstamped_infra_loss_is_a_caller_bug() {
        let job = sample_job(JobState::Work);
        let jt = agent_type();
        decide(
            &view(&job, &jt),
            WorkEvent::InfraLost {
                task: work_task(TaskState::Failed, 1),
                losses: 1,
            },
        );
    }
}

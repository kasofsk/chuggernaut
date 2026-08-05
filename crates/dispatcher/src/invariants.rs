//! Executable invariant checker (docs/reference/contracts.md §3, docs/design/215-refactor-plan.md B1).
//!
//! accepts:   a read-only [`CoreState`] view of the single-writer's in-memory state.
//! emits:     a [`Vec<Violation>`] — empty when every invariant holds.
//! guarantees: pure and total (no I/O, no `.await`, never panics); each check is a
//!             negative-space assertion (docs/reference/style.md Tier 2 #2) naming the offending
//!             job/queue entry so a failure localizes itself.
//! spec §:    §1.4/§2.3 (graph), §2.1 (state machine), §3.1 (ready queue),
//!            §3.2 (one attempt in flight), §3.3 (merge gate depth-1).
//!
//! This is the harvested form of the "must/always/never" statements that
//! previously lived only as comments, `get_or_insert_with` defensive code, and
//! assertions scattered through the integration tests. It is the source of
//! truth for the dispatcher's data invariants; run it after every message in
//! tests to convert tribal knowledge into a regression net. Because all state
//! lives in one place (single-writer design), the check is cheap.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use types::JobState;

use crate::exec::ExecState;
use crate::graph::JobGraph;
use crate::queue::ReadyQueue;
use chuggernaut_domain::decide::merge_gate::MergeGateState;

/// A single broken invariant. [`invariant`](Violation::invariant) is a stable,
/// greppable identifier; [`detail`](Violation::detail) names the offending
/// job/queue entry so the failure points at the corrupted datum, not just the
/// rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub invariant: &'static str,
    pub detail: String,
}

impl Violation {
    fn new(invariant: &'static str, detail: impl Into<String>) -> Self {
        Violation {
            invariant,
            detail: detail.into(),
        }
    }
}

/// Everything one message broke: the message that was just handled, plus the
/// violations live state carried once it had been. The message name is what turns
/// a violation into a diagnosis — it names the writer that introduced the
/// corruption, not merely the corrupted datum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breach {
    /// The [`Msg`](crate::core::Msg) variant the single writer had just handled.
    pub message: &'static str,
    pub violations: Vec<Violation>,
}

/// A shared log of [`Breach`]es observed *inside* the single writer, in message
/// order (refactor-plan B1a).
///
/// A test-only observability hook, the same shape as [`crate::trace::TraceSink`]:
/// a test attaches a clone to a [`Core`](crate::core::Core) before
/// [`spawn`](crate::core::spawn) moves it, then drains this one to assert. That
/// indirection is what lets a test driving the actor over a `CoreHandle` check
/// invariants at all — the `Core` itself is gone into the actor task.
///
/// Draining is a plain mutex read with **no round trip through the actor**, which
/// is the property that matters: threading a message between every pair of real
/// messages would let the actor finish its post-message drains before the test's
/// next observation, changing the very timing these tests pin.
#[derive(Debug, Clone, Default)]
pub struct InvariantSink(Arc<Mutex<Vec<Breach>>>);

impl InvariantSink {
    /// A fresh, empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Check `state` and log a [`Breach`] if anything is broken. Clean states
    /// record nothing, so the log length is the number of *broken* messages.
    pub(crate) fn check(&self, message: &'static str, state: &CoreState) {
        let violations = check_invariants(state);
        if !violations.is_empty() {
            self.lock().push(Breach {
                message,
                violations,
            });
        }
    }

    /// Take every breach logged since the last drain. Draining (rather than
    /// snapshotting) keeps one broken message from failing every later assertion
    /// in the test.
    pub fn drain(&self) -> Vec<Breach> {
        std::mem::take(&mut *self.lock())
    }

    /// Lock the shared log. A poisoned mutex means a thread panicked mid-record;
    /// recovering the guard lets that panic surface as the real failure rather
    /// than a lock-poison red herring.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Breach>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A read-only view of the single-writer's in-memory scheduling state — the
/// subset the invariants constrain; borrowing, so building it costs nothing,
/// and [`Core::state`](crate::core::Core::state) hands one out.
/// Distinct from `&Core` so the checker stays pure and independently
/// constructible in unit tests, and so the future decider view
/// (docs/reference/contracts.md layer 2) has a seam to grow from.
pub struct CoreState<'a> {
    /// One job graph per project slug (`owner/project`).
    pub graphs: &'a HashMap<String, JobGraph>,
    /// FIFO of Ready jobs awaiting a work slot (spec §3.1).
    pub queue: &'a ReadyQueue,
    /// Execution slice per in-flight job, keyed `(owner, project, seq)`.
    pub active: &'a HashMap<(String, String, u64), ExecState>,
    /// Per-project landing pipeline (spec §3.3): the FIFO queue + the
    /// depth-1 gate slot, as the decider-owned value (refactor-plan C2 —
    /// depth-1 is enforced by `gating: Option<u64>`'s type, so this checker
    /// keeps only the value-level clauses).
    pub merge_gates: &'a HashMap<String, MergeGateState>,
}

/// Slug is what `graphs`/`merge_gates` are keyed by (`owner/project`).
fn slug_of(owner: &str, project: &str) -> String {
    format!("{owner}/{project}")
}

impl CoreState<'_> {
    /// The in-memory job record for a slug + seq, if the dispatcher knows it.
    fn job_state(&self, slug: &str, seq: u64) -> Option<JobState> {
        self.graphs
            .get(slug)
            .and_then(|g| g.get(seq))
            .map(|j| j.state)
    }
}

/// Check every data invariant against `state`; empty result means all hold.
///
/// Note: "at most one attempt in flight per job" (§3.2) and "merge gate is
/// depth-1 per project" (§3.3) are enforced *structurally* — `active` is keyed
/// by seq and `gating` by slug, so a `HashMap` cannot hold a second entry. The
/// checks below verify the *content* those structures must carry.
pub fn check_invariants(state: &CoreState) -> Vec<Violation> {
    let mut out = Vec::new();
    check_ready_queue_only_ready(state, &mut out);
    check_rdeps_inverts_deps(state, &mut out);
    check_active_is_executing(state, &mut out);
    check_merge_queue_is_wrapup(state, &mut out);
    check_terminal_is_absorbing(state, &mut out);
    check_one_live_job_per_schedule(state, &mut out);
    out
}

/// Design #310 Decision 4: at most one **non-terminal** job per
/// `(project, schedule)`. A second live job means the at-most-one-in-flight
/// backpressure failed and the schedule is multiplying, so this is the negative
/// space the skip rule exists to protect.
fn check_one_live_job_per_schedule(state: &CoreState, out: &mut Vec<Violation>) {
    for (slug, graph) in state.graphs {
        let mut live: HashMap<&str, u64> = HashMap::new();
        for job in graph.jobs() {
            let Some(name) = job.schedule.as_deref().filter(|_| !job.state.is_terminal()) else {
                continue;
            };
            if let Some(first) = live.insert(name, job.id) {
                out.push(Violation::new(
                    "one_live_job_per_schedule",
                    format!(
                        "{slug}: schedule '{name}' has live jobs #{first} and #{}",
                        job.id
                    ),
                ));
            }
        }
    }
}

/// §3.1: the ready queue holds only jobs that exist and are `Ready`.
fn check_ready_queue_only_ready(state: &CoreState, out: &mut Vec<Violation>) {
    for q in state.queue.iter() {
        let slug = slug_of(&q.owner, &q.project);
        match state.job_state(&slug, q.seq) {
            Some(JobState::Ready) => {}
            Some(other) => out.push(Violation::new(
                "ready_queue_only_ready",
                format!("{slug}#{} is queued but {other:?}, not Ready", q.seq),
            )),
            None => out.push(Violation::new(
                "ready_queue_only_ready",
                format!("{slug}#{} is queued but has no job record", q.seq),
            )),
        }
    }
}

/// §1.4/§2.3: the reverse-dependency index is the exact inverse of the forward
/// `deps` edges — every forward edge has its reverse, and no reverse edge is
/// invented. Checked in both directions per project graph.
fn check_rdeps_inverts_deps(state: &CoreState, out: &mut Vec<Violation>) {
    for (slug, graph) in state.graphs {
        for job in graph.jobs() {
            for &upstream in &job.deps {
                if !graph.dependents(upstream).contains(&job.id) {
                    out.push(Violation::new(
                        "rdeps_inverts_deps",
                        format!(
                            "{slug}: {} deps on {upstream} but is absent from its dependents",
                            job.id
                        ),
                    ));
                }
            }
        }
        for job in graph.jobs() {
            for &dependent in graph.dependents(job.id) {
                let depends = graph
                    .get(dependent)
                    .is_some_and(|d| d.deps.contains(&job.id));
                if !depends {
                    out.push(Violation::new(
                        "rdeps_inverts_deps",
                        format!(
                            "{slug}: {dependent} is a reverse-dep of {} but does not depend on it",
                            job.id
                        ),
                    ));
                }
            }
        }
    }
}

/// §3.2/§3.3: an execution slice exists only for a job that is genuinely
/// executing (Work / Evaluation / WrapUp / Escalated). A job with no record, or
/// one still pre-work or already terminal, must not carry one.
fn check_active_is_executing(state: &CoreState, out: &mut Vec<Violation>) {
    for (owner, project, seq) in state.active.keys() {
        let slug = slug_of(owner, project);
        match state.job_state(&slug, *seq) {
            Some(
                JobState::Work | JobState::Evaluation | JobState::WrapUp | JobState::Escalated,
            ) => {}
            Some(other) => out.push(Violation::new(
                "active_is_executing",
                format!("{slug}#{seq} holds an execution slice but is {other:?}"),
            )),
            None => out.push(Violation::new(
                "active_is_executing",
                format!("{slug}#{seq} holds an execution slice but has no job record"),
            )),
        }
    }
}

/// §3.3: every job in a merge queue — queued or currently gating — is in
/// `WrapUp`, and the gating seq has already left the queue (it is popped before
/// the gate starts, so it must not appear in both).
fn check_merge_queue_is_wrapup(state: &CoreState, out: &mut Vec<Violation>) {
    for (slug, gate) in state.merge_gates {
        for &seq in &gate.queue {
            if state.job_state(slug, seq) != Some(JobState::WrapUp) {
                out.push(Violation::new(
                    "merge_queue_is_wrapup",
                    format!("{slug}#{seq} is in the merge queue but not WrapUp"),
                ));
            }
        }
        if let Some(seq) = gate.gating {
            if state.job_state(slug, seq) != Some(JobState::WrapUp) {
                out.push(Violation::new(
                    "merge_queue_is_wrapup",
                    format!("{slug}#{seq} is gating but not WrapUp"),
                ));
            }
            if gate.queue.contains(&seq) {
                out.push(Violation::new(
                    "merge_queue_is_wrapup",
                    format!("{slug}#{seq} is gating yet still sits in the merge queue"),
                ));
            }
        }
    }
}

/// §2.1: terminal states (Done/Revoked) are absorbing — a terminal job is
/// invisible to scheduling, so no live structure may still reference it.
fn check_terminal_is_absorbing(state: &CoreState, out: &mut Vec<Violation>) {
    let mut flag = |slug: &str, seq: u64, structure: &str| {
        if state
            .job_state(slug, seq)
            .is_some_and(JobState::is_terminal)
        {
            out.push(Violation::new(
                "terminal_is_absorbing",
                format!("{slug}#{seq} is terminal but still referenced by the {structure}"),
            ));
        }
    };
    for q in state.queue.iter() {
        flag(&slug_of(&q.owner, &q.project), q.seq, "ready queue");
    }
    for (owner, project, seq) in state.active.keys() {
        flag(&slug_of(owner, project), *seq, "active set");
    }
    for (slug, gate) in state.merge_gates {
        for &seq in &gate.queue {
            flag(slug, seq, "merge queue");
        }
        if let Some(seq) = gate.gating {
            flag(slug, seq, "gating slot");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::collections::HashMap;
    use types::{Job, JobState};

    fn job(slug: &str, id: u64, deps: &[u64], state: JobState) -> Job {
        Job {
            r#type: "build".into(),
            deps: deps.to_vec(),
            state,
            ..test_utils::fixture::job(slug, id)
        }
    }

    fn graph_of(jobs: Vec<Job>) -> HashMap<String, JobGraph> {
        let mut graphs: HashMap<String, JobGraph> = HashMap::new();
        for j in jobs {
            graphs.entry(j.project.clone()).or_default().insert(j);
        }
        graphs
    }

    /// Empty `active` map — building an [`ExecState`] needs a full `JobType`, so
    /// the execution-slice invariants are exercised at the integration tier
    /// where a real Core populates it; here we hold it empty.
    fn no_active() -> HashMap<(String, String, u64), ExecState> {
        HashMap::new()
    }

    struct Fixture {
        graphs: HashMap<String, JobGraph>,
        queue: ReadyQueue,
        active: HashMap<(String, String, u64), ExecState>,
        merge_gates: HashMap<String, MergeGateState>,
    }

    impl Fixture {
        fn new(jobs: Vec<Job>) -> Self {
            Fixture {
                graphs: graph_of(jobs),
                queue: ReadyQueue::default(),
                active: no_active(),
                merge_gates: HashMap::new(),
            }
        }

        fn state(&self) -> CoreState<'_> {
            CoreState {
                graphs: &self.graphs,
                queue: &self.queue,
                active: &self.active,
                merge_gates: &self.merge_gates,
            }
        }

        fn check(&self) -> Vec<Violation> {
            check_invariants(&self.state())
        }
    }

    fn queued(seq: u64) -> crate::queue::QueuedJob {
        crate::queue::QueuedJob {
            owner: "acme".into(),
            project: "api".into(),
            seq,
        }
    }

    #[test]
    fn clean_state_has_no_violations() {
        let mut f = Fixture::new(vec![
            job("acme/api", 1, &[], JobState::Ready),
            job("acme/api", 2, &[1], JobState::Blocked),
            job("acme/api", 3, &[], JobState::Done),
        ]);
        f.queue.enqueue(queued(1));
        assert_eq!(f.check(), vec![]);
    }

    #[test]
    fn non_ready_job_in_queue_is_flagged() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::Blocked)]);
        f.queue.enqueue(queued(1));
        let v = f.check();
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].invariant, "ready_queue_only_ready");
        assert!(v[0].detail.contains("acme/api#1"));
    }

    #[test]
    fn queued_job_without_record_is_flagged() {
        let mut f = Fixture::new(vec![]);
        f.queue.enqueue(queued(9));
        let v = f.check();
        assert!(
            v.iter().any(|x| x.invariant == "ready_queue_only_ready"),
            "{v:?}"
        );
    }

    /// Design #310 Decision 4: a schedule's finished runs stack up freely, but
    /// two live ones mean the skip rule stopped holding.
    #[test]
    fn a_second_live_job_for_one_schedule_is_flagged() {
        let scheduled = |id: u64, state: JobState| Job {
            schedule: Some("nightly".into()),
            ..job("acme/api", id, &[], state)
        };
        let clean = Fixture::new(vec![
            scheduled(1, JobState::Done),
            scheduled(2, JobState::Revoked),
            scheduled(3, JobState::Work),
            job("acme/api", 4, &[], JobState::Work),
        ]);
        assert_eq!(clean.check(), vec![]);

        let doubled = Fixture::new(vec![
            scheduled(1, JobState::Work),
            scheduled(2, JobState::Ready),
        ]);
        let v = doubled.check();
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].invariant, "one_live_job_per_schedule");
        assert!(v[0].detail.contains("nightly"), "{v:?}");
    }

    #[test]
    fn terminal_job_in_queue_is_absorbing_violation() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::Done)]);
        f.queue.enqueue(queued(1));
        let v = f.check();
        assert!(v.iter().any(|x| x.invariant == "ready_queue_only_ready"));
        assert!(v.iter().any(|x| x.invariant == "terminal_is_absorbing"));
    }

    #[test]
    fn rdeps_stays_inverse_across_reinsert() {
        let mut f = Fixture::new(vec![
            job("acme/api", 1, &[], JobState::Done),
            job("acme/api", 2, &[1], JobState::Ready),
        ]);
        f.graphs
            .get_mut("acme/api")
            .unwrap()
            .insert(job("acme/api", 2, &[], JobState::Ready));
        assert!(
            !f.check()
                .iter()
                .any(|x| x.invariant == "rdeps_inverts_deps"),
            "{:?}",
            f.check()
        );
    }

    #[test]
    fn gating_non_wrapup_job_is_flagged() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::Evaluation)]);
        f.merge_gates.entry("acme/api".into()).or_default().gating = Some(1);
        let v = f.check();
        assert!(
            v.iter().any(|x| x.invariant == "merge_queue_is_wrapup"),
            "{v:?}"
        );
    }

    #[test]
    fn merge_queue_non_wrapup_job_is_flagged() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::Ready)]);
        f.merge_gates.entry("acme/api".into()).or_default().queue =
            std::collections::VecDeque::from([1]);
        let v = f.check();
        assert!(
            v.iter().any(|x| x.invariant == "merge_queue_is_wrapup"),
            "{v:?}"
        );
    }

    #[test]
    fn gating_seq_still_in_queue_is_flagged() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::WrapUp)]);
        f.merge_gates.entry("acme/api".into()).or_default().gating = Some(1);
        f.merge_gates.entry("acme/api".into()).or_default().queue =
            std::collections::VecDeque::from([1]);
        let v = f.check();
        assert!(
            v.iter()
                .any(|x| x.invariant == "merge_queue_is_wrapup" && x.detail.contains("still sits")),
            "{v:?}"
        );
    }

    /// The sink is plain in-memory data (no NATS/Docker), so its record/drain
    /// contract is unit-testable directly — the tier a test belongs at
    /// (`docs/reference/testing.md`).
    #[test]
    fn sink_logs_only_broken_messages_and_drains_once() {
        let clean = Fixture::new(vec![job("acme/api", 1, &[], JobState::Ready)]);
        let mut broken = Fixture::new(vec![job("acme/api", 1, &[], JobState::Blocked)]);
        broken.queue.enqueue(queued(1));

        let sink = InvariantSink::new();
        sink.check("CreateJob", &clean.state());
        assert_eq!(sink.drain(), vec![]);

        sink.check("ReleaseJob", &broken.state());
        let breaches = sink.drain();
        assert_eq!(breaches.len(), 1, "{breaches:?}");
        assert_eq!(breaches[0].message, "ReleaseJob");
        assert!(
            breaches[0]
                .violations
                .iter()
                .any(|v| v.invariant == "ready_queue_only_ready"),
            "{breaches:?}"
        );
        assert_eq!(sink.drain(), vec![]);
    }

    /// Clones share one log, which is what lets a test keep a handle on the sink
    /// after `spawn` has moved the `Core` that records into it.
    #[test]
    fn sink_clones_share_one_log() {
        let mut broken = Fixture::new(vec![job("acme/api", 1, &[], JobState::Done)]);
        broken.queue.enqueue(queued(1));
        let sink = InvariantSink::new();
        sink.clone().check("TaskExited", &broken.state());
        assert_eq!(sink.drain().len(), 1);
    }
}

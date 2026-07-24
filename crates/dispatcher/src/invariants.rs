//! Executable invariant checker (contracts.md §3, refactor-plan.md B1).
//!
//! accepts:   a read-only [`CoreState`] view of the single-writer's in-memory state.
//! emits:     a [`Vec<Violation>`] — empty when every invariant holds.
//! guarantees: pure and total (no I/O, no `.await`, never panics); each check is a
//!             negative-space assertion (STYLE.md Tier 2 #2) naming the offending
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

use std::collections::{HashMap, VecDeque};

use types::JobState;

use crate::exec::ExecState;
use crate::graph::JobGraph;
use crate::queue::ReadyQueue;

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

/// A read-only view of the single-writer's in-memory scheduling state — the
/// subset the invariants constrain. Borrowing, so building it costs nothing;
/// [`Core::state`](crate::core::Core::state) hands one out. Kept as a distinct
/// type (rather than checking `&Core` directly) so the checker stays pure and
/// independently constructible in unit tests, and so the future decider view
/// (contracts.md layer 2) has a seam to grow from.
pub struct CoreState<'a> {
    /// One job graph per project slug (`owner/project`).
    pub graphs: &'a HashMap<String, JobGraph>,
    /// FIFO of Ready jobs awaiting a work slot (spec §3.1).
    pub queue: &'a ReadyQueue,
    /// Execution slice per in-flight job, keyed `(owner, project, seq)`.
    pub active: &'a HashMap<(String, String, u64), ExecState>,
    /// Per-project merge queue: seqs landing, in FIFO order (spec §3.3).
    pub merge_queue: &'a HashMap<String, VecDeque<u64>>,
    /// Project slug → the one seq whose merge gate is currently running.
    pub gating: &'a HashMap<String, u64>,
}

/// Slug is what `graphs`/`merge_queue`/`gating` are keyed by (`owner/project`).
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
    out
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
        // Forward → reverse: every dep edge is reflected in `dependents`.
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
        // Reverse → forward: every dependent genuinely depends on its upstream.
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
    for (slug, queued) in state.merge_queue {
        for &seq in queued {
            if state.job_state(slug, seq) != Some(JobState::WrapUp) {
                out.push(Violation::new(
                    "merge_queue_is_wrapup",
                    format!("{slug}#{seq} is in the merge queue but not WrapUp"),
                ));
            }
        }
    }
    for (slug, &seq) in state.gating {
        if state.job_state(slug, seq) != Some(JobState::WrapUp) {
            out.push(Violation::new(
                "merge_queue_is_wrapup",
                format!("{slug}#{seq} is gating but not WrapUp"),
            ));
        }
        if state
            .merge_queue
            .get(slug)
            .is_some_and(|q| q.contains(&seq))
        {
            out.push(Violation::new(
                "merge_queue_is_wrapup",
                format!("{slug}#{seq} is gating yet still sits in the merge queue"),
            ));
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
    for (slug, queued) in state.merge_queue {
        for &seq in queued {
            flag(slug, seq, "merge queue");
        }
    }
    for (slug, &seq) in state.gating {
        flag(slug, seq, "gating map");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use types::{Job, JobState};

    fn job(slug: &str, id: u64, deps: &[u64], state: JobState) -> Job {
        Job {
            id,
            project: slug.to_string(),
            r#type: "build".into(),
            title: String::new(),
            description: String::new(),
            members: vec![],
            batch_id: None,
            cover_html: None,
            deps: deps.to_vec(),
            state,
            branch: format!("job/{id}"),
            base_ref: None,
            knowledge_tags: vec![],
            eval: vec![],
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            created_at: chrono::Utc::now(),
            ready_at: None,
            completed_at: None,
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
        merge_queue: HashMap<String, VecDeque<u64>>,
        gating: HashMap<String, u64>,
    }

    impl Fixture {
        fn new(jobs: Vec<Job>) -> Self {
            Fixture {
                graphs: graph_of(jobs),
                queue: ReadyQueue::default(),
                active: no_active(),
                merge_queue: HashMap::new(),
                gating: HashMap::new(),
            }
        }

        fn check(&self) -> Vec<Violation> {
            check_invariants(&CoreState {
                graphs: &self.graphs,
                queue: &self.queue,
                active: &self.active,
                merge_queue: &self.merge_queue,
                gating: &self.gating,
            })
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

    #[test]
    fn terminal_job_in_queue_is_absorbing_violation() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::Done)]);
        f.queue.enqueue(queued(1));
        let v = f.check();
        // Done is neither Ready nor absorbing-clean: both invariants fire.
        assert!(v.iter().any(|x| x.invariant == "ready_queue_only_ready"));
        assert!(v.iter().any(|x| x.invariant == "terminal_is_absorbing"));
    }

    #[test]
    fn rdeps_stays_inverse_across_reinsert() {
        // The graph maintains rdeps internally, so a well-formed graph is always
        // clean — including after a dep-changing re-insert (the pruning path).
        let mut f = Fixture::new(vec![
            job("acme/api", 1, &[], JobState::Done),
            job("acme/api", 2, &[1], JobState::Ready),
        ]);
        // Re-insert #2 dropping its dep on #1: rdeps(1) must lose #2.
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
        f.gating.insert("acme/api".into(), 1);
        let v = f.check();
        assert!(
            v.iter().any(|x| x.invariant == "merge_queue_is_wrapup"),
            "{v:?}"
        );
    }

    #[test]
    fn merge_queue_non_wrapup_job_is_flagged() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::Ready)]);
        f.merge_queue.insert("acme/api".into(), VecDeque::from([1]));
        let v = f.check();
        assert!(
            v.iter().any(|x| x.invariant == "merge_queue_is_wrapup"),
            "{v:?}"
        );
    }

    #[test]
    fn gating_seq_still_in_queue_is_flagged() {
        let mut f = Fixture::new(vec![job("acme/api", 1, &[], JobState::WrapUp)]);
        f.gating.insert("acme/api".into(), 1);
        f.merge_queue.insert("acme/api".into(), VecDeque::from([1]));
        let v = f.check();
        assert!(
            v.iter()
                .any(|x| x.invariant == "merge_queue_is_wrapup" && x.detail.contains("still sits")),
            "{v:?}"
        );
    }
}

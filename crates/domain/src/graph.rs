//! In-memory job graph, one per project (spec §1.4, §2.3). Rebuilt from
//! `jobs.*` KV on startup; the KV record stays the source of truth — this is
//! the dispatcher's working copy for wiring checks, dependency queries, and
//! revoke cascades.
//!
//! - **Accepts:** the `jobs.*` KV on startup; dependency/wiring queries and
//!   revoke cascades from `core`.
//! - **Emits:** rdeps maintenance and dependency answers — no KV writes.
//! - **Guarantees:** the KV record stays the source of truth; this is a derived
//!   working copy, rebuilt on startup.
//! - **Spec:** §1.4, §2.3.

use std::collections::{HashMap, HashSet};
use types::{Job, JobState};

#[derive(Default)]
pub struct JobGraph {
    jobs: HashMap<u64, Job>,
    /// Inverse dependency index: seq → jobs that depend on it.
    rdeps: HashMap<u64, Vec<u64>>,
}

impl JobGraph {
    pub fn insert(&mut self, job: Job) {
        if let Some(old) = self.jobs.get(&job.id) {
            for &upstream in &old.deps {
                if !job.deps.contains(&upstream)
                    && let Some(rd) = self.rdeps.get_mut(&upstream)
                {
                    rd.retain(|&d| d != job.id);
                }
            }
        }
        for &upstream in &job.deps {
            let deps = self.rdeps.entry(upstream).or_default();
            if !deps.contains(&job.id) {
                deps.push(job.id);
            }
        }
        self.jobs.insert(job.id, job);
    }

    pub fn get(&self, seq: u64) -> Option<&Job> {
        self.jobs.get(&seq)
    }

    pub fn get_mut(&mut self, seq: u64) -> Option<&mut Job> {
        self.jobs.get_mut(&seq)
    }

    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    /// Jobs that directly depend on `seq`.
    pub fn dependents(&self, seq: u64) -> &[u64] {
        self.rdeps.get(&seq).map(Vec::as_slice).unwrap_or_default()
    }

    /// All upstream dependencies of `seq` are Done.
    pub fn deps_done(&self, seq: u64) -> bool {
        self.jobs
            .get(&seq)
            .map(|job| {
                job.deps.iter().all(|dep| {
                    self.jobs
                        .get(dep)
                        .is_some_and(|d| d.state == JobState::Done)
                })
            })
            .unwrap_or(false)
    }

    /// Would wiring `candidate` (with its dependencies) close a cycle?
    /// DFS upstream from the candidate's deps looking for the candidate.
    pub fn creates_cycle(&self, candidate_seq: u64, deps: &[u64]) -> bool {
        let mut stack: Vec<u64> = deps.to_vec();
        let mut seen = HashSet::new();
        while let Some(seq) = stack.pop() {
            if seq == candidate_seq {
                return true;
            }
            if !seen.insert(seq) {
                continue;
            }
            if let Some(job) = self.jobs.get(&seq) {
                stack.extend(job.deps.iter().copied());
            }
        }
        false
    }

    /// Transitive dependents of `seq` currently in a cascade-eligible state
    /// (Frozen/Blocked/Ready — spec §2.1 Revoked row). Dependents in
    /// Work/Evaluation/Escalated stop the cascade at that edge.
    pub fn cascade_targets(&self, seq: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack = vec![seq];
        let mut seen = HashSet::new();
        while let Some(current) = stack.pop() {
            for &dep in self.dependents(current) {
                if !seen.insert(dep) {
                    continue;
                }
                if let Some(job) = self.jobs.get(&dep)
                    && matches!(
                        job.state,
                        JobState::Frozen | JobState::Blocked | JobState::Ready
                    )
                {
                    out.push(dep);
                    stack.push(dep);
                }
            }
        }
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use chrono::Utc;

    fn job(seq: u64, deps: &[u64], state: JobState) -> Job {
        Job {
            id: seq,
            project: "acme/api".into(),
            r#type: "t".into(),
            title: String::new(),
            description: String::new(),
            members: vec![],
            batch_id: None,
            cover_html: None,
            deps: deps.to_vec(),
            state,
            branch: format!("job/{seq}"),
            base_ref: None,
            knowledge_tags: vec![],
            eval: vec![],
            require_approval: false,
            timeout: None,
            model: None,
            claim_next: false,
            escalation: None,
            factory: None,
            schedule: None,
            created_at: Utc::now(),
            ready_at: None,
            completed_at: None,
            inputs: Default::default(),
            groups: vec![],
            task_time_ms: None,
        }
    }

    #[test]
    fn deps_done_and_dependents() {
        let mut g = JobGraph::default();
        g.insert(job(1, &[], JobState::Done));
        g.insert(job(2, &[], JobState::Work));
        g.insert(job(3, &[1, 2], JobState::Blocked));
        assert!(!g.deps_done(3));
        g.get_mut(2).unwrap().state = JobState::Done;
        assert!(g.deps_done(3));
        assert_eq!(g.dependents(1), &[3]);
    }

    #[test]
    fn cycle_detection() {
        let mut g = JobGraph::default();
        g.insert(job(1, &[], JobState::Frozen));
        g.insert(job(2, &[1], JobState::Frozen));
        g.insert(job(3, &[2], JobState::Frozen));
        assert!(g.creates_cycle(1, &[3]));
        assert!(!g.creates_cycle(4, &[3]));
        assert!(g.creates_cycle(2, &[2]));
    }

    #[test]
    fn reinsert_prunes_stale_reverse_edges() {
        let mut g = JobGraph::default();
        g.insert(job(1, &[], JobState::Frozen));
        g.insert(job(2, &[1], JobState::Draft));
        assert_eq!(g.dependents(1), &[2]);
        g.insert(job(2, &[], JobState::Draft));
        assert!(g.dependents(1).is_empty());
        assert!(g.cascade_targets(1).is_empty());
    }

    #[test]
    fn cascade_stops_at_in_flight_dependents() {
        let mut g = JobGraph::default();
        g.insert(job(1, &[], JobState::Work));
        g.insert(job(2, &[1], JobState::Blocked));
        g.insert(job(3, &[2], JobState::Frozen));
        g.insert(job(4, &[1], JobState::Work));
        g.insert(job(5, &[4], JobState::Frozen));
        assert_eq!(g.cascade_targets(1), vec![2, 3]);
    }
}

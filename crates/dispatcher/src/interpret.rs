//! The effect interpreter (refactor-plan B2, `contracts.md` §2).
//!
//! One place that turns an [`Effect`] value into the port call it names. It is
//! the executable other half of [`crate::effects`]: the enum is the vocabulary
//! the deciders emit, and [`Core::interpret`] is what performs it. Every phase
//! decider (C1–C6) now runs its effects through here. The interpreter holds the
//! *only* remaining coupling to `&mut Core`, so the deciders that feed it stay
//! pure.
//!
//! - **Accepts:** one [`Effect`] at a time, in the single-writer actor.
//! - **Emits:** exactly the side effect the variant names — a KV write, a
//!   container op, a publish, a repo mutation — through the existing ports.
//! - **Guarantees:** the match is exhaustive (every variant has an arm) and each
//!   arm dispatches to the port method [`Effect::port`] and the module-header
//!   table promise. No decision logic lives here — an arm never chooses *what*
//!   to do, only performs the effect handed to it.
//! - **Spec:** §3.1–3.4; `contracts.md` §2.

use crate::core::{Core, Result};
use crate::effects::{CredentialAccess, Effect};

/// Map the vocabulary's serde mirror back onto the auth crate's type. Lives
/// here (not as a `From` impl) because the domain crate must not depend on the
/// async `auth` crate, and the orphan rule forbids a dispatcher-side `From`
/// between the two foreign types — the interpreter is the boundary where pure
/// effect data meets the port types, so the mapping is its to own.
fn cert_access(access: CredentialAccess) -> auth::ssh::CertAccess {
    match access {
        CredentialAccess::ReadWrite => auth::ssh::CertAccess::ReadWrite,
        CredentialAccess::ReadOnly => auth::ssh::CertAccess::ReadOnly,
    }
}

/// What performing an effect produced, for the effects whose results are
/// decision inputs (contracts.md §2, the continuation contract): the shim
/// maps an `Outcome` onto the decider's next event against a fresh view.
/// Dispatcher-owned — it carries `vcs` port types the pure crate must not see.
#[derive(Debug)]
pub enum Outcome {
    /// The effect produced nothing a decider branches on.
    Done,
    /// A `SquashMerge`/`CreateSquashCandidate` build outcome.
    Merge(vcs::MergeOutcome),
    /// An `AdvanceDefault` CAS refusal — HEAD moved under the candidate. A
    /// decision input ("refinalize"), never bubbled as a failure (§3.3).
    CasRefused,
    /// A `RebaseOntoWithConflict` outcome (conflict files as data).
    Rebase(vcs::ConflictRebaseOutcome),
    /// The `LaunchGateStage` slots — the gate round the shim parks.
    GateSlots(Vec<crate::eval::EvalSlot>),
    /// The `LaunchEvalStage` slots — the Evaluation stage now in flight, which
    /// re-enters the eval decider as `StageLaunched` (refactor-plan C5).
    EvalSlots(Vec<crate::eval::EvalSlot>),
    /// A `LaunchEvaluator` task id — the slot's new task after a retry or an
    /// evidence-free relaunch.
    EvaluatorTask(u64),
}

impl Core {
    /// Execute one [`Effect`] through the port it maps to. See
    /// [`crate::effects`] for the variant → port-method table.
    ///
    /// Returns the effect's [`Outcome`]; most arms produce [`Outcome::Done`].
    /// Result-carrying arms hand their port's answer back so the calling shim
    /// can re-enter its decider with it as the next event — the effect stays
    /// fire-and-forget from the decider's side.
    #[allow(
        clippy::too_many_lines,
        reason = "TODO(track-C): pre-existing debt, dissolved as this path moves to a pure decider."
    )]
    pub async fn interpret(&mut self, effect: Effect) -> Result<Outcome> {
        match effect {
            Effect::SetJobState { job, to } => {
                let mut job = *job;
                self.set_state(&mut job, to).await?;
            }
            Effect::PutJob { job } => {
                self.jobs.put(&job).await?;
                self.graphs
                    .entry(job.project.clone())
                    .or_default()
                    .insert((*job).clone());
            }
            Effect::AppendRdep {
                owner,
                project,
                dep_seq,
                dependent_seq,
            } => {
                self.rdeps
                    .append(&owner, &project, dep_seq, dependent_seq)
                    .await?;
            }
            Effect::RemoveRdep {
                owner,
                project,
                dep_seq,
                dependent_seq,
            } => {
                self.rdeps
                    .remove(&owner, &project, dep_seq, dependent_seq)
                    .await?;
            }

            Effect::PutTask { task } => {
                self.task_put(&task).await?;
                if let Some(trace) = &self.trace
                    && matches!(task.kind, types::TaskKind::Human { .. })
                    && task.phase == types::TaskPhase::Escalation
                {
                    trace.effect("PutTask Human(escalation)");
                }
            }
            Effect::CreateTask {
                owner,
                project,
                task,
                extra,
            } => {
                self.task_create(&owner, &project, &task, extra).await?;
            }
            Effect::PutProject {
                owner,
                project,
                record,
            } => {
                self.projects.put(&owner, &project, &record).await?;
            }

            Effect::PublishEvent {
                owner,
                project,
                seq,
                event_type,
                extra,
            } => {
                self.publish(&owner, &project, seq, &event_type, extra)
                    .await?;
            }
            Effect::PublishStatus { subject, payload } => {
                self.store.publish(&subject, &payload).await?;
            }
            Effect::WriteKv { bucket, key, value } => {
                self.store
                    .raw_bucket(&bucket)
                    .await?
                    .put_json(&key, &value)
                    .await?;
            }

            Effect::KillContainer { container_id } => {
                self.backend.kill(&container_id).await?;
            }
            Effect::RemoveContainer { container_id } => {
                self.backend.remove(&container_id).await?;
            }

            Effect::LaunchWorkTask {
                owner,
                project,
                seq,
                cycle,
                attempt,
                resume,
            } => {
                Box::pin(self.launch_work_task(&owner, &project, seq, cycle, attempt, resume))
                    .await?;
            }
            Effect::LaunchWrapupTask {
                owner,
                project,
                seq,
                attempt,
            } => {
                self.launch_wrapup_task(&owner, &project, seq, attempt)
                    .await?;
            }
            Effect::LaunchGateFix {
                owner,
                project,
                seq,
                new_base,
                failures,
                compiler_output,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect("LaunchGateFix");
                }
                Box::pin(self.launch_gate_fix(
                    &owner,
                    &project,
                    seq,
                    new_base,
                    failures,
                    compiler_output,
                ))
                .await?;
            }
            Effect::DeferLaunch {
                owner,
                project,
                seq,
                task,
                reason,
            } => {
                let mut task = *task;
                self.defer_launch(&owner, &project, seq, &mut task, reason)
                    .await?;
            }

            Effect::SquashMerge {
                owner,
                project,
                seq,
                base_ref,
                job_type,
                summary,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect("SquashMerge");
                }
                let outcome = self
                    .repos
                    .squash_merge(
                        &owner,
                        &project,
                        seq,
                        &base_ref,
                        &job_type,
                        summary.as_deref(),
                    )
                    .await?;
                return Ok(Outcome::Merge(outcome));
            }
            Effect::DeleteBranch {
                owner,
                project,
                branch,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect(format!("DeleteBranch {branch}"));
                }
                let _ = self.repos.delete_branch(&owner, &project, &branch).await;
            }
            Effect::CreateSquashCandidate {
                owner,
                project,
                seq,
                base_ref,
                job_type,
                summary,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect("CreateSquashCandidate");
                }
                let outcome = self
                    .repos
                    .create_squash_candidate(
                        &owner,
                        &project,
                        seq,
                        &base_ref,
                        &job_type,
                        summary.as_deref(),
                    )
                    .await?;
                return Ok(Outcome::Merge(outcome));
            }
            Effect::AdvanceDefault {
                owner,
                project,
                commit,
                expected_old_head,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect("AdvanceDefault");
                }
                if let Err(e) = self
                    .repos
                    .advance_default(&owner, &project, &commit, &expected_old_head)
                    .await
                {
                    tracing::warn!(
                        "gate promote for {owner}/{project}: HEAD moved under candidate ({e}); refinalizing"
                    );
                    return Ok(Outcome::CasRefused);
                }
            }
            Effect::RebaseOntoWithConflict {
                owner,
                project,
                seq,
                new_base,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect("RebaseOntoWithConflict");
                }
                let outcome = self
                    .repos
                    .rebase_onto_with_conflict(&owner, &project, seq, &new_base)
                    .await?;
                return Ok(Outcome::Rebase(outcome));
            }
            Effect::LaunchGateStage {
                owner,
                project,
                seq,
                gate_branch,
                cycle,
                evaluators,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect("LaunchGateStage");
                }
                let slots = self
                    .launch_gate_stage(&owner, &project, seq, &gate_branch, cycle, evaluators)
                    .await?;
                return Ok(Outcome::GateSlots(slots));
            }
            Effect::LaunchEvalStage {
                owner,
                project,
                seq,
                branch,
                cycle,
                evaluators,
            } => {
                let slots = self
                    .launch_eval_stage(&owner, &project, seq, &branch, cycle, evaluators)
                    .await?;
                return Ok(Outcome::EvalSlots(slots));
            }
            Effect::LaunchEvaluator {
                owner,
                project,
                seq,
                branch,
                cycle,
                evaluator,
                attempt,
            } => {
                let task_id = self
                    .launch_evaluator_task(
                        &owner,
                        &project,
                        seq,
                        types::TaskPhase::Evaluation,
                        &branch,
                        cycle,
                        &evaluator,
                        attempt,
                    )
                    .await?;
                return Ok(Outcome::EvaluatorTask(task_id));
            }
            Effect::EnterWork {
                owner,
                project,
                seq,
                cycle,
                eval_context,
                merge_conflict,
                rework_reason,
            } => {
                if let Some(trace) = &self.trace {
                    trace.effect("EnterWork");
                }
                Box::pin(self.enter_work(
                    &owner,
                    &project,
                    seq,
                    cycle,
                    eval_context,
                    merge_conflict,
                    rework_reason,
                ))
                .await?;
            }
            Effect::IssueCredentials {
                owner,
                project,
                seq,
                access,
                ttl_secs,
            } => {
                if let Some(ca_key) = &self.config.ssh_ca {
                    let ca = auth::ssh::SshCa::new(ca_key);
                    let ttl = chrono::Duration::seconds(ttl_secs as i64);
                    ca.issue_job_credential(&owner, &project, seq, cert_access(access), ttl)
                        .await
                        .map_err(|e| {
                            crate::core::CoreError::Config(format!("issuing job ssh cert: {e}"))
                        })?;
                }
            }

            Effect::Escalate {
                owner,
                project,
                seq,
                reason,
                detail,
                failing_task,
            } => {
                self.escalate(&owner, &project, seq, &reason, detail, failing_task)
                    .await?;
            }
            Effect::Stall {
                owner,
                project,
                seq,
                reason,
                detail,
                failing_task,
            } => {
                self.stall(&owner, &project, seq, &reason, detail, failing_task)
                    .await?;
            }
        }
        Ok(Outcome::Done)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    //! The vocabulary ↔ port-type mapping is the one piece of interpreter
    //! logic that is pure, so it is unit-tested here; the port dispatch itself
    //! is exercised end-to-end by the Tier-2 tests.
    use super::*;

    #[test]
    fn cert_access_maps_the_serde_mirror_onto_auth() {
        assert_eq!(
            cert_access(CredentialAccess::ReadWrite),
            auth::ssh::CertAccess::ReadWrite,
        );
        assert_eq!(
            cert_access(CredentialAccess::ReadOnly),
            auth::ssh::CertAccess::ReadOnly,
        );
    }
}

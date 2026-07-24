//! The effect interpreter (refactor-plan B2, `contracts.md` §2).
//!
//! One place that turns an [`Effect`] value into the port call it names. It is
//! the executable other half of [`crate::effects`]: the enum is the vocabulary
//! deciders will emit, and [`Core::interpret`] is what performs it. Today no
//! decider exists, so nothing in production calls this yet — it lands ahead of
//! its callers (Track C migrates them one decider at a time). The interpreter
//! holds the *only* remaining coupling to `&mut Core`, so the deciders that
//! feed it stay pure.
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

impl Core {
    /// Execute one [`Effect`] through the port it maps to. See
    /// [`crate::effects`] for the variant → port-method table.
    ///
    /// Skeleton, per refactor-plan B2: it performs the effect but does not yet
    /// consume any *value* an effect produces (a `SquashMerge` outcome, a minted
    /// credential) — the deciders that branch on those results are extracted in
    /// Track C, which is where those return paths get wired.
    pub async fn interpret(&mut self, effect: Effect) -> Result<()> {
        match effect {
            // --- Job records & graph ---
            Effect::SetJobState { job, to } => {
                let mut job = *job;
                self.set_state(&mut job, to).await?;
            }
            Effect::PutJob { job } => {
                self.jobs.put(&job).await?;
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

            // --- Task & project records ---
            Effect::PutTask { task } => {
                self.tasks.put(&task).await?;
                // Golden-trace label parity: Human escalation task writes are
                // the one PutTask production callers recorded before the
                // decider migration (B3 fixtures pin the exact string).
                if let Some(trace) = &self.trace
                    && matches!(task.kind, types::TaskKind::Human { .. })
                    && task.phase == types::TaskPhase::Escalation
                {
                    trace.effect("PutTask Human(escalation)");
                }
            }
            Effect::PutProject {
                owner,
                project,
                record,
            } => {
                self.projects.put(&owner, &project, &record).await?;
            }

            // --- Events & status snapshots ---
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

            // --- Container lifecycle ---
            Effect::KillContainer { container_id } => {
                self.backend.kill(&container_id).await?;
            }
            Effect::RemoveContainer { container_id } => {
                self.backend.remove(&container_id).await?;
            }

            // --- Task launches ---
            Effect::LaunchWorkTask {
                owner,
                project,
                seq,
                cycle,
                attempt,
                resume,
            } => {
                self.launch_work_task(&owner, &project, seq, cycle, attempt, resume)
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
                self.launch_gate_fix(&owner, &project, seq, new_base, failures, compiler_output)
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

            // --- Repository mutations ---
            Effect::SquashMerge {
                owner,
                project,
                seq,
                base_ref,
                job_type,
                summary,
            } => {
                // The `MergeOutcome` (Merged/Conflict/…) drives the finalize
                // decider, which is not extracted yet (Track C); the skeleton
                // performs the merge and drops the outcome.
                let _ = self
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
            }
            Effect::DeleteBranch {
                owner,
                project,
                branch,
            } => {
                self.repos.delete_branch(&owner, &project, &branch).await?;
            }

            // --- Credentials (§7.4) ---
            Effect::IssueCredentials {
                owner,
                project,
                seq,
                access,
                ttl_secs,
            } => {
                // No SSH front configured (file:// dev repos, tests) → nothing to
                // mint, exactly as `ssh_credential_files` short-circuits.
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

            // --- Escalation composites ---
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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

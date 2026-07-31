//! The Ready-phase shim (spec §2.1, §2.2, §3.1) — refactor-plan C4, the
//! dispatcher half of `chuggernaut_domain::decide::ready`.
//!
//! Everything here is imperative shell: the decision itself — dependency
//! satisfaction, the `base_ref` pin, queue admission at both ends, the
//! Blocked→Ready re-validation fork — is a pure function in the domain crate,
//! and this module only gathers its view, applies its transitions, interprets
//! its effects, and performs the bookkeeping its `ReadyStep` names. The one
//! piece of real work it owns is the §2.2 Ready-transition pass, which reads
//! refs and so cannot be pure: the decider *gates* it, this *performs* it, and
//! the verdict re-enters as the decider's next event (contracts.md §2's
//! continuation contract).
//!
//! - **Accepts:** a job's Ready-phase events — a validated release
//!   (`core::release_job`), a dependency reaching Done (`core::on_job_done`) or
//!   restart reconciliation re-checking a parked job, and the ready queue
//!   handing a job a launch slot (`core::drain_queue`).
//! - **Emits:** the §2.1 transitions Frozen|Draft→Ready|Blocked and
//!   Blocked→Ready|Stalled through the `set_state` funnel; the decider's
//!   effects through `Core::interpret`; queue admission, a Draft batch's
//!   membership commit, and the Ready→Work hand-off. The write that first pins
//!   `base_ref` also carries the declared-input default fill, so this shim hands
//!   the decider the `inputs:` block of the type it loaded at that same ref
//!   (§1.1, design #311 Decision 3).
//! - **Guarantees:** no decision of its own — every branch here is a `match` on
//!   a value the decider returned. Queue admission and the membership commit
//!   land between the transitions and the effects (admission is part of
//!   committing the §2.1 record); the re-validation runs only when the decider
//!   asked for it, so a job that was never going to move costs no ref reads.
//! - **Spec:** §2.1, §2.2, §3.1 steps 2 and 5, §3.6 step 3; contracts.md §2.

use crate::core::{Core, Result};
use crate::decide::ready;
use crate::queue::QueuedJob;
use crate::release;
use chrono::Utc;
use types::Job;

impl Core {
    /// §2.1 Blocked→Ready with the §2.2 Ready-transition re-validation pass.
    /// No-op unless the job is Blocked with all dependencies Done — the C4
    /// decider's `DepsChanged` decision, whose `Revalidate` step drives the
    /// pass below. Also used by restart reconciliation (§3.6 step 3), which
    /// re-drives it for every parked job.
    pub(crate) async fn try_unblock(&mut self, owner: &str, project: &str, seq: u64) -> Result<()> {
        if self.must_get(owner, project, seq).is_err() {
            return Ok(());
        }
        self.run_ready(owner, project, seq, ready::ReadyEvent::DepsChanged)
            .await
    }

    /// The C4 shim (contracts.md §2), the four-step shape C1 set: gather the
    /// reads into the view, call the pure decider, apply its transitions through
    /// `set_state`, run its effects through `interpret` — plus the
    /// dispatcher-side bookkeeping the returned [`ready::ReadyStep`] names,
    /// which touches shell state (the ready queue, the batch members, the `vcs`
    /// port, the Work phase) the pure crate cannot see.
    pub(crate) async fn run_ready(
        &mut self,
        owner: &str,
        project: &str,
        seq: u64,
        event: ready::ReadyEvent,
    ) -> Result<()> {
        let job = self.must_get(owner, project, seq)?.clone();
        let deps_done = self
            .graphs
            .get(&job.project)
            .is_some_and(|g| g.deps_done(seq));
        let view = ready::ReadyView {
            job: &job,
            deps_done,
            now: Utc::now(),
        };
        let (transitions, effects, step) = ready::decide(&view, event);
        for mut t in transitions {
            self.set_state(&mut t.job, t.to).await?;
        }
        if let ready::ReadyStep::Admitted { enqueue, absorb } = &step {
            self.apply_ready_admission(owner, project, &job, *enqueue, absorb)
                .await?;
        }
        for effect in effects {
            self.interpret(effect).await?;
        }
        match step {
            ready::ReadyStep::Idle | ready::ReadyStep::Admitted { .. } => Ok(()),
            ready::ReadyStep::Revalidate => {
                let event = self.ready_revalidation(owner, project, &job).await?;
                Box::pin(self.run_ready(owner, project, seq, event)).await
            }
            ready::ReadyStep::StartWork { cycle } => {
                self.enter_work(owner, project, seq, cycle, Vec::new(), None, None)
                    .await
            }
        }
    }

    /// The [`ready::ReadyStep::Admitted`] bookkeeping: put an admitted job on
    /// the ready queue (§3.1 — the queue holds only Ready jobs), and let a
    /// released Draft batch absorb its members Frozen→Batched, indexing the
    /// newly-committed union deps first (best-effort, §2.3).
    async fn apply_ready_admission(
        &mut self,
        owner: &str,
        project: &str,
        job: &Job,
        enqueue: bool,
        absorb: &[u64],
    ) -> Result<()> {
        if enqueue {
            self.queue.enqueue(QueuedJob {
                owner: owner.into(),
                project: project.into(),
                seq: job.id,
            });
        }
        if !absorb.is_empty() {
            for &upstream in &job.deps {
                let _ = self.rdeps.append(owner, project, upstream, job.id).await;
            }
            self.absorb_batch(owner, project, job.id, absorb).await?;
        }
        Ok(())
    }

    /// Run the §2.2 Ready-transition pass at the current default-branch HEAD and
    /// hand the verdict back as the decider's next event. Reads only (the `vcs`
    /// port plus the config tree), which is exactly why it lives shell-side:
    /// [`ready::decide`] gates it, this performs it.
    async fn ready_revalidation(
        &self,
        owner: &str,
        project: &str,
        job: &Job,
    ) -> Result<ready::ReadyEvent> {
        let default_branch = self.repos.default_branch(owner, project).await?;
        let head = self
            .repos
            .resolve_ref(owner, project, &default_branch)
            .await?;
        let (errors, declared_inputs) = match release::load_job_type(
            &self.repos,
            owner,
            project,
            &head,
            &job.r#type,
            Some(job.id),
        )
        .await
        .and_then(|jt| release::with_job_evaluators(jt, job))
        {
            Ok(jt) => {
                let errors =
                    release::static_errors(&self.repos, owner, project, &head, job, &jt, None)
                        .await
                        .unwrap_or_else(|errs| errs);
                (errors, jt.inputs)
            }
            Err(errs) => (errs, Vec::new()),
        };
        Ok(ready::ReadyEvent::Revalidated {
            head,
            errors,
            declared_inputs,
        })
    }

    /// Ready→Work entry (§3.2 steps 1–6): the queue handed this job a launch
    /// slot, so the decider's `Dequeued` decision says whether it may still
    /// take it — a job revoked or parked while it waited forfeits it silently.
    pub(crate) async fn start_job(&mut self, q: QueuedJob) -> Result<()> {
        self.run_ready(&q.owner, &q.project, q.seq, ready::ReadyEvent::Dequeued)
            .await
    }
}

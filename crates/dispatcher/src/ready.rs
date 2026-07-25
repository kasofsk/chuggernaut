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
//!   membership commit, and the Ready→Work hand-off.
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
        // The fan-out is advisory: a dependent that vanished (revoked, unknown
        // project) is not an error, it is nothing to decide.
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
        // 1. Reads feed the view — they are not effects.
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
        // 2. The decision, made purely.
        let (transitions, effects, step) = ready::decide(&view, event);
        // 3. Commit the decision: transitions first (§2.1 record is the source
        // of truth; the announcements are its artifacts, and restart
        // reconciliation re-drives `try_unblock` for anything a crash lost).
        for mut t in transitions {
            self.set_state(&mut t.job, t.to).await?;
        }
        // Queue admission and a batch's membership commit are part of
        // committing the decision, so they run BEFORE the effects (the same
        // placement C3's `drops_exec` established, and the pre-C4 write order).
        if let ready::ReadyStep::Admitted { enqueue, absorb } = &step {
            self.apply_ready_admission(owner, project, &job, *enqueue, absorb)
                .await?;
        }
        // 4. The artifacts of the decision.
        for effect in effects {
            self.interpret(effect).await?;
        }
        // 5. The bookkeeping the step names.
        match step {
            ready::ReadyStep::Idle | ready::ReadyStep::Admitted { .. } => Ok(()),
            // The continuation hop (contracts.md §2): the §2.2 pass is
            // ref-reading I/O, so its verdict re-enters the decider as the next
            // event against a freshly gathered view. Boxed because that
            // re-entry is a self-recursive future.
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
        let errors = match release::load_job_type(
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
            // KV names are re-checked at launch, not here (§2.2): a secret set
            // after release must not strand a job that is otherwise ready.
            Ok(jt) => release::static_errors(&self.repos, owner, project, &head, job, &jt, None)
                .await
                .unwrap_or_else(|errs| errs),
            Err(errs) => errs,
        };
        Ok(ready::ReadyEvent::Revalidated { head, errors })
    }

    /// Ready→Work entry (§3.2 steps 1–6): the queue handed this job a launch
    /// slot, so the decider's `Dequeued` decision says whether it may still
    /// take it — a job revoked or parked while it waited forfeits it silently.
    pub(crate) async fn start_job(&mut self, q: QueuedJob) -> Result<()> {
        self.run_ready(&q.owner, &q.project, q.seq, ready::ReadyEvent::Dequeued)
            .await
    }
}

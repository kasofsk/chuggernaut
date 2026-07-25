//! The §2.1 transition table. No transition exists outside this function —
//! every state write in `core` goes through [`assert_transition`] first.
//!
//! - **Accepts:** a current `JobState` and the event driving the change.
//! - **Emits:** the permitted next state, or a rejected assertion for an
//!   illegal edge.
//! - **Guarantees:** pure and synchronous (no `.await`); terminal states are
//!   absorbing; the sole authority on legal transitions.
//! - **Spec:** §2.1.

use thiserror::Error;
use types::JobState;

#[derive(Debug, Clone, PartialEq, Error)]
#[error("invalid transition {from:?} → {to:?}")]
pub struct InvalidTransition {
    pub from: JobState,
    pub to: JobState,
}

pub fn assert_transition(from: JobState, to: JobState) -> Result<(), InvalidTransition> {
    use JobState::*;
    let valid = match (from, to) {
        // Draft→Ready|Blocked: release finalizes the edited definition in one
        // step (same as a Frozen release). Draft→Frozen: finalize parks the
        // edited definition Frozen (re-batchable) instead of scheduling it
        // (#166). Frozen→Draft: a never-released job moves back to Draft for
        // editing (§2.1). Draft→Revoked is covered by the generic Revoked row
        // below (Draft is non-terminal).
        (Draft, Ready | Blocked | Frozen) => true,
        (Frozen, Draft) => true,
        (Frozen, Ready | Blocked) => true,
        // Batches (spec §2.1). Frozen→Batched: a member is absorbed at batch
        // creation. Batched→Frozen: the batch was revoked/failed, so the member
        // is returned (re-batchable). Batched→Done: the batch merged, fanning
        // completion out to each member. Batched→Revoked via the generic row.
        (Frozen, Batched) => true,
        (Batched, Frozen | Done) => true,
        // Blocked→Stalled: Ready-transition re-validation failed (pre-work).
        (Blocked, Ready | Stalled) => true,
        // Ready→Stalled: job_deadline elapsed before work started (pre-work).
        (Ready, Work | Stalled) => true,
        // Work→Work: retry within cycle. Work→Evaluation: work succeeded.
        (Work, Work | Evaluation | Escalated) => true,
        // Evaluation→Evaluation: eval retry only (gate fan-out lives in WrapUp).
        // Evaluation→Work: product-failure rework. Evaluation→WrapUp: eval passed,
        // wrap_up: merge. Evaluation→Done: eval passed, wrap_up: none.
        (Evaluation, Evaluation | Work | WrapUp | Done | Escalated) => true,
        // WrapUp→WrapUp: merge-gate fan-out. WrapUp→Work: squash conflict or
        // gate failure (free rework). WrapUp→Done: clean squash / no-op.
        // WrapUp→Escalated: unexpected hard wrap-up failure (git plumbing).
        (WrapUp, WrapUp | Work | Done | Escalated) => true,
        // Post-work escalation resolution. Retry resumes at the phase that
        // failed (#141): Work exhaustion→Work, eval exhaustion→Evaluation,
        // wrap-up failure→WrapUp. Resolve→Evaluation (operator did the work).
        (Escalated, Work | Evaluation | WrapUp) => true,
        // Pre-work (Stalled) escalation: Retry re-runs the failed step →Ready
        // (or self-loops if re-validation still fails). Resolve is not in the
        // table, so it is structurally impossible (§1.2).
        (Stalled, Ready | Stalled) => true,
        // Revoked is reachable from any non-terminal state.
        (Done | Revoked, Revoked) => false,
        (_, Revoked) => true,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(InvalidTransition { from, to })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use types::JobState::*;

    #[test]
    fn table_edges() {
        for (from, to) in [
            (Draft, Ready),
            (Draft, Blocked),
            (Draft, Frozen),
            (Draft, Revoked),
            (Frozen, Draft),
            (Frozen, Ready),
            (Frozen, Blocked),
            (Frozen, Batched),
            (Batched, Frozen),
            (Batched, Done),
            (Batched, Revoked),
            (Blocked, Ready),
            (Blocked, Stalled),
            (Ready, Work),
            (Ready, Stalled),
            (Work, Evaluation),
            (Evaluation, Work),
            (Evaluation, WrapUp),
            (Evaluation, Done),
            (WrapUp, WrapUp),
            (WrapUp, Work),
            (WrapUp, Done),
            (WrapUp, Escalated),
            (Escalated, Work),
            (Escalated, Evaluation),
            (Escalated, WrapUp),
            (Stalled, Ready),
            (Stalled, Stalled),
            (Frozen, Revoked),
            (Escalated, Revoked),
            (Stalled, Revoked),
            (WrapUp, Revoked),
        ] {
            assert!(assert_transition(from, to).is_ok(), "{from:?}→{to:?}");
        }
        for (from, to) in [
            // Draft leaves via release (Ready/Blocked), finalize (Frozen), or
            // revoke — never straight into execution. Frozen is a one-way door
            // out of it (release, not un-draft, is the only forward path once
            // finalized).
            (Draft, Work),
            (Draft, Evaluation),
            (Frozen, Work),
            (Frozen, Done),
            // Batched is invisible to scheduling: it never jumps into execution
            // or evaluation, only to Done (merge), Frozen (revoke), or Revoked.
            (Batched, Work),
            (Batched, Ready),
            (Batched, Blocked),
            (Blocked, Work),
            // Pre-work escalations use Stalled, never Escalated.
            (Blocked, Escalated),
            (Ready, Escalated),
            (Ready, Evaluation),
            (Ready, Done),
            (Work, Done),
            (Work, WrapUp),
            (Done, Work),
            (Done, Revoked),
            (Revoked, Revoked),
            (Escalated, Done),
            // Post-work escalation resolves to Work/Evaluation, not Ready.
            (Escalated, Ready),
            // Stalled (pre-work) may not jump into execution or evaluation.
            (Stalled, Work),
            (Stalled, Evaluation),
            (Stalled, Escalated),
            // WrapUp only follows an eval pass; nothing re-enters Ready/Evaluation.
            (WrapUp, Ready),
            (WrapUp, Evaluation),
        ] {
            assert!(assert_transition(from, to).is_err(), "{from:?}→{to:?}");
        }
    }
}

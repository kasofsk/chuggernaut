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
        (Draft, Ready | Blocked | Frozen) => true,
        (Frozen, Draft) => true,
        (Frozen, Ready | Blocked) => true,
        (Frozen, Batched) => true,
        (Batched, Frozen | Done) => true,
        (Blocked, Ready | Stalled) => true,
        (Ready, Work | Stalled) => true,
        (Work, Work | Evaluation | Escalated) => true,
        (Evaluation, Evaluation | Work | WrapUp | Done | Escalated) => true,
        (WrapUp, WrapUp | Work | Done | Escalated) => true,
        (Escalated, Work | Evaluation | WrapUp) => true,
        (Stalled, Ready | Stalled) => true,
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
            (Draft, Work),
            (Draft, Evaluation),
            (Frozen, Work),
            (Frozen, Done),
            (Batched, Work),
            (Batched, Ready),
            (Batched, Blocked),
            (Blocked, Work),
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
            (Escalated, Ready),
            (Stalled, Work),
            (Stalled, Evaluation),
            (Stalled, Escalated),
            (WrapUp, Ready),
            (WrapUp, Evaluation),
        ] {
            assert!(assert_transition(from, to).is_err(), "{from:?}→{to:?}");
        }
    }
}

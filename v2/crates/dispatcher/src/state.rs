//! The §2.1 transition table. No transition exists outside this function —
//! every state write in `core` goes through [`assert_transition`] first.

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
        (Frozen, Ready | Blocked) => true,
        (Blocked, Ready | Escalated) => true,
        (Ready, Work | Escalated) => true,
        // Work→Work: retry within cycle. Evaluation→Evaluation: eval retry or
        // merge-gate fan-out. Evaluation→Work: rework/conflict/gate failure.
        (Work, Work | Evaluation | Escalated) => true,
        (Evaluation, Evaluation | Work | Done | Escalated) => true,
        // Escalated→Ready: pre-work escalation Retry passing re-validation
        // (§1.2 pre-work escalations; §2.1).
        (Escalated, Work | Evaluation | Ready) => true,
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
    use super::*;
    use types::JobState::*;

    #[test]
    fn table_edges() {
        for (from, to) in [
            (Frozen, Ready),
            (Frozen, Blocked),
            (Blocked, Ready),
            (Blocked, Escalated),
            (Ready, Work),
            (Work, Evaluation),
            (Evaluation, Work),
            (Evaluation, Done),
            (Escalated, Evaluation),
            (Frozen, Revoked),
            (Escalated, Revoked),
        ] {
            assert!(assert_transition(from, to).is_ok(), "{from:?}→{to:?}");
        }
        for (from, to) in [
            (Frozen, Work),
            (Frozen, Done),
            (Blocked, Work),
            (Ready, Evaluation),
            (Ready, Done),
            (Work, Done),
            (Done, Work),
            (Done, Revoked),
            (Revoked, Revoked),
            (Escalated, Done),
        ] {
            assert!(assert_transition(from, to).is_err(), "{from:?}→{to:?}");
        }
    }
}

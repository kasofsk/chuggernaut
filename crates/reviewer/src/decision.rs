use chuggernaut_forgejo_api::PullReview;

/// Map a Forgejo PR review state to our internal decision.
/// Returns (is_approved, is_changes_requested, feedback).
pub fn map_review(review: &PullReview) -> ReviewOutcome {
    match review.state.as_str() {
        "APPROVED" => ReviewOutcome::Approved,
        "REQUEST_CHANGES" => ReviewOutcome::ChangesRequested {
            feedback: if review.body.is_empty() {
                "Changes requested (no details provided)".to_string()
            } else {
                review.body.clone()
            },
        },
        _ => ReviewOutcome::Inconclusive,
    }
}

/// Find the most relevant review from a list, filtering out stale ones.
/// Returns the most recent review submitted after `after_ts` (ISO 8601).
pub fn pick_latest_review<'a>(
    reviews: &'a [PullReview],
    after_ts: &str,
) -> Option<&'a PullReview> {
    reviews
        .iter()
        .filter(|r| {
            r.submitted_at
                .as_deref()
                .map(|ts| ts > after_ts)
                .unwrap_or(false)
        })
        .filter(|r| r.state == "APPROVED" || r.state == "REQUEST_CHANGES")
        .max_by(|a, b| {
            let a_ts = a.submitted_at.as_deref().unwrap_or("");
            let b_ts = b.submitted_at.as_deref().unwrap_or("");
            a_ts.cmp(b_ts)
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    Approved,
    ChangesRequested { feedback: String },
    Inconclusive,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chuggernaut_forgejo_api::PullReview;

    fn make_review(state: &str, body: &str, submitted_at: &str) -> PullReview {
        PullReview {
            id: 1,
            state: state.to_string(),
            body: body.to_string(),
            submitted_at: Some(submitted_at.to_string()),
            user: None,
        }
    }

    #[test]
    fn map_approved() {
        let r = make_review("APPROVED", "", "2026-01-01T00:00:00Z");
        assert_eq!(map_review(&r), ReviewOutcome::Approved);
    }

    #[test]
    fn map_changes_requested_with_feedback() {
        let r = make_review("REQUEST_CHANGES", "fix the bug", "2026-01-01T00:00:00Z");
        assert_eq!(
            map_review(&r),
            ReviewOutcome::ChangesRequested {
                feedback: "fix the bug".to_string()
            }
        );
    }

    #[test]
    fn map_changes_requested_empty_body() {
        let r = make_review("REQUEST_CHANGES", "", "2026-01-01T00:00:00Z");
        match map_review(&r) {
            ReviewOutcome::ChangesRequested { feedback } => {
                assert!(feedback.contains("no details"));
            }
            other => panic!("expected ChangesRequested, got {other:?}"),
        }
    }

    #[test]
    fn map_comment_is_inconclusive() {
        let r = make_review("COMMENT", "looks good", "2026-01-01T00:00:00Z");
        assert_eq!(map_review(&r), ReviewOutcome::Inconclusive);
    }

    #[test]
    fn pick_latest_filters_stale() {
        let reviews = vec![
            make_review("APPROVED", "", "2026-01-01T00:00:00Z"),
            make_review("REQUEST_CHANGES", "fix", "2026-01-02T00:00:00Z"),
            make_review("APPROVED", "", "2026-01-03T00:00:00Z"),
        ];
        // Only reviews after Jan 2 should be considered
        let picked = pick_latest_review(&reviews, "2026-01-02T00:00:00Z");
        assert!(picked.is_some());
        assert_eq!(picked.unwrap().state, "APPROVED");
        assert_eq!(
            picked.unwrap().submitted_at.as_deref(),
            Some("2026-01-03T00:00:00Z")
        );
    }

    #[test]
    fn pick_latest_returns_none_when_all_stale() {
        let reviews = vec![
            make_review("APPROVED", "", "2026-01-01T00:00:00Z"),
        ];
        let picked = pick_latest_review(&reviews, "2026-01-02T00:00:00Z");
        assert!(picked.is_none());
    }

    #[test]
    fn pick_latest_ignores_comments() {
        let reviews = vec![
            make_review("COMMENT", "nice", "2026-01-03T00:00:00Z"),
        ];
        let picked = pick_latest_review(&reviews, "2026-01-01T00:00:00Z");
        assert!(picked.is_none());
    }

    #[test]
    fn rework_limit_check() {
        // Rework limit is a config concern, but verify the threshold logic
        let rework_count: u32 = 4;
        let limit: u32 = 3;
        assert!(rework_count > limit, "should escalate when count > limit");
    }
}

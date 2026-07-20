# Produce a code review

Review this repository as it stands and produce a concise, prioritized
review. Focus on: correctness risks, unclear or misleading code, missing
tests, and anything that would bite the next contributor. If the operator
committed a `REVIEW_SCOPE.md` at the repo root, limit yourself to that scope.

The review itself is your work product — do not change any code.

1. Narrate with `update_status` as you work through the codebase — one
   update per area you inspect; these stream live to the operator.
2. When done, call `submit_result` with:
   - `summary`: a one-paragraph overall verdict,
   - `structured`: `{ "findings": [ { "file": "...", "severity":
     "high|medium|low", "issue": "...", "suggestion": "..." } ] }`.
3. Exit 0.

This job merges nothing — your submitted result and this session's transcript
are the deliverable.

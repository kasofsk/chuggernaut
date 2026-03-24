# Review

Review is handled by the worker binary in review mode (`--mode review`), dispatched as a Forgejo Action. There is no standalone reviewer process — the dispatcher dispatches review actions directly when a job enters InReview.

---

## Overview

```
Worker yields PR → dispatcher transitions to InReview
  │
  ▼
┌──────────────────────────────────────────┐
│ Dispatcher: dispatch_review_action       │
│  1. Dispatch review.yml Forgejo Action   │
│     inputs: job_key, nats_url, pr_url,   │
│             review_level                 │
└──────────────┬───────────────────────────┘
               │
               ▼
┌──────────────────────────────────────────┐
│ Review Action (inside container)         │
│                                          │
│  1. Clone repo, checkout PR branch       │
│  2. Get diff (origin/main...HEAD)        │
│  3. Run Claude with review prompt        │
│  4. Parse decision from Claude output    │
│  5. Post PR review to Forgejo            │
│  6. If approved: attempt merge (retries) │
│  7. Publish ReviewDecision to NATS       │
│  8. Exit                                 │
└──────────────────────────────────────────┘
```

---

## Review Lifecycle

### 1. Trigger

When a worker yields (Yield outcome with pr_url), the dispatcher:

1. Transitions the job to InReview
2. Reads the job's `review` level
3. Dispatches `review.yml` Forgejo Actions workflow with inputs:
   - `job_key` — job identifier
   - `nats_url` — NATS server address
   - `pr_url` — the PR to review
   - `review_level` — low/medium/high

### 2. Review Action

The review action runs `chuggernaut-worker --mode review`. The worker:

1. Clones the repo and checks out the PR's head branch
2. Gets the diff between `origin/main` and `HEAD`
3. Runs Claude as a subprocess with the diff as context
4. Parses Claude's stdout for a JSON decision:
   ```json
   {"decision": "approved", "feedback": "Looks good"}
   ```
   or:
   ```json
   {"decision": "changes_requested", "feedback": "Fix the tests"}
   ```
5. Posts a PR review to Forgejo (APPROVED or REQUEST_CHANGES)
6. If approved: attempts to merge the PR
7. Publishes `ReviewDecision` to NATS with optional `token_usage`

### 3. Decision Handling

The dispatcher receives the `ReviewDecision` and acts:

#### Approved

1. Transition job to Done
2. Walk reverse deps — unblock dependents
3. Try to dispatch next queued job

The review action already merged the PR before publishing the Approved decision.

#### Changes Requested

1. Transition job to ChangesRequested
2. Dispatch a new work action with `review_feedback` and `is_rework=true`
3. The worker checks out the existing branch and addresses the feedback

#### Escalated

1. Transition job to Escalated
2. The job stays Escalated until manually resolved (admin requeue/close)

Escalation happens when:
- The review subprocess fails or cannot parse a decision
- Claude returns `{"decision": "escalate"}`
- Review level is `human` (dispatcher would skip automated review — not yet implemented)

---

## Merge Strategy

The review action attempts to merge the PR directly after approval:

1. Call Forgejo merge API (merge strategy)
2. If it fails (conflict, lock, etc.): retry up to 3 times with 5s/10s/15s delays
3. If all retries fail: publish `ChangesRequested` with merge conflict feedback instead of `Approved`

This simplifies the old merge queue: concurrent merges to the same repo may occasionally conflict, but the retry + rework cycle handles it naturally. At the scale where multiple PRs for the same repo are approved within seconds of each other, this is rare.

---

## Done Detection

The review action is the mechanism for transitioning jobs to Done:

1. Review action merges PR
2. Publishes `ReviewDecision{Approved, job_key, pr_url}`
3. Dispatcher transitions to Done
4. Dispatcher walks reverse deps, unblocks dependents

**No CDC needed.** The review action already knows it merged the PR — it reports success directly.

---

## Token Usage

Both work and review actions report `TokenUsage` in their outcomes:

```json
{
  "input_tokens": 15000,
  "output_tokens": 3500,
  "cache_read_tokens": 8000,
  "cache_write_tokens": 2000
}
```

The dispatcher stores each action's usage as an `ActionTokenRecord` on the Job:

```json
{
  "action_type": "review",
  "token_usage": { "input_tokens": 15000, "output_tokens": 3500, ... },
  "completed_at": "2026-03-24T10:05:00Z"
}
```

This allows the UI to show per-action token usage broken down by work vs review, including across rework cycles.

---

## Configuration

Review-specific dispatcher config:

| Env Var | Default | Notes |
|---------|---------|-------|
| `CHUGGERNAUT_REVIEW_WORKFLOW` | `review.yml` | Review action workflow file |
| `CHUGGERNAUT_REVIEW_RUNNER_LABEL` | `ubuntu-latest` | Runner label for review actions |
| `CHUGGERNAUT_REWORK_LIMIT` | `3` | Max rework cycles before escalation |
| `CHUGGERNAUT_HUMAN_LOGIN` | `you` | Human reviewer login for escalation |

Worker review-mode args (passed as workflow inputs):

| Env Var / Arg | Notes |
|---------------|-------|
| `CHUGGERNAUT_MODE=review` | Switches worker to review mode |
| `CHUGGERNAUT_PR_URL` | PR URL to review |
| `CHUGGERNAUT_REVIEW_LEVEL` | low/medium/high |

---

## Workflow Definition (`action/review.yml`)

```yaml
name: chuggernaut-review
on:
  workflow_dispatch:
    inputs:
      job_key: { required: true, type: string }
      nats_url: { required: true, type: string }
      pr_url: { required: true, type: string }
      review_level: { required: false, type: string, default: "high" }
jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: chuggernaut-worker --mode review
               --job-key "${{ inputs.job_key }}"
               --nats-url "${{ inputs.nats_url }}"
               --forgejo-url "${{ github.server_url }}"
               --pr-url "${{ inputs.pr_url }}"
               --review-level "${{ inputs.review_level }}"
        env:
          CHUGGERNAUT_FORGEJO_TOKEN: ${{ secrets.CHUGGERNAUT_FORGEJO_TOKEN }}
          CHUGGERNAUT_COMMAND: claude
```

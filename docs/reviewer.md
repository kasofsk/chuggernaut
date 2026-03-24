# Reviewer

The reviewer is a standalone **singleton** process that automates PR review, merge, and rework routing. It subscribes to job transitions and reacts when a job enters InReview. Exactly one instance must run — multiple instances would race on the in-memory dedup set and merge queue.

**Dependency:** The reviewer reads job data from the dispatcher's HTTP API. If the dispatcher is down, the reviewer cannot process new reviews (it retries until the API is available).

---

## Overview

```
CHUGGERNAUT-TRANSITIONS stream
  │ filter: to_state == InReview
  ▼
┌─────────────────────────────────────────┐
│ Reviewer                                 │
│                                          │
│  1. Read job from dispatcher API         │
│  2. Read pr_url from job metadata        │
│  3. Dispatch review action (Forgejo CI)  │
│  4. Poll until action completes          │
│  5. Read PR review state from Forgejo    │
│  6. Act on decision:                     │
│     - approve → merge PR via queue       │
│     - changes_requested → report back    │
│     - escalate → add human reviewer,     │
│       poll until human acts              │
│  7. Publish ReviewDecision to NATS       │
└─────────────────────────────────────────┘
```

---

## Review Lifecycle

### 1. Trigger

The reviewer subscribes to `CHUGGERNAUT-TRANSITIONS` with a durable pull consumer that filters for `to_state == "in-review"`. On each transition:

1. Deduplicate: check an in-memory `DashSet<String>` (job_key) to prevent concurrent handling of the same job. The entry is removed after `ReviewDecision` is published, allowing the same job to be reviewed again in future rounds.
2. Delay: wait `CHUGGERNAUT_REVIEWER_DELAY_SECS` (default 3) to avoid race with PR creation
3. Fetch job from dispatcher API: `GET /jobs/{key}`
4. Read `pr_url` from job metadata

### 2. Review Requirement

The review requirement is stored in the job's `review` field:

| Value | Behavior |
|-------|----------|
| `"human"` | Skip automated review, immediately escalate to human reviewer |
| `"low"` | Low confidence threshold — Claude reviewer is lenient |
| `"medium"` | Medium confidence threshold |
| `"high"` (default) | High confidence threshold — Claude reviewer is strict |

### 3. Dispatch Review Action

If not `review: human`:

1. Find the PR's head branch from `pr_url`
2. Dispatch the `review-work.yml` Forgejo Actions workflow on that branch
3. Pass inputs: `pr_url`, `review_level`, `job_key`
4. Poll the action run status every `CHUGGERNAUT_REVIEWER_POLL_SECS` (default 10)
5. Timeout after `CHUGGERNAUT_REVIEWER_ACTION_TIMEOUT_SECS` (default 600)
6. If the action times out or errors: escalate to human (do not retry the action)

The review action (a Forgejo Actions workflow) runs Claude to evaluate the PR and submits a PR review with state APPROVED, REQUEST_CHANGES, or COMMENT.

### 4. Read Decision

After the action completes:

1. Fetch PR reviews from Forgejo API
2. Find the most recent review submitted **after the action run started** (filters out stale reviews from previous rounds on the same PR)
3. Map review state to decision:
   - `APPROVED` → approve
   - `REQUEST_CHANGES` → changes_requested (extract feedback from review body)
   - `COMMENT` → treat as informational, escalate to human

### 5. Act on Decision

#### Approved → Merge

1. Acquire per-repo merge lock from `chuggernaut.merge-queue` KV (CAS)
2. If lock held by another: queue this PR in `{owner}.{repo}.queue`
3. If lock acquired:
   a. Merge PR via Forgejo API (rebase strategy)
   b. On success: publish `ReviewDecision{Approved}`
   c. On failure (merge conflict): publish `ReviewDecision{ChangesRequested, feedback: "Merge conflict..."}`
   d. Release lock
   e. Process next entry from queue (if any)

#### Changes Requested → Report Back

1. Increment `chuggernaut.rework-counts` KV for this job
2. If rework_count > 3: escalate to human instead
3. Otherwise: publish `ReviewDecision{ChangesRequested, feedback: "..."}`

The dispatcher receives the review decision and:
- Transitions job to ChangesRequested
- Routes to the original worker with `is_rework: true` + feedback

#### Escalate → Add Human Reviewer

1. Add human reviewer to the PR via Forgejo API
2. Publish `ReviewDecision{Escalated, reviewer_login: "..."}`
3. Dispatcher transitions job to Escalated

The reviewer then polls the Forgejo PR API every `CHUGGERNAUT_REVIEWER_ESCALATION_POLL_SECS` (default 30) to detect the human's action:

- **Human approves:** Reviewer detects the approval via PR review state, merges the PR through the merge queue, and publishes `ReviewDecision{Approved}`. Dispatcher transitions to Done.
- **Human requests changes:** Reviewer detects `REQUEST_CHANGES` review state and publishes `ReviewDecision{ChangesRequested, feedback}`. Dispatcher transitions to ChangesRequested and routes rework to the original worker.

The human approves or requests changes in Forgejo's PR interface — they do not merge directly. The reviewer handles the merge to maintain merge queue serialization.

---

## Merge Queue

Prevents concurrent merges in the same repo that could cause rebase conflicts.

### Lock Protocol

```
Key: chuggernaut.merge-queue / {owner}.{repo}.lock
Value: { "holder": "job_key", "acquired_at": "..." }
TTL: 5 minutes (refreshed during long merges)

1. CAS create (revision 0) → lock acquired
2. On success: proceed with merge
3. On failure (key exists): queue in {owner}.{repo}.queue
4. After merge: delete lock, check queue, process next
```

The 5-minute TTL acts as a safety net: if the reviewer crashes while holding a lock, the lock expires and the queue unblocks. On startup, the reviewer also clears any locks it previously held (see Startup Reconciliation).

### Queue

```
Key: chuggernaut.merge-queue / {owner}.{repo}.queue
Value: [
  { "job_key": "acme.payments.57", "pr_url": "...", "queued_at": "..." },
  { "job_key": "acme.payments.58", "pr_url": "...", "queued_at": "..." }
]
```

Queue is processed FIFO (by `queued_at`). After each merge, the lock holder:
1. Reads the queue entry
2. Pops the first entry
3. Writes the updated queue back (CAS)
4. Merges the next PR (or releases lock if queue empty)

This serialization is important at scale: with many workers finishing concurrently, the queue prevents a storm of competing rebase attempts.

### Merge Strategy

Uses Forgejo's `rebase` merge method. Forgejo rebases the PR branch onto the target before fast-forwarding. This catches conflicts at merge time rather than after.

If the rebase fails (conflict), the reviewer:
1. Publishes `ReviewDecision{ChangesRequested, feedback: "Merge conflict with main. Please rebase and resolve."}`
2. The worker gets a rework assignment and resolves the conflict

---

## Done Detection

The reviewer is the primary mechanism for transitioning jobs to Done:

1. Reviewer merges PR
2. Publishes `ReviewDecision{Approved, job_key, pr_url}`
3. Dispatcher receives decision, transitions to Done
4. Dispatcher walks reverse deps, unblocks dependents

**No CDC needed.** The reviewer already knows it merged the PR — it reports success directly. No need to poll PostgreSQL or watch for issue closure.

**Merge invariant:** All merges go through the reviewer and merge queue. Humans review and approve PRs in Forgejo but do not merge directly. Forgejo branch protection should be configured to prevent direct merges (require review approval, restrict merge to the `chuggernaut-reviewer` account).

---

## Activity and Journal

The reviewer records its actions via fire-and-forget NATS messages:

```
chuggernaut.activity.append → { job_key, entry: { kind: "review_started", ... } }
chuggernaut.activity.append → { job_key, entry: { kind: "review_action_dispatched", ... } }
chuggernaut.activity.append → { job_key, entry: { kind: "review_approved", ... } }
chuggernaut.journal.append  → { action: "merged", job_key, pr_url, ... }
```

---

## Startup Reconciliation

On startup, the reviewer:

1. **Clear stale merge locks:** scan `chuggernaut.merge-queue` for lock keys, delete any that exist (the reviewer is restarting, so any lock it held is stale)
2. **Resume InReview jobs:** query dispatcher API for all jobs in InReview state (`GET /jobs?state=in-review`), trigger the review flow for any not already in the dedup set
3. **Resume Escalated jobs:** query dispatcher API for all jobs in Escalated state (`GET /jobs?state=escalated`), resume polling Forgejo for human decisions

This handles the case where the reviewer restarts while reviews or escalations are pending.

### Crash Recovery Edge Cases

**Duplicate review actions:** If the reviewer crashes mid-review, the action may still be running. On restart, before dispatching a new review action, the reviewer checks Forgejo for in-progress action runs on the PR branch. If one exists, it waits for that run instead of dispatching a duplicate.

**Already-merged PR:** If the reviewer crashes after merging a PR but before acking the transition, the durable consumer replays the event. On re-processing, the reviewer detects the PR is already merged (via Forgejo API) and publishes `ReviewDecision{Approved}` without attempting to merge again.

---

## Forgejo API Resilience

All Forgejo API calls (merge, review submission, adding reviewers, fetching PR state) retry with exponential backoff: 3 attempts at 2s/4s/8s intervals. If all retries fail, the reviewer escalates the job to a human reviewer rather than retrying indefinitely.

---

## Configuration

| Env Var | Default | Notes |
|---------|---------|-------|
| `CHUGGERNAUT_NATS_URL` | `nats://localhost:4222` | NATS connection |
| `CHUGGERNAUT_FORGEJO_URL` | (required) | Forgejo base URL |
| `CHUGGERNAUT_REVIEWER_FORGEJO_TOKEN` | (required) | Reviewer identity token |
| `CHUGGERNAUT_REVIEWER_HUMAN_LOGIN` | `you` | Human reviewer for escalation |
| `CHUGGERNAUT_REVIEWER_DELAY_SECS` | `3` | Delay before reviewing |
| `CHUGGERNAUT_REVIEWER_WORKFLOW` | `review-work.yml` | Review action workflow file |
| `CHUGGERNAUT_REVIEWER_RUNNER` | `ubuntu-latest` | Runner label for review action |
| `CHUGGERNAUT_REVIEWER_ACTION_TIMEOUT_SECS` | `600` | Review action timeout |
| `CHUGGERNAUT_REVIEWER_POLL_SECS` | `10` | Poll interval for action status |
| `CHUGGERNAUT_REVIEWER_ESCALATION_POLL_SECS` | `30` | Poll interval for human decisions on escalated PRs |
| `CHUGGERNAUT_DISPATCHER_URL` | `http://localhost:8080` | Dispatcher HTTP API |

---

## Forgejo Identity

The reviewer uses a dedicated Forgejo account (`chuggernaut-reviewer`) for all review operations:
- Submitting PR reviews (both directly and via the review action — the action receives the reviewer's token)
- Merging PRs
- Adding human reviewers
- Posting escalation comments

This provides a clear audit trail: all automated review activity is attributable to `chuggernaut-reviewer`.

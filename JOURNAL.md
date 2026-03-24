# chuggernaut Dev Journal

## 2026-03-23 — Phase 1 Implementation Complete

### What was built
- **5 Rust crates** in a workspace (now 8), all compiling on latest deps (async-nats 0.46, petgraph 0.8, reqwest 0.13, axum 0.8)
- **chuggernaut-types**: 45 types with `JsonSchema` derive, NATS subject/bucket/stream constants, helpers. 12 unit tests.
- **chuggernaut-forgejo-api**: Thin reqwest client covering PRs, reviews, merge, actions, repos (~10 endpoints).
- **chuggernaut-dispatcher**: Full coordinator — NATS handlers (14 subscriptions), HTTP API (10 endpoints + SSE + `/schema`), monitor (5 scans), petgraph DAG, CAS claim lifecycle, worker assignment with preemption, restart recovery (10-step). 2 unit tests.
- **chuggernaut-worker**: SDK with lifecycle loop, git helpers (clone/branch/commit/push), SimWorker.
- **chuggernaut-cli**: All-in-one `chuggernaut` binary — create, jobs, show, requeue, close, interact respond, sim, seed.
- **Infra**: Docker Compose (NATS + Forgejo), init script, sample fixture.

### Design consistency check
The implementation covers ~70% of the full design. Verified consistent:
- All 11 job states and state transition table ✓
- All KV buckets (10 dispatcher-owned) and 3 JetStream streams ✓
- All NATS subjects and message types ✓
- All dispatcher config env vars ✓
- All worker config env vars (minus recording TTL) ✓
- All HTTP API endpoints from design ✓
- Dependency management with cycle detection and unblock propagation ✓
- Claim lifecycle with CAS, heartbeat, lease expiry ✓
- Worker assignment with capabilities, blacklist, preemption ✓
- Monitor with all 5 scan types ✓
- Restart recovery with all 10 reconciliation steps ✓

### Intentionally deferred to Phase 2
- **Reviewer process** (entire crate) — merge queue, rework counts, review action dispatch, escalation polling
- **InteractiveWorker** — Claude Code in tmux/ttyd, peek/attach/detach
- **ActionWorker** — Forgejo Actions dispatch + polling
- **Graph viewer SPA** — frontend
- **Session recording** — object store upload
- **CLI commands**: attach, detach, action, agent

### Known simplifications in Phase 1
- SSE uses 1s polling instead of KV watchers (noted in code)
- Forgejo API client has all method signatures but only SimWorker exercises create PR and find-by-head
- ~~No integration tests yet (unit tests only)~~ — resolved, see 2026-03-24 entry

---

## 2026-03-24 — Integration Tests & Phase 2 Roadmap

### What changed
- **15 dispatcher integration tests** added using testcontainers with real NATS JetStream
  - Job creation (4): on-deck, on-ice, sequential keys, dot-rejection
  - Dependencies (2): blocked-then-unblocked, diamond partial unblock
  - Worker NATS protocol (5): register, idle→assign, yield→in-review, fail→retry, abandon→blacklist
  - Admin commands (2): close-unblocks-dependents, revoke-keeps-dependents-blocked
  - Heartbeat (1): lease renewal
  - Recovery (1): rebuild state from KV
- Test infra: shared NATS container via `OnceLock`, UUID-prefixed buckets for parallel isolation
- **Total test count: 48** (21 dispatcher unit + 15 dispatcher integration + 12 types unit) — all green

**5 additional integration tests** added covering Phase 2 priority 1:
- Review decisions (2): approved→Done with dep unblock, ChangesRequested transition
- Preemption (1): high-priority job sends PreemptNotice to busy worker
- Monitor (1): lease expiry detected by scan → job Failed (1s lease, real timer)
- Admin (1): requeue from Failed → OnDeck via NATS request-reply
- **Updated total: 53 tests** (21 dispatcher unit + 20 dispatcher integration + 12 types unit) — all green

**Reviewer crate (`chuggernaut-reviewer`)** created with full skeleton:
- `config.rs` — all 13 env vars from spec (delay, workflow, poll intervals, rework limit, merge lock TTL)
- `error.rs` — ReviewerError with Nats/Kv/Forgejo/Dispatcher/Serde variants
- `state.rs` — ReviewerState with DashSet dedup, Forgejo client, KV handles
- `nats_setup.rs` — initializes reviewer-owned KV buckets (merge_queue, rework_counts)
- `dispatcher_client.rs` — HTTP client for `GET /jobs/{key}` and `GET /jobs?state=`
- `decision.rs` — maps Forgejo review states to ReviewOutcome, stale review filtering
- `merge.rs` — merge lock (CAS create), queue (FIFO with CAS updates), PR merge (rebase), URL parsing
- `review.rs` — full pipeline: delay → fetch job → dispatch action → poll → decide → merge/rework/escalate
- `trigger.rs` — CHUGGERNAUT-TRANSITIONS durable pull consumer, InReview filter, dedup, reconciliation
- `main.rs` — entry point with tracing, NATS connect, KV init, reconcile, trigger loop
- 13 unit tests (decision mapping, stale filtering, rework threshold, PR URL parsing)
- Escalation polling loop implemented: polls Forgejo PR for human APPROVED/REQUEST_CHANGES after escalation, handles already-merged and closed PRs
- `ChangesOutcome` enum breaks async recursion between `poll_escalation` ↔ `handle_changes_requested`

**3 reviewer integration tests** with real NATS (testcontainers) + real Forgejo 14 (testcontainers):
- `review_approved_merges_and_completes` — submit APPROVED review → reviewer merges PR → job Done
- `review_changes_requested_routes_rework` — submit REQUEST_CHANGES → job ChangesRequested
- `human_review_level_escalates_then_human_approves` — Human review level → escalate → simulate human approval → merge → Done
- Test infra: shared NATS + Forgejo containers (OnceLock), admin + reviewer users (separate to avoid "approve own PR"), NATS bridge for namespace forwarding, real Forgejo API calls (create branches, files, PRs, reviews, merges)

**Updated total: 69 tests** (21 dispatcher unit + 20 dispatcher integration + 13 reviewer unit + 3 reviewer integration + 12 types unit) — all green

### Remaining coverage gaps vs `docs/testing.md`
Not yet covered:
- Job timeout (monitor scan — similar to lease expiry but `claimed_at + timeout`)
- Orphan detection (claim with unknown worker, claimless on-the-stack)
- Auto-retry with backoff (scan_retry picks up Failed jobs past retry_after)
- Retry exhaustion (retry_count >= max_retries stays Failed)
- Full E2E: create → SimWorker → PR → review action → merge → Done (requires Forgejo Actions runner)

---

## Next: Phase 2 Priorities

### ~~1. Expand dispatcher integration tests~~ ✅ Done
All 5 key tests added: ReviewDecision (approved + changes_requested), preemption, monitor lease expiry, admin requeue.

### ~~2. Reviewer crate skeleton~~ ✅ Done
Created `crates/reviewer/` with 10 modules. Full review pipeline + escalation polling loop implemented.
3 integration tests with real Forgejo 14 (testcontainers): approved→merge, changes_requested→rework, human escalation→poll→merge.

### ~~3. Merge queue + Forgejo review dispatch~~ ✅ Done
- `forgejo_retry` module: 3-attempt exponential backoff (2s/4s/8s) for all Forgejo API calls
- Already-merged PR detection in `handle_approved` (crash recovery)
- Stale merge lock cleanup in reconciliation (checks job terminal state)
- Action dispatch failures propagated as hard errors (not silently escalated)
- Integration test: two approved jobs merge sequentially through the queue
- Upgraded to Forgejo 14, noop `review-work.yml` workflow in test repos
- **Updated total: 73 tests** (21 dispatcher unit + 20 dispatcher integration + 16 reviewer unit + 4 reviewer integration + 12 types unit) — all green

### ~~4. Rework routing + escalation~~ ✅ Done (implemented as part of priorities 2-3)
Rework count tracking (`chuggernaut.rework-counts` KV), escalation threshold (count > rework_limit → human),
escalation polling loop, `ChangesOutcome` enum for recursion safety — all in `review.rs`.

### 5. E2E test harness
- Docker Compose test profile (dispatcher + reviewer + NATS + Forgejo)
- Single-job E2E: create → SimWorker → PR → review → merge → Done
- Dependency chain E2E, worker crash recovery

### ~~6. Typed NATS subject registry~~ ✅ Done
- **`chuggernaut-nats` crate** — shared typed NatsClient extracted from dispatcher, used by all crates
  - `publish_msg(&Subject<T>, &T)` — compile-time type-safe publish (wrong payload = compile error)
  - `request_msg(&RequestSubject<Req, Resp>, &Req)` — typed request-reply
  - `publish_to(&SubjectFn<T>, param, &T)` — typed dynamic subjects
  - `subscribe_subject`, `subscribe_request`, `subscribe_dynamic` — typed subscribe
  - Raw methods preserved for wildcards and NATS reply subjects
- **`subject_registry!` macro** in `chuggernaut-types` — single source of truth for all NATS subjects:
  - Defines typed constants (Subject, RequestSubject, SubjectFn) with payload types
  - Generates `subjects::registry() -> Vec<SubjectSchema>` for schema catalog
  - Impossible to add a subject without specifying its payload type
- **Full migration** — all publish/subscribe/request calls across dispatcher, worker, CLI, reviewer, and channel use typed API. Manual `serde_json::to_vec` + `Bytes::from` boilerplate eliminated.
- Deleted old `crates/dispatcher/src/nats_client.rs`
- **8 crates** in workspace, **8,528 LOC**, **102 tests** — all green

### Remaining Phase 2 work
- **Dispatcher trait abstraction** — assignment/preemption behind traits for swappable strategies (e.g., AI agent vs numerical priority)
- **InteractiveWorker** — Claude Code in tmux/ttyd, peek/attach/detach
- **ActionWorker** — Forgejo Actions dispatch + polling
- **Graph viewer SPA** — frontend
- **Session recording** — object store upload
- **CLI commands**: attach, detach, action, agent
- **Terraform** — Forgejo provisioning for test (`terraform/test/`) and deploy (`terraform/deploy/`)

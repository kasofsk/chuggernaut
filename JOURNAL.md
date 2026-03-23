# forge2 Dev Journal

## 2026-03-23 — Phase 1 Implementation Complete

### What was built
- **5 Rust crates** in a workspace, all compiling on latest deps (async-nats 0.46, petgraph 0.8, reqwest 0.13, axum 0.8)
- **forge2-types**: 45 types with `JsonSchema` derive, NATS subject/bucket/stream constants, helpers. 12 unit tests.
- **forge2-forgejo-api**: Thin reqwest client covering PRs, reviews, merge, actions, repos (~10 endpoints).
- **forge2-dispatcher**: Full coordinator — NATS handlers (14 subscriptions), HTTP API (10 endpoints + SSE + `/schema`), monitor (5 scans), petgraph DAG, CAS claim lifecycle, worker assignment with preemption, restart recovery (10-step). 2 unit tests.
- **forge2-worker**: SDK with lifecycle loop, git helpers (clone/branch/commit/push), SimWorker.
- **forge2-cli**: All-in-one `forge2` binary — create, jobs, show, requeue, close, interact respond, sim, seed.
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
- No integration tests yet (unit tests only)

---

## Next: Phase 2 Planning

### Priority order
1. **Integration tests** — testcontainers with real NATS, Forgejo API stub
2. **Reviewer crate** — complete review lifecycle
3. **Graph viewer SPA** — D3 or similar
4. **InteractiveWorker** — Claude Code integration

# MODULES — the module registry

The registry of **scoping-eligible modules**: the units a Chuggernaut job can
be scoped to (NORTH-STAR §4). Each module carries a contract-style `//!` doc
header (accepts / emits / guarantees / spec §); the rows here are the one-line
version of that contract, and this file is what jobs get scoped against.

Keep it in sync with the tree: every top-level dispatcher module
(`crates/dispatcher/src/*.rs`) has a row, and CI is meant to fail when a new
one lacks one (refactor-plan A3) — the mechanism that keeps this from drifting
the way `crates.md`'s dispatcher map did. Companion docs: `crates.md` (crate
map + rationale), `NORTH-STAR.md` (target factoring), `STYLE.md` (the rules).

**Web feature modules** (`web/src/features/*`, once the data layer and feature
folders land per NORTH-STAR priorities #1–2) join this registry as they are
created; today it covers the dispatcher.

## `dispatcher` — `crates/dispatcher/src/`

| Module | Contract | Spec |
| --- | --- | --- |
| `core` | Single-writer event loop: owns all mutable state; every other slice is `impl Core` reached via the `Msg` channel. | §3.1 |
| `state` | The transition table — the sole authority on legal state edges; pure, synchronous, terminal states absorbing. | §2.1 |
| `invariants` | Executable invariant checker: pure/total read-only `CoreState` view → `Vec<Violation>`; negative-space assertions run after every message in tests. | §1.4, §2.1, §3.1, §3.2, §3.3 |
| `graph` | In-memory per-project DAG (petgraph): rdeps maintenance, dependency queries, revoke cascades; a working copy, KV stays truth. | §1.4, §2.3 |
| `queue` | In-memory FIFO of Ready job IDs; lives in the actor, never persisted, rebuilt on restart. | §3.1 |
| `release` | Release validation: graph wiring rules + static config checks; pure, run at release and at Blocked→Ready re-validation. | §2.2, §2.3 |
| `exec` | Work-execution sequence: Ready→Work, container launch, crash recover-or-reset, rework/conflict re-entry. | §3.2 |
| `eval` | Evaluator fan-out/reduce and post-eval finalization: squash-merge, conflict re-entry, the depth-1 merge gate. | §3.3, §3.2 |
| `escalation` | Escalation task construction; owns the task shape only — performs no transition. | §1.2, §3.4 |
| `launch_queue` | Capacity-aware launch queue: park on `NoCapacity`, drain on slot-freed, escalate past `MAX_QUEUE_WAIT`. | §3.5 |
| `scan` | Task-timeout and one-shot job-deadline scans; run inside the single-writer loop; also drains the launch queue. | §3.5 |
| `reconcile` | Restart reconciliation of jobs left mid-execution; runs in the actor before the message loop. | §3.6 |
| `cd` | Config-snapshot freshness: republish live fleet/deploy-drift state from the scan tick when the bytes change. | CD plan C |
| `fleet` | Live fleet occupancy publishing, rebuilt from live containers (never stale bookkeeping); idle fleet writes nothing. | §3.1, §3.6 |
| `harvest` | Pull artifacts out of an exited container, then reclaim its overlay; runs off the actor thread, writes no state. | §3.2, §3.6 |
| `channel` | Agent → operator channel posts: dispatcher writes `channels` KV and publishes each post to `job-events`. | §4.2 |
| `triage` | Operator-dispatched advisory triage runs; purely advisory — never drives a transition. | §1.2 |
| `origin` | Linked-origin projects: the link flow and the origin-release PR surface; credentials never enter containers. | §5.3 |
| `github` | Minimal GitHub REST client (create/read PRs) behind a trait; PAT resolved per call, never held. | §5.3 |
| `seed` | Platform starter template embedded in the binary — the files a fresh project is seeded with. | §12.2 |
| `run` | Production startup: wire store, repos, Docker fleet, provider into a spawned core; fail fast. | §3.6, §12.4 |
| `handlers` | NATS `req.*` subject handlers: translate a request into a `CoreHandle` call and reply per the §6.5 envelope. | §6.1, §6.5 |
| `config` | Dispatcher configuration; `AGENT_PROVIDER_DEFAULT` required, refuses to start without it. | §12.4 |

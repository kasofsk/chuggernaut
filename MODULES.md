# MODULES — the module registry

The registry of **scoping-eligible modules**: the units a Chuggernaut job can
be scoped to (NORTH-STAR §4). Each module carries a contract-style `//!` doc
header (accepts / emits / guarantees / spec §); the rows here are the one-line
version of that contract, and this file is what jobs get scoped against.

Keep it in sync with the tree: every top-level dispatcher module
(`crates/dispatcher/src/*.rs`) and every domain module
(`crates/domain/src/**/*.rs`) has a row, and CI fails when a new one lacks
one (refactor-plan A3) — the mechanism that keeps this from drifting the way
`crates.md`'s dispatcher map did. Companion docs: `crates.md` (crate map +
rationale), `NORTH-STAR.md` (target factoring), `STYLE.md` (the rules).

**Web feature modules** (`web/src/features/*`, once the data layer and feature
folders land per NORTH-STAR priorities #1–2) join this registry as they are
created; today it covers the dispatcher and the pure domain crate.

## `chuggernaut-domain` — `crates/domain/src/`

The pure core (refactor-plan C1): no `tokio`, `async-nats`, `store`, `vcs`,
or `auth` anywhere in its resolve subtree — machine-checked by
`boundary_guard`. The dispatcher re-exports these modules so call sites keep
one surface.

| Module | Contract | Spec |
| --- | --- | --- |
| `state` | The transition table — the sole authority on legal state edges; pure, synchronous, terminal states absorbing. | §2.1 |
| `graph` | In-memory per-project DAG (petgraph): rdeps maintenance, dependency queries, revoke cascades; a working copy, KV stays truth. | §1.4, §2.3 |
| `queue` | In-memory FIFO of Ready job IDs; lives in the actor, never persisted, rebuilt on restart. | §3.1 |
| `release` | Release validation, pure half: error vocabulary, graph wiring rules, additive-evaluator merge; the ref-reading half stays dispatcher-side. | §2.2, §2.3 |
| `effects` | The effect vocabulary: an `Effect` enum naming each port action as `serde` data, with a variant→port-method table. Plain data, no I/O. | contracts.md §2 |
| `decide` | The decider layer: `Transition` + one pure module per lifecycle phase, each `decide(view, event) -> (Vec<Transition>, Vec<Effect>)`; never performs an effect. | contracts.md §2 |
| `decide/escalation` | The C1 template decider: the escalate/stall family — Human task + WHY stamp + Escalated/Stalled transition + announcement, as values. | §1.2, §3.4 |
| `decide/merge_gate` | The C2 landing decider: depth-1 serialization (`gating: Option` — by type), fast-vs-gate pivot, verdict classification, gate-fix budget, conflict re-entry — a continuation machine whose effect results re-enter as events. | §3.3 |

## `dispatcher` — `crates/dispatcher/src/`

| Module | Contract | Spec |
| --- | --- | --- |
| `core` | Single-writer event loop: owns all mutable state; every other slice is `impl Core` reached via the `Msg` channel. | §3.1 |
| `invariants` | Executable invariant checker: pure/total read-only `CoreState` view → `Vec<Violation>`; negative-space assertions run after every message in tests. | §1.4, §2.1, §3.1, §3.2, §3.3 |
| `release` | Release validation, ref-reading half: `jobs/*.yaml` loading + prompt/KV checks through the `vcs` port; re-exports the pure half. | §2.2, §14 |
| `exec` | Work-execution sequence: Ready→Work, container launch, crash recover-or-reset, rework/conflict re-entry. | §3.2 |
| `eval` | Evaluator fan-out/reduce, plus the merge-gate shim: the landing fold that drives `decide/merge_gate` (gather view → decide → swap state → apply → interpret, outcomes re-entering as events). | §3.3, §3.2 |
| `interpret` | The effect interpreter: `Core::interpret` executes one `Effect` through the port it names; the sole `&mut Core` coupling deciders keep. | contracts.md §2 |
| `trace` | Test-only golden-trace recorder: an inert-in-prod `TraceSink` a test attaches via `Core::attach_trace` to capture every `set_state` transition and `publish`/escalation effect as YAML fixtures (`tests/traces/`, regen `UPDATE_TRACES=1`); pins decisions during Track C. | refactor-plan B3 |
| `launch_queue` | Capacity-aware launch queue: park on `NoCapacity`, drain on slot-freed, escalate past `MAX_QUEUE_WAIT`. | §3.5 |
| `scan` | Task-timeout and one-shot job-deadline scans; run inside the single-writer loop; also drains the launch queue. | §3.5 |
| `reconcile` | Restart reconciliation of jobs left mid-execution, incl. re-deriving a parked job's missing escalation task from its stamped record; runs in the actor before the message loop. | §3.6 |
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

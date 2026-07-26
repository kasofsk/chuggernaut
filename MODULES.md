# MODULES — the module registry

The registry of **scoping-eligible modules**: the units a Chuggernaut job can
be scoped to (NORTH-STAR §4). Each module carries a contract-style `//!` doc
header (accepts / emits / guarantees / spec §); the rows here are the one-line
version of that contract, and this file is what jobs get scoped against.

Keep it in sync with the tree: every dispatcher module
(`crates/dispatcher/src/**/*.rs`) and every domain module
(`crates/domain/src/**/*.rs`) has a row, and CI fails when a new one lacks
one (refactor-plan A3) — the mechanism that keeps this from drifting the way
`crates.md`'s dispatcher map did. Both trees nest, so a directory module
registers under its own name (`handlers`, from its `mod.rs`) and each child
under `handlers/<child>`. Companion docs: `crates.md` (crate map +
rationale), `NORTH-STAR.md` (target factoring), `STYLE.md` (the rules).

A **named context** (NORTH-STAR §1) — a directory of modules sharing one
charter — gets its own section here, headed by the row for its `mod.rs`
charter; its members are rows named by their src-relative path
(`platform_ops/fleet`). A job can be scoped to a whole context or to one
module inside it.

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
| `queue` | In-memory FIFO of Ready job IDs, plus the §3.5 launch queue's pure half (drain-priority class, max-wait budget arithmetic); lives in the actor, never persisted, rebuilt on restart. | §3.1, §3.5 |
| `release` | Release validation, pure half: error vocabulary, graph wiring rules, additive-evaluator merge; the ref-reading half stays dispatcher-side. | §2.2, §2.3 |
| `effects` | The effect vocabulary: an `Effect` enum naming each port action as `serde` data, with a variant→port-method table. Plain data, no I/O. | contracts.md §2 |
| `decide` | The decider layer: `Transition` + one pure module per lifecycle phase, each `decide(view, event) -> (Vec<Transition>, Vec<Effect>)`; never performs an effect. | contracts.md §2 |
| `decide/escalation` | The C1 template decider: the escalate/stall family — Human task + WHY stamp + Escalated/Stalled transition + announcement, as values. | §1.2, §3.4 |
| `decide/merge_gate` | The C2 landing decider: depth-1 serialization (`gating: Option` — by type), fast-vs-gate pivot, verdict classification, gate-fix budget, conflict re-entry — a continuation machine whose effect results re-enter as events. | §3.3 |
| `decide/ready` | The C4 Ready-phase decider: dependency satisfaction at release, the `base_ref` pin, queue admission both ends (enqueue on Ready, still-eligible on dequeue), and the Blocked→Ready re-validation fork — a continuation machine whose §2.2 pass re-enters as an event. | §2.1, §2.2, §3.1 |
| `decide/wrapup` | The C3 wrap-up decider: the post-merge fork (`wrap_up.run` publish vs straight to Done), the publish exit verdict, the operator's publish-only Retry, and terminal stamping incl. a batch's Done fan-out. | §3.2, §2.1 |
| `decide/work` | The C6 Work-phase decider: the launch-time validation fork (skew parks pre-work, everything else escalates), one attempt's task record incl. claim parking and claim consumption, the exit verdict with the finish-line guard, and the one retry policy every Work failure spends. | §3.2, §1.2 |
| `decide/eval` | The C5 evaluation decider: the staged fan-out (later stages uncreated when one fails), each evaluator type's verdict incl. the verdict-less/evidence-free class, the `eval_retries` + evidence-free + `rework_budget` budgets, and the reduce's pass / rework / abort / escalate fork. Owns the round as a value. | §3.3, §3.2 |

## `dispatcher` — `crates/dispatcher/src/`

| Module | Contract | Spec |
| --- | --- | --- |
| `core` | Single-writer event loop: owns all mutable state; every other slice is `impl Core` reached via the `Msg` channel. | §3.1 |
| `invariants` | Executable invariant checker: pure/total read-only `CoreState` view → `Vec<Violation>`; negative-space assertions run after every message in tests. | §1.4, §2.1, §3.1, §3.2, §3.3 |
| `release` | Release validation, ref-reading half: `jobs/*.yaml` loading + prompt/KV checks through the `vcs` port; re-exports the pure half. | §2.2, §14 |
| `ready` | Ready-phase shim: gathers the view, applies `decide/ready`'s transitions and effects, then does the bookkeeping its step names — queue admission, a Draft batch's membership commit, the §2.2 re-validation hop, the Work hand-off. | §2.1, §2.2, §3.1 |
| `exec` | Work-execution shim: gathers the view, applies `decide/work`'s transitions and effects, then performs the I/O its step names — container launch, crash recover-or-reset, the finish-line branch read, the Evaluation hand-off. | §3.2 |
| `eval` | Evaluation shim: the launch/monitor half (evaluator prompts, the §3.3 re-review context, the pre-eval rebase) driving `decide/eval`, plus the merge-gate landing fold driving `decide/merge_gate` — both gather view → decide → swap state → apply → interpret, outcomes re-entering as events. | §3.3, §3.2 |
| `interpret` | The effect interpreter: `Core::interpret` executes one `Effect` through the port it names; the sole `&mut Core` coupling deciders keep. | contracts.md §2 |
| `trace` | Test-only golden-trace recorder: an inert-in-prod `TraceSink` a test attaches via `Core::attach_trace` to capture every `set_state` transition and `publish`/escalation effect as YAML fixtures (`tests/traces/`, regen `UPDATE_TRACES=1`); pins decisions during Track C. | refactor-plan B3 |
| `launch_queue` | Capacity-aware launch queue: park on `NoCapacity`, drain on slot-freed, escalate past `MAX_QUEUE_WAIT`. | §3.5 |
| `scan` | Task-timeout and one-shot job-deadline scans; run inside the single-writer loop; also drains the launch queue. | §3.5 |
| `reconcile` | Restart reconciliation of jobs left mid-execution, incl. re-deriving a parked job's missing escalation task from its stamped record; runs in the actor before the message loop. | §3.6 |
| `channel` | Agent → operator channel posts: dispatcher writes `channels` KV and publishes each post to `job-events`. | §4.2 |
| `run` | Production startup: wire store, repos, Docker fleet, provider into a spawned core; fail fast. | §3.6, §12.4 |
| `handlers` | NATS `req.*` subject handlers: translate a request into a `CoreHandle` call and reply per the §6.5 envelope. One module per subject family; `mod.rs` is wiring only — the family table plus the three spawn entry points. | §6.1, §6.5 |
| `handlers/reply` | The §6.5 reply envelope: resource JSON on success, `{"error":{status,message,errors?}}` on failure; total, so a serializer failure never fails a reply. | §6.5 |
| `handlers/container` | The container-facing subjects: `req.{work,eval}.submit` and `req.channel.{update,reply}`, incl. the agent `cover_html` cap. | §4.2, §6.1 |
| `handlers/worker` | `req.worker.announce`: forwards a node's heartbeat into the live fleet; transient by design (losing the stream deregisters the node). | §3.1 |
| `handlers/status` | `req.health` and `req.queue.list`: the two probes that round-trip the core actor, so a wedged state loop reads as unhealthy. | §3.5, §6.1 |
| `handlers/projects` | `req.projects.{create,link}`: bare-repo creation (repo before counter) and the linked-origin flow. | §12.2, §5.3 |
| `handlers/origin` | `req.origin.{release,status,sync}`: the origin PR surface; read-only with respect to job state. | §5.3 |
| `handlers/access` | `req.ssh.sign-user-cert` and `req.members.*`: cert minting from the user's stored roles, and §7.5 role writes (single writer of `users.*`). | §7.3, §7.5 |
| `handlers/jobs` | The `req.jobs.*` family: wire bodies, the `cover_html` cap, and one handler per verb — create, the Draft edits, release/revoke, claims, triage, criteria. | §6.2, §2.1, §1.2 |
| `handlers/jobs_reply` | The jobs family's reply bodies: the derived `awaiting_human` view, the resolved criteria, and the channel-progress join — all derived on read, never stored. | §1.1, §4.2 |
| `handlers/graph` | `req.graph.get`: every job record in the project, unsummarized; a pure store read. | §6.1, §1.4 |
| `handlers/tasks` | The `req.tasks.*` family: the human inbox, a job's task log, operator resolutions, and the live container tail served off the core actor. | §6.1, §4.2 |
| `handlers/jobtypes` | `req.jobtypes.{list,get}`: the job-type library at default-branch HEAD; a broken type still lists, with its errors. | §1.1, §6.1 |
| `handlers/repo` | `req.vcs.{file,tree,diff}` and `req.tags.list`: repo-backed reads, each pinned to one resolved ref. | §6.1, §5.2 |
| `config` | Dispatcher configuration; `AGENT_PROVIDER_DEFAULT` required, refuses to start without it. | §12.4 |

## `platform_ops` — `crates/dispatcher/src/platform_ops/`

The platform-ops context (refactor-plan C8): keeping the platform itself
observable and tidy, as distinct from driving any one job's lifecycle. No
member decides a state transition — a bug here degrades visibility or disk,
never job correctness.

| Module | Contract | Spec |
| --- | --- | --- |
| `platform_ops` | The context charter: live platform visibility (snapshots rebuilt from live state, written only when their bytes change) plus post-run housekeeping; never a job transition. | §3.1, §3.2, §3.6, §12.2 |
| `platform_ops/cd` | Config-snapshot freshness: republish live fleet/deploy-drift state from the scan tick when the bytes change. | CD plan C |
| `platform_ops/fleet` | Live fleet occupancy publishing, rebuilt from live containers (never stale bookkeeping); idle fleet writes nothing. | §3.1, §3.6 |
| `platform_ops/harvest` | Pull artifacts out of an exited container, then reclaim its overlay; runs off the actor thread, writes no state. | §3.2, §3.6 |
| `platform_ops/seed` | Platform starter template embedded in the binary — the files a fresh project is seeded with. | §12.2 |

## `forge_ingest` — `crates/dispatcher/src/forge_ingest/`

The forge-ingest context (refactor-plan C8): where work and code cross the
platform's edge — the outside forge on one side, operator-dispatched advisory
runs on the other. NORTH-STAR §1 names it the one dispatcher subsystem worth
considering as its own process someday, since it is the only one not part of
the single-writer state loop's core job.

| Module | Contract | Spec |
| --- | --- | --- |
| `forge_ingest` | The context charter: everything that talks to something the dispatcher does not own; no member drives a transition, and credentials never leave the context. | §1.2, §5.3 |
| `forge_ingest/triage` | Operator-dispatched advisory triage runs; purely advisory — never drives a transition. | §1.2 |
| `forge_ingest/origin` | Linked-origin projects: the link flow and the origin-release PR surface; credentials never enter containers. | §5.3 |
| `forge_ingest/github` | Minimal GitHub REST client (create/read PRs) behind a trait; PAT resolved per call, never held. | §5.3 |

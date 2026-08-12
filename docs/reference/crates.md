# Chuggernaut v2 — Crate and Module Breakdown

Companion to `docs/spec.md` (normative behavior) and `docs/design/000-rationale.md` (rationale). This document maps the spec onto a Rust workspace: which crates exist, what each owns, and how the dispatcher is decomposed internally.

**See also** — for where this map is *heading*: `docs/README.md` (target factoring), `docs/reference/structure-assessment.md` (current-state audit against that target), `docs/reference/contracts.md` (extracting the dispatcher's internal interfaces), and `docs/design/210-ts-rewrite-plan.md` (the TypeScript dispatcher rewrite).

## Workspace location

The Cargo workspace lives at the repo root (`Cargo.toml`, `crates/*`).

## Binary strategy

One fat binary plus three tiny ones:

- **`chuggernaut`** — single deployable with subcommands: `chuggernaut dispatcher`, `chuggernaut api`, `chuggernaut webhooks`, `chuggernaut init`, `chuggernaut admin ...`. Dispatcher and API still run as **separate processes** with different mounted keys (age/SSH-CA/NATS-operator private keys are dispatcher-only; JWT keys and VAPID private key on the API — spec §7, §12.1); sharing a binary just means one artifact to version and deploy. Service logic lives in library crates; the bin crate is argument parsing and wiring.
- **`chuggernaut-channel`** and **`chuggernaut-ko`** — the two MCP servers (spec §4.2). Built as small static binaries (musl) because they are injected into arbitrary agent images (put-archive, spec §3.1) and must run with no runtime dependencies.
- **`chuggernaut-harness`** — the inline review loop driver (spec §4.5). Same static-musl injection story; runs as the work container CMD when `work.review` is declared, alternating author and reviewer agent processes and reporting steps via `req.step.report.*`.

## Crates

| Crate | Kind | Spec | Purpose |
|---|---|---|---|
| `types` | lib | §1 | Shared domain types, no I/O |
| `chuggernaut-domain` | lib | §2.1–2.2, §3, docs/reference/contracts.md §2 | The pure core (`crates/domain`): state machine, DAG, queue, release validation, effect vocabulary, deciders — no async, no I/O, by construction |
| `chuggernaut-platform-ops` | lib | §3.1, §3.2, §3.6, §12.2 | The platform-ops context (`crates/platform-ops`): fleet occupancy, config/deploy-drift snapshots, container harvest, project seed template — the platform's own observability and housekeeping, never a job transition |
| `store` | lib | §1.4–1.5, §8, §9 | NATS KV/stream access; the only crate that talks to NATS |
| `auth` | lib | §7 | JWT, SSH CA, per-job credentials, permission rules |
| `container` | lib | §3.1 | `ContainerBackend` trait + the Docker, host-process and (stub) k8s implementations |
| `worker` | lib | §3.1 | Worker-node daemon (`chuggernaut worker`) + NATS-proxying `FleetBackend` for mixed fleets |
| `agent` | lib | §4 | `AgentProvider` trait + Claude/Codex impls, prompt assembly, KO injection |
| `vcs` | lib | §5 | Bare repo management, git CLI shell-out, diff/tree/blob/log |
| `dispatcher` | lib | §2, §3 | State machine, DAG, work queue, scans, reconciliation |
| `api` | lib | §6, §10.4, §11 | axum HTTP↔NATS bridge, SSE, Web Push, PWA static serving |
| `webhooks` | lib | §6.4 | Stream consumer → external endpoint pusher |
| `cli` | lib | §12 | `init` bootstrap and `admin` commands |
| `chuggernaut` | bin | — | Subcommand wiring for all of the above |
| `chuggernaut-channel` | bin | §4.2 | Channel MCP server (static, mounted into agent containers) |
| `chuggernaut-ko` | bin | §4.2, §9 | Knowledge MCP server (static, mounted into agent containers) |
| `chuggernaut-harness` | bin | §4.5 | Inline review loop driver (static, injected as work CMD when `work.review` is declared) |
| `test-utils` | lib | — | Embedded NATS harness, fixture builders, fake backend/provider |

### `types`

Pure data: `Job`, `JobState`, `Task`, `TaskKind`/`TaskState`/`TaskResult`, `TaskResolution`, `EscalationAction`, `TokenUsage`, `User`, `Identity`, `ProjectRole`, `ChannelEntry`/`ChannelUpdate`/`AgentReply`, `EvalResult`, event payloads (§6.3), error envelope (§6.5), the job type YAML schema (`JobType`) with its field-rules validation (§1.1 tables), the `CloudIdentity` record a `workload_identities:` name resolves to (§8.3), and the schedule YAML schema (`Schedule`) with the five-field UTC cron parser and matcher it validates against (`CronExpr`, §1.1). Serde derives throughout; the YAML field-rules matrices are enforced here so every consumer (dispatcher validation, CLI linting, tests) shares one implementation. No async, no I/O — everything depends on `types`, so it stays dependency-light.

Behind the off-by-default `schema` feature the wire types also derive
`JsonSchema`, which is what makes `types` the single source of the §6.2 HTTP
contract: `chuggernaut schema api` emits them to `.chug/schemas/api.schema.json` and
the `committed_schemas_are_current` drift test fails CI when a covered type
changes without re-emission (refactor-plan D1). The feature is a derive-macro
dependency only — the purity rule above still holds, machine-checked by
`boundary_guard`.

The operator UI's `web/src/api/types.gen.ts` is generated from that schema, so
these types are now the compiled-against definition on both sides of the wire
(refactor-plan D2). Responses are emitted under schemars' **serialize**
contract and request bodies under the deserialize one — a `#[serde(default)]`
field a record always writes is required in the generated client, while the
same field on a request body stays optional for the caller.
`chuggernaut schema api-samples` emits one serialized example per response type
into `web/src/api/wire-samples.json`, which the web round-trip test parses
against the generated types.

### `store`

The single NATS integration point, wrapping `async-nats`:

- Bucket definitions and creation (§1.4 bucket model, §1.5 config) — used by `cli` init and `test-utils`
- **Key encoding** (§1.4): base64url helpers for emails and KO subject/predicate segments; name validation for vars/secrets
- Typed accessors per bucket: `JobStore`, `TaskStore`, `UserStore`, `ChannelStore`, `PushStore`, `CounterStore`, `RdepsStore`
- `VarStore` and `SecretStore` traits (§8) with the NATS impls; age encrypt/decrypt lives behind `SecretStore` (public-key-only construction for the API side, private-key construction for the dispatcher side)
- The plaintext `cloud-identities` bucket (§8.3) — its own namespace, deliberately unreachable from `SecretStore`, so a cloud identity can never ride the `global/agents` grant; read through the raw bucket (admin CLI writes, release validation lists) until a consumer needs a typed accessor
- Knowledge store (§9): O(1) get, prefix-scan list, three-scope merge with narrower-wins dedup
- Stream helpers: event publish (`job.events.*`), `channel-inbox` append/replay-from-sequence, request-reply with bounded retry (§4.2 reliability)

### `auth`

- JWT (RS256) issue/verify, cookie construction (§7.1); `AuthProvider` trait for later swap-out
- SSH CA: user cert signing (§7.3) and per-job cert issuance with `job:{owner}/{project}:{seq}` principals (§7.4)
- Per-job NATS JWT minting with the §7.4 allow-lists
- The OIDC issuer's identity (`oidc`): the `kid` — the RFC 7638 JWK thumbprint of `oidc_public.pem`, pure and stable across restarts (design #313 A2) — plus the RFC 7517 JWK set, the §6.7 discovery document, and `issuer_from_env`, the one resolver of `OIDC_ISSUER` — the api's routes read it today and the dispatcher hands the same value to `workload`'s minter
- Workload-token claim assembly, minting and delivery shape (`workload`): the #313 A1 claim set from a typed request, the A3 TTL rule (`min(task_timeout, cap)`, cap 3600) and one audience per token, RS256 with that `kid`, plus the two container paths a granted identity occupies, its ADC document and the single-identity `GOOGLE_APPLICATION_CREDENTIALS` rule. Pure — `now` and the `jti` are arguments — and the minted token has no `Debug`/`Serialize` route out (§10.2). `dispatcher::workload` is the I/O around it: the record read, the mint at launch and the injected files (§8.3)
- Permission rules table (§7.5) as a pure `authorize(identity, action)` function used by the API layer
- The SSH server's ref-authorization hook (§5.2): a small `chuggernaut-ssh-authz` helper (exposed via the fat binary) invoked by sshd to enforce push/pull rules per principal

### `container`

`ContainerBackend` trait exactly as specced (§3.1), plus:

- `DockerBackend` (v1 production default): a **fleet of one or more Docker daemons** — local socket single-node, TCP+mTLS/SSH-tunnel endpoints multi-node. Slot-capped least-loaded placement; `ContainerId` encodes the owning node (`{node}/{docker_id}`). The dispatcher is the scheduler, so nodes are dumb endpoints — no Swarm, no cluster state
- `HostBackend` (design #309 P0, prototype, **off by default**): host processes on the node itself, reachable only from a node whose `WORKER_MODES` names `host`. A task is a process group rooted in a per-task directory under `WORKER_HOST_ROOT` — that directory is what makes `inspect`, the two listings and `remove` implementable. It **refuses** a launch declaring an `image` — the image's absence is what selects it (#309 §1) — cannot enforce `resources.cpu`/`memory`, and runs **one task per node** (`slots: 1`, refused at boot otherwise) — which outlived the `/workspace` collision that motivated it and stays for phase 1 because a Mac's simulator state is machine-global (#322 §5). `/workspace` and `/chuggernaut` are **wire paths** it rebases into the task directory over all five surfaces — clone destination (`CHUG_WORKSPACE`), injected file, `copy_file`, `find_file`'s directory and env values naming one — refusing anything else by name (#322 §2). `find_file` is the one surface where the mapping also runs in **reverse**: it answers in wire paths, so a real path it cannot express as one is refused rather than returned raw (#490 D1a). It runs host work only for the projects the node's `WORKER_HOST_PROJECTS` names (`HostTenancy`, #309 §10, job #525): a launch whose `JOB_PROJECT` the list does not carry is a `Launch` refusal naming the project and the node, never the retryable `NoCapacity`, and an undeclared list admits nobody — the same node's *container* launches never reach this backend and are untouched by it. It serves an **agent-shaped** launch, one whose env sets `CLAUDE_CONFIG_DIR`, since #490 slice 5 — and tests the node's `AgentCapability` rather than the shape: no CLI discovered on the daemon's `PATH` (#490 D3, `crates/worker/src/agent_cli.rs`) or no runnable channel binary of the node's own (#490 D2) is a `Launch` refusal **naming which**, at the launch and never at boot, so a Mac that cannot serve agent work keeps every other slot it has. The capability is handed in at construction the way `Supervision` is: the daemon discovers node facts, this backend is told them. The channel binary is the one file a host task's agent needs that is neither injected nor injectable — a Mach-O built for the node, at a path outside the mapping — so the launch names it and the node provides it. Each task is launched into its own supervision unit rather than the daemon's — a transient systemd scope on Linux, the process group on macOS (design #440 D3) — and a node that cannot create one refuses the boot rather than advertising `host`. A task runs as the **daemon's own uid** unless the node's `WORKER_HOST_USERS` binds each of those projects to a unix user of its own (`HostUsers`, design #537 D1/D3, resolved at boot by `crates/worker/src/host_users.rs`): a bound launch escalates with `sudo -n -u chug-{project} -H`, takes its `HOME` and a group-shared task directory from that user, and — since `sudo` resets the environment — is handed its composed environment as a file it alone owns and its wrapper sources, because argv is readable across users. A project the node **declares** and cannot resolve is a `Launch` refusal naming the project, the user and the node, never a fall back to the daemon's uid
- `K8sBackend` (Jobs API: create Job, watch pod status, stream logs; scale-out beyond a small fleet, built when needed)
- The **workspace bootstrap wrapper** (§4.1): wraps every CMD with clone-to-`${CHUG_WORKSPACE:-/workspace}` + exec — unset is a container task's behaviour exactly, and a host backend sets it
- **File injection** (put-archive after create, before start) for MCP binaries, prompt, and event batch — no host bind-mounts, so remote fleet nodes need nothing on disk
- **Node properties** a worker sets on its own backend and the dispatcher never does (§3.1): the node-local build cache bind, the `/dev/kvm` passthrough with its read-only toolchain mounts (design #367 A1), the Xcodes a host-capable node discovered at boot, which a launch's `xcode:<version>` resolves against (`crates/worker/src/xcode.rs`, design #322 W4), the agent CLI the same node discovers on the daemon's own `PATH` (`crates/worker/src/agent_cli.rs`, design #490 D3), and the per-project unix users a host node resolves out of its own passwd database (`crates/worker/src/host_users.rs`, design #537 D3). None of them reaches the wire or `ContainerLaunchConfig` — each discovered fact is advertised on `NodeCapabilities`, which is a report rather than an input
- Launch config assembly helpers (env, injected files, limits)

No knowledge of jobs or state — it launches, waits, kills, inspects, copies files.

### `agent`

- `AgentProvider` trait, `ClaudeProvider`, `CodexProvider` (§4.3)
- Prompt resolution: read from repo at `base_ref`, append rework/conflict context block, deliver via temp file mounted at `/chuggernaut/prompt.md`
- Upfront KO injection composition (§4.4): resolve tags via `store`, build system prompt
- MCP server config serialization per provider (inline JSON for Claude, config.toml for Codex)

Depends on `container` (launches through the backend) and `store` (KO resolution).

### `vcs`

- Bare repo lifecycle (`{repos_root}/{owner}/{project}.git`), project creation with initial commit and HEAD symref (§12.2)
- All git operations by shelling out (§5.1): branch create/delete/hard-reset, squash-merge with the §3.2 commit-message format, conflict detection
- Conflict-context builder (§4.3): status/log/diff-stat between old and new `base_ref`
- Read API: diff-by-job-state (§6.2 behavior table, including the Done-state `git log --grep` recovery), tree, blob, log
- `diff_page` — cursor paging of a diff (§6.2): pure slicing of the diff text into byte-offset pages capped against their JSON-escaped length, so a diff of any size crosses a `max_payload`-bound reply; each page carries a sha-256 of the whole diff, because the diff is regenerated per page and a moving job branch must be detected rather than spliced

### `dispatcher`

The core. Internal module map:

Each module opens with a contract-style `//!` header (accepts / emits /
guarantees / spec §); `docs/reference/modules.md` at the repo root is the one-line registry.
The map below mirrors the actual `crates/dispatcher/src/**/*.rs` tree; a
directory in it groups modules under a `mod.rs` that carries the charter they
share — for `forge_ingest/` a **named context** (NORTH-STAR §1), for
`handlers/` one module per `req.*` subject family. The other named context,
platform-ops, has left this tree for `crates/platform-ops` (refactor-plan C9);
what remains here is `platform_ops.rs`, the adapter.

The pure pieces live in `crates/domain` (`chuggernaut-domain`, refactor-plan
C1) and are re-exported by the dispatcher so call sites keep one surface:

```
domain/ (chuggernaut-domain — pure: no tokio/async-nats/store/vcs/auth)
  state.rs       — the §2.1 transition table (`assert_transition`)
  graph.rs       — in-memory DAG, rdeps maintenance and dependency queries (§1.4, §2.3)
  queue.rs       — in-memory FIFO of Ready job IDs (§3.1 step 5)
  release.rs     — release validation, pure half: error vocabulary, wiring rules,
                   additive-evaluator merge (§2.2, §2.3)
  inputs.rs      — job inputs vs their declaration: the `inputs.{name}` semantic verdict
                   shared by release and the Ready-transition re-check, the add-only
                   default fill the first base_ref pin performs, and delivery —
                   `CHUG_INPUT_*` env injection + the event audit fragment
                   (§1.1, §2.2, §4.1, §10.3)
  effects.rs     — the Effect vocabulary: each port action as serde data (docs/reference/contracts.md §2)
  decide/        — the decider layer: pure `(view, event) -> (transitions, effects)`
    escalation.rs— the C1 template decider: the escalate/stall family (§1.2, §3.4)
    merge_gate.rs— the C2 landing decider: depth-1 queue + gate as a decider-owned
                   value, continuation events for effect results (§3.3)
    wrapup.rs    — the C3 wrap-up decider: the post-merge publish fork and terminal
                   stamping, incl. a batch's Done fan-out (§3.2 step 12, §2.1)
    ready.rs     — the C4 Ready-phase decider: dep satisfaction, the base_ref pin (and
                   the declared-input default fill that rides on the first one),
                   queue admission both ends, the Blocked→Ready re-validation fork
                   (§2.1, §2.2, §3.1)
    eval.rs      — the C5 evaluation decider: the staged fan-out, each evaluator type's
                   verdict, the retry/rework budgets, the reduce's pass/rework/abort/
                   escalate fork; owns the round as a value (§3.3)
    work.rs      — the C6 Work decider: the launch-time validation fork, one attempt's
                   task record incl. claim parking, the exit verdict with the
                   finish-line guard, and the one retry policy (§3.2, §1.2)
```

```
dispatcher/
  core.rs        — the single-writer event loop (see below); all mutable state lives here
  release.rs     — release validation, ref-reading half: .chug/jobs/*.yaml loading + prompt/KV
                   checks through the vcs port (§2.2, §14); re-exports the pure half
  ready.rs       — the Ready-phase shim: view/decide/apply/interpret for decide/ready,
                   plus queue admission, batch absorption and the Work hand-off (§2.1, §3.1)
  exec.rs        — the Work-phase shim: view/decide/apply/interpret for decide/work, plus
                   the I/O it names — container launch, crash recover-or-reset, the
                   finish-line branch read, the Evaluation hand-off (§3.2)
  eval.rs        — the Evaluation shim: evaluator prompts and launches driving
                   decide/eval (§3.3), post-eval finalization and the depth-1 merge
                   gate driving decide/merge_gate (§3.2 step 12)
  interpret.rs   — the effect interpreter: `Core::interpret` runs one Effect through its
                   port; the sole `&mut Core` coupling deciders keep (docs/reference/contracts.md §2)
  invariants.rs  — executable invariant checker over the read-only CoreState view (B1)
  trace.rs       — test-only golden-trace recorder pinning decisions (B3)
  launch_queue.rs— capacity-aware launch queue: park on NoCapacity, drain on slot-freed (§3.5)
  scan.rs        — task-timeout and one-shot job-deadline scans, plus the schedule tick
                   driving decide/schedule and originating the jobs it fires (§3.5, §1.1)
  schedules.rs   — the in-memory schedule table: .chug/schedules/*.yaml read at
                   default-branch HEAD, invalid files skipped and logged (§1.1)
  reconcile.rs   — restart reconciliation of mid-execution jobs, incl. the escalation
                   inbox heal (§3.6)
  channel.rs     — agent → operator channel posts: writes `channels` KV + job-events (§4.2)
  workload.rs    — workload-token minting and delivery at container launch: the
                   cloud-identities.* read, auth::workload's pure mint per (container,
                   declared identity), the two injected files and the identity — never
                   the token — on the task record and its event (§7.4, §8.3, §10.3)
  platform_ops.rs— the adapter for the platform-ops context, which is now its own crate
                   (C9): lends it `JobLookup`/`FleetView`/`ConfigSnapshot` off Core's
                   fields, decides nothing (§3.1, §3.6)
  forge_ingest/  — the forge-ingest context (C8): where work and code cross the platform's
                   edge; no member drives a job transition, credentials never leave it
    triage.rs    — operator-dispatched advisory triage runs; never drives a transition (§1.2)
    origin.rs    — linked-origin projects: the link flow and the origin-release PR surface (§5.3)
    github.rs    — minimal GitHub REST client (create/read PRs) behind a trait for the origin surface (§5.3)
  run.rs         — production startup: wire store, repos, fleet, provider; fail fast (§3.6, §12.4)
  handlers/      — NATS req.* subject handlers, one module per subject family (§6.1, §6.5):
                   mod.rs (wiring + the three spawn entry points), reply.rs (§6.5 envelope),
                   container.rs, worker.rs, status.rs, projects.rs, origin.rs, access.rs,
                   jobs.rs + jobs_reply.rs, graph.rs, groups.rs, tasks.rs, jobtypes.rs, repo.rs
  config.rs      — dispatcher config (AGENT_PROVIDER_DEFAULT etc., §12.4)
```

Every `handlers/` module carries its own contract header and a `docs/reference/modules.md`
row (the registry gate covers nested modules); `mod.rs` holds no request
handling of its own — it names the families and hands each one its ports.

**Single-writer core:** `core.rs` owns all mutable state (job records, task log tail, DAG, work queue) inside one tokio task. Everything else — NATS request handlers, container monitors, scan timers — sends messages over an `mpsc` channel and never mutates state directly. Container monitoring is concurrent (one lightweight task per running container, each just awaiting `backend.wait()` and posting the exit back to the core loop). This makes the "state transitions are processed one at a time" guarantee (§3.1) structural rather than disciplinary: there is no lock to misuse because there is no shared mutable state.

### `api`

- axum router for the full §6.2 surface; every handler is translate-authenticate-forward (no orchestration)
- Auth middleware (`auth` crate), permission enforcement (§7.5)
- SSE bridge with `Last-Event-ID` replay (§6.4)
- Secret encryption on write (age public key only)
- Ingest endpoint (§13.2): Bearer-token auth against hashed tokens, envelope wrapping, publish to the `ingest` stream
- Web Push: subscription CRUD, the `task-created`→push background consumer (§11)
- The §6.7 issuer documents (`oidc`): the two `.well-known` routes, built apart from the authenticated surface so the exemption is explicit, and served from documents built once at startup
- Serves the PWA's static assets from the same origin (§10.4)

### `webhooks`, `cli`

Thin: `webhooks` consumes `job-events` and POSTs to configured endpoints; `cli` implements §12.1 init (keygen, bucket/stream creation, admin user) and the §12.3 admin commands. Both are library crates invoked from the fat binary.

### MCP binaries

`chuggernaut-channel`: `update_status`, `channel_check` (with `since`), `reply`, `submit_result`, `submit_eval` — a stdio MCP server bridging to NATS using the injected scoped JWT, with the bounded-retry submit behavior (§4.2). `chuggernaut-ko`: read-only KO queries against the three scopes. Both depend only on `types` + `store` (and an MCP server library), built for `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`.

## Dependency graph

```
types ──────────────────────────────┐
chuggernaut-domain ► types          │
store ──► types                     │
auth ───► types, store              │
vcs ────► types                     │
container ► types                   │
agent ──► types, store, container   │
chuggernaut-platform-ops ► types, store, vcs, container, agent
dispatcher ► types, chuggernaut-domain, chuggernaut-platform-ops, store, auth, vcs, container, agent
api ────► types, store, auth
webhooks ► types, store
cli ────► types, store, auth, vcs
chuggernaut (bin) ► dispatcher, api, webhooks, cli
chuggernaut-channel / chuggernaut-ko / chuggernaut-harness (bins) ► types, store
test-utils ► types, store, container (fake backend), agent (fake provider), vcs (temp repos)
```

Invariants worth enforcing (enforced in `test-utils/tests/boundary_guard.rs` over `cargo metadata`, refactor-plan A3): only `store` depends on `async-nats`; only `container` and `agent` know about containers; `api` never depends on `dispatcher` (they communicate exclusively over NATS); `types` has no async runtime dependency; `chuggernaut-domain` resolves neither `tokio` nor `async-nats` (nor `store`/`vcs`/`auth`) anywhere in its subtree, plus a zero-`.await` sweep over its sources; and `chuggernaut-platform-ops` (refactor-plan C9) declares only its charter's edges — the port crates it drives, never `dispatcher`, dev-deps included — with the reverse edge asserted so the arrow between context and lifecycle stays one-way. The sibling `test-utils/tests/lint_guard.rs` (refactor-plan A4) guards the other half of docs/reference/style.md Tier 1 — the `clippy.toml` line limit, the `[workspace.lints.clippy]` denies, and every member's `lints.workspace = true` opt-in, without which a new crate would sit silently outside the clippy gate.

## Not crates

- **PWA** (Part 11) — frontend workspace at `web/`: React + TypeScript + Vite (React Flow, `react-diff-view`, `vite-plugin-pwa`); built assets embedded into or served by `api`.
- **Host preparation** — `nix/`: the `chug-node` NixOS and nix-darwin modules a worker node's host repo imports (`flake.nix` exports them as `nixosModules.chug-node` / `darwinModules.chug-node`, design #372). Nix, not Rust; **nothing in CI evaluates it** (#372 §2.3) — the consuming host repo's `nixos-rebuild build` is the gate. Since design #440 slice 7 it also declares the worker daemon's systemd unit, from a template `nix/chug-node/chug-worker-unit.test.sh` pins against the one `deploy/prod/build-worker.sh` renders; that suite is text over text and still evaluates no nix.
- **SSH server** — stock `sshd` with `TrustedUserCAKeys` and an `AuthorizedPrincipalsCommand`/forced-command hook calling the `auth` ref-authorization helper; configuration, not code.

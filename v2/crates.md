# Chuggernaut v2 — Crate and Module Breakdown

Companion to `spec.md` (normative behavior) and `design.md` (rationale). This document maps the spec onto a Rust workspace: which crates exist, what each owns, and how the dispatcher is decomposed internally.

## Workspace location

v2 is developed as a fresh workspace under `v2/` (`v2/Cargo.toml`, `v2/crates/*`) so the working v1 system at the repo root stays intact during the build-out. When v2 replaces v1, the workspace moves to the root and the v1 crates are deleted.

## Binary strategy

One fat binary plus two tiny ones:

- **`chuggernaut`** — single deployable with subcommands: `chuggernaut dispatcher`, `chuggernaut api`, `chuggernaut webhooks`, `chuggernaut init`, `chuggernaut admin ...`. Dispatcher and API still run as **separate processes** with different mounted keys (age/SSH-CA/NATS-operator private keys are dispatcher-only; JWT keys and VAPID private key on the API — spec §7, §12.1); sharing a binary just means one artifact to version and deploy. Service logic lives in library crates; the bin crate is argument parsing and wiring.
- **`chuggernaut-channel`** and **`chuggernaut-ko`** — the two MCP servers (spec §4.2). Built as small static binaries (musl) because they are volume-mounted into arbitrary agent images and must run with no runtime dependencies.

## Crates

| Crate | Kind | Spec | Purpose |
|---|---|---|---|
| `types` | lib | §1 | Shared domain types, no I/O |
| `store` | lib | §1.4–1.5, §8, §9 | NATS KV/stream access; the only crate that talks to NATS |
| `auth` | lib | §7 | JWT, SSH CA, per-job credentials, permission rules |
| `container` | lib | §3.1 | `ContainerBackend` trait + Docker and k8s implementations |
| `agent` | lib | §4 | `AgentProvider` trait + Claude/Codex impls, prompt assembly, KO injection |
| `vcs` | lib | §5 | Bare repo management, git CLI shell-out, diff/tree/blob/log |
| `dispatcher` | lib | §2, §3 | State machine, DAG, work queue, scans, reconciliation |
| `api` | lib | §6, §10.4, §11 | axum HTTP↔NATS bridge, SSE, Web Push, PWA static serving |
| `webhooks` | lib | §6.4 | Stream consumer → external endpoint pusher |
| `cli` | lib | §12 | `init` bootstrap and `admin` commands |
| `chuggernaut` | bin | — | Subcommand wiring for all of the above |
| `chuggernaut-channel` | bin | §4.2 | Channel MCP server (static, mounted into agent containers) |
| `chuggernaut-ko` | bin | §4.2, §9 | Knowledge MCP server (static, mounted into agent containers) |
| `test-utils` | lib | — | Embedded NATS harness, fixture builders, fake backend/provider |

### `types`

Pure data: `Job`, `JobState`, `Task`, `TaskKind`/`TaskState`/`TaskResult`, `TaskResolution`, `EscalationAction`, `TokenUsage`, `User`, `Identity`, `ProjectRole`, `ChannelEntry`/`ChannelUpdate`/`AgentReply`, `EvalResult`, event payloads (§6.3), error envelope (§6.5), and the job type YAML schema (`JobType`) with its field-rules validation (§1.1 tables). Serde derives throughout; the YAML field-rules matrices are enforced here so every consumer (dispatcher validation, CLI linting, tests) shares one implementation. No async, no I/O — everything depends on `types`, so it stays dependency-light.

### `store`

The single NATS integration point, wrapping `async-nats`:

- Bucket definitions and creation (§1.4 bucket model, §1.5 config) — used by `cli` init and `test-utils`
- **Key encoding** (§1.4): base64url helpers for emails and KO subject/predicate segments; name validation for vars/secrets
- Typed accessors per bucket: `JobStore`, `TaskStore`, `UserStore`, `ChannelStore`, `PushStore`, `CounterStore`, `RdepsStore`
- `VarStore` and `SecretStore` traits (§8) with the NATS impls; age encrypt/decrypt lives behind `SecretStore` (public-key-only construction for the API side, private-key construction for the dispatcher side)
- Knowledge store (§9): O(1) get, prefix-scan list, three-scope merge with narrower-wins dedup
- Stream helpers: event publish (`job.events.*`), `channel-inbox` append/replay-from-sequence, request-reply with bounded retry (§4.2 reliability)

### `auth`

- JWT (RS256) issue/verify, cookie construction (§7.1); `AuthProvider` trait for later swap-out
- SSH CA: user cert signing (§7.3) and per-job cert issuance with `job:{owner}/{project}:{seq}` principals (§7.4)
- Per-job NATS JWT minting with the §7.4 allow-lists
- Permission rules table (§7.5) as a pure `authorize(identity, action)` function used by the API layer
- The SSH server's ref-authorization hook (§5.2): a small `chuggernaut-ssh-authz` helper (exposed via the fat binary) invoked by sshd to enforce push/pull rules per principal

### `container`

`ContainerBackend` trait exactly as specced (§3.1), plus:

- `DockerBackend` (socket; dev and the v1 production default) and `K8sBackend` (Jobs API: create Job, watch pod status, stream logs; scale-out, built when needed)
- The **workspace bootstrap wrapper** (§4.1): wraps every CMD with clone-to-`/workspace` + exec
- Launch config assembly helpers (env, volumes, limits)

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

### `dispatcher`

The core. Internal module map:

```
dispatcher/
  core.rs        — the single-writer event loop (see below)
  state.rs       — the §2.1 transition table: one function per transition, guards + effects
  graph.rs       — in-memory DAG (petgraph), rdeps maintenance and startup rebuild
  queue.rs       — in-memory FIFO of Ready job IDs (§3.1 step 5)
  release.rs     — three-pass validation (§2.2), graph validate/release (§2.3)
  exec.rs        — the §3.2 work-execution sequence
  eval.rs        — evaluator fan-out and reduce (§3.3), per-evaluator image resolution
  escalation.rs  — escalation task creation, resolution actions incl. pre-Work rules (§1.2)
  launch.rs      — launch-time validation, secret/var injection, credential issuance, container config
  scan.rs        — task-timeout and one-shot job-deadline scans (§3.5)
  factory.rs     — factory reload from default-branch HEAD, durable ingest consumers,
                   batching, triage job creation, auto-release policy (§13)
  reconcile.rs   — restart reconciliation (§3.6)
  handlers/      — one module per req.* subject family (jobs, graph, vcs, vars, secrets,
                   knowledge, channel, tasks, work/eval submit, usage, ssh)
  config.rs      — dispatcher config (AGENT_PROVIDER_DEFAULT etc., §12.4)
```

**Single-writer core:** `core.rs` owns all mutable state (job records, task log tail, DAG, work queue) inside one tokio task. Everything else — NATS request handlers, container monitors, scan timers — sends messages over an `mpsc` channel and never mutates state directly. Container monitoring is concurrent (one lightweight task per running container, each just awaiting `backend.wait()` and posting the exit back to the core loop). This makes the "state transitions are processed one at a time" guarantee (§3.1) structural rather than disciplinary: there is no lock to misuse because there is no shared mutable state.

### `api`

- axum router for the full §6.2 surface; every handler is translate-authenticate-forward (no orchestration)
- Auth middleware (`auth` crate), permission enforcement (§7.5)
- SSE bridge with `Last-Event-ID` replay (§6.4)
- Secret encryption on write (age public key only)
- Ingest endpoint (§13.2): Bearer-token auth against hashed tokens, envelope wrapping, publish to the `ingest` stream
- Web Push: subscription CRUD, the `task-created`→push background consumer (§11)
- Serves the PWA's static assets from the same origin (§10.4)

### `webhooks`, `cli`

Thin: `webhooks` consumes `job-events` and POSTs to configured endpoints; `cli` implements §12.1 init (keygen, bucket/stream creation, admin user) and the §12.3 admin commands. Both are library crates invoked from the fat binary.

### MCP binaries

`chuggernaut-channel`: `update_status`, `channel_check` (with `since`), `reply`, `submit_result`, `submit_eval` — a stdio MCP server bridging to NATS using the injected scoped JWT, with the bounded-retry submit behavior (§4.2). `chuggernaut-ko`: read-only KO queries against the three scopes. Both depend only on `types` + `store` (and an MCP server library), built for `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`.

## Dependency graph

```
types ──────────────────────────────┐
store ──► types                     │
auth ───► types, store              │
vcs ────► types                     │
container ► types                   │
agent ──► types, store, container   │
dispatcher ► types, store, auth, vcs, container, agent
api ────► types, store, auth
webhooks ► types, store
cli ────► types, store, auth, vcs
chuggernaut (bin) ► dispatcher, api, webhooks, cli
chuggernaut-channel / chuggernaut-ko (bins) ► types, store
test-utils ► types, store, container (fake backend), agent (fake provider), vcs (temp repos)
```

Invariants worth enforcing (e.g. via CI lint): only `store` depends on `async-nats`; only `container` and `agent` know about containers; `api` never depends on `dispatcher` (they communicate exclusively over NATS); `types` has no async runtime dependency.

## Not crates

- **PWA** (Part 11) — frontend workspace at `v2/web/`: React + TypeScript + Vite (React Flow, `react-diff-view`, `vite-plugin-pwa`); built assets embedded into or served by `api`.
- **SSH server** — stock `sshd` with `TrustedUserCAKeys` and an `AuthorizedPrincipalsCommand`/forced-command hook calling the `auth` ref-authorization helper; configuration, not code.

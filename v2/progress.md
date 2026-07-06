# Chuggernaut v2 — Implementation Progress

Companion to `spec.md` (normative), `design.md` (rationale), `crates.md`
(crate map). This file tracks what is actually built, where it deviates from
or extends the spec, and what comes next. Update it at the end of each
implementation session.

**As of:** 2026-07-05 (session 3) · 83 tests passing, clippy clean. The
system is runnable outside tests (`init` → `admin project create` →
`dispatcher`), and per-job credentials (§7.4) are now real: launches mint
scoped NATS user JWTs, live-verified against an operator-mode server.

---

## Status by crate

| Crate | Status | Notes |
|---|---|---|
| `types` | ✅ done | Full domain model incl. `ReviewSpec`, `TaskPhase::MergeGate`, `StepRecord`, `ProjectDefaults`, `Task.evaluator`, shared duration parser |
| `store` | ✅ done (core) | `NatsStore` + typed accessors (jobs/tasks/steps/counters/rdeps), topology creation, bounded-retry request, `subscribe_requests`, `read_subject_after`, `read_stream`, `AgeSecretStore` | 
| `vcs` | ✅ done | Pre-existing + `create_squash_candidate` / `advance_default` (merge gate) |
| `container` | ✅ done | `DockerBackend` over bollard 0.19: fleet nodes, label-counted slot placement, put-archive injection, `{node}/{id}` routing. `k8s.rs` still stub |
| `dispatcher` | ✅ done (orchestration) | Everything in spec Parts 2–3: lifecycle, execution, rework, merge gate, human tasks, reconciliation, scans, NATS submit handlers |
| `agent` | 🟡 partial | `ClaudeProvider` done (invocation, prompt injection, MCP config, self-enforced timeout). `CodexProvider` stub. KO/system-prompt injection not wired |
| `chuggernaut-channel` | 🟡 partial | Working stdio MCP server: update_status/reply/channel_check + role-gated submit_result/submit_eval. Missing: `submit_review` local mode (§4.5), `create_job` (factories §13), push-mode inbox |
| `chuggernaut-harness` | 🔴 scaffold | Config types + loop protocol as TODO (§4.5) |
| `chuggernaut-ko` | 🔴 stub | |
| `auth` | ✅ done (lib) | §7 complete as a library: RS256 platform JWTs + cookie + `JwtAuthProvider`, SSH CA signing (user 24h / per-job ephemeral keypair+cert, authz context in force-command), §5.2 ref-authz pure functions, hand-rolled NATS JWTs (operator/account/user) + §7.4 allow-lists + `.creds`/resolver-config rendering. Not yet deployed: sshd front (`ssh-shell` subcommand, pre-receive hooks) — repos still `file://` |
| `api` | 🔴 stub | No HTTP surface; only container-facing NATS subjects handled |
| `cli` / `chuggernaut` bin | 🟡 done (core) | `init` (§12.1: keygen incl. NATS operator/SYS/CHUG seeds + derived `nats-resolver.conf` and `dispatcher.creds`, topology, VAPID pub in KV, admin user), `admin user create/list/delete`, `admin project create/list` (§12.2), `dispatcher` subcommand wired. Missing: ingest tokens, role set/unset, key rotation, seed, `ssh-shell`/`ssh-authz` |
| `webhooks` | 🔴 stub | |
| `test-utils` | ✅ done | `FakeBackend`, `FakeProvider` (async run hooks mirroring submit-ack-then-exit ordering), `NatsTestServer` (Docker `nats:2-alpine`, skip-guarded; `spawn_with_config` boots operator-mode servers), `TempRepo`/`WorkClone`/`clone_branch_from` |

## What works end to end (all under test)

- Create → release (3-pass §2.2 validation incl. `jobs/_defaults.yaml` merge)
  → Ready/Blocked → Work (branch at pinned `base_ref`, launch-time
  validation, real container or provider launch) → Evaluation fan-out
  (command exit-code verdicts + `eval-result.json`, agent `submit_eval`-or-
  infra-retry, human via inbox) → reduce → squash-merge → Done → dependents
  unblock.
- Rework loops: eval failure (budget-consuming, §4.3 findings block appended
  to prompt), merge conflict (budget-free, conflict context), merge-gate
  failure (budget-free, findings + conflict context, `base_ref` rebased).
- **Merge gate (§3.3)**: per-project depth-1 merge queue; HEAD-moved → candidate
  squash commit parked on `merge-gate/{seq}`, required command evaluators
  re-run against it (`JOB_BRANCH=merge-gate/{seq}`), promote-by-CAS (the
  candidate IS the merge). Fast path skips when HEAD unmoved.
- Human tasks (§1.2): work Pass/Fail, evaluator verdicts, escalation
  Retry (same cycle, attempt++, branch as-is) / Resolve / Revoke, pre-work
  escalation Retry re-running re-validation.
- Restart reconciliation (§3.6): orphaned Running tasks (persisted result →
  backend inspect → not-found rules), lost-transition replay, eval-round
  rebuild by evaluator name, in-flight gate superseded, Blocked unblocking.
- Scans (§3.5): task timeout (kill + normal failure paths), one-shot
  job_deadline (`[deadline]` marker keyed on a resolved escalation task).
- Wire contract: submits arrive over `req.work.submit`/`req.eval.submit`
  (bounded-retry-until-ack); the real channel binary passes an MCP-over-stdio
  test against real NATS.
- Launch payload: prompt + channel binary injected (put-archive), MCP config
  entry with NATS_URL/NATS_CREDS, `CHANNEL_ROLE`/`JOB_TASK_ID` env, secrets
  age-decrypted (`for_api` encrypts / `for_dispatcher` decrypts, §8.2).
- **Per-job NATS credentials (§7.4)**: when `nats_account.seed` is present,
  every launch mints a fresh user nkey + JWT scoped to the work/eval
  allow-list, TTL = task_timeout, injected as `NATS_CREDS` (creds-file
  format); the channel binary connects with it. Live-verified end to end
  (`auth/tests/nats_live.rs`): operator-mode server accepts the hand-rolled
  JWTs, permits exactly the channel binary's operations (channel KV
  write, own-job direct get, inbox consumer fetch, work submit), and denies
  cross-project reads/writes and eval submits.
- Dispatcher and `init` connect with `dispatcher.creds` when present (works
  against both open dev servers and operator-mode servers).
- Bootstrap + boot: `chuggernaut init` (idempotent §12.1 — keypairs skip-if-
  exist, `ensure_topology`, `platform.vapid.public`, admin user), `admin
  project create` (§12.2 counter + bare repo + HEAD symref), `chuggernaut
  dispatcher` (env config §12.4 → `dispatcher::run`: git-version check,
  `ping_all`, Core spawn, container handlers).

## Key implementation decisions (beyond the spec text)

- **Actor core**: `core.rs` is one tokio task owning all state; `CoreHandle`
  (mpsc + oneshot) is the only way in; container monitors post
  `Msg::TaskExited`. Reconcile runs inside the actor before the first message.
- **Agent eval ordering**: `submit_eval` marks the task Done *before* the
  container exits (ack-then-exit); the exit event completes the round slot.
  `on_task_exited` therefore must NOT skip Done eval tasks (bug found by test).
- **Merge queue serializes ALL finalization** per project, not just gated
  merges — the fast path re-checks HEAD when dequeued.
- **`Task.evaluator` field added** (types + spec §1.2) so reconciliation and
  the UI can map eval tasks to their `eval:` declaration.
- **`Escalated → Ready` transition added** to §2.1 (pre-work escalation Retry
  passing re-validation) — was implied by §1.2 but missing from the table.
- **Provider-enforced task_timeout** for agent containers (they have no
  dispatcher-visible container id yet, so the §3.5 scan can't kill them).
- **Placement counts live containers by label** (`chuggernaut.managed`) —
  stateless, restart-proof.
- **Channel binary is a hand-rolled newline-JSON-RPC MCP server** (no
  framework; musl static build wants a short dep list).
- **Docker over host installs** for test deps (user preference): NATS test
  harness runs `nats:2-alpine` in Docker with a skip guard.
- **Keygen shells out** to `openssl` (JWT RSA, VAPID P-256) and `ssh-keygen`
  (ed25519 CA) — same standard tooling the deploy host needs anyway; only the
  age key is generated in-process (dispatcher consumes it directly). Private
  key files are chmod 0600.
- **§12.4 defaults live in `CoreConfig`** (`agent_provider_default`/
  `agent_model_default`, both Option so test configs stay terse); the
  declaration→platform fallback is applied at task-record and launch time.
  `DispatcherConfig::from_env` enforces "refuses to start without
  AGENT_PROVIDER_DEFAULT".
- **`REPO_URL_BASE` defaults to `file://{repos_root}`** until the SSH front
  (auth crate) lands — single-node dev works out of the box.
- Crate invariant held: only `store` touches `async-nats`
  (`subscribe_requests` / `read_stream` / `read_subject_after` wrappers).
- **NATS JWTs are hand-rolled** (`auth::nats`): `alg: ed25519-nkey`, claims
  v2, signed with `nkeys` (already in the tree via async-nats) — no Rust
  NATS-jwt library dependency. Verified against a real server, plus signature
  round-trip unit tests.
- **NATS deployment model**: one operator + two accounts (SYS without
  JetStream — the server refuses JS on the system account — and CHUG with
  unlimited JS) in a **memory resolver**; `init` derives `nats-resolver.conf`
  and `dispatcher.creds` from the seeds and re-creates them when missing.
- **§7.4 KV grants map to concrete subjects**: KV read = `$JS.API.DIRECT.GET.
  KV_{bucket}.$KV.{bucket}.{key}` + `STREAM.INFO` (async-nats buckets are
  allow_direct), KV write = `$KV.{bucket}.{key}`, inbox poll = consumer
  create/fetch on `channel-inbox`. Gotcha encoded in a comment: unnamed
  ephemeral consumer creation publishes `CONSUMER.CREATE.{stream}` with *no*
  trailing token, which `.>` alone would not match.
- **SSH authz context rides in the certificate's force-command**
  (`chuggernaut ssh-shell --kind job --principal job:acme/api:42 --access ro`,
  user roles as b64 JSON) — stock sshd enforces it via `TrustedUserCAKeys`;
  no `ExposeAuthInfo` parsing. Per-job certs mint an ephemeral ed25519
  keypair; eval certs are read-only via the `--access` flag.

## Known gaps / accepted debt (grep for TODO)

- `reworks_used` resets to 0 when ExecState is rebuilt after escalation or
  restart (should derive from the event stream).
- Docker containers are never removed (copy_file runs post-exit); prune by
  label is an ops concern until the trait grows a cleanup op.
- Command eval timeout becomes a product fail (exit −1) rather than the
  spec's Failed-infra treatment — arguably better (rework not escalate).
- Command task `TaskResult::Command.output` is always empty (no log capture).
- Docker fleet TCP endpoints have no mTLS; `ping_all` exists but isn't called
  by anything yet (no bin wiring).
- SSH front not deployed: `ssh-shell`/`ssh-authz` subcommands, pre-receive
  hook installation at project creation, and sshd config are still missing —
  job SSH certs can be issued (`SshCa::issue_job_credential`) but aren't
  injected into launches; `REPO_URL` stays `file://`.
- Triage (factory) containers don't get their extra `req.jobs.create`
  permission yet — factories themselves are unimplemented (§13).
- Dispatcher creds never expire (no rotation story yet); per-launch minting
  reconstructs the signer from the seed each call (cheap, stateless).
- Step reporting (`req.step.report`, `steps.*` KV, §4.5) has store/spec
  support but no dispatcher handler — lands with the harness.
- Spec ambiguity noted: §4.2 says `submit_result` "transitions to Evaluation";
  §3.2 completeness contract says exit code decides. Implemented: exit code
  decides; submit only records context. Spec cleanup pending.

## Next steps (recommended order)

1. **First live agent job**: build the musl channel binary, point
   `CHANNEL_BINARY` at it, run a real `work.type: agent` job end to end on
   the booted stack (now with the operator-mode server + scoped creds) —
   shakes out anything the FakeProvider hides.
2. **SSH front deployment** (§5.2): `chuggernaut ssh-shell` (parse
   SSH_ORIGINAL_COMMAND, gate pull, exec git-{upload,receive}-pack with
   principal env) + pre-receive hook calling the §5.2 ref rules (auth lib
   functions exist), hook installation in `admin project create`, job cert
   injection into launches, `REPO_URL` → ssh.
3. **api crate** (§6): axum routes → NATS request-reply, SSE bridge, plus the
   dispatcher-side handlers for the remaining `req.*` families (jobs, graph,
   vcs, tasks/resolve, vars/secrets/knowledge, steps). Auth middleware
   building blocks (`JwtAuthProvider`, `authorize`, cookie helpers) are done.
4. **chuggernaut-ko + §4.4 knowledge injection** (system_prompt assembly).
5. **Inline review harness** (§4.5): loop implementation + `submit_review`
   local mode in the channel binary + `req.step.report` dispatcher handler +
   step events; CodexProvider validation already rejects at release time.
6. **Task factories + ingest (§13)** (incl. wiring
   `triage_container_permissions`), webhooks, then the PWA.

## Test layout (tiers per testing.md)

- Unit: types (duration/schema/steps), state table, graph, provider
  invocation, channel protocol, docker tar/memory parsing.
- Unit additions this session: auth (JWT round-trip/expiry/tamper, §5.2
  principal + pull/push tables, cert issuance via ssh-keygen -L, NATS JWT
  shape + signature verify, §7.4 allow-list contents).
- Tier-2 (skip-guarded on Docker): `store/tests/nats_store.rs`,
  `container/tests/docker_backend.rs`, `chuggernaut-channel/tests/stdio.rs`,
  `cli/tests/init_admin.rs`, `auth/tests/nats_live.rs` (operator-mode server),
  and `dispatcher/tests/{lifecycle,execution,gate_and_human,recovery,nats_submit}.rs`.
- Run everything: `cargo test --workspace` (from `v2/`; needs Docker up).

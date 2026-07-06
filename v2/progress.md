# Chuggernaut v2 — Implementation Progress

Companion to `spec.md` (normative), `design.md` (rationale), `crates.md`
(crate map). This file tracks what is actually built, where it deviates from
or extends the spec, and what comes next. Update it at the end of each
implementation session.

**As of:** 2026-07-05 · commit `4cccd11` · 64 tests passing, clippy clean.

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
| `auth` | 🔴 stub | `NATS_TOKEN` injected empty; SSH CA absent; repos unauthenticated (tests use `file://` paths) |
| `api` | 🔴 stub | No HTTP surface; only container-facing NATS subjects handled |
| `cli` / `chuggernaut` bin | 🔴 stub | No init, no subcommand wiring — the system is not yet runnable outside tests |
| `webhooks` | 🔴 stub | |
| `test-utils` | ✅ done | `FakeBackend`, `FakeProvider` (async run hooks mirroring submit-ack-then-exit ordering), `NatsTestServer` (Docker `nats:2-alpine`, skip-guarded), `TempRepo`/`WorkClone`/`clone_branch_from` |

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
  entry with NATS_URL/NATS_TOKEN, `CHANNEL_ROLE`/`JOB_TASK_ID` env, secrets
  age-decrypted (`for_api` encrypts / `for_dispatcher` decrypts, §8.2).

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
- Crate invariant held: only `store` touches `async-nats`
  (`subscribe_requests` / `read_stream` / `read_subject_after` wrappers).

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
- Step reporting (`req.step.report`, `steps.*` KV, §4.5) has store/spec
  support but no dispatcher handler — lands with the harness.
- Spec ambiguity noted: §4.2 says `submit_result` "transitions to Evaluation";
  §3.2 completeness contract says exit code decides. Implemented: exit code
  decides; submit only records context. Spec cleanup pending.

## Next steps (recommended order)

1. **Bin wiring + init** (`chuggernaut` bin, `cli` crate): `chuggernaut init`
   (keygen incl. `generate_age_keypair`, `ensure_topology`, admin user),
   `chuggernaut dispatcher` (Core::new + spawn + `spawn_container_handlers` +
   `ping_all`, config from env per §12.4). Makes the system runnable against
   real Docker — first live agent job.
2. **auth crate** (§7): per-job NATS JWT minting + allow-lists, SSH CA and
   cert issuance, sshd authz hook; replaces the empty `NATS_TOKEN` and
   `file://` repo URLs.
3. **api crate** (§6): axum routes → NATS request-reply, SSE bridge, plus the
   dispatcher-side handlers for the remaining `req.*` families (jobs, graph,
   vcs, tasks/resolve, vars/secrets/knowledge, steps).
4. **chuggernaut-ko + §4.4 knowledge injection** (system_prompt assembly).
5. **Inline review harness** (§4.5): loop implementation + `submit_review`
   local mode in the channel binary + `req.step.report` dispatcher handler +
   step events; CodexProvider validation already rejects at release time.
6. **Task factories + ingest (§13)**, webhooks, then the PWA.

## Test layout (tiers per testing.md)

- Unit: types (duration/schema/steps), state table, graph, provider
  invocation, channel protocol, docker tar/memory parsing.
- Tier-2 (skip-guarded on Docker): `store/tests/nats_store.rs`,
  `container/tests/docker_backend.rs`, `chuggernaut-channel/tests/stdio.rs`,
  and `dispatcher/tests/{lifecycle,execution,gate_and_human,recovery,nats_submit}.rs`.
- Run everything: `cargo test --workspace` (from `v2/`; needs Docker up).

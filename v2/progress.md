# Chuggernaut v2 — Implementation Progress

Companion to `spec.md` (normative), `design.md` (rationale), `crates.md`
(crate map). This file tracks what is actually built, where it deviates from
or extends the spec, and what comes next. Update it at the end of each
implementation session.

**As of:** 2026-07-18 (session 9) · all workspace suites passing, clippy clean.
**FIRST LIVE AGENT JOB COMPLETED** (the "not yet exercised" milestone from
session 8): job #3 in acme/demo (`feature-impl`, ticket "Add fortune.txt")
ran create → release → Haiku work agent (committed fortune.txt, submitted
result) → Haiku review agent (`submit_eval` pass) → squash-merge to main —
in under a minute. Both task records carry `model: claude-haiku-4-5`;
measured token usage present; transcripts + logs captured and served for
both tasks; the per-job SSE stream replayed the full event trail. Gap found:
Haiku skipped the optional `update_status` calls — all work/review prompts
now make channel narration a required step (templates + embedded Code
starter updated, demo reseeded). UI: release button renamed **▶ run**.
**This session — lifecycle generalization** (`design-lifecycle.md`, new): jobs
are no longer necessarily code changes. `finalize: merge | none` on the job
type — `none` (deploys, reports) goes straight to Done after eval-pass, no
merge queue/gate, scratch branch deleted unmerged. Evaluators gained an
`abort` verdict ("not satisfiable by rework"): a required agent/human
evaluator's abort skips the remaining rework budget and escalates with its
findings (`eval_abort`); command evaluators can't abort. Unexpected
finalization errors (git plumbing, repo IO — not Conflict) now escalate
(`finalize_failed`) and the merge queue advances instead of wedging the job in
Evaluation. Job creation accepts additive per-job evaluators (`Job.eval`,
layered onto the type's list; name collisions and field rules validated at
release — the type's evaluators are a floor, never overridable). Spec updated
(§1.1, §1.2, §3.2 step 12/13, §3.3, §4.2, §6.2); five new tier-2 tests in
`dispatcher/tests/execution.rs`. Triage agents (design's outlet 2) deferred
with factories (§13). **Channel binary changed** (submit_eval schema gained
`abort`) — `deploy/dev/build.sh` re-run this session, `out/` is current.
**Criteria surface**: `req.jobs.criteria.{o}.{p}.{seq}` + `GET
.../jobs/{seq}/criteria` return the resolved evaluator list (type + defaults
+ per-job extras, source-annotated, at the ref execution uses) plus the
finalize mode; job detail page renders it, the create form composes per-job
evaluators (command/agent/human rows → `eval[]`), and the inbox Fail action
on human evaluator tasks grew the abort checkbox. Eval **packs**
(`evals/{name}.yaml` + `use:` references, GHA-style composition) are
proposed, not built — see design-lifecycle.md "Proposed: composable
evaluation criteria". **Schema/authoring tooling**: `types` grew a `schema`
feature (schemars 1.x derives on the §1.1 YAML types); `chuggernaut schema
job-type|defaults` emits JSON Schema (canonical copies in `schemas/`, guarded
by a drift test in `cli`); `chuggernaut validate <files>` runs parse + field
rules offline with sibling `_defaults.yaml` merged. Editor story: commit the
schema into a project repo + yaml-language-server modeline (spec §1.1
"Authoring Support"). Decision: own schema kept over adopting
GHA/Tekton/CWL syntax — their semantics don't cover agent/human evaluators,
required/advisory, abort, or finalize; we borrow GHA *vocabulary* only.
**Job type library**: `req.jobtypes.get` + `GET .../job-types/{name}` (raw
YAML + parsed-with-defaults view) and a Library tab in the UI (Jobs |
Library per project; type names in the jobs table deep-link to it).
Vocabulary settled in design-lifecycle.md: jobs are graph nodes, **both work
and evaluation run tasks**; UI copy now says "work task" / "evaluation
tasks". Proposal extended: reusable **task definitions** (`tasks/{name}.yaml`
+ `use:` from both `work:` and `eval:`) generalize eval packs.
**Job title/description (ticket identity)**: `Job.title`/`Job.description`
(serde-defaulted), set at creation (API/UI), injected as the **§4.3 Job
Brief** block into the work agent prompt, every agent evaluator prompt
(evaluators judge against the same brief), and Human task prompts. UI:
create form gained Title + Description (textarea), jobs table shows titles,
job detail shows the brief; "Inputs" relabeled **Dependencies** (wire name
stays `inputs`). Dev templates now prefer the brief over the FEATURE.md
convention (FEATURE.md kept as fallback; demo repo reseeded).
**Named inputs replaced by plain dependencies** (breaking, spec-wide):
`Job.deps: Vec<u64>` supersedes `inputs: HashMap<name, id>`; `JobType.inputs`
/ `InputDecl` deleted (dependencies are per-instance, chosen at creation —
the type no longer declares slots). Wiring validation simplified to exists /
non-Revoked / no self-edge / no dup / no cycle. Old KV records still parse
(unknown `inputs` field ignored; deps default empty). UI: searchable
dependency picker in the create form (filter by #/title/type; a job being
created can never close a cycle since nothing depends on it yet, so all
non-revoked jobs are valid targets), deps shown as #N links; jobs referred to
as **#N** GitHub-style throughout. Schemas regenerated. Named-role data flow
("the result of my `spec` input") was the one capability lost — if needed
later it returns as an optional label on a dep edge, not as type-declared
slots.
**Project creation via UI + Code starter**: `req.projects.create` (dispatcher:
repo + pre-receive hook + seeded first commit + counter; `HOOK_BIN` env for
the sshd-container binary path; `RepoManager::seed_files` commits via a temp
worktree) → `POST /api/v1/projects` (platform admins only) → Home-page form.
The **Code starter template is embedded in the binary**
(`dispatcher/templates/code/`, guarded by a template-validity unit test):
`jobs/code.yaml` (agent implements the ticket, second agent reviews),
`tasks/ci.sh` + `tasks/review-code.md`, `prompts/work/code.md`, README.
**Reusable tasks DECIDED as files, not schema** (design-lifecycle.md
"Resolution"): command task = script (`tasks/*.sh`), agent task = markdown
(`tasks/*.md`); `run:`/`prompt:` already reference them by path; git is the
registry. The `use:`/TaskDef proposal is dropped. NOTE: owner in
`{owner}/{name}` is a bare namespace string — no org entity, no user↔owner
link; users via `admin user create`, roles via the user record only. Org
model is open design work.
**Secrets scoped under `work:`** (breaking YAML): top-level `secrets:` moved
to `work.secrets` — the schema now *shows* the §4.1 scoping (work container
only; evaluators declare their own) instead of stating it in prose.
`container_env` takes an explicit secrets slice (the eval-side clone-and-
override hack is gone); human work disallows `work.secrets`. Templates,
demo repo, schemas, spec updated. Wart FIXED same session: **platform agent
credentials** — every secret in the reserved `global/agents` scope is
auto-injected into every *agent* container (work agents + agent evaluators;
never command containers), declared secrets winning on collision
(`inject_platform_agent_secrets`, under test). `admin secret copy` moves
stored ciphertext between scopes without decrypting (used to promote the
live token). All templates and the demo repo no longer mention
CLAUDE_CODE_OAUTH_TOKEN; dev README step 6 sets `global/agents` once.
**Wrap-up is the job type's third section** (breaking YAML): the `finalize:`
scalar became a `wrap_up: { type: merge | none }` block (extensible for e.g.
tag refs later); the job type now reads work → eval → wrap_up. Criteria and
library payloads say `wrap_up`; the Rust enum stays `Finalize` internally.
Library cards restructured: display-name title with the slug underneath (no
wrap-up badge or commit hash in the title), sections "1 · Work / 2 ·
Evaluation / 3 · Wrap-up", ref demoted to the raw-YAML fold-out line; job
detail shows wrap-up as a meta row. Demo repo reseeded (code-review.yaml).
**Per-job retros proposed** (design-lifecycle.md "Proposed: per-job retros"):
a factory-triggered, batched retrobot job (wrap_up: none) reads completed
jobs' archives (brief, transcripts, channel history, findings, outcomes) and
emits *suggestions* — tag/prompt/evaluator/budget/doc changes as concrete
diffs — that a human accepts (commit or follow-up Code job) or dismisses.
Never auto-committed: retro output writes to the files that steer every
future agent. Wants factories (§13) and a job-archive bundle accessor.
**Backups** (`deploy/backup.sh`, run + restore-verified live): per-repo
`git bundle --all` (verified; also the answer to "remote repos?" — the ask
became reliability, not remote-authoritative execution; mirror-mode push-back
remains undone by choice), consistent JetStream snapshot via
`nats account backup` (nats-box container on the compose network with
dispatcher.creds), and the keys dir, tarred with a RESTORE.md. Restore path:
keys → nats restore → `git clone --mirror <bundle>` + re-set allowFilter +
hook → dispatcher reconciles (§3.6). Offsite shipping is the operator's job;
backup tarballs contain the keys — guard them accordingly.
**Machine access + /chug skill**: the API accepts `Authorization: Bearer
<jwt>` (same verify path as the session cookie); `admin user token --email
… --ttl 720h` mints one. A dedicated machine user `claude@dev.local` exists
with its token at `deploy/dev/data/keys/claude.token` (0600), and the
project-level `/chug` skill (.claude/skills/chug) was rewritten for the v2
API — Claude auths **as that user** (attributed, role-bounded), never with
dispatcher.creds. Spec note: token carries roles at mint time (no
revocation list yet — short TTLs are the mitigation).
**Create-job page + type display names**: the create form moved to
`/p/{o}/{p}/jobs/new` (navigates to the new job's detail on success; Library
cards link to it with `?type=` prefill). `JobType` gained
`display_name`/`description`; `req.jobtypes.list` now returns
`[{name, display_name, description}]` (unparseable files list stem-only);
the type picker is an autocomplete showing "Display — description",
defaulting to the Feature type. Title/description placeholders removed.
**Knowledge tags are repo-versioned** (decision): a tag's meaning lives in
`tags/{tag}.md` on the default branch. `req.tags.list` + `GET .../tags`
enumerate the stems; the create form shows them as toggleable chips (plus
free-text extras with datalist suggestions). Demo repo seeded with
backend/frontend/style tags. §4.4 upfront injection is now
**wired**: union of type `knowledge:` + job tags → `tags/{tag}.md` at
`base_ref` → `## Project Knowledge` system-prompt block, work agents only
(missing files skipped; under test). chuggernaut-ko/global buckets stay
deferred for cross-project knowledge. A **Tags tab** in the UI browses each
tag's markdown. Session 8: job artifacts (transcripts +
logs captured, encrypted, served; channel posts through the dispatcher;
measured token usage).
Session 8 detail: every agent run's Claude session transcript
and every container's logs are now captured after exit, gzipped + age-encrypted
into a NATS object store, and served to the UI; channel `update_status`/`reply`
posts route through the dispatcher (single writer restored) and land as durable
event history instead of an overwritten 7-day KV entry; token usage is measured
from the CLI's JSON result; `WorkSubmission` survives a restart. Transcript path
verified inside the real agent image. **Not yet exercised**: a live agent job
end to end (see Next steps 1). Session 7: clone-cost narrowing
(`--single-branch --filter=blob:none`) + protocol-v2 through the SSH front.
Prior: the platform runs live — `deploy/dev/` boots the full single-node stack,
and a real command-work job went create → release → clone → push `job/2` →
command eval → squash-merge to `main` in ~10s over the HTTP API.

---

## Status by crate

| Crate | Status | Notes |
|---|---|---|
| `types` | ✅ done | Full domain model incl. `ReviewSpec`, `TaskPhase::MergeGate`, `StepRecord`, `ProjectDefaults`, `Task.evaluator`, shared duration parser |
| `store` | ✅ done (core) | `NatsStore` + typed accessors (jobs/tasks/steps/counters/rdeps), topology creation, bounded-retry request, `subscribe_requests`, `read_subject_after`, `read_stream`, `AgeSecretStore`, `ArtifactStore` (object store + gzip + age) | 
| `vcs` | ✅ done | Pre-existing + `create_squash_candidate` / `advance_default` (merge gate) |
| `container` | ✅ done | `DockerBackend` over bollard 0.19: fleet nodes, label-counted slot placement, put-archive injection, `logs()` capture, `{node}/{id}` routing. `k8s.rs` still stub |
| `dispatcher` | ✅ done (orchestration) | Everything in spec Parts 2–3: lifecycle, execution, rework, merge gate, human tasks, reconciliation, scans, NATS submit handlers |
| `agent` | 🟡 partial | `ClaudeProvider` done (invocation, prompt injection, MCP config, self-enforced timeout, `--session-id` + `--output-format json`, container id + measured usage out). `CodexProvider` stub. KO/system-prompt injection not wired |
| `chuggernaut-channel` | 🟡 partial | Working stdio MCP server: update_status/reply (now posted over `req.channel.*`, no KV writes)/channel_check + role-gated submit_result/submit_eval. Missing: `submit_review` local mode (§4.5), `create_job` (factories §13), push-mode inbox |
| `chuggernaut-harness` | 🔴 scaffold | Config types + loop protocol as TODO (§4.5) |
| `chuggernaut-ko` | 🔴 stub | |
| `auth` | ✅ done | §7 complete: RS256 platform JWTs + cookie + `JwtAuthProvider`, SSH CA signing (user 24h / per-job ephemeral keypair+cert, authz context in force-command, `git` login principal), §5.2 parsing + pull/push-entry/per-ref authz pure functions, hand-rolled NATS JWTs (operator/account/user) + §7.4 allow-lists + `.creds`/resolver-config rendering |
| `api` | 🟡 core done | Axum bridge: login/logout/me (argon2 vs users KV → JWT cookie), jobs CRUD + release/revoke, graph, tasks pending/list/resolve, diff, SSE (project + per-job, Last-Event-ID replay), **artifacts list + get (decrypt + stream)**, §7.5 authz per route, §6.5 error envelope, static SPA serving. Missing: vars/secrets/knowledge/usage/steps/channel-status/vcs tree-blob-log routes, ssh-cert, push, ingest |
| `web` (not a crate) | 🟡 core done | `v2/web`: Vite + React 19 + TS, no framework state — SSE-triggered refetch. Login, project chooser, job table + create/release/revoke, operator inbox (Pass/Fail + escalation Retry/Resolve/Revoke keyed on job state), job detail (tasks, colored unified diff, live event log, **per-task transcript/log viewer + channel timeline**). PWA manifest/service worker + React Flow graph view still pending |
| `cli` / `chuggernaut` bin | 🟡 done (core) | `init` (§12.1: keygen incl. NATS operator/SYS/CHUG seeds + derived `nats-resolver.conf` and `dispatcher.creds`, topology, VAPID pub in KV, admin user), `admin user create/list/delete`, `admin project create/list` (§12.2, installs the pre-receive hook), `dispatcher`, `api` (env: NATS_URL/KEYS_DIR/BIND_ADDR/UI_DIST/SESSION_TTL), `ssh-shell` (forced command: parse SSH_ORIGINAL_COMMAND, gate entry, exec git service with identity env), `ssh-authz` (pre-receive hook body). Missing: ingest tokens, role set/unset, key rotation, seed |
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
  JWTs, permits exactly the channel binary's operations (own-job direct get,
  channel entry *read*, inbox consumer fetch, work submit, `req.channel.*`
  posts), and denies cross-project reads, eval submits, and any direct write
  to `channels` KV.
- Dispatcher and `init` connect with `dispatcher.creds` when present (works
  against both open dev servers and operator-mode servers).
- **SSH front (§5.2)**: `admin project create` installs a pre-receive hook;
  certificates carry `chuggernaut ssh-shell --principal ... --access ...` as
  their forced command; ssh-shell parses `SSH_ORIGINAL_COMMAND`, gates
  pull/push entry, and execs `git-{upload,receive}-pack` with the identity in
  `CHUGGERNAUT_*` env; the hook enforces the per-ref table. Verified by real
  `git clone`/`push` through git's `ext::` transport (protocol-identical to
  sshd, no daemon needed): job certs push only `job/{seq}` in their own
  project, ro certs can't push at all, Viewer pulls but can't push, Member
  pushes job branches but not `main`/tags, dispatcher pushes protected refs,
  and local `file://` access passes through untouched.
- When `ssh_ca` is present and `REPO_URL_BASE` is `ssh://`, every launch
  (work agent/command, eval agent/command) injects a freshly issued job cert
  (`/chuggernaut/ssh/id{,-cert.pub}`, rw work / ro eval, TTL = task_timeout)
  plus a static `GIT_SSH_COMMAND` pointing at it.
- Bootstrap + boot: `chuggernaut init` (idempotent §12.1 — keypairs skip-if-
  exist, `ensure_topology`, `platform.vapid.public`, admin user), `admin
  project create` (§12.2 counter + bare repo + HEAD symref), `chuggernaut
  dispatcher` (env config §12.4 → `dispatcher::run`: git-version check,
  `ping_all`, Core spawn, container + API handlers).
- **HTTP surface (§6, core slice)**: `api/tests/http_bridge.rs` drives the
  full loop over HTTP against a real NATS + core — 401s, login (argon2 vs
  users KV) → JWT cookie → `/auth/me`, create job (201) → 422 release with
  the §6.5 `errors` envelope for a bad type → release → human work task in
  `/tasks/pending` → resolve → human eval task → resolve → Done; jobs/graph/
  diff/task-log reads en route; SSE stream replays the event trail from seq 0
  with `id:` carrying the stream sequence.
- **Web UI (`v2/web`)**: builds clean under strict TS (`npm run build`);
  dev mode proxies to `:8080` (`npm run dev`), production is served by
  `chuggernaut api` via `UI_DIST=web/dist`. SSE is the refresh signal —
  every screen refetches on any project event, no polling.
- **Live dev stack (`v2/deploy/dev/`, session 6)**: `compose.yaml` (operator-
  mode NATS with the init-derived resolver conf; sshd container built by
  `Dockerfile.ssh` with the linux `chuggernaut` binary as forced command +
  hook), `Dockerfile.agent` (node:22 + claude CLI + git, `IS_SANDBOX=1`),
  `build.sh` (also extracts the linux `chuggernaut-channel` to `out/` for
  `CHANNEL_BINARY`), README with the full bootstrap. Verified live: hand-
  issued job cert clone/push matrix through the real sshd, then the smoke
  job loop above. Admin CLI grew `secret set`/`var set` (age-encrypt via
  `age_public.key`), `--keys-dir` (connects with `dispatcher.creds`), and
  `project create --hook-bin`; dispatcher grew `NATS_URL_CONTAINER`.

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
- **`REPO_URL_BASE` defaults to `file://{repos_root}`** — single-node dev
  works out of the box. The SSH front activates only when it's set to
  `ssh://...` *and* `ssh_ca` is present (cert injection is keyed on both).
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
- **Every cert carries a second principal `git`** (`SSH_LOGIN_PRINCIPAL`):
  sshd only accepts certs whose principals include the login account, so all
  git traffic logs in as `git` (its `AuthorizedPrincipalsFile` lists exactly
  that) while the semantic §5.2 principal travels alongside.
- **Entry gate vs per-ref split**: ssh-shell can only gate at repo level
  (receive-pack learns refs after the pack arrives), so `authorize_push_entry`
  guards entry and the pre-receive hook applies `authorize_ref_push` per ref.
  **No identity env → the hook allows** — local/`file://` access means you're
  already on the host (the dispatcher's own path); sshd traffic always has
  the env because ssh-shell is the unavoidable forced command.
- **Hook body bakes `current_exe`** at `admin project create` time — assumes
  the admin CLI runs on the SSH host with the same artifact path (true for
  the single-node compose deploy).
- **API reply envelope over NATS**: success is the resource JSON verbatim;
  failure is `{"error":{"status":u16,"message",..,"errors":[..]?}}` — the
  api crate maps it straight onto §6.5 without a shared error type (zero
  coupling between the crates; the shape is checked by the bridge test).
- **Job mutations require Member+** (create/release/revoke ride the §7.5
  "complete/fail a task" row; the spec table has no explicit row for them).
- **SSE bridge is one ephemeral pull consumer per connection**
  (`store::subscribe_stream`, filter `job.events.{owner}.{project}.>`,
  `ByStartSequence(last_event_id+1)`); EventSource reconnect + replay comes
  free from the NATS stream.
- **UI state layer is "refetch on SSE event"** — no client-side store to
  drift; right for test-project scale, revisit if event volume hurts.
- **Escalation vs Pass/Fail resolution in the UI keys on `job.state ==
  Escalated`** (mirrors `exec.rs` resolve validation), not on any task field.
- **Pre-receive hook carries the no-identity fast path in the script itself**
  (`[ -z "$CHUGGERNAUT_PRINCIPAL" ] && exit 0`) — found live: the baked
  binary path exists only inside the sshd container, so local/`file://`
  pushes (the dispatcher's own) must not depend on it.
- **Claude invocation includes `--dangerously-skip-permissions`** (headless
  agents can't answer prompts; the agent image sets `IS_SANDBOX=1` so the
  CLI accepts it as root). Container-facing NATS URL is a separate config
  knob (`NATS_URL_CONTAINER` → `CoreConfig.nats_url`) because on Docker
  Desktop containers reach the host at `host.docker.internal`, not
  `localhost`.
- **Task clones are narrowed, not cached** (`container::bootstrap_cmd`):
  `--single-branch --filter=blob:none`. Every task in a job re-clones, so the
  clone flags are the whole cost story. Measured on a 3.4k-commit repo with 20
  in-flight job branches carrying unmerged work: **1.67s/9.6M → 0.67s/5.3M**.
  `--single-branch` is the bulk of it — without it each task also drags in
  every *concurrent* job's work (with no in-flight jobs the win is ~0, which is
  why the naive benchmark misleads). `--filter=blob:none` was chosen over
  `--depth 1` to keep `git log`/`blame` as agent context. A node-local
  reference cache was rejected: per-node volumes, `flock` around concurrent
  fetches, and a `git gc`-corrupts-alternates hazard, for a second-order win.
- **Partial clone has two server-side prerequisites, both fail quietly**:
  `uploadpack.allowFilter` on the bare repo (set by `create_project`; older
  repos need a one-time backfill) or the filter is ignored and the full history
  ships; and **git protocol v2 through the SSH front** (`AcceptEnv
  GIT_PROTOCOL` in `sshd_config`) or upload-pack runs v0, refuses the promisor
  remote's follow-up fetch for unadvertised blobs, and every task container
  clones "successfully" into an **empty workspace**. git adds the client half
  itself. `file://` clones get v2 automatically, so this reproduces only
  through the front — the `ext::` suite can't cover it (ext never propagates
  `GIT_PROTOCOL`), hence `dev_sshd_accepts_git_protocol_v2` guards the config
  and the live path was verified by hand with a minted job cert.

- **Job artifacts are captured, encrypted, and served** (`store::artifacts`,
  `dispatcher::harvest`). Every agent run now yields its Claude session
  transcript and container logs; command containers yield logs. Stored in a
  JetStream **object store** (`artifacts`, 90d) — not KV — because a transcript
  routinely exceeds the 1MB `max_payload` a req/reply reply cannot carry
  (tier-2 test round-trips a 3MB incompressible blob). Pipeline is gzip → age →
  put. **Spec deviations to amend** (both intentional): §5.1's "No separate
  artifact store for v1" — we added one, but it is NATS-internal, not the
  deferred S3/Minio, and §5.1's clause is about artifact *passing between jobs*,
  not observability. §10.2's "the age private key is dispatcher-only" — still
  true of the *secrets* key; a second `age_artifacts` key is API-readable
  (next bullet). spec.md should name both keys.
- **Artifacts use their own age key** (`age_artifacts.key`), *not* the secrets
  key. §10.2 keeps the secrets identity dispatcher-only, but the API must
  decrypt to display a transcript, and proxying blobs through the dispatcher
  would reintroduce the `max_payload` cap the object store exists to dodge.
  Different key, different trust boundary: it guards artifacts at rest from
  anyone holding NATS creds or a disk backup, not from the API.
- **The transcript is opaque; the CLI's JSON result is the contract.** Anthropic
  documents the `.jsonl` format as internal and version-unstable, so nothing
  parses it. `--session-id` (a dispatcher-minted UUID persisted on the `Task`)
  makes the filename deterministic; `--output-format json` supplies session id,
  cost, and real `usage`, which now supersedes the agent's self-report on both
  work and eval paths. `CLAUDE_CONFIG_DIR=/chuggernaut/claude` decouples the
  path from `HOME` (only `/root` because `Dockerfile.agent` sets no `USER`).
  **Measured in the real agent image**, contradicting the published docs: the
  cwd slug keeps its leading dash, so `/workspace` → `-workspace`.
- **`AgentOutput` carries the container id, including on timeout.** It used to
  be `{ exit_code }` only, with `ClaudeProvider::run` dropping the id — which is
  why agent tasks stored `container_id: None` and transcripts, though still
  physically on the node, were unaddressable. The timeout branch now returns
  `Ok(-1)` rather than `Err` (exit-code-identical: `exec.rs` already mapped
  `Err` → -1) so a timed-out run — the one most worth reading — is harvested.
- **Channel posts go through the dispatcher** (`req.channel.update|reply` →
  `dispatcher::channel`). The container used to write `channels` KV itself: a
  second writer, invisible to the dispatcher, last-write-wins, in a bucket with
  a 7-day TTL — so an agent's progress narrative was destroyed as it was
  written. The KV entry remains as the latest-value cache §6.2's
  `GET .../status` reads; the **event stream is the history** (90d), and reaches
  the UI over existing SSE for free. Containers now write **no KV bucket at
  all** (`kv_write` deleted as dead code); live-verified against an
  operator-mode server that the direct write is denied.
- **`submit_result` is persisted on arrival, not just cached.** It lands while
  the container still runs (§4.2 ack-then-exit), so a restart in that window
  lost the summary — and the summary is the squash commit's message body.
  `ensure_exec_state` now rehydrates it from the task log.

## Known gaps / accepted debt (grep for TODO)

- **Docker Desktop VirtioFS + hardlinked loose objects**: host-side
  `git clone` of a bare repo hardlinks objects; the sshd container can then
  read them as corrupt ("repository corruption on the remote side") while
  host fsck is clean. Hit live on acme/demo (session 9). Manual seeds must
  use `--no-hardlinks`; recovery is `repack -ad + prune-packed` (now in the
  dev README). `RepoManager` itself never local-clones (worktree seeding),
  so only hand seeding is exposed.

- `reworks_used` resets to 0 when ExecState is rebuilt after escalation or
  restart (should derive from the event stream).
- **Session *resume* is not wired**, only archival. The pieces are in place
  (`--session-id`, `CLAUDE_CONFIG_DIR`, the stored transcript); resume needs
  re-injecting the `.jsonl` and adding `--resume`. Confirmed cheap: the
  transcript alone suffices (no registry file), and lookup only requires the
  same cwd, which `bootstrap_cmd` pins to `/workspace`. Pairs with §4.5, which
  already needs `--continue`. Note a stored transcript may not resume on a
  future CLI version — the format is explicitly internal; archival and analysis
  are unaffected.
- `TaskResult::Command.output` is still `String::new()` **by choice**: §10.2
  forbids plaintext secrets in task records, and logs are exactly where they
  leak. The full log is an encrypted artifact instead; the field is vestigial
  and should be dropped from the type.
- **Rebuild `deploy/dev/out/chuggernaut-channel` (via `deploy/dev/build.sh`)
  whenever the channel binary changes** — it is injected into agent containers,
  so a stale copy runs old code. Since this session it posts over
  `req.channel.*` instead of writing KV, and the dispatcher no longer grants the
  KV write, so a stale binary's `update_status`/`reply` calls fail outright.
  (Regenerated at the end of this session.)
- Docker containers are never removed (copy_file runs post-exit); prune by
  label is an ops concern until the trait grows a cleanup op.
- Command eval timeout becomes a product fail (exit −1) rather than the
  spec's Failed-infra treatment — arguably better (rework not escalate).
- Docker fleet TCP endpoints have no mTLS; `ping_all` exists but isn't called
  by anything yet (no bin wiring).
- **`REPO_URL_BASE` defaults to `file://{repos_root}`, which cannot work for
  containers**: `DockerBackend` sets no mounts at all, so that path never
  exists inside a task container and the clone fails. Only the dispatcher's own
  host-side `RepoManager` and host-side test/seed clones use `file://`. The
  live stack works solely because `deploy/dev/README.md` sets
  `ssh://git@host.docker.internal:2222`. The config comment still says
  "until the SSH front lands" — it landed; the default should become a required
  var (or the ssh form).
- **Agent tasks silently ignore `resources:`** — `claude.rs` hardcodes
  `cpu_limit: None, memory_limit: None` while both command paths pass
  `job_type.resources` through. Declared CPU/memory limits are dropped for all
  agent work (related: agent containers also have no dispatcher-visible
  container id, hence the provider-enforced timeout above).
- `deploy/dev/Dockerfile.ssh` recompiles the whole release workspace whenever
  any `crates/` file changes (`COPY crates ./crates` precedes `cargo build`),
  so a one-line `sshd_config` change costs a full rebuild. Splitting the
  dep-manifest copy from the source copy would restore layer caching.
- sshd itself is configuration, not code (crates.md): `TrustedUserCAKeys`,
  a `git` account with `AuthorizedPrincipalsFile` containing `git`, and the
  chuggernaut binary on the host. Container host-key verification is
  disabled (`StrictHostKeyChecking=no`) — mTLS/known-hosts story deferred.
- User SSH cert issuance flow (§7.3 `POST /auth/ssh-cert` →
  `req.ssh.sign-user-cert`) needs the api crate; `SshCa::sign_user_cert` is
  ready.
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

Priority (user): iterate from the UI on a test project.

1. **First live agent job — the one thing not yet exercised end to end.** The
   artifacts/channel/logs/usage work is unit- and tier-2-tested and the
   transcript path is verified inside the real agent image, but no live agent
   job has run since. To run: re-run `chug init` (generates `age_artifacts.key`),
   re-run `deploy/dev/build.sh` (channel binary now posts over `req.channel.*`),
   boot the stack, set `CLAUDE_CODE_OAUTH_TOKEN`, release job 1 (`hello`).
   Then confirm on the job detail page: transcript viewer, per-task logs, the
   channel timeline, and a populated usage figure. Model default is Haiku
   (user quota).
2. **Remaining §6.2 routes as they bite**: `GET .../jobs/{seq}/status`
   (ChannelStatus — type exists, no route), `.../usage` + `.../jobs/{seq}/usage`
   (define `UsageSummary`; aggregate `TokenUsage`), vcs tree/blob/log,
   vars/secrets/knowledge, steps, `POST /auth/ssh-cert`; React Flow graph,
   react-diff-view, PWA manifest + push.
3. **Session resume** (additive to the archival just landed): re-inject the
   stored `.jsonl` into a fresh container and add `--resume`. Cheap per the CLI
   findings; pairs with the §4.5 harness, which already needs `--continue`.
4. **Operator → agent channel is one-way**: `channel.inbox.*` has no production
   publisher, so `channel_check` always reads empty. Needs `req.channel.send` +
   a route + push perms before operators can message a running agent.
5. **chuggernaut-ko + §4.4 knowledge injection** (system_prompt assembly).
6. **Inline review harness** (§4.5): loop + `submit_review` local mode +
   `req.step.report` handler + step events; CodexProvider already release-gated.
7. **Task factories + ingest (§13)** (incl. wiring
   `triage_container_permissions`), webhooks.

## Test layout (tiers per testing.md)

- Unit: types (duration/schema/steps), state table, graph, provider
  invocation, channel protocol, docker tar/memory parsing.
- Unit, auth suite: JWT round-trip/expiry/tamper, §5.2 git-command parsing +
  principal + pull/push tables, cert issuance via ssh-keygen -L, NATS JWT
  shape + signature verify, §7.4 allow-list contents.
- Tier-2 (skip-guarded on Docker): `store/tests/nats_store.rs` (incl. a >1MB
  artifact blob — the case req/reply cannot carry),
  `container/tests/docker_backend.rs`, `chuggernaut-channel/tests/stdio.rs`,
  `cli/tests/init_admin.rs`, `auth/tests/nats_live.rs` (operator-mode server),
  `api/tests/http_bridge.rs` (tower-driven router against real NATS + core),
  and `dispatcher/tests/{lifecycle,execution,gate_and_human,recovery,nats_submit}.rs`.
- `chuggernaut/tests/ssh_front.rs` (git + the compiled binary, no Docker):
  the §5.2 matrix over git's `ext::` transport.
- Run everything: `cargo test --workspace` (from `v2/`; needs Docker up).

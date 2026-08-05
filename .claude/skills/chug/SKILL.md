---
name: chug
description: "Query and command the running Chuggernaut v2 platform. Use when the user says /chug followed by a question or command about projects, jobs, job types, tasks, or the running system."
argument-hint: "<question or command about the running system>"
allowed-tools: [Bash, Read, Grep, Glob]
---

You are interacting with a **live Chuggernaut v2 platform** on behalf of the
user. Translate their input into API calls, execute them, and present the
results clearly.

## Target — prod by default

The default target is the **prod deployment on gumbo-mini-0** (reached over
the tailnet via Tailscale Serve):

```sh
BASE=https://gumbo-mini-0.tail20c474.ts.net
TOK=$(cat ~/.config/chuggernaut/gumbo.token)
curl -s -H "Authorization: Bearer $TOK" $BASE/api/v1/projects | jq
```

The dogfood project — chuggernaut developing itself — is
**`kasofsk/chuggernaut`**. It is a **classic, platform-owned** project: the
platform's bare repo owns `main`, agents merge job branches straight into it,
and GitHub is a **read-only mirror**. To ship a change, **create + release a
`deploy` job** (see "Shipping this repo" below). The operator checkout's
`origin` is the platform's SSH front
(`ssh://git@100.116.243.42:2222/kasofsk/chuggernaut.git`; setup in
`.claude/skills/chug-ops/SKILL.md`); GitHub is a read-only mirror.

If the user explicitly says **dev** (the local stack from `deploy/dev`):
`BASE=http://localhost:8081`, token at `deploy/dev/data/keys/claude.token`. <!-- runtime -->

## Auth — you act as a user, not the platform

Every action is attributed to the token's user and bounded by its roles —
never use `dispatcher.creds` (the platform identity) for API work. On 401 the
token has expired; re-mint:

```sh
# prod (minted on the Mini over ssh):
ssh gumbo-mini-0 '~/chuggernaut/target/release/chuggernaut admin \
  --keys-dir ~/chuggernaut-data/keys user token --email david@kasofsk.xyz \
  --ttl 720h' > ~/.config/chuggernaut/gumbo.token

# dev (minted locally):
cd ~/chuggernaut
./target/release/chuggernaut admin --keys-dir deploy/dev/data/keys \
  user token --email claude@dev.local > deploy/dev/data/keys/claude.token
```

## Vocabulary

**Job** = graph node (`#N`, ticket-style title/description, deps, lifecycle
work → evaluation → wrap-up). **Task** = one execution inside a job (phase
Work|Evaluation, kind command|agent|human). **Job type** = repo-versioned
definition (`.chug/jobs/{type}.yaml`). Reusable tasks are plain files in the
project repo: scripts (command tasks) and markdown instructions (agent
tasks).

## API reference (all under $BASE, JSON, Bearer auth)

### Read
| Endpoint | Returns |
|---|---|
| `GET /api/v1/projects` | project slugs |
| `GET /api/v1/projects/{o}/{p}/jobs` | Job[] (id, title, state, deps, type) |
| `GET /api/v1/projects/{o}/{p}/jobs/{seq}` | one Job |
| `GET /api/v1/projects/{o}/{p}/jobs/{seq}/criteria` | resolved evaluators + wrap_up + ref |
| `GET /api/v1/projects/{o}/{p}/jobs/{seq}/tasks` | Task[] (phase, kind, state, result, model) |
| `GET /api/v1/projects/{o}/{p}/jobs/{seq}/tasks/{id}/artifacts` | artifact kinds |
| `GET .../tasks/{id}/artifacts/session.jsonl` or `stdout.log` | transcript / logs (raw bytes) |
| `GET .../tasks/{id}/output?since=<offset>` | live container stdout: `{offset, data, running}` — cursor-paged (see "watch a running task") |
| `GET /api/v1/projects/{o}/{p}/diff/{seq}` | the job branch's diff |
| `GET /api/v1/projects/{o}/{p}/job-types` | [{name, display_name, description}] |
| `GET /api/v1/projects/{o}/{p}/job-types/{name}` | full type (raw YAML + parsed) |
| `GET /api/v1/projects/{o}/{p}/tags` | [{name, path}] — knowledge tags; `path` is where each `.chug/tags/*.md` resolved (§1.1) |
| `GET /api/v1/projects/{o}/{p}/tasks/pending` | human-task inbox |
| `GET /api/v1/projects/{o}/{p}/events` (SSE) | live events; `.../jobs/{seq}/events` per job |
| `GET /api/v1/projects/{o}/{p}/origin` | linked-origin status: release state, ahead_by, held |

### Write
| Endpoint | Body | Effect |
|---|---|---|
| `POST /api/v1/projects` | `{owner, name}` | create project (platform admins; seeds the Code starter) |
| `POST /api/v1/projects/link` | `{owner, name, origin_url, main_branch?}` | link an external GitHub origin (platform admins; needs CHUG_ORIGIN_* secrets set first) |
| `POST /api/v1/projects/{o}/{p}/jobs` | `{type, title?, description?, deps?: [id], knowledge_tags?, eval?: [Evaluator]}` | create job (lands Frozen). description = the ticket; injected into work AND eval prompts |
| `POST .../jobs/{seq}/release` | — | ▶ run the job |
| `POST .../jobs/{seq}/revoke` | — | revoke (cascades to Frozen/Blocked/Ready dependents) |
| `POST .../jobs/{seq}/claim` | — | claim the next work attempt for a human (§1.2 claims); 409 while an attempt is in flight |
| `DELETE .../jobs/{seq}/claim` | — | clear a pending claim that has not materialized into a parked task |
| `POST .../jobs/{seq}/tasks/{id}/resolve` | `{kind: "Pass"\|"Fail"\|"Escalation", structured, abort?, action?}` | resolve a human task; `abort: true` on an evaluator Fail = unfixable, escalate |
| **Shipping this repo** — `kasofsk/chuggernaut` is platform-owned; there is no origin release/sync. To ship, **create + release a `deploy` job** (`POST .../jobs {type:"deploy"}` then `.../release`): it ssh's into the Mini and runs `update.sh` at the released `main`. (`origin/release`, `origin/sync` still apply to *linked-origin* projects, not this one.) |

## Working a job locally (claims)

Any job's work attempt can be **claimed** by a human without changing its
declared kind — an agent-typed job stays agent-typed; the claim parks the
attempt as a Pending task with `performed_by: human` instead of launching a
container. Verbs to support conversationally:

- **"claim job N"** — `POST .../jobs/{N}/claim`; then `POST .../jobs/{N}/release`
  if it is still Frozen. When the job enters Work the attempt parks (visible
  as `awaiting_human: { kind: "work", claimed: true }` on the job).
- **"start working"** — set up a worktree on the job branch:
  `git fetch origin && git worktree add ../job-N -b job/N origin/job/N`
  (the branch exists once the job enters Work; push access per the user's cert).
- **"submit job N"** — after pushing the work to `job/N`, resolve the parked
  task Pass with a summary (it becomes the squash-merge commit body):
  `POST .../jobs/{N}/tasks/{task_id}/resolve` with
  `{"kind":"Pass","structured":null,"summary":"what was done"}`.
  Evaluation then proceeds exactly as if an agent had done the work.
- **"fail it out" / "let an agent take over"** — resolve Fail with structured
  notes: `{"kind":"Fail","structured":{"notes":"..."}}`. The next attempt
  launches per the DECLARED kind (an agent picks it back up); no un-conversion.
- **"unclaim job N"** — `DELETE .../jobs/{N}/claim` (only before the attempt
  parks; afterwards resolve the task instead).

The parked task id comes from `GET .../jobs/{N}/tasks` (the Pending Work-phase
task) or `GET .../tasks/pending`.

## Conventions

- Refer to jobs as `#N`; numbers are per-project and monotonic.
- **Creating a job does not start it** — release ("run") is separate. Release
  launches real agent containers (spends tokens): only do it when the user
  clearly asked to run, otherwise create and tell them it's ready to run.
- Write job descriptions like tickets: what to build, constraints,
  acceptance criteria. The work agent and its reviewer both see it verbatim.
- To watch a running job: poll `.../jobs/{seq}` + `.../tasks`, or read SSE
  with `curl -N --max-time 30`. `channel-update` events are the agent
  narrating its own progress.
- **To watch a running task's raw output** (agent stdout, or a command task's
  compile progress — the black box `channel-update` doesn't cover): poll
  `GET .../tasks/{id}/output?since=<offset>`. Start at `since=0`, then pass the
  returned `offset` back each poll; append `data` and show the last N lines.
  `running:true` while the container lives; after it exits the same endpoint
  serves the harvested `stdout.log` (`running:false`) at the same offsets, so
  the tail is never lost. Bound each call with `--max-time`, e.g.:
  ```sh
  off=0; while :; do
    r=$(curl -s --max-time 10 -H "Authorization: Bearer $TOK" \
      "$BASE/api/v1/projects/$O/$P/jobs/$SEQ/tasks/$TID/output?since=$off")
    printf %s "$(echo "$r" | jq -r .data)"
    off=$(echo "$r" | jq .offset)
    [ "$(echo "$r" | jq .running)" = "true" ] || break
    sleep 3
  done | tail -n 40
  ```
- Revoke kills running containers — confirm with the user first.
- **Never predict server-assigned ids** for follow-up calls — thread them
  from the create response (`ID=$(create … | jq -r .id)`), never hardcode a
  guessed next number. A guessed id once raced a concurrent create and
  released a different job. For consequential mutations (release, revoke),
  re-verify the target's title/state if any time passed since the last read.
- **Shipping this repo (`kasofsk/chuggernaut`)**: it's platform-owned — deploy
  is a **`deploy` job**, not an origin release. Create + release a `deploy` job
  to push the current `main` to prod (it ssh's the Mini and runs `update.sh`).
  Releasing it restarts the dispatcher that supervises it — that's by design
  (§3.6 reconciles); confirm before releasing, especially while jobs are
  mid-Work. GitHub is a read-only mirror; direct pushes to GitHub `main` get
  overwritten. (`origin/release`/`origin/sync` above are for linked-origin
  projects only.)
- Presenting: job lists as compact tables (#, title, state, type); job detail
  as state + title + tasks summary; don't dump raw JSON unless asked.
- If the API is unreachable: for prod check Tailscale is up and the Mini's
  stack is running (`deploy/prod/README.md`); for dev see
  `deploy/dev/README.md` "Run".
- For dispatcher internals beyond the API, read `~/chuggernaut/spec.md`
  (normative) and the repo's docs — `crates.md` for the crate map,
  `docs/implementation-notes.md` for per-module rationale, `docs/design/`
  for why a thing is shaped the way it is — rather than guessing.

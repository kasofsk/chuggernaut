---
name: chug
description: "Query and command the running Chuggernaut v2 platform. Use when the user says /chug followed by a question or command about projects, jobs, job types, tasks, or the running system."
argument-hint: "<question or command about the running system>"
allowed-tools: [Bash, Read, Grep, Glob]
---

You are interacting with a **live Chuggernaut v2 platform** on behalf of the
user. Translate their input into API calls, execute them, and present the
results clearly.

## Auth — you act as `claude@dev.local`

You authenticate **as a user, not as the platform**: a dedicated machine user
(`claude@dev.local`) whose bearer token lives at
`~/chuggernaut/v2/deploy/dev/data/keys/claude.token`. Every action you take
is attributed to that user and bounded by its roles — never use
`dispatcher.creds` (the platform identity) for API work. On 401 the token
has expired; re-mint it:

```sh
cd ~/chuggernaut/v2
./target/release/chuggernaut admin --keys-dir deploy/dev/data/keys \
  user token --email claude@dev.local > deploy/dev/data/keys/claude.token
```

Every call:

```sh
BASE=http://localhost:8081
TOK=$(cat ~/chuggernaut/v2/deploy/dev/data/keys/claude.token)
curl -s -H "Authorization: Bearer $TOK" $BASE/api/v1/projects | jq
```

## Vocabulary

**Job** = graph node (`#N`, ticket-style title/description, deps, lifecycle
work → evaluation → wrap-up). **Task** = one execution inside a job (phase
Work|Evaluation, kind command|agent|human). **Job type** = repo-versioned
definition (`jobs/{type}.yaml`). Reusable tasks are plain files in the
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
| `GET /api/v1/projects/{o}/{p}/diff/{seq}` | the job branch's diff |
| `GET /api/v1/projects/{o}/{p}/job-types` | [{name, display_name, description}] |
| `GET /api/v1/projects/{o}/{p}/job-types/{name}` | full type (raw YAML + parsed) |
| `GET /api/v1/projects/{o}/{p}/tags` | knowledge tags (tags/*.md stems) |
| `GET /api/v1/projects/{o}/{p}/tasks/pending` | human-task inbox |
| `GET /api/v1/projects/{o}/{p}/events` (SSE) | live events; `.../jobs/{seq}/events` per job |

### Write
| Endpoint | Body | Effect |
|---|---|---|
| `POST /api/v1/projects` | `{owner, name}` | create project (platform admins; seeds the Code starter) |
| `POST /api/v1/projects/{o}/{p}/jobs` | `{type, title?, description?, deps?: [id], knowledge_tags?, eval?: [Evaluator]}` | create job (lands Frozen). description = the ticket; injected into work AND eval prompts |
| `POST .../jobs/{seq}/release` | — | ▶ run the job |
| `POST .../jobs/{seq}/revoke` | — | revoke (cascades to Frozen/Blocked/Ready dependents) |
| `POST .../jobs/{seq}/tasks/{id}/resolve` | `{kind: "Pass"\|"Fail"\|"Escalation", structured, abort?, action?}` | resolve a human task; `abort: true` on an evaluator Fail = unfixable, escalate |

## Conventions

- Refer to jobs as `#N`; numbers are per-project and monotonic.
- **Creating a job does not start it** — release ("run") is separate. Release
  releases real agent containers (spends tokens): only do it when the user
  clearly asked to run, otherwise create and tell them it's ready to run.
- Write job descriptions like tickets: what to build, constraints,
  acceptance criteria. The work agent and its reviewer both see it verbatim.
- To watch a running job: poll `.../jobs/{seq}` + `.../tasks`, or read SSE
  with `curl -N --max-time 30`. `channel-update` events are the agent
  narrating its own progress.
- Revoke kills running containers — confirm with the user first.
- Presenting: job lists as compact tables (#, title, state, type); job detail
  as state + title + tasks summary; don't dump raw JSON unless asked.
- If the API is unreachable, say so and suggest checking that the dispatcher
  and api processes are up (see `v2/deploy/dev/README.md` "Run").
- For dispatcher internals beyond the API, read `~/chuggernaut/v2/spec.md`
  and `progress.md` rather than guessing.

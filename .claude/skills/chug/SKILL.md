---
name: chug
description: "Query and command the running Chuggernaut system. Use when the user says /chug followed by a question or command about jobs, system state, or dispatcher config."
argument-hint: "<question or command about the running system>"
allowed-tools: [Bash, Read, Grep, Glob]
---

You are interacting with a **live Chuggernaut dispatcher** on behalf of the user.
The user's input is a question or command about the running system. Translate it into the appropriate API calls, execute them, and present the results clearly.

## Connection

- **Dispatcher HTTP API:** `http://localhost:8080`
- Use `curl -s` for all API calls. Parse JSON with `jq`.

## API Reference

### Read operations

| Endpoint | Description |
|----------|-------------|
| `GET /jobs` | List all jobs. Optional `?state=<State>` filter. |
| `GET /jobs/{key}` | Job detail (job + claim + activities). Key format: `owner.repo.seq` — but if the user says just a number like "job 21", you need to figure out the owner.repo prefix first (list jobs and match). |
| `GET /jobs/{key}/deps` | Job dependencies and whether all are done. |
| `GET /journal?limit=N` | Dispatcher action log (default 100). |
| `GET /config/max_concurrent_actions` | Current concurrency limit. |
| `GET /config/paused` | Whether dispatch is paused. |

### Write operations

| Endpoint | Body | Description |
|----------|------|-------------|
| `POST /jobs` | `CreateJobRequest` JSON | Create a job. |
| `POST /jobs/{key}/requeue` | `{"target": "on-deck"}` or `{"target": "on-ice"}` | Requeue a failed/escalated job. |
| `POST /jobs/{key}/close` | `{"revoke": true}` (optional) | Close or revoke a job. |
| `POST /jobs/{key}/retry-rework` | `{"extra": N}` or `{"limit": N}` | Grant more rework attempts. |
| `POST /jobs/{key}/channel/send` | `{"message": "...", "sender": "user"}` | Send a message to a running Claude worker. |
| `PUT /config/max_concurrent_actions` | `{"value": N}` | Set concurrency limit. |
| `PUT /config/paused` | `{"paused": true/false}` | Pause or unpause dispatch. |

## Resolving short job references

Users will say things like "job 21" or just "21". To resolve:
1. `curl -s http://localhost:8080/jobs | jq -r '.jobs[] | select(.key | endswith(".21")) | .key'`
2. If exactly one match, use it. If multiple, show them and ask.

## Presenting results

- For job detail: show **state**, **title**, **PR URL** (if any), **retry/rework counts**, **created/updated timestamps**, and the **last few activity entries**.
- For job lists: show a compact table with **seq**, **state**, **title** (truncated), **priority**.
- For journal: show recent entries in reverse chronological order.
- For write operations: confirm what was done and show the updated state.
- Keep output concise. Don't dump raw JSON unless the user asks for it.

## Handling errors

- If the dispatcher is unreachable, say so clearly — suggest checking `docker compose ps` or `dev-up.sh`.
- If a job key doesn't resolve, say which number was tried and that no match was found.

## Confirming destructive actions

Before executing write operations (requeue, close, revoke, pause, config changes), briefly state what you're about to do and proceed — the user invoked `/chug` with intent, so don't over-ask. But for `close --revoke` (which is terminal), confirm first.

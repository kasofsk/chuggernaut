---
name: claim
description: "Claim a Chuggernaut job for local work and hand the implementation to an Opus subagent. Use when the user says /claim <job number> (optionally with extra guidance for the implementer)."
argument-hint: "<job number> [extra guidance for the implementer]"
allowed-tools: [Bash, Read, Grep, Glob, Agent, SendMessage]
---

Claim job **#N** on the prod platform, park its work attempt for a human, do
the implementation locally via an **Opus subagent**, then submit it. This
encodes the proven claim → release → implement → push → **verify** → resolve
flow; the ordering rules below exist because getting them wrong has already
caused a raced, empty-branch review once (#54).

## Target and auth

```sh
BASE=https://gumbo-mini-0.tail20c474.ts.net
TOK=$(cat ~/.config/chuggernaut/gumbo.token)
```

Project: `kasofsk/chuggernaut`. On 401, re-mint per `.claude/skills/chug/SKILL.md`.

## 1. Claim and release

1. `GET $BASE/api/v1/projects/kasofsk/chuggernaut/jobs/{N}` — check availability:
   - **Frozen** → claim, then release: `POST .../jobs/{N}/claim`, `POST .../jobs/{N}/release`.
   - **Ready/Work with no attempt in flight** → `POST .../jobs/{N}/claim` (a 409
     means an attempt is already running — report that and stop; offer to watch
     it instead).
   - **Blocked** → report the unmet deps and stop (claiming is fine, releasing
     is not — ask before releasing a job with unreleased deps).
   - Any terminal state (Done/Revoked/Failed) or already `awaiting_human` →
     report and stop.
2. Poll `.../jobs/{N}` (a few seconds) until `state == "Work"` and
   `awaiting_human: { kind: "work", claimed: true, task_id: T }`. Record
   `task_id` and `base_ref`. The parked attempt holds **no fleet slot**.
3. Save the full job `description` to the scratchpad — it is the subagent's
   ticket, verbatim.

## 2. Worktree

```sh
git fetch origin
git worktree add /Users/david/chug-job-{N} -b job/{N} origin/job/{N}
```

The branch exists once the job enters Work and starts at `base_ref`. If the
worktree already exists from an earlier session, reuse it after confirming it
is on `job/{N}` and clean.

## 3. Launch the Opus subagent

Launch a `general-purpose` agent with `model: opus` (background). The prompt
must contain, in this order:

- The worktree path and branch; work **only** there. Commit locally (single
  commit, short imperative message). **Do not push.**
- One paragraph of project context: Chuggernaut is a NATS-backed job
  orchestrator; point at the repo's `CLAUDE.md` (and `web/CLAUDE.md` for web
  jobs) and tell it to read them first and match existing conventions exactly.
- The job description **verbatim** under a `THE TICKET:` heading, plus any
  extra guidance the user gave after the job number.
- The verification the job type expects: `web` → `cd web && npm install (if
  needed) && npm run build`; `code` → `cargo fmt --all && cargo clippy
  --workspace --all-targets -- -D warnings && cargo test --workspace` (note
  any known pre-existing red tests, e.g. job #50's tier-2 set, so it reports
  its own failures separately).
- Return format: summary of changes per file, verification commands + exact
  outcomes, commit hash.
- **Verbatim in every prompt:** "Do NOT use run_in_background for anything.
  Run every command in the foreground to completion. Your final message must
  be the complete report; ending your turn while anything is pending means
  the work is lost." Subagents have repeatedly backgrounded `cargo test` and
  ended their turn "waiting" — nobody wakes a subagent, so the work dies
  silently.

While it runs, continue whatever else the session is doing; the completion
notification arrives on its own. Do not touch the worktree meanwhile.

When the subagent finishes, check whether its last message is a **report or a
wait-statement**. If it ended waiting on something, SendMessage-resume it
with: "no notifier exists — foreground and finish." Before pushing, verify
its commit actually exists in the worktree yourself.

## 4. Review, push, VERIFY, then resolve — in that order

1. Review the diff yourself (`git -C /Users/david/chug-job-{N} diff
   {base_ref}..HEAD`) against the brief. Fix-ups: prefer sending the subagent
   a follow-up message over editing its work silently.
2. Push: `git -C /Users/david/chug-job-{N} push origin job/{N}:job/{N}`.
   If rejected non-fast-forward, `git fetch origin` and look — never force-push
   over commits you have not read.
3. **Verify the remote tip** before resolving:
   `git fetch origin && git log --oneline -1 origin/job/{N}` must show the new
   commit on top of `base_ref`. Resolving before the push lands gives the
   review evaluator an empty branch (the #54 race).
4. Resolve the parked task Pass — the summary becomes the squash-commit body,
   so write it like one (what changed and why, not process narration):
   `POST .../jobs/{N}/tasks/{T}/resolve` with
   `{"kind":"Pass","structured":null,"summary":"..."}`.
5. Start a background watcher polling `.../jobs/{N}` every 60–90s until a
   terminal state, and report the outcome when it fires. On rework/escalation,
   surface the evaluator findings and offer to iterate in the same worktree
   (the branch is preserved across reworks).

## Conventions

- One in-flight claim per worktree path; clean up `git worktree remove
  /Users/david/chug-job-{N}` after the job reaches Done (or keep it if the
  user wants the artifacts).
- If the user asked only to *implement but not submit*, stop after step 4.1
  and say the push/resolve is pending their go-ahead.
- Never resolve Fail/Escalation without the user's direction.

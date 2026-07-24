---
name: local-web-tweak
description: "Rapid local iteration on the Chuggernaut web UI: worktree + branch, Vite dev server proxied to the prod API for real data, iterate with HMR, then wrap the work in a `web` job and submit it. Use when the user says /local-web-tweak or wants to do local UI iteration on the chuggernaut web app."
argument-hint: "[optional: what UI area to tweak]"
allowed-tools: [Bash, Read, Edit, Write, Grep, Glob, EnterWorktree, ToolSearch]
---

Set up a local UI-iteration loop on the Chuggernaut operator UI (`web/`),
iterate with the user against **real prod data**, and when they're happy,
package the work as a Chuggernaut `web` job and submit it through the normal
claim flow. Three phases; don't start phase 3 until the user says the work is
done.

## Target and auth (same as /chug)

```sh
BASE=https://gumbo-mini-0.tail20c474.ts.net
TOK=$(cat ~/.config/chuggernaut/gumbo.token)
```

Project `kasofsk/chuggernaut`. On 401, re-mint per `.claude/skills/chug/SKILL.md`.

## Phase 1 — worktree + dev server on prod data

1. **Worktree**: use the `EnterWorktree` tool (load via ToolSearch if deferred)
   with a name like `ui-<topic>`. It creates a branch off origin main and
   switches the session into the worktree. Work in `<worktree>/web`.
2. **Install**: `cd <worktree>/web && npm install --no-audit --no-fund`.
3. **Point the dev proxy at prod** (real data instead of a local api). Edit
   `web/vite.config.ts`:

   ```ts
   proxy: {
     // Temporarily pointed at prod (gumbo-mini-0) for UI iteration — revert
     // to http://localhost:8080 before submitting.
     '/api':  { target: 'https://gumbo-mini-0.tail20c474.ts.net', changeOrigin: true },
     '/auth': { target: 'https://gumbo-mini-0.tail20c474.ts.net', changeOrigin: true },
   },
   ```

   This change is scaffolding — it must **never be committed** (phase 3
   reverts it).
4. **Start the server**: `npm run dev` as a background task. Vite picks the
   next free port if 5173 is taken — read the startup output and tell the
   user the actual URL. Verify the proxy end-to-end:
   `curl -s -o /dev/null -w '%{http_code}' http://localhost:<port>/api/v1/health`
   → expect 200.
5. Warn the user once: mutating actions clicked in the UI (claim, release,
   revoke, resolve) **hit prod**.

## Phase 2 — iterate

- HMR shows edits live; the user watches the browser and gives feedback.
- Follow `web/CLAUDE.md` strictly: single stylesheet `src/styles.css`, design
  tokens (never hard-code hex in components), endpoints only via `src/api.ts`,
  **check every layout change at a ~360–390px viewport** and keep the mobile
  rules in the `@media (max-width: 640px)` block current.
- Commit checkpoints locally on the worktree branch as coherent steps land
  (never commit the vite.config.ts proxy change — commit paths explicitly or
  restore it first). Do not push anywhere yet.
- Before declaring any change done: `npm run build` (tsc + vite) must pass.

## Phase 3 — submit the work as a job (only when the user says ship it)

1. **Clean up**: revert the `vite.config.ts` proxy to `http://localhost:8080`
   (drop the temp comment), run `npm run build` one last time, and make sure
   everything intended is committed on the worktree branch. Squash noisy
   checkpoint commits into one or a few well-messaged commits.
2. **Create the job** — the description is the ticket *and* what the review
   evaluator sees, so write it as: what changed and why, per file, plus how it
   was verified (build passed, checked at mobile width, seen against prod
   data). Mention it was human-supervised local work.

   ```sh
   curl -s -X POST -H "Authorization: Bearer $TOK" -H 'Content-Type: application/json' \
     $BASE/api/v1/projects/kasofsk/chuggernaut/jobs \
     -d '{"type":"web","title":"...", "description":"..."}'   # → job #N, Frozen
   ```

3. **Claim, then release** (order matters — claim while Frozen so no agent
   container ever launches): `POST .../jobs/{N}/claim`, then
   `POST .../jobs/{N}/release`. Poll `.../jobs/{N}` until `state == "Work"`
   and `awaiting_human: {kind:"work", claimed:true, task_id:T}`; record
   `task_id` and `base_ref`.
4. **Push the local work onto the job branch**. The platform created
   `job/{N}` at `base_ref` (platform main). From the worktree:

   ```sh
   git fetch origin
   git rebase origin/job/{N}   # only if base_ref moved past the branch point
   git push origin HEAD:job/{N}
   ```

   If the push is rejected non-fast-forward, fetch and look — never
   force-push over commits you haven't read.
5. **Verify before resolving** (prevents the empty-branch review race, #54):
   `git fetch origin && git log --oneline -1 origin/job/{N}` must show your
   commit. Only then resolve the parked task Pass — the summary becomes the
   squash-merge commit body, so write it like one:

   ```sh
   curl -s -X POST -H "Authorization: Bearer $TOK" -H 'Content-Type: application/json' \
     $BASE/api/v1/projects/kasofsk/chuggernaut/jobs/{N}/tasks/{T}/resolve \
     -d '{"kind":"Pass","structured":null,"summary":"..."}'
   ```

6. **Watch**: poll `.../jobs/{N}` every 60–90s in the background until a
   terminal state and report the outcome. On rework, the evaluator findings
   come back and the same worktree/branch is used to iterate (branch is
   preserved); on Done, stop the dev server and offer to remove the worktree.

## Conventions

- Never resolve Fail/Escalation without the user's direction.
- If the user wants to keep iterating after a rework request, stay in phase 2
  and re-run phase 3 steps 4–5 (the job and parked-task flow repeats on the
  next attempt).
- Deploying the merged result is separate: offer a `deploy` job per
  `.claude/skills/chug/SKILL.md` ("Shipping this repo").

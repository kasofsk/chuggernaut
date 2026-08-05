---
name: chug-install
description: "Streamlined Chuggernaut installation + initial deployment. Use when the user says /chug-install, or wants to stand up a Chuggernaut platform, import an existing GitHub repo as a platform-owned project with a GitHub mirror, or join a worker node. Drives deploy/prod/chug-install.sh interactively and verifies each stage."
argument-hint: "[platform | import <repo-url> | worker]"
allowed-tools: [Bash, Read, Grep, Glob, Edit]
---

You are guiding a user through getting a repository onto **Chuggernaut** — a
one-time setup they'd otherwise run as a hand-copied runbook. Drive the scripts
in `deploy/prod/`, and **verify each stage before moving to the next**. Every
step degrades to a manual command from `deploy/prod/README.md` / `INSTALL.md`;
always say which script you are about to run before running it.

## The machine half you orchestrate

`deploy/prod/chug-install.sh` — idempotent, `--dry-run`-capable, composes the
existing pieces (never reinvent them):

- `chug-install.sh preflight` — deps + config check (non-destructive). **Always
  run this first.**
- `chug-install.sh platform` — stand up the single-host stack (dispatcher + api
  + NATS + ssh front) via `boot.sh`, `chuggernaut init`, `install-launchd.sh`,
  and a health gate (`deploy-health.sh`).
- `chug-install.sh project-import <git-url> [--owner O --name N]` — create a
  platform-owned project, push the repo history in as `main`, wire the GitHub
  remote as a read-only mirror, install the per-project mirror agent
  (`chug-mirror-install.sh`), and verify.
- `chug-install.sh worker-join [--node NAME --project O/N]` — mint the worker's
  NATS creds + read-only git key, build/start its images (`build-worker.sh`),
  and print the `DOCKER_NODES` membership seed to add (its slot field is a
  pre-observation fallback, not the node's capacity — spec §3.1).

## Detect what already exists (don't clobber)

Before acting, probe state so a re-run is safe:

1. **Is a platform reachable?** Try `curl -fsS "$CHUG_API_URL/api/v1/health"`
   (or the `/chug` skill's target). Reachable → skip `platform`, go to import.
2. **Is this repo already a project?** Ask the running platform (via `/chug` or
   the API) whether `owner/name` exists. If so, skip project creation.
3. **Is `deploy/prod/chuggernaut.env` present + filled?** If not, help the user <!-- runtime -->
   copy `deploy/prod/env.example` and fill the required vars before `platform`.

## Ask the model-choice question

Two ways to bring an existing GitHub repo onto the platform — **explain both,
help choose, default to platform-owned + mirror** (full dogfood usage):

- **Platform-owned + mirror (default).** The platform's bare repo OWNS `main`;
  GitHub becomes a **read-only mirror** force-pushed every 5 min. Changes land
  as jobs on the platform. This is how `kasofsk/chuggernaut` itself runs. Use
  `project-import`.
- **Linked-origin.** GitHub stays the source of truth; the platform tracks it
  via `POST /api/v1/projects/link` + `CHUG_ORIGIN_*` secrets and opens
  `chug/release-*` PRs back. Choose this when the team keeps GitHub-native PR
  review. (See README §12/§5.3 — currently the less-exercised path.)

State the trade-off in one line and let the user pick; proceed with
platform-owned unless they choose linked-origin.

## Flow

1. **Preflight.** `deploy/prod/chug-install.sh preflight`. Resolve any MISSING
   dep before continuing (macOS: `brew install colima docker node age`).
2. **Platform** (only if none reachable). Preview first with `--dry-run`, show
   the user the steps, then run for real. Confirm the health gate passes.
3. **Import.** Confirm owner/name and the mirror URL. Run with `--dry-run`
   first, then for real. Walk the user through the **deploy-key guidance** the
   mirror script prints (generate a dedicated key, add it as a GitHub deploy key
   with write access) — this is the one manual, out-of-band step; no secret is
   stored by the script.
4. **Verify the round trip.** Create a trivial job (via `/chug` or the API) that
   commits to `main`, let it merge, and confirm the commit appears on the GitHub
   mirror within ~5 min (or force the mirror agent once). Report the result.
5. **Worker (optional).** If the user wants a worker node, run `worker-join`,
   copy the creds to the node, and add the printed `DOCKER_NODES` entry on the
   dispatcher so the node survives a dispatcher restart. **No restart is
   needed to pick the node up** — the daemon announces itself and the
   dispatcher merges it into the live fleet. Its capacity comes from the node,
   not that entry (`docs/runbooks/worker-capacity.md`).

## Guardrails

- **Destructive/outward steps: preview with `--dry-run`, then confirm.** Project
  creation, pushing history, force-pushing to GitHub, and restarting services
  are outward-facing — show the user what will happen first.
- **GitHub is a read-only mirror.** Make sure the user understands direct pushes
  to GitHub `main` will be overwritten (README §3).
- **Single-host + one worker is the target.** Multi-node/HA stays README-level
  guidance; don't attempt it here.
- If any stage fails, surface the exact failing command and the README section
  that documents the manual equivalent, then stop and ask.

---
name: chug-ops
description: "Operate and troubleshoot the prod Chuggernaut deployment (gumbo-mini-0 + worker fleet): topology, access, deploy mechanics, worker refresh, failure diagnosis. Use for /chug-ops or any prod ops/deploy/fleet/outage question. For plain API queries use /chug instead."
argument-hint: "<ops question or task about the prod deployment>"
allowed-tools: [Bash, Read, Grep, Glob]
---

Operational knowledge for the **prod Chuggernaut deployment**. These are
point-in-time facts (last verified 2026-07-24) — for anything load-bearing,
cross-check `deploy/prod/README.md` and the live system before acting.

**Safety first:** never run destructive or state-changing ops (restarts,
deploys, prunes, data resets) without explicit user approval. Explain what
will run and wait.

## Topology

- **Control plane — gumbo-mini-0** (Mac mini). Dispatcher and api run as
  **native launchd services** (`com.chuggernaut.dispatcher`,
  `com.chuggernaut.api`). Colima on the Mini is deliberately small
  (2cpu/2GiB) and runs only **nats + ssh** containers.
- **API/UI**: https://gumbo-mini-0.tail20c474.ts.net (tailscale serve →
  native api on 127.0.0.1:8080). `GET /api/v1/health` returns
  `{"dispatcher":"ok",...}` end-to-end, 503 when the dispatcher is down —
  trust it; the old SPA-fallback-200 trap is fixed (#81).
- **Worker fleet**: `DOCKER_NODES="air|worker|4, nuc|worker|2"`.
  - **air** = dev-air.tail20c474.ts.net, arm64 mac, colima 6cpu/12GiB,
    daemon container `chug-worker` dials NATS at `100.116.243.42:4222`.
  - **nuc** = gumbo-nuc-0, Linux x86_64 12c/31GB, sccache hand-wired via
    `WORKER_CACHE_DIR` env + bind mount (until #122).
  - Changing the fleet requires a dispatcher restart until #137 (dynamic
    registration) lands.
- **Networking gotcha**: worker containers on macOS hosts **cannot reach the
  LAN** (colima user-mode NAT). Publish/deploy jobs therefore ssh the Mini
  over the **tailnet** on port **2200** (dedicated key-only sshd,
  LaunchDaemon `com.chuggernaut.sshd2200`, key = `MINI_DEPLOY_KEY` secret).

## Access

- Humans: `ssh worksalot@gumbo-mini-0` (Tailscale SSH). `sudo` needs an
  interactive password — ask the user to run `! ssh -t ...` themselves for
  root ops.
- Jobs: `ssh worksalot@100.116.243.42 -p 2200`.
- Git: the Mini checkout `~/chuggernaut` has origin = local bare repo — the
  **platform owns `main`; GitHub is a read-only mirror**. Direct pushes to
  GitHub main get overwritten.
- **Operator checkout on a new machine**: `origin` must point at the
  platform's SSH front, not GitHub. Add to `~/.ssh/config`:

  ```
  Host chug-mini
    HostName gumbo-mini-0
    Port 2222
    User git
    IdentityFile ~/.ssh/id_ed25519
    CertificateFile ~/.ssh/id_ed25519-cert-chug.pub
  ```

  then `git remote set-url origin ssh://chug-mini/kasofsk/chuggernaut.git`
  (equivalently `ssh://git@100.116.243.42:2222/...` over the tailnet). Push
  access comes from the per-user SSH certificate issued by the platform.
  Optionally keep the GitHub mirror as a `github` remote — never push to it.
- UI static files live in `~/chuggernaut-data/ui`, swapped by web-publish
  jobs.
- API tokens: see `.claude/skills/chug/SKILL.md` (mint on the Mini over ssh).

## Deploying

- Ship by **creating + releasing a `deploy` job** (see /chug "Shipping this
  repo"). The job ssh's the Mini and runs `update.sh` at released `main`.
- `update.sh` is **health-gated** (#77): `restart-verify.sh` proves the
  dispatcher answers on NATS and auto-rolls-back to `chuggernaut.prev` on
  failure.
- Releasing a deploy restarts the dispatcher that supervises it — by design
  (§3.6 reconciles). Confirm before releasing, especially with jobs mid-Work.
- **Web jobs self-publish on merge** via wrap_up (#63) — no manual
  web-publish jobs.
- Dispatcher restart (graceful drain, job-safe):
  `launchctl kickstart -k "gui/$(id -u)/com.chuggernaut.dispatcher"` on the
  Mini. Caveat #140: a saturated fleet at restart-adjacent times burned
  eval_retries pre-fix.

## Worker refresh

- Workers do **not** self-refresh on deploy (Tailscale blocks Mini→dev-air
  ssh). After a deploy touching `crates/worker`, `channel`, or
  `Dockerfile.agent*`, rebuild from the operator laptop at current `main`:

  ```sh
  WORKER_SSH=worksalot@dev-air.tail20c474.ts.net CHUG_WORKER_NODE=air \
    WORKER_NATS_URL=nats://100.116.243.42:4222 deploy/prod/build-worker.sh
  ```

  Daemon swap is safe mid-job (#82 filed to automate).
- A failed `worker-refresh:{node}` leg ("refresh not confirmed") is **fatal
  to the deploy**: update.sh fails the job, later legs skip, prod stays on
  the old SHA — nothing half-deploys.

## Troubleshooting a failed worker refresh

1. Build-failure output never leaves the node (#213). Diagnose by ssh'ing
   the node and reading `docker logs chug-worker` — output is buffer-flushed
   in one lump and may lag or be empty.
2. **Recurring cause on dev-air**: colima docker disk pressure (~80%+ full)
   kills the `cargo build` inside the image build. The refresh script only
   prunes after a *successful* refresh, so repeated failures accumulate
   stranded build generations.
3. Safe cleanup (**ask first — it deletes**):
   `docker image prune -f` (dangling only, **never `-a`**) and
   `docker builder prune -f --keep-storage 15GB`.
4. Verify with a manual rebuild (validate-first, can't hurt live tags):
   `docker exec chug-worker /usr/local/lib/chuggernaut/worker-refresh.sh build <sha> prod`
5. Retry the failed deploy via its escalation task:
   `POST .../tasks/{id}/resolve {"kind":"Escalation","action":"Retry"}`.

## Hard-won rules

- **Never predict server-assigned ids.** Thread ids from the create
  response (`ID=$(create | jq -r .id)`); a guessed id once released a
  *different user's* job. Before consequential mutations (release spends
  tokens, revoke kills containers), re-verify the target's title/state if
  any time passed since the last read.
- **Docker over host installs** for dev/test deps (nats-server etc.) —
  containers with skip-guards, not brew.
- Mini LAN addresses: Ethernet .200, Wi-Fi .128 — irrelevant to workers
  (LAN unreachable from containers; everything goes over the tailnet).

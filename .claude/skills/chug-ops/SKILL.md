---
name: chug-ops
description: "Operate and troubleshoot the prod Chuggernaut deployment (gumbo-mini-0 + worker fleet): topology, access, deploy mechanics, worker refresh, failure diagnosis. Use for /chug-ops or any prod ops/deploy/fleet/outage question. For plain API queries use /chug instead."
argument-hint: "<ops question or task about the prod deployment>"
allowed-tools: [Bash, Read, Grep, Glob]
---

Operational knowledge for the **prod Chuggernaut deployment**. These are
point-in-time facts (last verified 2026-07-30) — for anything load-bearing,
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
- **Worker fleet**: **air 2 slots / nuc 2** (verified 2026-07-26). Read the live
  numbers from `GET /api/v1/platform/fleet`, **never** from `DOCKER_NODES` — for
  a `worker` endpoint that seed is a membership entry plus a pre-observation
  fallback, and it can never override what the node reports (spec §3.1).
  - **air** = dev-air.tail20c474.ts.net, arm64 mac, colima 6cpu/12GiB,
    daemon container `chug-worker` dials NATS at `100.116.243.42:4222`.
  - **nuc** = gumbo-nuc-0, Linux x86_64 12c/31GB, sccache hand-wired via
    `WORKER_CACHE_DIR` env + bind mount (until #122).
  - **Changing capacity needs no restart, no ssh, no rebuild**: the Cluster
    page's per-node stepper, or `PUT /api/v1/platform/fleet/{node}/capacity`
    (202, platform admin). Adding a node needs no restart either — the daemon
    announces itself and the dispatcher merges it live (#137, landed).
  - `capacity_source` in the fleet snapshot is the field to check first: `seed`
    means the node has **never reported its own capacity**, which is the
    signature of the 2026-07-26 denied-announce incident. Full reference:
    `docs/runbooks/worker-capacity.md`.
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
- **Cluster drift banner reads "Deploy drift unavailable"?** `SELF_REPO` is
  unset in the dispatcher's `deploy/prod/chuggernaut.env` (#276 — it was in no
  install template until then, so every install was born without it). Set
  `SELF_REPO=kasofsk/chuggernaut` (the platform's own project slug, not the
  GitHub mirror) and restart the dispatcher; `main_tip_sha`/`commits_behind` in
  `GET /api/v1/platform/config` populate on the next scan tick (~30s). Leaving
  it unset breaks nothing else — the drift fields just stay null.

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

1. **Read the deploy job's own output first — no ssh (#270).** The failing
   node's `worker-refresh:{node}` leg carries the harvested failure detail,
   and after the fan-out update.sh recaps each node's transcript into the
   deploy's stdout (`update: [<node> log] …`, a bounded tail per node) so the
   nodes' interleaved live lines are readable per node. The deploy Work task's
   `stdout.log` artifact is harvested even when the deploy's own restart or a
   `task_timeout` kill ended the task.
2. On the node, `docker logs chug-worker` now actually carries the refresh
   phase lines — the daemon runs with `RUST_LOG=info,async_nats=warn`. A node
   whose daemon vanished after a swap: the swapper container is retained, so
   `docker logs chug-worker-swap` (or `journalctl
   CONTAINER_NAME=chug-worker-swap` on a journald node) holds the reason the
   replacement never started.
3. **Recurring cause on dev-air**: colima docker disk pressure (~80%+ full)
   kills the `cargo build` inside the image build. The script now owns this
   itself (#250): a **disk pre-flight** refuses in seconds with
   `insufficient docker disk: need ~30GB … have NGB free` (a leg failing that
   fast means grow the VM disk / prune, not debug the build), and a failed
   build runs the safe prune pair itself and reports the reclaim — a failure
   no longer strands a generation for the next attempt. The 30GB threshold is
   derived from a measured refresh against the post-shrink `agent-rust` image
   — ~2x the image (docker holds it twice while exporting, content blob plus
   unpacked overlay) on top of the live generation, plus BuildKit cache growth
   — and it is a **floor** checked once before the build, not a guarantee:
   free space moves while the build runs, as job-container overlays and the
   BuildKit cache grow and shrink. A node with a different disk shape overrides
   it with `WORKER_REFRESH_DISK_FREE_GB_MIN` (and `_DISK_PATH`) in the daemon's
   env at node creation — a self-refresh carries the override forward.
4. Safe cleanup by hand (**ask first — it deletes**), same pair the script runs:
   `docker image prune -f` (dangling only, **never `-a`**) and
   `docker builder prune -f --keep-storage 15GB`.
5. Verify with a manual rebuild (validate-first, can't hurt live tags):
   `docker exec chug-worker /usr/local/lib/chuggernaut/worker-refresh.sh build <sha> prod`
6. Retry the failed deploy via its escalation task:
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

# Production stack — standing Chuggernaut instance on a Mac Mini

This is the canonical runbook for the always-on instance we use to drive **other**
projects. NATS and the SSH front run as **compose containers**; the **dispatcher**
and the **api** (HTTP↔NATS bridge + web UI) run as **native host processes** under
**launchd** — the dispatcher needs the Docker socket and the repos filesystem, and
the api is the same `chuggernaut` binary the host already builds every deploy, so
containerizing it only forced a redundant Rust compile inside the VM (§2). The
colima VM is left holding just NATS + the ssh front and can run at **2 GiB**.
Deployed by an **explicit `deploy` job** on the platform itself (§3), and
**backed up hourly to Cloudflare R2**. There is no
GitHub-side CI — fmt/clippy/test run as the Chuggernaut `ci` evaluator on the
platform itself before any work can merge.

**The platform owns its own repo.** `kasofsk/chuggernaut` is now a *classic*
project: the bare repo's `main` (HEAD) is the source of truth, agents merge job
branches straight into it, and **GitHub is a read-only mirror** force-pushed from
the Mini (§3). Chuggernaut is developed *on the platform* — create a `code` or
`manual` job, land it on `main`, then ship it with a `deploy` job.

- **Deploy user**: the launchd services and all state run under the Mini's local
  account **`worksalot`** — that's the `<you>`/`$CHUG_DATA` user throughout this
  runbook, and the username for Tailscale SSH (`ssh worksalot@gumbo-mini-0`).

- **State lives outside the checkout** at `~/chuggernaut-data/{keys,repos,backups}`
  (+ the `nats-data` Docker volume), so a `git checkout`/deploy never touches it.
- **Config** is one gitignored file: `deploy/prod/chuggernaut.env` (from
  `env.example`). The launchd wrappers and scripts source it.

## Automated install (`chug-install.sh` / `/chug-install`)

New in job #80: the hand-run parts of this runbook are scripted and idempotent.
For a fresh single host, prefer the streamlined path — the sections below remain
the reference each script composes and the fallback when a step needs a human.

- **`deploy/prod/chug-install.sh`** — `preflight` (deps + config check),
  `platform` (stand up dispatcher + api + NATS + ssh front, §0–§2), `project-import`
  (bring an existing repo in as a platform-owned project and mirror `main` back
  to GitHub, §3 + §12.2), `worker-join` (provision a worker node, §6). Every
  subcommand takes `--dry-run` and is safe to re-run.
- **`/chug-install`** — the Claude Code skill (`.claude/skills/chug-install/`)
  that drives the above interactively, detects existing state, asks the
  platform-owned-vs-linked-origin question, and verifies each stage.
- **`INSTALL.md`** (repo root) — the 15-minute quickstart.

The per-project GitHub mirror is now a scripted artifact too
(`deploy/prod/chug-mirror-install.sh`), replacing the hand-built
`com.chuggernaut.mirror` agent (§3) with a per-project launchd job.

---

## 0. Prerequisites (one-time, on the Mini)

```sh
# Toolchain + runtime. buildx + compose are required (build.sh uses `docker build
# --output`; boot.sh/update.sh use `docker compose`). cloudflared is OPTIONAL —
# only for a public tunnel (§5); the default Tailscale Serve path needs no install.
brew install colima docker docker-buildx docker-compose node age rclone
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust (edition 2024 needs =>1.85)

# Homebrew ships buildx/compose as standalone binaries; link them as docker CLI
# plugins so `docker build --output` and `docker compose` resolve (else build.sh
# fails "unknown flag: --output" and boot.sh fails "unknown shorthand flag: 'f'").
mkdir -p ~/.docker/cli-plugins
ln -sfn "$(brew --prefix)/opt/docker-buildx/bin/docker-buildx"  ~/.docker/cli-plugins/docker-buildx
ln -sfn "$(brew --prefix)/opt/docker-compose/bin/docker-compose" ~/.docker/cli-plugins/docker-compose

# The VM now runs ONLY nats + the ssh front — the api runs natively (§2) and job
# containers run on worker nodes (§6), so no cargo build and no agent containers
# live in the VM. 2 CPU/2GiB is plenty. (Existing Mini on 6/6 for the old
# containerized api? shrink it in the §7 migration.) Verify with `colima list`;
# resize with `colima stop && colima start --cpu 2 --memory 2`.
colima start --cpu 2 --memory 2 --disk 100
```

Also:
- **Enable Automatic Login** (System Settings → Users & Groups) — the services run
  as **user LaunchAgents** in the GUI session (Colima + the sibling agent
  containers share the user's Docker socket), so the Mini must reach a logged-in
  session on boot.
- **GitHub Actions runner / GitHub deploy key** — *no longer needed for deploys.*
  Deploys are now `deploy` jobs (§3) and `origin` is the local bare repo, so the
  self-hosted runner and the GitHub read-only deploy key are out of the deploy
  path. The runner config on the Mini is harmless if left; removing it is a
  separate operator cleanup. The `deploy` job's own ssh key is `MINI_DEPLOY_KEY`
  (§3), not a GitHub key.
- **Colima has no `/var/run/docker.sock`** — the dispatcher (a native host process)
  reaches Docker via `DOCKER_NODES`, which **must** point at the colima socket, or
  it dies with `Socket not found: /var/run/docker.sock`. See §8 and `env.example`.

---

## 1. One-time bootstrap

```sh
export CHUG_REPO=~/chuggernaut
git clone git@github.com:Kasofsk/chuggernaut.git "$CHUG_REPO"
cd "$CHUG_REPO"

# --- config
mkdir -p ~/chuggernaut-data
cp deploy/prod/env.example deploy/prod/chuggernaut.env
$EDITOR deploy/prod/chuggernaut.env      # set model, BACKUP_AGE_RECIPIENT, RCLONE_REMOTE (see §3)

cargo build --release
alias chug="$PWD/target/release/chuggernaut"
set -a; . deploy/prod/chuggernaut.env; set +a

# 1. keys — the NATS step fails on this first run (server not up yet); expected.
chug init --keys-dir "$KEYS_DIR" --repos-root "$REPOS_ROOT" || true

# 2. ssh-front image + linux channel binary (build.sh no longer builds the api
#    or agent images — the api is native, worker nodes build their own agents)
GIT_UID=$(id -u) deploy/prod/build.sh

# 2b. build the web SPA and seed the served UI dir the native api reads (§7)
( cd web && npm ci && npm run build )
mkdir -p "$UI_ROOT" && rsync -a --delete web/dist/ "$UI_ROOT/"

# 3. boot the substrate (colima + NATS + sshd, waits for NATS). The api is not a
#    container anymore — it starts as a launchd service in step 5.
deploy/prod/boot.sh

# 4. finish init (topology, VAPID, admin user) — idempotent
chug init --keys-dir "$KEYS_DIR" --repos-root "$REPOS_ROOT" \
  --admin-email you@kasofsk.xyz --admin-password 'CHANGE-ME'

# 5. install + start the launchd services (boot, dispatcher, api, backups). The
#    api (com.chuggernaut.api / run-api.sh) reads the jwt/creds/artifacts keys
#    from step 1 and serves the UI seeded in step 2b; RunAtLoad starts it now.
deploy/prod/install-launchd.sh

# 7. agent provider credentials (injected into every agent container)
claude setup-token | tail -1 | \
  chug admin --keys-dir "$KEYS_DIR" secret set --project global/agents --name CLAUDE_CODE_OAUTH_TOKEN

# 8. mark the deployed commit so update.sh no-ops until the next deploy job
git rev-parse HEAD > .deployed-sha
```

Browse the UI at `http://localhost:8080` (or the tunnel hostname, §5) and log in.
Create projects with `chug admin project create --owner <o> --name <n> --repos-root
"$REPOS_ROOT" --hook-bin /usr/local/bin/chuggernaut`.

---

## 2. Services & operations

**Containers** (compose; `docker compose -f deploy/prod/compose.yaml ps`): just
`nats` and `ssh`. The **api runs natively** now (launchd, below), not in a
container — it is the same `chuggernaut` binary the host builds each deploy, so
containerizing it only forced a redundant in-VM Rust compile (2026-07-21 502
incident). That let the colima VM shrink to 2 GiB (§0).

**launchd agents** installed by `install-launchd.sh` (logs in
`~/Library/Logs/chuggernaut/`):

| Label | Kind | What |
|---|---|---|
| `com.chuggernaut.boot` | RunAtLoad | `boot.sh`: colima + compose (nats/ssh) + wait-for-NATS |
| `com.chuggernaut.dispatcher` | KeepAlive | `run-dispatcher.sh` (Docker socket + repos filesystem) |
| `com.chuggernaut.api` | KeepAlive | `run-api.sh`: `chuggernaut api` on `127.0.0.1:8080` (HTTP↔NATS bridge + web UI) |
| `com.chuggernaut.backup-hourly` | :00 hourly | `backup-r2.sh` |
| `com.chuggernaut.backup-daily` | 03:20 | `backup-r2.sh promote daily` |
| `com.chuggernaut.backup-monthly` | 1st 03:40 | `backup-r2.sh promote monthly` |
| `com.chuggernaut.mirror` | :05 interval | `git push origin main:main --force-with-lease` (GitHub mirror; §3) |

```sh
docker compose -f deploy/prod/compose.yaml ps          # container status (nats/ssh)
tail -f ~/Library/Logs/chuggernaut/api.log             # native api logs
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.api          # restart api
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher   # restart dispatcher (safe; §3.6 reconciles)
deploy/prod/install-launchd.sh            # reload after editing a plist template
deploy/prod/install-launchd.sh uninstall  # remove all agents
tail -f ~/Library/Logs/chuggernaut/*.log
```

### Cloning a platform repo (SSH front)

Platform repos live behind the SSH front (port 2222). Access is by a **CA-signed
user certificate** (§7.3), not a registered key — no per-user key upload. Mint a
24h cert for your existing SSH key with the `chuggernaut` CLI, authenticated by a
bearer token (`admin user token --email you@example.com --ttl 720h`, saved to a
file):

```sh
# One 24h cert; re-run when it expires (v1 UX is manual refresh, spec §7.5).
chuggernaut ssh-cert \
  --base-url https://<api-host> \
  --token-file ~/.config/chuggernaut/token
# → writes ~/.ssh/id_ed25519-cert-chug.pub next to ~/.ssh/id_ed25519.pub
```

<details><summary>curl equivalent (no CLI)</summary>

```sh
curl -sf https://<api-host>/auth/ssh-cert \
  -H "Authorization: Bearer $(cat ~/.config/chuggernaut/token)" \
  -H 'Content-Type: application/json' \
  --data "$(jq -Rn --arg k "$(cat ~/.ssh/id_ed25519.pub)" '{public_key:$k}')" \
  | jq -r .certificate > ~/.ssh/id_ed25519-cert-chug.pub
```
</details>

Point git at the SSH front with an `~/.ssh/config` `Host` block so the cert is
presented automatically (`IdentitiesOnly yes` stops the agent from offering other
keys first):

```
Host chug-mini
    HostName <mini>            # the Tailscale name of the Mini
    Port 2222
    User git
    IdentityFile ~/.ssh/id_ed25519
    CertificateFile ~/.ssh/id_ed25519-cert-chug.pub
    IdentitiesOnly yes
```

```sh
# Clone / fetch any repo you have Viewer+ on (owner/project → the repo path):
git clone ssh://git@chug-mini/kasofsk/chuggernaut.git
git -C chuggernaut fetch origin integration    # e.g. pull the integration branch
```

Authorization is by role (spec §5.2): pull needs Viewer+, push needs Member+ and
only `refs/heads/job/N`. A push to any other ref (e.g. `main`, `integration`) is
refused by the pre-receive hook.

---

## 3. Deployment (a `deploy` job) + the GitHub mirror

**GitHub Actions CD is gone.** There is no `.github/workflows/deploy.yml`, no
self-hosted runner in the deploy path, and no push-triggered deploy. Deploying is
now an **explicit chuggernaut `deploy` job** (`jobs/deploy.yaml`) — you create it
and release it, exactly like any other job.

> **When the normal deploy job can't run or finish** — a broken build toolchain
> on a node, a stranded/stale worker daemon, the dispatcher down, image loss —
> follow the [ad-hoc deploy runbook](../../docs/runbooks/adhoc-deploy.md): the
> sanctioned by-hand interventions, each with verification, and the paper-trail
> norm (file + claim a `deploy` job, resolve `Pass` with a machine-shaped
> record) so a manual deploy still lands in deploy history.

### Shipping a build

`main` is the source of truth on the platform. Once your change is on `main`
(via a merged `code`/`manual` job), ship it:

1. **Create** a `deploy` job (API `POST .../jobs` with `{type: "deploy"}`, or the
   UI create-form → type *Deploy*).
2. **Release** it. A deploy job carries no commits, so its `job/N` branch sits at
   `main`'s HEAD — that SHA is what ships. Its work step, `tasks/deploy.sh`,
   ssh's into the Mini and runs `deploy/prod/update.sh <sha>`. `wrap_up: none`,
   so on eval-pass the job goes straight to Done and its scratch branch is dropped.

`update.sh` itself is unchanged in spirit:

1. no-ops if `.deployed-sha` already matches the target,
2. else checks the SHA out in `$CHUG_REPO`, snapshots the old dispatcher binary
   to `chuggernaut.prev`, `cargo build --release` (host dispatcher **and** api —
   one binary), `build.sh` (ssh-front image + channel binary only), builds `web/`
   and seeds `UI_ROOT`, idempotent `chug init`, rebuilds + restarts the `ssh`
   container, then hands off to **`restart-verify.sh`** (below), which restarts
   the host services and gates the deploy on a real dispatcher health check.
   `.deployed-sha` is only advanced once that check passes. No cargo build runs
   inside the VM.

**Self-restart is by design.** `update.sh` `kickstart`s the dispatcher that
supervises the very `deploy` job running it. The restart drops in-memory state;
§3.6 reconciliation re-attaches to the still-running work container on the next
tick and processes its exit normally. Don't "fix" this.

### Post-restart health check + rollback (`restart-verify.sh`)

The last thing a deploy does is restart the dispatcher that supervises it — so a
bad binary/config used to crash-loop with nothing watching (the **2026-07-22
fleet-startup outage**: launchd retried a doomed dispatcher every 10s for ~40 min
until a human diagnosed it over ssh). `deploy/prod/restart-verify.sh` closes
that gap, on the Mini, before `update.sh` reports success:

1. `kickstart`s the dispatcher **and** the native api onto the new binary.
2. **Polls for genuine dispatcher health** for ~60s: a NATS `req.jobs.list`
   request (via `nats-box` on the NATS network, `dispatcher.creds` — the same
   pattern as `backup.sh`) that **only a live dispatcher answers**. This is
   deliberately *not* a curl to the api: the api is a separate launchd service
   and can answer HTTP while the dispatcher crash-loops beside it (on 2026-07-22
   the api returned `dispatcher unavailable: no responders`). Success → the
   deploy passes as before.
3. **On health-check failure it rolls back**: restores `chuggernaut.prev`
   (snapshotted by `update.sh` before the overwrite; it pairs with the previous
   `.deployed-sha`), restarts, and re-verifies. If the rollback is healthy it
   prints `new build failed health check, rolled back to <sha>, now healthy` and
   **exits non-zero**, so the deploy job goes red while **prod stays up**. A
   rollback that is *itself* unhealthy — or a missing `.prev` — exits with the
   loudest possible message (launchd is left retrying; a human is needed *now*).

The whole transcript streams back through the ssh session into the deploy task's
log (visible in the UI log viewer), so a failed deploy reads as a story.

**It survives its own supervisor's restart.** `restart-verify.sh` runs on the
Mini (invoked over ssh by the deploy task) and `trap '' HUP`s, so the health
check + rollback run to completion even if §3.6 reconciliation reaps the deploy
container mid-run and drops the ssh session. Reconciliation marks the deploy
task on the dispatcher's next start; the script keeps prod up independently.

Exit codes (also the deploy job's failure mode): `0` healthy · `1` rolled back,
prod healthy on the old binary · `2` rollback also unhealthy (prod down) · `3`
no `.prev` to roll back to.

**Test it** (no NATS/Docker/launchd needed — fakes `launchctl` and injects a
probe that reads a GOOD/BAD marker "binary"):

```sh
deploy/prod/restart-verify.test.sh   # exits 0 iff all four cases pass
```

**The deploy key.** `tasks/deploy.sh` ssh's in with the `MINI_DEPLOY_KEY` project
secret (injected as an env var — the private-key value; the script writes it to a
0600 tempfile for `ssh -i`). Provision it once before the first deploy job:
`chug admin ... secret set --project kasofsk/chuggernaut --name MINI_DEPLOY_KEY`
(register the public half in the Mini's `~/.ssh/authorized_keys`).

**Manual deploy / rollback** (still available by hand, from the checkout):
```sh
CHUG_REPO=~/chuggernaut deploy/prod/update.sh              # deploy origin/main now
CHUG_REPO=~/chuggernaut deploy/prod/update.sh <good-sha>   # roll back to a known-good commit
# fast host-binary rollback (dispatcher AND api are the same binary):
cp ~/chuggernaut/target/release/chuggernaut.prev ~/chuggernaut/target/release/chuggernaut
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.api
```

### GitHub mirror (`com.chuggernaut.mirror`, stopgap)

GitHub is a **read-only mirror**. The `com.chuggernaut.mirror` launchd agent
(§2) runs every 5 min:

```sh
git -C /Users/worksalot/chuggernaut-data/repos/kasofsk/chuggernaut.git \
  push origin main:main --force-with-lease
```

⚠️ **Direct pushes to GitHub `main` will be overwritten.** The bare repo on the
Mini owns `main`; the mirror force-pushes over whatever is on GitHub (the lease
only guards against a mid-push race, not against you pushing your own commits).
Never treat GitHub as writable for this repo — land changes as jobs on the
platform. This launchd mirror is a stopgap until mirroring becomes a dispatcher
feature (separate job).

> The old `CHUG_ORIGIN_DEPLOY_KEY` / `CHUG_ORIGIN_PAT` project secrets (from the
> linked-origin era) are **dead** — nothing reads them. There's no delete command
> yet, but they're harmless; leave them. The §12/§5.3 linked-origin flow
> (`chug/release-{n}` PRs, `origin/release`, `origin/sync`) **no longer applies to
> this project** — it's a classic project now, mirrored one-way to GitHub.

---

## 4. Backups → Cloudflare R2

`deploy/backup.sh` snapshots all three stores (verified git bundles + `nats
account backup` + keys) into one tarball. `deploy/prod/backup-r2.sh` wraps it:
**age-encrypts** the tarball (the tarball *is* the keys), **pushes** to
`r2:chuggernaut-backups/hourly/`, prunes locally; the daily/monthly launchd jobs
server-side-copy the newest hourly into `daily/` and `monthly/`.

### Provisioning (run once; verify current syntax with the `cloudflare`/`wrangler` skill)

```sh
# 1. Backup encryption key — KEEP THE PRIVATE KEY OFFLINE.
age-keygen -o /tmp/backup-identity.key      # prints "Public key: age1..."
#   -> put the age1... public key in chuggernaut.env as BACKUP_AGE_RECIPIENT
#   -> store /tmp/backup-identity.key in your password manager, then shred it here.

# 2. R2 bucket
npx wrangler r2 bucket create chuggernaut-backups

# 3. R2 S3 credentials — Zero Trust/R2 dashboard → "Manage R2 API Tokens" →
#    create an Object Read & Write token scoped to the bucket. Note the
#    Access Key ID, Secret, and the S3 endpoint https://<accountid>.r2.cloudflarestorage.com

# 4. rclone remote (config lands in ~/.config/rclone/rclone.conf)
rclone config create r2 s3 provider Cloudflare \
  access_key_id <AKID> secret_access_key <SECRET> \
  endpoint https://<ACCOUNTID>.r2.cloudflarestorage.com acl private

# 5. Lifecycle (retention) — per-prefix object expiry, via wrangler or the R2
#    dashboard. Intended tiers:
#      hourly/  → expire after 2 days   (keeps ~24-48)
#      daily/   → expire after 31 days
#      monthly/ → expire after 400 days
```

Smoke-test the push before trusting the schedule:
```sh
deploy/prod/backup-r2.sh          # make + encrypt + push one now
rclone lsf r2:chuggernaut-backups/hourly/
```

### Restore drill (do this — an untested backup is a hope)

```sh
rclone copy r2:chuggernaut-backups/hourly/<name>.tgz.age /tmp/
age -d -i <backup-identity.key from your password manager> -o /tmp/restore.tgz /tmp/<name>.tgz.age
mkdir /tmp/restore && tar xzf /tmp/restore.tgz -C /tmp/restore
# follow RESTORE.md inside the extracted directory (keys → NATS restore → repos)
```

---

## 5. Remote access

The native api binds `127.0.0.1:8080` (loopback only — `run-api.sh`), so nothing
is exposed except through one of the paths below.

### 5a. Tailscale Serve — private, tailnet-only (default)

If the Mini is on your tailnet, this is the simplest path and keeps the UI off the
public internet entirely. One-time: enable **HTTPS Certificates** for the tailnet
(admin console → DNS), then:

```sh
tailscale serve --bg http://127.0.0.1:8080     # serves https://<host>.<tailnet>.ts.net (tailnet only)
tailscale serve status                          # confirm the proxy
```

The api's own JWT sessions sit behind Tailscale device identity. Use **`serve`**
(private); **`funnel`** would expose it publicly — do not use it here.

### 5b. Cloudflare Tunnel — public hostname (optional)

Only if you need access from outside the tailnet. Requires `cloudflared` (§0):

```sh
cloudflared tunnel login
cloudflared tunnel create chuggernaut
# ~/.cloudflared/config.yml:
#   tunnel: <tunnel-id>
#   credentials-file: /Users/<you>/.cloudflared/<tunnel-id>.json
#   ingress:
#     - hostname: chug.kasofsk.xyz
#       service: http://localhost:8080
#     - service: http_status:404
cloudflared tunnel route dns chuggernaut chug.kasofsk.xyz
cloudflared service install     # runs cloudflared under launchd
```

Then add a **Cloudflare Access** application + policy for `chug.kasofsk.xyz` in the
Zero Trust dashboard (email OTP / SSO) — defense-in-depth over the api's own JWT
sessions.

The **git SSH front (`:2222`) stays off the public tunnel.** Push project code via
the project's linked GitHub origin, or reach `:2222` over LAN/Tailscale.

---

## 6. Worker nodes (gumbo-nuc-0)

Job containers run on a dedicated worker so heavy cargo builds never starve the
dispatcher node. The prod fleet is **worker-only**: the Mini's colima node is
registered at **0 slots** (placement never picks it; failback = bump the slots
and restart the dispatcher), and all work/eval containers land on
**gumbo-nuc-0** (12-core/31GiB, x86_64, NixOS, Docker preinstalled).

The node runs a **`chuggernaut worker` daemon** (spec §3.1): it dials OUT to
the Mini's NATS and executes container ops against its local Docker socket —
**no Docker endpoint, tunnel, or listening port on the node**. Launches are
small NATS messages; static artifacts (the channel binary, agent images) are
built on the node at deploy time, and the daemon injects its own channel-binary
copy into job containers. An unreachable worker is *out of service* (placement
skips it, dispatcher starts fine, no restart needed when it returns).

**Setup (one-time):**

```sh
# 1. Mini: mint the daemon's scoped NATS creds (subscribe req.worker.nuc.> only)
chug admin --keys-dir "$KEYS_DIR" worker-creds --node nuc
# 2. copy to the node (the worker container mounts this dir read-only)
ssh worksalot@gumbo-nuc-0 mkdir -p chuggernaut-worker/keys
scp "$KEYS_DIR/worker-nuc.creds" worksalot@gumbo-nuc-0:chuggernaut-worker/keys/worker.creds
# 3. env (chuggernaut.env): see env.example — worker fleet form
#    DOCKER_NODES="local|unix:///…/docker.sock|0, nuc|worker|4"
#    WORKER_SSH=worksalot@gumbo-nuc-0
#    WORKER_NATS_URL=nats://100.116.243.42:4222     # Mini's tailnet IP
# 4. build + start the daemon on the node (also runs on every CD deploy)
set -a; . deploy/prod/chuggernaut.env; set +a
deploy/prod/build-worker.sh
# 5. restart the dispatcher to pick up the fleet
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher
```

Notes:

- **Cross-node addressing** — job containers on the nuc reach the Mini's NATS
  and git front via the Mini's **tailnet IP**, not `host.docker.internal`:
  `NATS_URL_CONTAINER=nats://100.116.243.42:4222`,
  `REPO_URL_BASE=ssh://git@100.116.243.42:2222` (ports 4222/2222 are published
  on all interfaces by compose).
- **CD** — `update.sh` calls `build-worker.sh`: worker+agent images build
  natively on the node (context streamed over ssh from the deployed SHA) and
  the daemon restarts on the new image. Safe mid-job: containers survive and
  the dispatcher's poll-based wait re-attaches.
- **Verify placement** — during a job: `ssh worksalot@gumbo-nuc-0 docker ps`
  shows the work/eval containers; the Mini's colima shows none
  (`docker ps --filter label=chuggernaut.managed`).
- The daemon's version is reported in its ping; the dispatcher logs a warning
  when it drifts from the dispatcher's own (stale node artifacts).
- **Watch a refresh from the deploy job, not the node.** `worker-refresh.sh`
  announces each phase (`worker-refresh: phase build-image 3/3 agent-rust`)
  before it runs it; the daemon reports the current phase in its ping and the
  deploy's confirm loop relays it into the deploy job's task output, with a 30s
  elapsed-time heartbeat while a phase runs long. So a live deploy reads:

  ```
  refresh progress: node=nuc phase=build-image 3/3 agent-rust, 154s elapsed
  refresh progress: node=nuc still phase=build-image 3/3 agent-rust (204s in phase), 214s elapsed
  ```

  A leg that never confirms prints its last phase and output lines and folds
  them into the failing leg's `detail` — so `ssh` + `docker logs chug-worker` is
  the second resort now, not the first. If the progress lines are *absent*
  during a real refresh, the node is running a daemon older than this feature.
- **The fleet refreshes in parallel, and one failure stops the rest.** Every
  worker node is asked at once and confirmed concurrently, so this step costs
  the slowest node's build, not the sum (#254). The progress lines of the nodes
  therefore **interleave** — read the `node=` on each. When a node fails, the
  deploy cancels the refreshes still building on the others
  (`chuggernaut admin worker-refresh --cancel --node N --sha S`, which the
  daemon honours by signalling the build's process group) and each cancelled
  node gets a **failed** `worker-refresh:{node}` leg naming the node that
  aborted the deploy. A node that had already started swapping stays swapped —
  its leg says so, and the fleet snapshot's per-node version is the
  cross-check. To cancel a hand-run refresh, the same command works standalone.

### Fast image builds (BuildKit dependency caching, #115)

The worker/agent-rust image builds are the slow leg of a deploy: `Dockerfile.worker`
and `Dockerfile.agent-rust` compile the whole Rust workspace, so a cold Docker
build rebuilds all dependencies on every SHA change (~10 min observed on
dev-air). Both Dockerfiles now use **BuildKit `RUN --mount=type=cache`** mounts
for the cargo registry/git and the compiled target, so a SHA bump recompiles
only the changed crates. `CHUG_GIT_SHA` in `Dockerfile.worker` is placed *after*
the dependency layer so a SHA-only change never invalidates the deps.

- **Enable BuildKit on the node.** `build-worker.sh` and `worker-refresh.sh`
  both prefix the build with `DOCKER_BUILDKIT=1`, which turns on the engine's
  built-in BuildKit — **no `buildx` CLI plugin is required** for cache mounts.
  If the node's build log still shows the *legacy* builder ("Install the buildx
  component…"), the engine is too old for BuildKit; on colima, `colima start`
  with a recent Docker engine (23+) has BuildKit available. Cache mounts are
  ignored (build stays cold, still correct) on a legacy builder, so this
  degrades safely.
- **Cache safety.** The registry/git caches are shared-safe (cargo locks them);
  each target cache uses a per-image `id` (`chug-worker-target`,
  `chug-agent-rust-target`) with `sharing=locked` so concurrent node builds
  never collide. `agent-rust` still **bakes** its warm-target seed (#123) into
  the image — it compiles into the cache mount, then `cp -a`s the result into
  the baked `/opt/chug-prebuilt-target`.
- **Prune protection.** All three images (`worker`, `agent`, `agent-rust`) carry
  `LABEL chug.managed="true"`. A host-level `docker system prune --all`
  (gumbo-nuc-0 runs one on a daily timer) removes every image not backing a
  *running* container, and the agent images back nothing between jobs, so an
  unfiltered sweep deletes them and the next job on that node fails to launch
  with `404: No such image`. The host spares them with `--filter
  label!=chug.managed`; that filter is inert unless every image carries the
  label, so the pairing is checked statically:

  ```sh
  deploy/managed-label.test.sh   # all three carry it, and never the container key
  ```

  The label lives in the Dockerfiles (constant property of the image), unlike
  `chug.git.sha`, which stays a CLI `--label` because it is per-build.
- **The image key is NOT `chuggernaut.managed`** (#268). That key is the
  dispatcher's *container*-ownership marker (`MANAGED_LABEL`), and a container
  inherits its image's labels: while the images carried it (#266), `chug-worker`
  was indistinguishable from a job container, so the §3.6 startup orphan sweep
  killed the worker on **both nodes on every dispatcher restart** — and with no
  worker alive, nothing reported container exits, so the in-flight deploy hung
  in Evaluation until `docker start chug-worker` was run by hand on each node.
  `chuggernaut.*` is the container namespace, `chug.*` the image namespace; they
  must not converge. The sweep also now refuses to reap a marker-bearing
  container that lacks the `chuggernaut.project/.job/.task` identity labels every
  launch stamps, so a future collision degrades to a log line instead of an
  outage. **Deploying the fix reaps the workers one last time** — the running
  images still carry the old label until that deploy rebuilds them — so expect
  to `docker start chug-worker` on both nodes once more after it lands.
- **MEASURE cold vs warm (manual, run on the node).** CI cannot build images, so
  the warm-cache win is a manual measurement — capture it once on dev-air:

  ```sh
  # cold: clear the BuildKit cache, then time a full build
  docker builder prune -af
  time (git archive --format=tar HEAD | DOCKER_BUILDKIT=1 docker build \
    -t chuggernaut/agent-rust:measure -f deploy/prod/Dockerfile.agent-rust -)
  # warm: touch a single crate's source, rebuild — deps should be cached
  time (git archive --format=tar HEAD | DOCKER_BUILDKIT=1 docker build \
    -t chuggernaut/agent-rust:measure -f deploy/prod/Dockerfile.agent-rust -)
  ```

  Repeat with `Dockerfile.worker` (add `--build-arg CHUG_GIT_SHA=$(git rev-parse HEAD)`).
  Expect the warm build to skip dependency compilation entirely.

---

## 7. Instant web UI publish (`web-publish` job)

Front-end-only changes skip the full deploy. The **native api** serves the SPA
straight from a host directory (`UI_ROOT`, default `~/chuggernaut-data/ui`; set
as `UI_DIST` by `run-api.sh`), reading it live off disk per request — so
replacing that directory's **contents** changes what the SPA serves immediately.
No bind mount, no baked-in image copy, no container to restart: refresh the page.

- **Normal deploys stay authoritative**: `update.sh` builds `web/` on the host
  and rsyncs it into `UI_ROOT` on every deploy, so a full deploy and a
  web-publish always land the same content.
- **Fast path**: after a `web` job (jobs/web.yaml) merges, release a
  `web-publish` job (jobs/web-publish.yaml). It builds `web/dist` at main and
  tar-pipes it to the Mini, staging in `UI_ROOT.new` and rsyncing contents
  into place (~30s end to end). Same `MINI_DEPLOY_KEY` ssh path as deploy. The
  native api picks the new files up on the next request — no reload needed.
- **Swap contents, not the directory** — `rsync -a --delete web/dist/
  "$UI_ROOT/"`, not `rm -rf`/`mv` of `UI_ROOT` itself. (With the native reader
  a directory swap would actually be picked up too, since it opens by path each
  request; keeping the in-place rsync just matches web-publish and avoids a
  window where the dir is missing.)

### One-time migration on an existing Mini (containerized api → native)

Do this **once** on the running Mini to switch from the api container to the
native launchd service. It depends on the #36 `update.sh` re-exec already being
deployed (the old updater must never run against a compose.yaml with the `api`
service removed). Run it by hand on the Mini, `cd ~/chuggernaut`:

```sh
git fetch origin && git checkout --force origin/main   # or the deployed SHA

# 1. Build the api binary + UI FIRST, so the service has something to bind and
#    serve the instant it loads. node is a prerequisite (§0).
cargo build --release
set -a; . deploy/prod/chuggernaut.env; set +a
UI_ROOT="${UI_ROOT:-$HOME/chuggernaut-data/ui}"; mkdir -p "$UI_ROOT"
( cd web && npm ci && npm run build ) && rsync -a --delete web/dist/ "$UI_ROOT/"

# 2. Free :8080 BEFORE the native api is expected to bind — retire the old
#    container. Its compose service is gone in this checkout, so remove it by
#    name (compose rm can't target a service the file no longer defines).
docker rm -f chuggernaut-api-1 2>/dev/null || true

# 3. Render + load the launchd services (com.chuggernaut.api included; RunAtLoad
#    starts it now, on the freed port and the binary/UI from step 1).
deploy/prod/install-launchd.sh

# 4. Confirm the native api is running (not crash-looping on a taken port).
launchctl print gui/$(id -u)/com.chuggernaut.api | grep -E 'state|program'

# 5. Health check — retry, since a KeepAlive (re)bind can lag a second or two.
#    This hits the SPA root ON PURPOSE: the only claim here is "the api process
#    bound its port", and the SPA root answers 200 as soon as it does. It is NOT
#    a dispatcher proof — the SPA fallback answers 200 for any route, so a 200
#    says nothing about the dispatcher (that's the #77/#81 masquerade). For real
#    dispatcher liveness use /api/v1/health, which round-trips the core actor
#    (§6.6); that's what tasks/deploy-health.sh gates on and restart-verify.sh
#    proves over NATS.
for _ in $(seq 1 30); do
  curl -fsS http://127.0.0.1:8080/ >/dev/null 2>&1 && { echo "api OK"; break; }
  sleep 2
done

# 6. Shrink the VM — it now runs only nats + ssh (§0). boot.sh brings the stack
#    back up (colima start is a no-op; compose up recreates nats/ssh).
colima stop && colima start --cpu 2 --memory 2 --disk 100
deploy/prod/boot.sh
```

After this, normal `deploy` jobs manage the native api (kickstart, step 6b of
`update.sh`); no further manual steps.

## 8. Colima notes & gotchas

- **No `/var/run/docker.sock`; the dispatcher needs `DOCKER_NODES`.** bollard's
  default socket path doesn't exist under colima, so the dispatcher exits with
  `backend unavailable: Socket not found: /var/run/docker.sock`. Point it at the
  colima socket in `chuggernaut.env` — and **quote the value**, because the env
  file is `.`-sourced and the unquoted `|` separators are parsed as shell pipes
  (`line NN: unix:///…: No such file or directory`):
  ```sh
  DOCKER_NODES="local|unix:///Users/<you>/.colima/default/docker.sock|4"
  ```
  Find the exact path with `colima status` (“docker socket: …”).
- **`docker build --output` / `docker compose` errors** (`unknown flag: --output`,
  `unknown shorthand flag: 'f'`) mean the buildx/compose CLI plugins aren't linked
  — see the `~/.docker/cli-plugins` symlinks in §0.
- **`host.docker.internal`** — agent containers reach NATS/sshd on the host via
  it. Smoke-test after first boot:
  `docker run --rm alpine sh -c 'nc -zv host.docker.internal 4222'`.
- **Bind-mount UID** — the ssh image is built with `GIT_UID=$(id -u)` so the
  container's git user can write the bind-mounted bare repos. If pushes fail with
  permission errors, rebuild: `GIT_UID=$(id -u) deploy/prod/build.sh`.
- **"repository corruption on the remote side" that isn't** — cloning a bare repo
  on the host hardlinks loose objects and a VirtioFS-backed mount can serve them
  corrupt inside the container. Seed by hand with `git clone --no-hardlinks`; if
  hit, `git -C <bare> repack -ad && git prune-packed`.
- **Empty workspace** — check `git config uploadpack.allowFilter true` on the bare
  repo and git-protocol-v2 through the SSH front (`AcceptEnv GIT_PROTOCOL`, already
  in `sshd_config`).
- Recreating the NATS volume (`docker compose -f deploy/prod/compose.yaml down -v`)
  **wipes all platform state** — restore from a backup or re-run init step 4.

# Production stack — standing Chuggernaut instance on a Mac Mini

This is the canonical runbook for the always-on instance we use to drive **other**
projects. NATS, the SSH front, and the **api** (HTTP↔NATS bridge + web UI) run as
**compose containers**; only the **dispatcher** runs as a native host process
under **launchd** (it needs the Docker socket and the repos filesystem), launching
agent containers as siblings. Deployed by an **explicit `deploy` job** on the
platform itself (§3), and **backed up hourly to Cloudflare R2**. There is no
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

# Give Colima room for cargo builds + agent containers. On the 8-core/8GiB
# Mini we run 6/6 (leaves ~2GiB for macOS + the native dispatcher); anything
# smaller than ~4/5 can't hold a cold cargo build plus nats/ssh/api. NOTE:
# `colima start` with no flags on a fresh machine defaults to 2 CPU/2GiB and
# a job whose resources.cpu exceeds the VM's CPUs fails at container launch
# ("range of CPUs is from 0.01 to N"). Verify with `colima list`; resize an
# existing VM with `colima stop && colima start --cpu 6 --memory 6`.
colima start --cpu 6 --memory 6 --disk 100
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
  it dies with `Socket not found: /var/run/docker.sock`. See §7 and `env.example`.

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

# 2. images + linux channel binary
GIT_UID=$(id -u) deploy/prod/build.sh

# 3. boot the substrate (colima + NATS + sshd + api, waits for NATS). The api
#    container mounts the jwt/creds/artifacts keys generated in step 1.
deploy/prod/boot.sh

# 4. finish init (topology, VAPID, admin user) — idempotent
chug init --keys-dir "$KEYS_DIR" --repos-root "$REPOS_ROOT" \
  --admin-email you@kasofsk.xyz --admin-password 'CHANGE-ME'

# 5. install + start the launchd services (boot, dispatcher, backups). The api
#    is a compose container (step 3), not a launchd service.
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

**Containers** (compose; `docker compose -f deploy/prod/compose.yaml ps`): `nats`,
`ssh`, and `api` — the api publishes the UI to `127.0.0.1:8080`.

**launchd agents** installed by `install-launchd.sh` (logs in
`~/Library/Logs/chuggernaut/`):

| Label | Kind | What |
|---|---|---|
| `com.chuggernaut.boot` | RunAtLoad | `boot.sh`: colima + compose (nats/ssh/api) + wait-for-NATS |
| `com.chuggernaut.dispatcher` | KeepAlive | `run-dispatcher.sh` (the only host service) |
| `com.chuggernaut.backup-hourly` | :00 hourly | `backup-r2.sh` |
| `com.chuggernaut.backup-daily` | 03:20 | `backup-r2.sh promote daily` |
| `com.chuggernaut.backup-monthly` | 1st 03:40 | `backup-r2.sh promote monthly` |
| `com.chuggernaut.mirror` | :05 interval | `git push origin main:main --force-with-lease` (GitHub mirror; §3) |

```sh
docker compose -f deploy/prod/compose.yaml ps          # container status (nats/ssh/api)
docker compose -f deploy/prod/compose.yaml logs -f api  # api logs
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher   # restart dispatcher (safe; §3.6 reconciles)
deploy/prod/install-launchd.sh            # reload after editing a plist template
deploy/prod/install-launchd.sh uninstall  # remove all agents
tail -f ~/Library/Logs/chuggernaut/*.log
```

---

## 3. Deployment (a `deploy` job) + the GitHub mirror

**GitHub Actions CD is gone.** There is no `.github/workflows/deploy.yml`, no
self-hosted runner in the deploy path, and no push-triggered deploy. Deploying is
now an **explicit chuggernaut `deploy` job** (`jobs/deploy.yaml`) — you create it
and release it, exactly like any other job.

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
   to `chuggernaut.prev`, `cargo build --release` (host dispatcher), `build.sh`
   (ssh/agent/api images — the api image builds the web UI itself), idempotent
   `chug init`, rebuilds + restarts the `ssh` and `api` containers, `kickstart`s
   the dispatcher, then health-checks `http://127.0.0.1:8080/` (non-zero fails
   the deploy job).

**Self-restart is by design.** `update.sh` `kickstart`s the dispatcher that
supervises the very `deploy` job running it. The restart drops in-memory state;
§3.6 reconciliation re-attaches to the still-running work container on the next
tick and processes its exit normally. Don't "fix" this.

**The deploy key.** `tasks/deploy.sh` ssh's in with the `MINI_DEPLOY_KEY` project
secret (injected as an env var — the private-key value; the script writes it to a
0600 tempfile for `ssh -i`). Provision it once before the first deploy job:
`chug admin ... secret set --project kasofsk/chuggernaut --name MINI_DEPLOY_KEY`
(register the public half in the Mini's `~/.ssh/authorized_keys`).

**Manual deploy / rollback** (still available by hand, from the checkout):
```sh
CHUG_REPO=~/chuggernaut deploy/prod/update.sh              # deploy origin/main now
CHUG_REPO=~/chuggernaut deploy/prod/update.sh <good-sha>   # roll back to a known-good commit
# fast dispatcher-only rollback (host binary):
cp ~/chuggernaut/target/release/chuggernaut.prev ~/chuggernaut/target/release/chuggernaut
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher
# the api is a container — roll it back by rebuilding at the good SHA (update.sh)
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

The api container publishes to `127.0.0.1:8080` (loopback only), so nothing is
exposed except through one of the paths below.

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

---

## 7. Colima notes & gotchas

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

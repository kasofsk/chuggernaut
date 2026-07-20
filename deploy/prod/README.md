# Production stack — standing Chuggernaut instance on a Mac Mini

This is the canonical runbook for the always-on instance we use to drive **other**
projects. NATS, the SSH front, and the **api** (HTTP↔NATS bridge + web UI) run as
**compose containers**; only the **dispatcher** runs as a native host process
under **launchd** (it needs the Docker socket and the repos filesystem), launching
agent containers as siblings. Deployed **automatically on every green `main`** via
the Mini's GitHub self-hosted runner, and **backed up hourly to Cloudflare R2**.

Chuggernaut itself is still developed on the laptop and pushed to GitHub; the Mini
only *consumes* `main`.

- **State lives outside the checkout** at `~/chuggernaut-data/{keys,repos,backups}`
  (+ the `nats-data` Docker volume), so a `git checkout`/deploy never touches it.
- **Config** is one gitignored file: `deploy/prod/chuggernaut.env` (from
  `env.example`). The launchd wrappers and scripts source it.

---

## 0. Prerequisites (one-time, on the Mini)

```sh
# Toolchain + runtime
brew install colima docker docker-compose node age rclone cloudflared
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust (edition 2024 needs =>1.85)

# Give Colima room for cargo builds + agent containers
colima start --cpu 4 --memory 8 --disk 100
```

Also:
- **Enable Automatic Login** (System Settings → Users & Groups) — the services run
  as **user LaunchAgents** in the GUI session (Colima + the sibling agent
  containers share the user's Docker socket), so the Mini must reach a logged-in
  session on boot.
- **Runner label** — the Mini is already a self-hosted runner in the **Kasofsk
  org**. Add the dedicated label **`chug`** to it (Settings → Actions →
  Runners → the Mini → Labels, or re-run its `config.sh --labels chug`)
  so `deploy.yml` lands only here. Confirm the runner's user has
  `cargo`/`npm`/`colima`/`launchctl` on PATH.
- **Repo access** — `deploy.yml` runs only if `Kasofsk/chuggernaut` is granted to
  the org runner group. Transfer/keep the repo under the org, and **make it
  private** (self-hosted runners + public repos = fork PRs running on your box).

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

# 8. mark the deployed commit so auto-deploy no-ops until the next push
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

```sh
docker compose -f deploy/prod/compose.yaml ps          # container status (nats/ssh/api)
docker compose -f deploy/prod/compose.yaml logs -f api  # api logs
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher   # restart dispatcher (safe; §3.6 reconciles)
deploy/prod/install-launchd.sh            # reload after editing a plist template
deploy/prod/install-launchd.sh uninstall  # remove all agents
tail -f ~/Library/Logs/chuggernaut/*.log
```

---

## 3. Continuous deployment (auto on green `main`)

`.github/workflows/deploy.yml` fires on `workflow_run` after **CI** succeeds on
`main`, runs on the `[self-hosted, chug]` runner, and executes
`deploy/prod/update.sh <head_sha>`. `update.sh`:

1. no-ops if `.deployed-sha` already matches the target,
2. else checks the SHA out in `$CHUG_REPO`, snapshots the old dispatcher binary
   to `chuggernaut.prev`, `cargo build --release` (host dispatcher), `build.sh`
   (ssh/agent/api images — the api image builds the web UI itself), idempotent
   `chug init`, rebuilds + restarts the `ssh` and `api` containers, `kickstart`s
   the dispatcher, then health-checks `http://127.0.0.1:8080/` (non-zero → the
   Actions job fails).

CI now also runs on `push: [main]` (see `.github/workflows/ci.yml`) so there is a
green run for `deploy.yml` to gate on.

**Manual deploy / rollback:**
```sh
CHUG_REPO=~/chuggernaut deploy/prod/update.sh              # deploy origin/main now
CHUG_REPO=~/chuggernaut deploy/prod/update.sh <good-sha>   # roll back to a known-good commit
# fast dispatcher-only rollback (host binary):
cp ~/chuggernaut/target/release/chuggernaut.prev ~/chuggernaut/target/release/chuggernaut
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher
# the api is a container — roll it back by rebuilding at the good SHA (update.sh)
```

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

## 5. Remote access — Cloudflare Tunnel

The api container publishes to `127.0.0.1:8080` (loopback only), so nothing is
exposed except through the tunnel.

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

## 6. Colima notes & gotchas

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

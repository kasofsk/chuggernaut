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
- **Config** is one gitignored file: `deploy/prod/chuggernaut.env` (from <!-- runtime -->
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
- **`README.md`** (repo root) — the 15-minute quickstart.

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
now an **explicit chuggernaut `deploy` job** (`.chug/jobs/deploy.yaml`) — you create it
and release it, exactly like any other job.

> **When the normal deploy job can't run or finish** — a broken build toolchain
> on a node, a stranded/stale worker daemon, the dispatcher down, image loss —
> follow the [ad-hoc deploy runbook](../../docs/reference/runbooks/adhoc-deploy.md): the
> sanctioned by-hand interventions, each with verification, and the paper-trail
> norm (file + claim a `deploy` job, resolve `Pass` with a machine-shaped
> record) so a manual deploy still lands in deploy history.

### Shipping a build

`main` is the source of truth on the platform. Once your change is on `main`
(via a merged `code`/`manual` job), ship it:

1. **Create** a `deploy` job (API `POST .../jobs` with `{type: "deploy"}`, or the
   UI create-form → type *Deploy*).
2. **Release** it. A deploy job carries no commits, so its `job/N` branch sits at
   `main`'s HEAD — that SHA is what ships. Its work step, `.chug/tasks/deploy.sh`,
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

### Rolling back to an earlier commit (a `rollback` job)

Shipping an *older* `main` commit is a first-class job type, not a by-hand-only
operation: `.chug/jobs/rollback.yaml` is `deploy.yaml`'s shape plus one required
**input**, the target SHA (spec §1.1 `inputs:`, design
[#311](../../docs/design/311-job-inputs.md)).

1. **Create** a `rollback` job with the input: `POST .../jobs` with
   `{type: "rollback", inputs: {sha: "<sha>"}}` (7–40 hex characters; the full
   40 is preferred, and an abbreviation is expanded before it ships). The input is
   `required`, so a job released without it is rejected at release validation
   (`inputs.sha`) — never at launch. The UI create-form does **not** render
   input fields yet (#311 slice B), so use the API until it does; a job created
   in the UI would simply be rejected at release, not run blind.
2. **Release** it. The work step, `.chug/tasks/rollback.sh`, reads the value as
   `$CHUG_INPUT_SHA`, resolves it against the repo, and hands the resolved
   40-char SHA to `.chug/tasks/deploy.sh` — same ssh, same key, same
   `update.sh <sha>` as a deploy. Same stage-0 `deploy-health.sh` gate and the
   same `wrap_up: none`.

**It refuses more than it ships.** The effect is external and revoking the job
does not undo it, so the script fails closed *before* the ssh, and says what it
is about to do first (the resolved SHA, its subject, how far behind `main` it
is). It exits non-zero without deploying anything when:

- the input is absent (no value ⇒ no `CHUG_INPUT_SHA` key at all ⇒ `set -u`),
- the SHA resolves to no commit — the case that matters most, because
  `update.sh` would otherwise fall back to `origin/main` and silently deploy the
  *newest* commit in response to a rollback request,
- the commit exists but was never on `main` (a job branch, an abandoned
  attempt): it has passed no merge gate, so it is not a rollback target.

Re-running the same job type with the same SHA is safe: `update.sh` no-ops when
`.deployed-sha` already matches. Inputs are immutable, so a *different* target
is a different job — which is what keeps the job record an honest account of
what shipped. Test the refusals without a Mini: `.chug/tasks/rollback.test.sh`.

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

**The deploy key.** `.chug/tasks/deploy.sh` ssh's in with the `MINI_DEPLOY_KEY` project
secret (injected as an env var — the private-key value; the script writes it to a
0600 tempfile for `ssh -i`). Provision it once before the first deploy job:
`chug admin ... secret set --project kasofsk/chuggernaut --name MINI_DEPLOY_KEY`
(register the public half in the Mini's `~/.ssh/authorized_keys`).

**The health-gate API token.** `.chug/tasks/deploy-health.sh` asserts a live *fleet*
as well as a live dispatcher (deploy #267: both worker daemons were dead, the
gate passed on the dispatcher alone, and the deploy then hung in Evaluation with
nothing alive to report its container's exit). Fleet liveness comes from `GET
/api/v1/platform/fleet`, which is platform-admin only, so the gate authenticates
with the `DEPLOY_HEALTH_API_TOKEN` project secret — declared on the `health`
evaluator in `.chug/jobs/deploy.yaml`, injected into that eval container only. (The
name avoids the reserved `CHUG_` prefix: declaring one of those is itself a
release-validation error, spec §11.) Provision it with a long-TTL admin bearer
token:
`chuggernaut admin user token --email you@example.com --ttl 8760h`, then
`chug admin ... secret set --project kasofsk/chuggernaut --name DEPLOY_HEALTH_API_TOKEN`.

**Until the secret exists, releasing a `deploy` job is rejected up front** —
release validation errors with `secret 'DEPLOY_HEALTH_API_TOKEN' is not set` and
the job never reaches Work, so `update.sh` is not invoked and nothing lands.
That is deliberate and is strictly safer than the hang it replaces: a gate that
cannot see the fleet must not pass. The remedy is to set the secret and release
again. If the secret is set but *stale* (expired or non-admin), the deploy does
run and then fails its health gate with `fleet endpoint refused our
credentials` — at that point the deploy has already landed (`wrap_up: none`), so
refresh the token and re-run the job.

**Manual deploy / rollback** (the by-hand path — prefer a `deploy` or
`rollback` job, which leaves a record; this is for when the platform itself is
too broken to run one, per the [ad-hoc deploy
runbook](../../docs/reference/runbooks/adhoc-deploy.md)):
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

### 5c. OIDC issuer documents — served, deliberately unexposed

The api serves two more unauthenticated routes (spec §6.7), over the public half
of the OIDC issuer keypair `chuggernaut init` generates:

```sh
curl -s localhost:8080/.well-known/openid-configuration   # issuer + jwks_uri
curl -s localhost:8080/.well-known/jwks.json              # one RS256 key, RFC 7517
```

They are public, integrity-only documents — no project data, no private key. They
are **not reachable from outside**, and that is the design (#313 D1): the GCP
workload-identity provider is registered with an **uploaded** JWK set
(`--jwk-json-path`) and `--issuer-uri https://chug.kasofsk.xyz`, an identifier
nobody fetches. Serving them is code; **exposing them is an operator action, and
one nobody has to take.** Do not point `tailscale funnel` at them (§5a rules
Funnel out for this host); if a second cloud ever insists on fetching a JWKS,
design #313 A4 prices the three options — a path-scoped `cloudflared` ingress
and an R2 static relay are the two live ones.

Two operational consequences of the uploaded set:

- **Key rotation is a terraform apply**, per consumer provider
  (`providers update-oidc --jwk-json-path`), not a service restart. The 8-key
  allowance makes overlapping rotation cheap: publish both, wait out the longest
  token TTL (≤1h), retire the old.
- **The upload is not validated when the provider is created.** A malformed or
  stale set surfaces later as `Error connecting to the given credential's
  issuer` — which names the issuer and is therefore actively misleading. Compare
  the `kid` in `/.well-known/jwks.json` against what the provider holds before
  believing the issuer is at fault.

The issuer string comes from `OIDC_ISSUER` (default `https://chug.kasofsk.xyz`).
Only the api reads it today; #313 S2's minter
(`auth::workload::WorkloadTokenSigner`) already takes the issuer as an argument
and S4 will hand it this same resolver's value, so every workload token's `iss`
comes from here too. `chuggernaut.env` is sourced by both `run-api.sh` and
`run-dispatcher.sh` (`set -a`), so one line there covers both. It must equal the
minted token's `iss`, and the api refuses to start on a value that is not an
absolute `https` identifier without a trailing slash.

---

## 6. Worker nodes (gumbo-nuc-0)

Job containers run on a dedicated worker so heavy cargo builds never starve the
dispatcher node. The prod fleet is **worker-only**: the Mini's colima node is
registered at **0 slots** (placement never picks it; failback = bump the slots
and restart the dispatcher), and all work/eval containers land on
**gumbo-nuc-0** (12-core/31GiB, x86_64, NixOS, Docker preinstalled).

The node runs a **`chuggernaut worker` daemon** (spec §3.1): it dials OUT to
the Mini's NATS and executes container ops against its local Docker socket —
**no Docker endpoint, tunnel, or listening port on the node**. It is supervised
**natively** — a `chug-worker.service` systemd unit on Linux, a
`com.chuggernaut.worker` launchd agent in the login user's GUI domain on
macOS — over an environment file that carries the whole run spec (design
[#440](../../docs/design/440-native-worker-daemon.md) D2). `build-worker.sh`
installs all three, extracting the binary from the worker image it just built —
**on Linux**. That image is a Linux container, so on a **mac** it extracts a
binary launchd loops on with `cannot execute binary file`, and a Darwin node
instead **compiles** its own daemon on the node from a declared `WORKER_CARGO`
(#440's [2026-08-07 correction](../../docs/design/440-native-worker-daemon.md#correction-2026-08-07--d6-holds-on-linux-only-and-the-endpoint-was-never-rendered-job-476),
measured on the air; "Converting a mac" below).
**That split is per artifact, decided by who execs it.** The daemon runs on the
node; `chuggernaut-channel` never does — the daemon injects it into every agent
**container** — so it rides out of the worker image on **both** platforms, and a
mac's own Mach-O copy is the one that breaks (#440's
[2026-08-08 correction](../../docs/design/440-native-worker-daemon.md#correction-2026-08-08--the-correction-above-generalised-over-two-binaries-with-opposite-platforms-job-480),
jobs #477/#478). `worker-refresh.sh` is the source file either way. Each staged
binary is asked the question **its own executor** asks before anything is
installed: the daemon must exec on the node, and the channel binary must exec in
a container — on Darwin that is read off its object header against the
architecture `docker version --format '{{.Server.Arch}}/{{.Server.Os}}'` reports,
because a binary that cannot exec inside a container produces no error the
operator ever sees.
**A node's own configuration may own the supervision half instead**, which is
what slice 7 added: a NixOS node declares the unit with
`chug.node.daemon.enable` ([`nix/chug-node/`](../../nix/chug-node/), off by
default, and the seam it opens with this script is in
[the adoption runbook](../../docs/reference/runbooks/chug-node-adoption.md) §4a),
and a mac that this deploy never reaches installs its agent by hand with
[`install-worker-launchd.sh`](install-worker-launchd.sh) — opt-in, refusing the
Mini, and removing the containerized `chug-worker` at its bootstrap exactly as
this script does at its own, because both would otherwise leave two daemons on
one `WORKER_NODE`. Unlike this script it has not driven docker on that node
already, so it asks `docker inspect` whether there is one and **refuses** a
docker it cannot ask at all rather than assuming there is none. Either way the environment file, the binary and the
credentials stay this script's, and the run spec is never declared twice.

Job containers stay siblings on the node's docker socket, exactly as they were
when the daemon was itself a container. **Converting a node is what puts it back
on the self-refresh path:** since
[#440](../../docs/design/440-native-worker-daemon.md) slice 6 the swap installs
the daemon binary out of the image the build phase just made — on a **mac**, out
of the Mach-O daemon that node compiled in its own build phase, while
`chuggernaut-channel` still comes out of the image there too — and asks the
supervisor to restart, so a converted node updates itself again. A converted mac
whose run spec carries no reachable `WORKER_CARGO` (or whose cargo cannot find
`rustc` on the agent's `PATH`) **refuses its own build by name** and stays on the
SHA it has; re-apply its spec with `build-worker.sh`, which resolves the
toolchain and writes both halves. A node nobody
has converted refuses its own swap (`this daemon is running INSIDE a container …
REFUSING swap`) and is deployed over ssh with `build-worker.sh` until it is —
a loud stop, never a second daemon on one node name. Launches are
small NATS messages; static artifacts (the channel binary, agent images) are
built on the node at deploy time, and the daemon injects its own channel-binary
copy into job containers. An unreachable worker is *out of service* (placement
skips it, dispatcher starts fine, no restart needed when it returns).

**Setup (one-time):**

```sh
# 1. Mini: mint the daemon's scoped NATS creds (subscribe req.worker.nuc.> only)
#    UNCHANGED by the native daemon — what moved is where the node keeps it.
chug admin --keys-dir "$KEYS_DIR" worker-creds --node nuc
# 2. install it into the node's ROOT-OWNED 0700 credential directory (design
#    #440 D5). scp to a staging path the login user owns, then `install` it —
#    scp CANNOT write into a 0700 root directory, and that is the point.
scp "$KEYS_DIR/worker-nuc.creds" worksalot@gumbo-nuc-0:/tmp/worker.creds
ssh worksalot@gumbo-nuc-0 '
  sudo install -d -o root -g root -m 0700 /etc/chuggernaut/keys
  sudo install -o root -g root -m 0600 /tmp/worker.creds /etc/chuggernaut/keys/worker.creds
  rm -f /tmp/worker.creds'
#    Same for the node's git key when you mint one (`admin worker-git-key`):
#    worker_git and worker_git-cert.pub, 0600, into the same directory.
# 3. env (chuggernaut.env): see env.example — worker fleet form. The worker
#    entry's slot field is a pre-observation fallback, not the node's capacity.
#    DOCKER_NODES="local|unix:///…/docker.sock|0, nuc|worker|0"
#    WORKER_SSH=worksalot@gumbo-nuc-0
#    WORKER_NATS_URL=nats://100.116.243.42:4222     # Mini's tailnet IP
#    WORKER_SLOTS_nuc=2                             # the node's FIRST-BOOT value
#    WORKER_CACHE_DIR_nuc=/var/cache/chuggernaut/sccache
#    WORKER_REFRESH_GIT_URL=ssh://git@100.116.243.42:2222/<owner>/chuggernaut.git
#    WORKER_GIT_KEY=/etc/chuggernaut/keys/worker_git   # or drop it: this is the default
#    (the whole run spec, per node — "The run spec is declared" below)
# 4. build the images, install the daemon and its unit, start it (also runs on
#    every CD deploy)
set -a; . deploy/prod/chuggernaut.env; set +a
deploy/prod/build-worker.sh
# 5. no dispatcher restart needed: the daemon announces itself and the
#    dispatcher merges it into the live fleet (spec §3.1). Confirm on the
#    Cluster page, or GET /api/v1/platform/fleet.
```

**Where the credentials live, and why it is not the login user's home.**

On **Linux** the daemon's NATS credential and git key live in
**`/etc/chuggernaut/keys`, owned by `root` at mode `0700`**, beside the unit's
own environment file and outside any user's home (design
[#440](../../docs/design/440-native-worker-daemon.md) D5). The unit runs as
`root`, so the daemon reads them and nothing else on the node does. That is not
tidiness: the login user this deploy `ssh`s in as is in the `docker` group, so a
credential file under that user's home is readable by anything that user runs —
a **weaker** boundary than the read-only bind mount the native daemon replaces,
and going native must not lower it. `WORKER_KEYS_DIR_<node>` moves the directory;
`WORKER_GIT_KEY` already defaults to `worker_git` inside it.

`build-worker.sh` checks all of it **before it builds anything**, with the live
daemon untouched, because a daemon that cannot read its own credential does not
come up degraded — it **fails to start**, and `Restart=always` loops that on a
node you have just converted. Four distinct refusals, each naming its own
remedy: the directory is missing (`sudo install -d …`), it is there with the
wrong owner or mode (`sudo chown root:root … && sudo chmod 0700 …`, printing
what it actually found), `worker.creds` is not in it (mint + `sudo install`), or
the check *cannot look* — the directory is `0700` and the login user has no
`sudo -n`, which is a third state, not a missing file, and refuses saying so.

On **macOS** the boundary does not port and the script says so on every run: the
daemon is a `launchd` agent running as the login user in their GUI domain
(CoreSimulator and the keychain are per-user-session services, #322), so there
is no user a root-owned directory would exclude. The keys stay at
`~/chuggernaut-worker/keys` at `0600` per file — the status quo — and cross-task
secret isolation on that platform remains given up (#322 §7).

**Converting a mac: three things it needs that a Linux node does not, and it is
one-way.** Items 2 and 3 are **container-capable macs only**: a node whose
`WORKER_MODES` names `host` and not `container` needs neither, and needs no
docker at all — see *A host-only mac needs no docker* below.

The first two measured on `gumbo-air-0`, 2026-08-06 (#440's
[2026-08-07 correction](../../docs/design/440-native-worker-daemon.md#correction-2026-08-07--d6-holds-on-linux-only-and-the-endpoint-was-never-rendered-job-476)),
the third from its
[2026-08-08 narrowing](../../docs/design/440-native-worker-daemon.md#correction-2026-08-08--the-correction-above-generalised-over-two-binaries-with-opposite-platforms-job-480)
(jobs #477/#478); all three are refusals in the script today:

1. **A Rust toolchain on the node.** The worker image is a Linux container, so
   the binary `docker cp` lifts out of it is an ELF file — the air installed one
   and launchd looped `cannot execute binary file` until the health probe timed
   out at 60s, with the container daemon already removed. A Darwin node compiles
   its own daemon instead, so it needs `cargo`, and it needs one **this deploy's
   ssh shell can see** — a nix-darwin or rustup cargo usually is not on it. The
   script asks `command -v cargo` in the same round trip as `uname -s`, before
   it builds anything, and refuses by name if the answer is empty:

   ```sh
   ssh <node> 'command -v cargo'      # the exact question the deploy asks
   # if that is empty but cargo works interactively, declare the absolute path:
   #   WORKER_CARGO_air=/etc/profiles/per-user/<you>/bin/cargo
   # and check rustc is BESIDE it — cargo finds its compiler through PATH:
   ssh <node> 'PATH=$(dirname <that path>):$PATH; command -v rustc'
   ```

   **`rustc` must live where `cargo` does**, and that is the second question the
   script asks: cargo resolves its compiler through `PATH`, and an absolute
   `WORKER_CARGO` is declared precisely because the bare name is not on the
   `PATH` in question — so a node with cargo and no visible rustc would pass a
   naive check and then fail mid-compile. A rustup or nix-darwin toolchain puts
   both in one directory and needs nothing extra. The script also refuses a
   cargo that does not exec at all (a rustup shim with no default toolchain).

   `WORKER_CARGO` rides in the node's run spec and its **directory** leads the
   launchd agent's `PATH`, because the node's own self-refresh compiles too and
   the daemon's `PATH` is the agent's — the declaration alone would leave every
   refresh failing to find `rustc`. `WORKER_BUILD_DIR_<node>` moves the tree and
   target directory it builds in (default `~/chuggernaut-worker/build`, kept
   between deploys so a rebuild is incremental — budget a few GB).

2. **The docker socket the mac actually has.** The daemon defaults to
   `/var/run/docker.sock`, which was correct when it *was* a container with that
   bind mount; colima listens at `~/.colima/default/docker.sock`, and the air
   answered every launch with `backend unavailable: Socket not found` until the
   endpoint was written down. `build-worker.sh` now **derives** it from the
   node's own `docker context inspect` and writes `WORKER_DOCKER_ENDPOINT` into
   the run spec, so an ordinary mac needs no declaration —
   `WORKER_DOCKER_ENDPOINT_<node>=unix:///path/to/docker.sock` overrides it, and
   an absent socket refuses the deploy rather than producing a node that
   announces slots and fails every launch. **The derived value is a snapshot**:
   change the node's docker context afterwards and the daemon keeps dialling the
   old socket until the node is re-converted.

3. **That docker must be *running*, because it stages one of the artifacts.**
   `chuggernaut-channel` is injected into agent containers and never runs on the
   mac, so it comes out of the worker image the node's own docker just built —
   not out of the native compile, whose Mach-O copy is what left every agent on
   the air without `update_status` or `submit_eval` (#440's
   [2026-08-08 correction](../../docs/design/440-native-worker-daemon.md#correction-2026-08-08--the-correction-above-generalised-over-two-binaries-with-opposite-platforms-job-480)).
   The deploy also reads the container architecture off that docker to check the
   staged file against it:

   ```sh
   ssh <node> "docker version --format '{{.Server.Arch}}/{{.Server.Os}}'"
   # colima on an arm mac answers: arm64/linux
   ```

   An empty or non-Linux answer **refuses the deploy** rather than installing an
   unchecked binary — guessing `arm64` because the mac is one is how a
   `linux/amd64` colima would ship the same silent failure with a green deploy.

**A host-only mac needs no docker.** `WORKER_MODES` is what decides it, and
`build-worker.sh` reads it with the daemon's own rule (`serves_container` in
`crates/worker/src/daemon.rs`: names `container`, or names nothing at all). A
node that names only `host` gets **no** socket check, **no** agent images, **no**
container-platform probe and **no** `chuggernaut-channel` binary — because
`local_backend` returns its host backend before it ever opens a docker endpoint,
a host job type cannot declare an `image:`, and the channel binary is injected
into *agent* containers only (`Core::channel_mcp`), while host mode serves
`work.type: command` alone. On Darwin the daemon is compiled natively, so
nothing is left that needs docker and **the worker image is skipped too**.

Two consequences worth knowing before you convert one:

- **The daemon logs one warning at boot** — `channel binary unavailable` — and
  carries an empty artifact map. That is the correct state: only a
  `FileSource::LocalArtifact` launch reads it, and a command-only node never
  makes one.
- **On Linux the worker image is still built**, because #440 D6 holds there and
  that image is the only place a Linux node's daemon binary comes from. So
  "needs no docker at all" is a **Darwin** property; a host-only Linux node
  still needs a docker to build and extract from.

The host guard set is unchanged: `WORKER_SLOTS=1` and `WORKER_SLOTS_MAX=1` node
-wide (#309 §2 option (iii)), a creatable `WORKER_HOST_ROOT`, and the
supervision probe. Converting a node is an operator step — declare
`WORKER_MODES_<node>=host` in `chuggernaut.env` and re-run the deploy; nothing
in this repo converts one for you.

**Still open: the refresh disk pre-flight on a *container-capable* mac.**
`worker-refresh.sh`'s floor is measured on `WORKER_REFRESH_DISK_PATH`, default
`/` — which was the docker filesystem while the daemon was a container, and is
the boot volume now that it is native. dev-air measured **7.2GB free on `/`
against 76.3GB free inside colima**, so the guard refused a refresh
(deploy #486) over space the build would never have touched, and the knob's documented
remedy does not apply because that filesystem is not reachable from the mac at
all. A host-only mac sidesteps this — it runs no build to protect, so the
pre-flight is skipped outright — but a dual-mode or container mac still meets
it. Workaround: declare `WORKER_REFRESH_DISK_FREE_GB_MIN_<node>=0`, which turns
the guard off for that node alone. The argument for leaving it is in
`worker-refresh.sh` beside `DISK_PATH`.

**And a conversion is one-way.** #440 slice 6 deleted the `docker run` path, so
there is no scripted way back to a container daemon: a conversion that fails
leaves the node out of the fleet until it is fixed forward. Everything that can
refuse now refuses *before* the live daemon is touched — the toolchain, the
credential directory, the endpoint, the container platform, the write
permissions, and the staged binaries themselves — the daemon must run on the
node (`chuggernaut --version`) and the channel binary must be one an agent
container can exec — before anything is
installed — but the window between the supervisor bootout and the health probe
is real. **Drain the node first**
([`worker-capacity.md`](../../docs/reference/runbooks/worker-capacity.md) §4.1:
slots 0, wait for `occupied: 0`) and convert one you can afford to lose for the
length of a build.

**A mac converted before 2026-08-07 must be re-converted before the next prod
deploy.** The daemon runs the `worker-refresh.sh` **installed on the node**, and
a copy written by a pre-correction conversion still extracts the Linux binary
out of the worker image — so the next deploy that asks it to refresh renames an
ELF file over its working Mach-O daemon and kickstarts launchd, which is exactly
the 2026-08-06 failure. This applies to `gumbo-air-0`, whose daemon was built by
hand. Re-convert it (the command above), or drain it and `launchctl bootout
gui/$(id -u)/com.chuggernaut.worker` until you can.

**And a mac converted before 2026-08-08 is injecting a Mach-O
`chuggernaut-channel` into every agent container right now.** The exec fails
silently: Claude Code reports the `chuggernaut-channel` MCP server as `pending`
forever, so work tasks lose `update_status` and **agent evaluators fail
outright** — jobs #477 and #478 escalated on four "produced no output" failures
before the air was drained to 0 slots. Re-converting the node is the fix; the
next `build-worker.sh` run installs the image's `linux/<arch>` copy and refuses
if it is not one.

**Migrating a node that already has keys under `$HOME` — you do this by hand.**

`build-worker.sh` does **not** move them: moving a credential is privileged and
irreversible, and the old copy has to be *deleted* or the boundary is nominal —
neither is something a deploy script should do to a node on its own. It refuses
and names the commands instead. On a Linux node whose keys are still in the
login user's home:

```sh
ssh worksalot@gumbo-nuc-0 '
  sudo install -d -o root -g root -m 0700 /etc/chuggernaut/keys
  for f in worker.creds worker_git worker_git-cert.pub; do
    [ -e "$HOME/chuggernaut-worker/keys/$f" ] &&
      sudo install -o root -g root -m 0600 "$HOME/chuggernaut-worker/keys/$f" \
        /etc/chuggernaut/keys/$f
  done
  ls -l /etc/chuggernaut/keys                      # verify BEFORE deleting
  # then, and only then:
  rm -f "$HOME"/chuggernaut-worker/keys/worker.creds "$HOME"/chuggernaut-worker/keys/worker_git'
```

**Then drop the `WORKER_GIT_KEY` line that named the old path** — the `rm -f`
above deletes exactly the file it points at. Step 3 above used to instruct
`WORKER_GIT_KEY=$HOME/chuggernaut-worker/keys/worker_git`, so a node adopted
before this slice is likely still declaring it (bare or `_<node>`) in
`deploy/prod/chuggernaut.env` on the Mini. <!-- runtime --> Delete the line — the
default is now `/etc/chuggernaut/keys/worker_git` — or repoint it there.
`build-worker.sh` refuses a Linux node whose `WORKER_GIT_KEY` resolves under the
login user's home and outside the credential directory, naming both remedies,
rather than handing the daemon a run spec that names a key the migration just
deleted: a forgotten line is a loud stop, not a node that keeps serving jobs and
quietly stops updating.

Do it **before** the run that converts the node, not after: the deploy refuses
until it is done, and until the home copy is gone the node has the old boundary
with the new layout. A node still running the **containerized** daemon is
unaffected by the credential move until it is converted, but since #440 slice 6
its self-refresh **refuses** (`this daemon is running INSIDE a container …
REFUSING swap`) — the live daemon and its job containers are untouched and the
node stays on the SHA it has, so deploy it over ssh with `build-worker.sh` until
you convert it.

**The run spec is declared, not inherited.**

The file a human edits is **`deploy/prod/chuggernaut.env` on the Mini** <!-- runtime -->
(`~/chuggernaut/deploy/prod/chuggernaut.env`). It is gitignored, so this repo
holds only [`env.example`](env.example) — editing that changes nothing on any
node. Everything a `chug-worker` daemon runs with is composed from that file by
`build-worker.sh` at node (re)creation and written to the node's own environment
file (`/etc/chuggernaut/worker.env` on Linux, `~/chuggernaut-worker/worker.env`
on macOS); the supervisor hands the daemon that file on every start, so a value
survives a self-refresh because it is **declared there**, not because the swap
copies it forward (#440 D6/D7). Being written down is how a
value *survives*, and this file is the only place it is *declared* — a setting
that lives only in the daemon's own environment is gone the moment the node is
recreated without it,
and each of these fails quietly: `WORKER_CACHE_DIR` ⇒ caching off,
`WORKER_SLOTS` ⇒ the node boots at the daemon's default of 4,
`WORKER_REFRESH_GIT_URL` / `WORKER_GIT_KEY` ⇒ the node keeps serving jobs and
stops updating.

Declare **per node**: `WORKER_*_<node>` wins over the bare `WORKER_*` (the node
is `CHUG_WORKER_NODE`). A fleet's nodes do not share paths, so one value cannot
be true of both:

```sh
# on the Mini, in deploy/prod/chuggernaut.env
WORKER_SLOTS_air=2
WORKER_SLOTS_nuc=2
WORKER_CACHE_DIR_air=/Users/<you>/chuggernaut-worker/sccache
WORKER_CACHE_DIR_nuc=/var/cache/chuggernaut/sccache
WORKER_REFRESH_GIT_URL=ssh://git@100.116.243.42:2222/<owner>/chuggernaut.git
WORKER_GIT_KEY_air=/Users/<you>/chuggernaut-worker/keys/worker_git
# nuc needs no WORKER_GIT_KEY: /etc/chuggernaut/keys/worker_git is the default
```

Read a node's **live** values off the node before declaring them — what it is
running is not what anyone wrote down:

```sh
ssh <node> 'cat /etc/chuggernaut/worker.env'      # a converted node
ssh <node> 'docker inspect chug-worker --format "{{range .Config.Env}}{{println .}}{{end}}"' \
  | grep '^WORKER_'                               # one still running the container
```

A colima node's `WORKER_CACHE_DIR` must sit under a prefix colima shares into
the VM (the mac home by default): `dockerd` runs inside the VM, so a path
outside any shared prefix binds a VM-local directory that dies with the VM while
the mac-side path never appears at all.

**Drift is reported, both ways round.**

- `build-worker.sh` compares the node's own environment file with the run it is
  about to compose and **refuses**, live daemon untouched, when the new run
  would drop a setting the node is running — including one this script never
  forwards (the daemon reads more `WORKER_*` than the run spec composes;
  `WORKER_REFRESH_SCRIPT` is one). Declare it, or pass `WORKER_SPEC_DROP_OK=1` to
  drop it on purpose. Both are loud; neither is silent. A node that still runs
  the container daemon has no environment file yet, so the live container's
  environment is read instead — the conversion is exactly the recreate this
  guard exists for, and it says which side it read. The file is installed
  **`0644`** — it carries paths, URLs and settings and no secret, it only
  *names* the credential — so the `cat` above works as the login user and so
  does the guard on the next deploy. A file the guard cannot **read** is a third
  case, distinct from one that is not there, and it **refuses** rather than
  reading as a fresh node: a guard blind to the declaration is not a guard that
  passes.
- On the no-ssh path nothing can push this file to a node, so each refresh
  **reports the node's own spec** on stdout, which the daemon relays into the
  deploy's task output (`worker-refresh: run spec on air (build): …`, plus a
  `WARNING` line for an unset cache dir or capacity). Compare that against the
  file above; there is no UI for it and none is needed.

**The consequence, stated plainly:** `WORKER_SSH` is unset for both prod nodes —
Tailscale SSH blocks tagged→tagged, so the Mini cannot reach either and
`build-worker.sh` no-ops on every deploy (the air's images are built from the
operator laptop). Nothing scheduled ever re-applies `chuggernaut.env` to a prod
node. The declaration is what a human recreates a node *from*, and the refresh's
report is how they see what a node is running *meanwhile*. Fixing the routing is
its own job.

The laptop that *can* reach a node does not have `chuggernaut.env` — it lives on
the Mini. Fetch it before rebuilding one, so the node is recreated from the
declaration rather than from whatever is remembered:

```sh
scp gumbo-mini-0:chuggernaut/deploy/prod/chuggernaut.env /tmp/chug.env
set -a; . /tmp/chug.env; set +a
WORKER_SSH=worksalot@dev-air.tail20c474.ts.net CHUG_WORKER_NODE=air \
  deploy/prod/build-worker.sh
```

Notes:

- **Cross-node addressing** — job containers on the nuc reach the Mini's NATS
  and git front via the Mini's **tailnet IP**, not `host.docker.internal`:
  `NATS_URL_CONTAINER=nats://100.116.243.42:4222`,
  `REPO_URL_BASE=ssh://git@100.116.243.42:2222` (ports 4222/2222 are published
  on all interfaces by compose).
- **CD** — `update.sh` calls `build-worker.sh`: worker+agent images build
  natively on the node (context streamed over ssh from the deployed SHA), the
  daemon binary is extracted from the worker image (**Linux**) or compiled on
  the node with its declared `WORKER_CARGO` (**macOS**, see "Converting a mac"
  above), installed, and the supervisor is asked to restart. Safe mid-job: job
  containers survive and the dispatcher's poll-based wait re-attaches.
- **Verify placement** — during a job: `ssh worksalot@gumbo-nuc-0 docker ps`
  shows the work/eval containers; the Mini's colima shows none
  (`docker ps --filter label=chuggernaut.managed`).
- **Capacity is changed from the UI, not here** —
  [`docs/reference/runbooks/worker-capacity.md`](../../docs/reference/runbooks/worker-capacity.md) is
  the reference for all of it. The node owns its capacity and the scheduler reads
  exactly one number per node: the one the node reported (spec §3.1). To change
  it, use the Cluster page's per-node stepper (or
  `PUT /api/v1/platform/fleet/{node}/capacity`) — **no ssh, no rebuild, no
  dispatcher restart**. Editing the `DOCKER_NODES` seed still does nothing to a
  node that has reported. Prod runs **air 2 / nuc 2** (verified 2026-07-26);
  confirm from the fleet snapshot (`GET /api/v1/platform/fleet` → `nodes`), never
  from `DOCKER_NODES`, and read `capacity_source` while you are there — `seed`
  means the node has never reported its own number.

  What follows is the **bootstrap** story only: `WORKER_SLOTS` is the value a
  node *starts* at before any operator intent exists. Set it at (re)creation —
  prod reaches air over the no-ssh self-refresh path, whose swap inherits the
  *live daemon's* env, so it survives every deploy:

  ```sh
  # from a machine that can ssh the node; the node's spec comes from the file,
  # so only the destination is typed (WORKER_SSH is never resolved per node).
  set -a; . deploy/prod/chuggernaut.env; set +a
  WORKER_SSH=worksalot@dev-air.tail20c474.ts.net CHUG_WORKER_NODE=air \
    deploy/prod/build-worker.sh
  ```

  After a swap the node reports that boot value until the dispatcher reconciles
  its recorded intent back onto it — one scan tick, seconds of small over- or
  under-cap. `WORKER_SLOTS_MAX` (default: the node's CPU count) is the ceiling a
  capacity command is validated against; it rides the same `<VAR>_<node>`
  resolution and is written into the node's environment file beside
  `WORKER_SLOTS`, so a lowered ceiling survives deploys.

  A **fresh** install writes `|worker|0` above because its dispatcher and its
  daemons are built from the same SHA, so observed capacity is arriving from the
  first probe. Zeroing the seed on an **existing** deployment is a sequenced
  change with a precondition — do not do it from memory; follow
  [the runbook §6](../../docs/reference/runbooks/worker-capacity.md).
- **KVM for Android emulator work is a per-node opt-in** —
  [`docs/reference/runbooks/worker-kvm.md`](../../docs/reference/runbooks/worker-kvm.md) is the
  procedure. `WORKER_KVM`, `WORKER_KVM_PROJECTS`, `WORKER_ANDROID_SDK_DIR` and
  the optional `WORKER_FLUTTER_DIR` and `WORKER_JDK_DIR` (further toolchain
  leaves at `/opt/flutter` and `/opt/jdk`; unset ⇒ no mount and no
  `FLUTTER_ROOT` / `JAVA_HOME`) go
  on the daemon at (re)creation like `WORKER_SLOTS`. A natively supervised
  daemon needs no `--device` alongside them: its own view of the node is the
  node's, so it sees `/dev/kvm` if the node has one and refuses to start if it
  does not. The self-refresh swap carries no device at all any more (#440 slice
  6): a node still running the container daemon refuses its own swap and is
  converted with `build-worker.sh` instead.
- **`WORKER_MODES` declares the runtimes a node offers** (design #309 P0, #322
  W1) — `container` (the default, and what the whole fleet runs) and/or `host`.
  It rides the same `<VAR>_<node>` resolution as `WORKER_SLOTS`, and it survives
  a self-refresh by being written into the node's environment file, not by the
  swap copying it forward (#440 D6/D7). Declaring `host` now **routes** work: since
  #309 P2 (jobs #483, #484) a node's modes ride its ping and announce, and the
  dispatcher places a host launch — one carrying no image — only onto a node
  advertising `host`, with no pin needed. What still gates real host work is
  that no node in this fleet names it and no job type declares
  `runtime.mode: host`; both are operator steps, not code. It **is** additive
  since #309 P1 (job #479): a node naming both constructs both backends and
  routes each launch by whether it carries an image, and one naming only `host`
  needs no Docker and refuses any launch that carries one. What every node naming
  `host` still pays is capacity — it refuses to boot below `WORKER_SLOTS=1` +
  `WORKER_SLOTS_MAX=1`, node-wide, one task at a time of either kind. Both are
  declared in `chuggernaut.env` and forwarded by `build-worker.sh`, which refuses
  the deploy — live daemon untouched — when either is anything else, naming the
  one that is wrong. An unset ceiling is refused with them: the daemon defaults
  it to the node's CPU count.
- **A host node's task root is `WORKER_HOST_ROOT`, and a mac has to declare
  one** (design [#322](../../docs/design/322-macos-native-runtime.md) W2). The
  daemon defaults it to `/var/lib/chuggernaut/host-tasks` and creates it while
  constructing its host backend at boot, so a root the daemon's user cannot
  create is a boot failure the supervisor loops — and on macOS `/var/lib` is
  root-owned under a sealed read-only root volume, which makes the default that
  case. `build-worker.sh` forwards it per node like every other knob (unset
  stays unset) and refuses the deploy, live daemon untouched, when the node
  cannot create the root it would use. It asks **the user the daemon will run
  as**, which differs by platform: the systemd unit sets `User=root`, so a Linux
  node is probed unprivileged and then through `sudo -n` like every other
  directory this script provisions, while launchd runs the mac agent in the
  login user's GUI domain, so there the login user's own answer is the whole
  answer. The two refusals name different remedies for that reason — a Linux one
  never sends the root daemon's credential tree into the home of the user that
  is in the `docker` group. Every wire path a task names — `/workspace`,
  `/chuggernaut` — is rebased under that root, one directory per task.
- **Per-task nix GC roots are the same shape of per-node opt-in** (spec §3.1,
  [the runbook §7](../../docs/reference/runbooks/worker-kvm.md)). `WORKER_NIX_GCROOTS_DIR`
  turns them on; `build-worker.sh` provisions the directory and checks the node
  has a store, a profiles tree and a nix daemon socket, which a native daemon
  reaches directly — the four bind mounts it used to compose are gone with the
  container. On a node with KVM on it also requires the toolchain path to
  **resolve into the store**, which is what the daemon's own boot check demands.
  The swap carries no mount forward either — there is none left to carry.
  Without roots a task holds store paths no GC root protects.
- **`WORKER_NIX_PROJECTS` is the grant that lets a PROJECT declare its own
  toolchain** (spec §3.1, [the runbook §8](../../docs/reference/runbooks/worker-kvm.md)):
  an allow-listed project's `runtime.env` is realised here from its job branch
  and put on the task's `PATH`. Empty grants nobody, granting it grants
  *evaluation* of that project's flake in the node's daemon process, and the environment
  must already be substituted on the node — the realise is capped at 45s, so an
  unwarmed toolchain fails the launch rather than running slowly.
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
- **The deploy keeps the paper trail (#270).** Three things make a failed
  refresh diagnosable from the deploy job alone:
  1. **Per-node transcript recap.** The fan-out keeps each node's CLI
     transcript in a `mktemp` dir; before deleting it, `update.sh` copies a
     bounded tail of each node's `.log` (and `.cancel`) into the deploy's own
     stdout, one node at a time, labelled `update: [<node> log] …`. That is
     the interleaved live stream sorted back into per-node stories.
     Overridable per deploy with `WORKER_REFRESH_TRANSCRIPT_LINES_MAX` /
     `_COLS_MAX` — a bound, not a dump: this stdout becomes the task record.
  2. **The daemon actually logs.** The daemon runs with
     `RUST_LOG=info,async_nats=warn` (set by `build-worker.sh` at node creation
     as `WORKER_RUST_LOG` and written into the node's environment file, so every
     start reads it). Without it the binary's tracing default is `error` and the
     daemon's log says *nothing* about a refresh — the silence deploy #267 was
     reconstructed around. An override survives a refresh because it is
     declared, not because the swap copies it.
  3. **The supervisor keeps the swap's own record.** The swap installs a binary
     and restarts the unit (#440 D6), so a replacement that will not start says
     why in `journalctl -u chug-worker` (Linux) or the launchd agent's
     `StandardOutPath` (macOS) — where the retained `chug-worker-swap` sibling
     used to hold it. This one *cannot* reach the deploy job: the daemon that
     reports to the dispatcher is the very thing being replaced.

  On the dispatcher side, the deploy Work task's `stdout.log` artifact is now
  harvested on the exit paths a self-deploy actually takes — a container found
  already exited by restart reconciliation (§3.6), and a task the
  `task_timeout` scan kills — not only when a live monitor sees the exit.

### Fast image builds (BuildKit dependency caching, #115)

The worker image build is the slow leg of a deploy: `Dockerfile.worker` compiles
the Rust workspace's binaries, so a cold Docker build rebuilds all dependencies
on every SHA change (~10 min observed on dev-air). It uses **BuildKit `RUN
--mount=type=cache`** mounts for the cargo registry/git and the compiled target,
so a SHA bump recompiles only the changed crates. `CHUG_GIT_SHA` is placed
*after* the dependency layer so a SHA-only change never invalidates the deps.

`Dockerfile.agent-rust` used to compile the workspace too, to bake a warm-target
seed; #352 deleted that (below), so it now only fetches binaries and has no
cache mounts.

- **Enable BuildKit on the node.** `build-worker.sh` and `worker-refresh.sh`
  both prefix the build with `DOCKER_BUILDKIT=1`, which turns on the engine's
  built-in BuildKit — **no `buildx` CLI plugin is required** for cache mounts.
  If the node's build log still shows the *legacy* builder ("Install the buildx
  component…"), the engine is too old for BuildKit; on colima, `colima start`
  with a recent Docker engine (23+) has BuildKit available. Cache mounts are
  ignored (build stays cold, still correct) on a legacy builder, so this
  degrades safely.
- **Cache safety.** The registry/git caches are shared-safe (cargo locks them);
  the worker's target cache uses a per-image `id` (`chug-worker-target`) with
  `sharing=locked` so concurrent node builds never collide.
- **The warm-target seed is gone** (#123, #347, deleted by #352). `agent-rust`
  baked a prebuilt `target/` on the premise that sccache could not cover the
  dependency graph. #350 made sccache native on arm64 — before it, this
  workspace's Rust units had never once been cached on air — and #352 then
  measured the two head to head on air against the real `.chug/tasks/ci.sh`
  command set (`cargo clippy --workspace --all-targets` + `cargo test
  --workspace --no-run`), same source, nothing else on the node:

  | arm | target dir | clippy | test `--no-run` | total | Rust cache hits |
  |---|---|---|---|---|---|
  | A — seeded (status quo) | 2.1GB baked seed | 16s | 125s | **141s** | 0 / 14 |
  | B — unseeded, cold cache | empty | 44s | 231s | **275s** | 0 / 674 |
  | C — unseeded, warm cache | empty | 22s | 164s | **186s** | **674 / 674** |

  Clippy caches (100% hits, 22s against 44s cold), so the residual 45s of A-vs-C
  is linking, which no compiler cache covers. That 45s/task was buying a 2.26GB
  layer in every image, ~600s on air's `worker-refresh` leg, and the bulk of the
  ~32GB refresh disk peak — against a ~479MB node cache that is *shared* by all
  four concurrent containers rather than copy-on-written per container, and that
  stays warm between deploys instead of going stale until the next image
  rebuild. Deleted.
- **The target path must stay literal and stable.** sccache's hash covers the
  target-derived paths cargo passes rustc (`-L dependency=$CARGO_TARGET_DIR/…`).
  Measured in the same run: identical source and cache, cold target both times,
  **same path 100% hits / 22s, different path 0% hits / 54s**. `agent-rust`
  bakes `CARGO_TARGET_DIR=/opt/chug-cargo-target` — out-of-tree so a ~10GB
  `target/` never lands in the clone agents commit from, and a literal so the
  node cache keeps working. A per-container path would silently turn it off.
- **The cache is now the only reuse there is.** `.chug/tasks/ci.sh`'s sccache
  liveness guard used to degrade to the seed; it now degrades to a fully cold
  compile (arm B: +89s), so both the guard and a 0%-hit run shout in the `!!!`
  idiom rather than logging a quiet warning.
- **A refresh deliberately does NOT pre-warm the node cache.** Running one build
  after the swap to populate `SCCACHE_DIR` was considered (#352 item 5) and
  rejected on its own numbers: it would cost 186–275s on *every* refresh of
  *every* node to save the +89s a cold cache costs *once*, which is most of the
  refresh time item 1 just reclaimed. `SCCACHE_DIR` is a host dir that survives
  deploys, so cold is the rare case — a new node, a toolchain or dependency
  bump, or an evicted cache — and the first job after one warms it for the rest.
  Revisit if the `!!!` 0%-hit line starts showing up in ordinary job logs.
- **Both downloaded binaries are arch-selected** (#347). `sccache` and
  `nats-server` pick their tarball from `dpkg --print-architecture`, because the
  fleet is mixed (gumbo-nuc-0 amd64, dev-air's colima arm64) and a foreign-arch
  binary runs under qemu rather than failing — an emulated `RUSTC_WRAPPER` on
  every rustc call, and an emulated `nats-server` under the tier-2 tests
  `.chug/tasks/ci.sh` reports as executed. An unrecognised arch fails the build.
  `deploy/prod/agent-rust-image.test.sh` asserts both statically — along with
  the absence of any workspace build and the `CARGO_TARGET_DIR` shape above. It
  is the only gate that reads `Dockerfile.agent-rust` at all: nothing in CI
  builds a container image, and agent reviewers run read-only (spec §4.3).
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
- **Fast path**: after a `web` job (.chug/jobs/web.yaml) merges, release a
  `web-publish` job (.chug/jobs/web-publish.yaml). It builds `web/dist` at main and <!-- runtime -->
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
#    (§6.6); that's what .chug/tasks/deploy-health.sh gates on and restart-verify.sh
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

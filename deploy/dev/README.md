# Dev stack — full platform on one machine (Docker Desktop)

> For the always-on Mac Mini deployment (launchd services, auto-deploy on green
> `main`, hourly R2 backups), see [`deploy/prod/README.md`](../prod/README.md).

Layout: NATS (operator mode) and the SSH front run in containers; the
dispatcher and api run on the host; agent containers are launched by the
dispatcher as siblings and reach NATS/SSH via `host.docker.internal`.

All state lives under `deploy/dev/data/` (gitignored): `data/keys` (§12.1 <!-- runtime -->
keypairs) and `data/repos` (bare repos, bind-mounted into the ssh container).

## One-time bootstrap

```sh
cd v2
cargo build --release
alias chug=target/release/chuggernaut

# 1. Generate keys (the NATS step fails on the first run — the server isn't
#    up yet and needs the resolver conf this generates; that's expected).
chug init --keys-dir deploy/dev/data/keys --repos-root deploy/dev/data/repos || true

# 2. Build images + extract the linux channel binary to deploy/dev/out/.
deploy/dev/build.sh

# 3. Boot NATS + sshd.
docker compose -f deploy/dev/compose.yaml up -d

# 4. Finish init (topology, VAPID key, admin user) — idempotent.
chug init --keys-dir deploy/dev/data/keys --repos-root deploy/dev/data/repos \
  --admin-email you@example.com --admin-password changeme

# 5. Create a project. --hook-bin points at the binary INSIDE the ssh
#    container (the pre-receive hook runs there, not on the host).
chug admin --keys-dir deploy/dev/data/keys project create \
  --owner acme --name demo \
  --repos-root deploy/dev/data/repos \
  --hook-bin /usr/local/bin/chuggernaut

# 6. Agent containers need Claude credentials. The provider-credential names
#    under the platform scope `global/agents` (CLAUDE_CODE_OAUTH_TOKEN,
#    ANTHROPIC_API_KEY) reach every agent container (work agents and agent
#    evaluators) — set one once, no per-project or per-job-type declaration
#    needed; any OTHER name under that scope is declined (design #529 S1b).
#    Subscription auth via `claude setup-token`. Reads from stdin.
claude setup-token | tail -1 | chug admin --keys-dir deploy/dev/data/keys \
  secret set --project global/agents --name CLAUDE_CODE_OAUTH_TOKEN
```

### Clone cost: two server-side prerequisites

Task containers clone with `--single-branch --filter=blob:none`
(`container::bootstrap_cmd`) — each job runs one clone per task, so the flags
are the whole cost story. Both flags depend on server-side setup, and both
degrade quietly if it is missing:

1. **`uploadpack.allowFilter`** on the bare repo. `project create` sets it on
   new repos; repos created before that landed need it once, or the filter is
   ignored and the full blob history ships anyway:
   ```bash
   git -C deploy/dev/data/repos/acme/demo.git config uploadpack.allowFilter true
   ```
2. **git protocol v2 through the SSH front** — `AcceptEnv GIT_PROTOCOL` in
   `sshd_config` (already set; guarded by a test). git adds the client half
   (`-o SendEnv=GIT_PROTOCOL`) on its own. Without it upload-pack runs v0 and
   refuses the promisor remote's follow-up fetch: the clone reports success and
   the workspace checks out **empty**.

If a job's workspace is mysteriously empty, check these two first.

3. **"repository corruption on the remote side" that isn't**: cloning a bare
   repo locally on the host (`git clone data/repos/...`) hardlinks loose
   objects, and Docker Desktop's VirtioFS can serve corrupt content for
   hardlinked files *inside the container* even when the host-side repo
   fscks clean. Always clone with `--no-hardlinks` when seeding by hand; if
   it has already happened, `git -C <bare> repack -ad && git prune-packed`
   replaces the loose objects with a fresh packfile and the container reads
   clean again. Rebuilding
the ssh image after changing `sshd_config`:

```bash
docker compose -f deploy/dev/compose.yaml up -d --build ssh
```

### Job artifacts (transcripts, logs)

Every agent run's Claude session transcript and every container's stdout/stderr
are captured after exit, gzipped, age-encrypted, and stored in a NATS object
store. The web UI serves them per task (transcript viewer + logs on the job
detail page); channel `update_status`/`reply` posts show as a timeline built
from the event stream.

Two operational notes when **updating an existing deploy**:

- **Re-run `chug init`** to generate `data/keys/age_artifacts.key` — a *separate*
  age key from the secrets one, held by both the dispatcher (encrypt) and the
  api (decrypt for display). `init` is idempotent and only creates missing
  keys; without this key, capture is silently off and the artifact routes 404.
- **Re-run `deploy/dev/build.sh`** after any change to the channel binary. It
  now posts `update_status`/`reply` over `req.channel.*` instead of writing KV
  directly, and the dispatcher no longer grants the old KV-write permission — a
  stale `deploy/dev/out/chuggernaut-channel` will fail those calls at runtime. <!-- runtime -->

## Run

Dispatcher (one terminal):

```sh
cd v2
NATS_URL=nats://localhost:4222 \
NATS_URL_CONTAINER=nats://host.docker.internal:4222 \
REPOS_ROOT=$PWD/deploy/dev/data/repos \
REPO_URL_BASE=ssh://git@host.docker.internal:2222 \
KEYS_DIR=$PWD/deploy/dev/data/keys \
CHANNEL_BINARY=$PWD/deploy/dev/out/chuggernaut-channel \
HOOK_BIN=/usr/local/bin/chuggernaut \
AGENT_PROVIDER_DEFAULT=claude \
AGENT_MODEL_DEFAULT=claude-haiku-4-5 \
target/release/chuggernaut dispatcher
```

API + UI (another terminal; `npm run build` in `web/` first):

```sh
cd v2
NATS_URL=nats://localhost:4222 \
KEYS_DIR=$PWD/deploy/dev/data/keys \
UI_DIST=$PWD/web/dist \
target/release/chuggernaut api
```

Browse http://localhost:8080 and log in with the admin user.

## Backups

Everything on this box lives in `deploy/dev/data/` (repos, keys) plus the <!-- runtime -->
`nats-data` volume (JetStream). One command snapshots all of it:

```sh
deploy/backup.sh     # → deploy/dev/data/backups/chug-backup-<ts>.tgz
```

Per-project verified `git bundle --all` files, a consistent `nats account
backup` (jobs, tasks, users, secrets, events, artifacts), and the keys dir —
with `RESTORE.md` inside the tarball. The tarball lands on the same disk:
**ship it offsite** (`rclone`/`scp`/S3) and schedule it, e.g.:

```
0 * * * * cd ~/chuggernaut/v2 && ./deploy/backup.sh >> /tmp/chug-backup.log 2>&1
```

Keys are the crown jewels — the encrypted state in a backup is unreadable
without them, so anywhere the tarball goes, it carries them: treat backup
storage with the same care as the keys dir itself.

## Job types

Job types live in the project repo (`.chug/jobs/*.yaml` on the default branch).
Seed the repo through the SSH front or clone the bare repo directly:

```sh
DEV=~/chuggernaut/deploy/dev
git clone $DEV/data/repos/acme/demo.git /tmp/demo && cd /tmp/demo
mkdir -p .chug/jobs .chug/prompts/work
cp $DEV/jobs-hello.yaml .chug/jobs/hello.yaml
cp $DEV/prompt-hello.md .chug/prompts/work/hello.md
git add . && git commit -m "add hello job type" && git push origin main
```

`jobs-hello.yaml` in this directory is a known-good first agent job:
`image: chuggernaut/agent:dev`, agent work, a command evaluator.

## Notes / gotchas

- **Boot order matters once**: keys before compose (the NATS container mounts
  `nats-resolver.conf`), compose before the rest of init.
- `GIT_UID` build arg on `Dockerfile.ssh` defaults to 501 (macOS first user)
  so the container's git user can write the bind-mounted repos. Override to
  your uid: `docker compose build --build-arg GIT_UID=$(id -u) ssh`.
- The dispatcher and api read `data/keys/dispatcher.creds` automatically; the
  admin CLI needs `--keys-dir` pointed at the same directory.
- Recreating the NATS volume (`docker compose down -v`) wipes all platform
  state; re-run init step 4.

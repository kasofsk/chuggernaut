# Ad-hoc (out-of-band) deploy runbook

**Audience:** the on-call operator, mid-incident, when the normal `deploy` job
(`.chug/jobs/deploy.yaml` → `.chug/tasks/deploy.sh` → `deploy/prod/update.sh`) cannot run or
cannot finish, and you must ship or repair prod **by hand**.

This is a checklist, not an essay. Two hard rules first, then the interventions,
then a worked example.

> **The whole point of this page:** every manual deploy action leaves the same
> paper trail a normal deploy does — a `deploy`-type job, resolved with a
> machine-shaped record. See [§2](#2-the-paper-trail-non-negotiable). No
> exceptions; a genuine emergency only lets you file the record *after* instead
> of *before*.

Background you should not re-derive: [`deploy/prod/README.md`](../../deploy/prod/README.md)
(the standing-instance runbook — CD, the deploy legs, BuildKit caching),
[`spec.md` §1.2](../../spec.md) (claims — human-performed work attempts) and
`spec.md` §3.1 (worker nodes, self-refresh, node-local caching).

---

## 0. First: is a manual deploy actually warranted?

The normal path self-heals more than it looks like it does. Confirm the failure
is real before reaching for a shell:

- **A worker node reports drift but the deploy "passed"** — a credential-less
  node logs `refresh SKIPPED — no git credential` and the deploy still goes
  green (`update.sh` step 3). That is not a failed deploy; it is a node that
  never refreshed. Fix the node (§1c), don't re-run the deploy.
- **Dispatcher crash-looping while the api answers HTTP** — the api can serve
  `dispatcher unavailable: no responders` for a long time next to a dead
  dispatcher. That is §1d, not an api problem.

If the normal `deploy` job can still reach `main` and run, prefer it. Reach for
this page only when the toolchain, a daemon, the dispatcher, or an image is
broken enough that `update.sh` cannot do the job.

---

## 1. Interventions, by symptom

Each block: **when → do → verify.** Run them from a **laptop/operator machine on
the tailnet** that can SSH the node (the dispatcher host itself cannot SSH a
tagged worker — Tailscale blocks tagged→tagged, which is why prod normally uses
the no-SSH self-refresh RPC). Load prod config first:

```sh
cd <chuggernaut checkout>
set -a; . deploy/prod/chuggernaut.env; set +a   # WORKER_*, NATS_*, KEYS_DIR, …
```

### 1a. Node build toolchain broken — BuildKit / `buildx` missing

**When:** a node's image build errors `the --mount option requires BuildKit` or
`Install the buildx component to build images with BuildKit`, or silently builds
cold. The image Dockerfiles (`deploy/prod/Dockerfile.worker`,
`deploy/prod/Dockerfile.agent-rust`) use `RUN --mount=type=cache` mounts (#115),
so **the build needs BuildKit** — without it the deps recompile on every SHA.

The build scripts request in-daemon BuildKit with `DOCKER_BUILDKIT=1`
(`build-worker.sh`, `worker-refresh.sh`). On a **recent engine (Docker 23+)**
that is enough — **no `buildx` CLI plugin is required**, and on an engine too old
for BuildKit the mounts are simply ignored (build stays cold but correct). So:

- **colima nodes:** `colima start` on a recent engine has BuildKit; nothing to
  install.
- **A macOS node without Homebrew** whose engine still errors "Install the
  buildx component" — drop the `buildx` CLI plugin in by hand (the cli-plugins
  binary drop; no brew needed):

  ```sh
  # On the node. Pick the asset matching its arch (darwin-arm64 / darwin-amd64).
  BUILDX_VER=v0.14.0
  mkdir -p ~/.docker/cli-plugins
  curl -fsSL -o ~/.docker/cli-plugins/docker-buildx \
    "https://github.com/docker/buildx/releases/download/${BUILDX_VER}/buildx-${BUILDX_VER}.darwin-arm64"
  chmod +x ~/.docker/cli-plugins/docker-buildx
  ```

**Verify:**

```sh
ssh "$WORKER_SSH" 'docker buildx version && docker info --format "{{.ClientInfo.Plugins}}" 2>/dev/null'
```

`docker buildx version` printing a version means the plugin is live; a cache-warm
rebuild (§1b) should then skip dependency compilation on the second run.

### 1b. Rebuild the worker/agent images and swap the daemon — `build-worker.sh`

**When:** a node is on stale images (daemon version drifts from the
dispatcher's), an agent image is missing/corrupt (image loss — e.g. restoring
`chuggernaut/agent-rust` on the nuc), or you just installed buildx (§1a) and need
a warm rebuild.

`deploy/prod/build-worker.sh` builds all three node images (`worker`, `agent`,
`agent-rust`) natively on the node — context streams over SSH via `git archive`,
so the node needs nothing but Docker and your authorized key — then restarts the
daemon on the new `worker` image. **Safe mid-job:** job containers survive and
the dispatcher's poll-based wait re-attaches (`spec.md` §3.1).

```sh
WORKER_SSH=worksalot@gumbo-nuc-0 \
CHUG_WORKER_NODE=nuc \
WORKER_NATS_URL=nats://100.x.y.z:4222 \
  deploy/prod/build-worker.sh
```

- `WORKER_SSH` — `user@node` the script SSHes (unset ⇒ the script no-ops for a
  single-node deploy).
- `CHUG_WORKER_NODE` — the daemon's `WORKER_NODE`; **must match its
  `DOCKER_NODES` entry** or the dispatcher won't schedule to it. Default `nuc`.
- `WORKER_NATS_URL` — **required**; the **tailnet** NATS URL of the dispatcher
  host (the tailnet-IP form of `NATS_URL_CONTAINER`), not `localhost`.
- Optional but usually wanted here: `WORKER_CACHE_DIR`,
  `WORKER_REFRESH_GIT_URL`, `WORKER_GIT_KEY` — the script forwards them to the
  daemon's `docker run` (see §1c for why they matter). All three normally come
  from `chuggernaut.env`. `WORKER_CACHE_DIR`'s host path must already exist on
  the node — this script does not create it, and since #379 a missing one fails
  every launch there (§1c).

The image tag is `CHUG_IMAGE_TAG` (default `prod`).

**Verify:**

```sh
ssh "$WORKER_SSH" 'docker ps --filter name=chug-worker --format "{{.Image}} {{.Status}}"'
# then confirm the dispatcher no longer warns about a drifting node version:
#   watch the dispatcher log / fleet snapshot — the node's ping version should
#   match the deployed SHA (spec §3.1, #109).
```

### 1c. Stranded or stale worker daemon — the full `docker run` recipe

**When:** the daemon is wedged, was started without the cache/refresh env, or you
need to (re)start it standalone without a full image rebuild. This is the exact
`docker run` `build-worker.sh` issues — reproduce it by hand:

Passing `WORKER_CACHE_DIR`? Provision the host path first, on the node —
nothing else creates it since #379, and every launch fails without it (below):

```sh
ssh "$WORKER_SSH" 'sudo mkdir -p /var/cache/chuggernaut/sccache'
```

```sh
ssh "$WORKER_SSH" '
docker rm -f chug-worker >/dev/null 2>&1 || true
docker run -d --restart=always --name chug-worker \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v $HOME/chuggernaut-worker/keys:/data/keys:ro \
  -e WORKER_NODE=nuc \
  -e NATS_URL=nats://100.x.y.z:4222 \
  -e NATS_CREDS=/data/keys/worker.creds \
  -e WORKER_REFRESH_GIT_URL=ssh://git@100.x.y.z:2222/<owner>/chuggernaut.git \
  -e WORKER_GIT_KEY=/data/keys/worker_git \
  -e WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache \
  -e RUST_LOG=info,async_nats=warn \
  chuggernaut/worker:prod'
```

Three env details are load-bearing:

- **`RUST_LOG`.** Omit it and the daemon logs **nothing**: the binary filters on
  `RUST_LOG` and its default directive is `error`, so `docker logs chug-worker`
  shows no "worker up", no refresh phase markers, and none of the relayed
  `worker-refresh.sh` output — the silence that made deploy #267 a live
  post-mortem across three hosts (#270). `info` is where those lines are;
  `async_nats=warn` keeps a reconnect storm from drowning them. `build-worker.sh`
  and the self-refresh swap both set this default now, so a daemon you start by
  hand is the only way back to a silent node.

- **`WORKER_CACHE_DIR` + sccache.** This is passed to the **daemon as env
  only** — there is **no sccache mount on the daemon itself**. The daemon reads
  it and bind-mounts that **host** path into every *sibling* job container it
  launches (at `/cache/sccache`) and sets `RUSTC_WRAPPER=sccache` there
  (`Dockerfile.agent-rust`), so cargo builds reuse compilation across jobs.
  **Unset ⇒ caching stays off** — this is the durable fix for the dormant cache
  (#55): the baked-in sccache only warms when the daemon actually runs with
  `WORKER_CACHE_DIR` set. **Set ⇒ the host path must already exist on the
  node**: since #379 it is a typed mount, so a missing source fails every launch
  on that node with the path in the error (`invalid mount config for type
  "bind": bind source path does not exist: …`) rather than silently giving each
  container an empty directory. **Nothing provisions it for you** — the daemon's
  own `create_dir_all` runs inside the daemon container, which does not mount
  that path (above), so it never touches the host; and dockerd no longer creates
  it as a side effect of the first launch, which is how it came into being
  before #379 (design #372 C3). Create it once per node —
  `ssh "$WORKER_SSH" 'sudo mkdir -p /var/cache/chuggernaut/sccache'` — or, if
  launches are already failing, fix the path or unset the variable.
- **No empty-string refresh env.** Give `WORKER_REFRESH_GIT_URL` (and
  `WORKER_GIT_KEY`) their **real** values, not an empty string. `build-worker.sh`
  passes `WORKER_REFRESH_GIT_URL=${WORKER_REFRESH_GIT_URL:-}` — **empty when
  unset — and the daemon then rejects every refresh request** ("no git
  credential"). A daemon you started by hand with an empty refresh URL is exactly
  the credential-less node that makes the *next* normal deploy log
  `refresh SKIPPED` and refresh nothing (#114). Set the real coordinates so this
  node can self-refresh on the next deploy and you never have to come back here.

**Verify:**

```sh
ssh "$WORKER_SSH" 'docker inspect chug-worker --format "{{.State.Status}} {{range .Config.Env}}{{println .}}{{end}}"' \
  | grep -E "running|WORKER_CACHE_DIR|WORKER_REFRESH_GIT_URL"
# WORKER_REFRESH_GIT_URL must be non-empty; status must be running.
```

### 1d. Dispatcher down / bad binary — `restart-verify.sh` by hand

**When:** the dispatcher is down or crash-looping after a build, or a deploy's
own health check couldn't run. `deploy/prod/restart-verify.sh` restarts the
dispatcher + api launchd services onto a target binary, **proves** the dispatcher
answered on NATS (`req.jobs.list` — a probe only a live dispatcher answers), and
**rolls back** to the previous binary (`$BIN.prev`) if it didn't. Run it **on the
Mini**:

```sh
ssh worksalot@gumbo-mini-0 \
  '~/chuggernaut/deploy/prod/restart-verify.sh <target-sha> <prev-sha>'
```

- It ignores `SIGHUP`, so the health check + rollback finish even if the SSH
  session drops.
- **Exit codes:** `0` healthy on the new build · `1` new build failed, **rolled
  back and healthy** on the old one (the deploy still failed, loudly) · `2`
  catastrophe — new build **and** rollback both unhealthy, prod is DOWN · `3`
  rollback impossible (no `$BIN.prev`). On `2`/`3` you are hand-restoring a good
  binary now.

**Verify:** the script's own transcript (`health: dispatcher answered on NATS …
healthy`) is the proof. Independently:

```sh
ssh worksalot@gumbo-mini-0 \
  'launchctl print gui/$(id -u)/com.chuggernaut.dispatcher | grep -E "state|pid"'
```

---

## 2. The paper trail (non-negotiable)

**No out-of-band deploy action without its deploy-record job.** The manual work
above is invisible to the platform unless you record it — and an unrecorded fix
is exactly the audit gap this runbook closes. A manual deploy is still a deploy;
it belongs in deploy history like any other.

**Before** the intervention (or, in a genuine emergency, **immediately after**):

1. **File a `deploy`-type job** on the platform (the normal `deploy` job type,
   `.chug/jobs/deploy.yaml`).
2. **Claim it** — `POST .../jobs/{seq}/claim` (`spec.md` §1.2). Claiming parks
   the job's work attempt as a **pending, human-performed task** (`performed_by:
   human`) instead of launching the deploy container. It **holds no fleet slot**
   (it is Pending, not Running) and is exempt from the task-timeout scan, so it
   can sit while you work and survives a dispatcher restart. `awaiting_human`
   carries `"claimed": true`.
3. **Do the manual work** — the interventions in §1.
4. **Resolve `Pass`** — `POST .../jobs/{seq}/tasks/{task_id}/resolve`. The `Pass`
   `summary` flows into the squash-merge commit body **exactly like an agent's
   `submit_result`**, and both `summary` and `structured` persist on the task
   record. (`deploy` is `wrap_up: none`, so `Pass` takes the job straight to
   Evaluation/Done with nothing merged — the branch is scratch.)

```jsonc
// POST .../jobs/{seq}/tasks/{task_id}/resolve
{
  "kind": "Pass",
  "summary": "Manual deploy: installed buildx on air, rebuilt nuc images, restarted daemon with cache env. main baac632→71eb28b.",
  "structured": { /* the leg-protocol record — see below */ }
}
```

The `summary` must answer: **commands run · nodes touched · from→to SHAs · why
the normal path couldn't work.**

### The `structured` deploy-record ("leg-protocol JSON")

`structured` is free-form JSON (`serde_json::Value`), so shape it like the deploy
`update.sh` runs — **one entry per leg, in `update.sh`'s leg order** (build →
worker refresh → restart-verify; see `crates/types/src/version.rs` and
`deploy/prod/README.md` §3). This makes a hand deploy machine-comparable with an
automated one:

```json
{
  "deploy": "manual",
  "reason": "stale worker daemon on nuc + missing buildx on air blocked no-SSH self-refresh",
  "from_sha": "baac632",
  "to_sha": "71eb28b",
  "nodes": ["gumbo-air-0", "gumbo-nuc-0", "gumbo-mini-0"],
  "legs": [
    { "leg": "node-toolchain", "node": "gumbo-air-0", "action": "installed docker-buildx cli-plugin", "result": "ok" },
    { "leg": "worker-images",  "node": "gumbo-nuc-0", "cmd": "deploy/prod/build-worker.sh", "result": "ok" },
    { "leg": "worker-daemon",  "node": "gumbo-nuc-0", "action": "docker run chug-worker with WORKER_CACHE_DIR + real refresh env", "result": "restarted" },
    { "leg": "restart-verify", "node": "gumbo-mini-0", "cmd": "restart-verify.sh 71eb28b baac632", "result": "healthy" }
  ]
}
```

The resolved job then appears in deploy history like any other deploy — a
human-resolved `deploy` renders in the Reports thread from its `Pass` summary,
distinguished by `performed_by: human`.

---

## 3. Worked example — 2026-07-23 buildx / stale-daemon incident

The exemplar this runbook exists to make routine. A normal `deploy` could not
self-refresh the fleet: the **air** node's Docker engine errored on the new
cache-mount Dockerfiles (`Install the buildx component`), and the **nuc** daemon
was stale and missing its agent image.

**What was done, in order:**

1. **File + claim a `deploy` job** for the SHA on `main`. It parks as a
   human-performed work task holding no fleet slot.
2. **air — toolchain (§1a):** no Homebrew, so dropped the `buildx` CLI plugin
   binary into `~/.docker/cli-plugins/docker-buildx`, `chmod +x`. `docker buildx
   version` confirmed.
3. **nuc — images (§1b):** `WORKER_SSH=worksalot@gumbo-nuc-0
   CHUG_WORKER_NODE=nuc WORKER_NATS_URL=nats://100.x.y.z:4222
   deploy/prod/build-worker.sh` — rebuilt `worker`/`agent`/`agent-rust`,
   restoring the missing agent image.
4. **nuc — daemon (§1c):** restarted `chug-worker` by hand with
   `WORKER_CACHE_DIR=/var/cache/chuggernaut/sccache` **and the real**
   `WORKER_REFRESH_GIT_URL` + `WORKER_GIT_KEY` (not empty), so the node self-
   refreshes on the next normal deploy.
5. **Mini — verify (§1d):** `restart-verify.sh <to> <from>` → `health: dispatcher
   answered on NATS … healthy`.
6. **Resolve `Pass`** with the `summary` and the leg-protocol `structured` above.

**Result:** the fix that would otherwise have lived only in an operator chat log
is now a first-class `deploy` record in history — commands, nodes, from→to SHAs,
and why the normal path couldn't work, all machine-shaped.

---

## See also

- [`deploy/prod/README.md`](../../deploy/prod/README.md) — the standing prod
  instance: CD, the deploy legs, BuildKit caching (#115), web-publish.
- [`spec.md`](../../spec.md) — §1.2 claims (human-performed attempts), §3.1
  worker nodes / self-refresh / node-local caching, §3.3 staged evaluation.
- Scripts referenced here: [`deploy/prod/build-worker.sh`](../../deploy/prod/build-worker.sh),
  [`deploy/prod/restart-verify.sh`](../../deploy/prod/restart-verify.sh),
  [`deploy/prod/worker-refresh.sh`](../../deploy/prod/worker-refresh.sh),
  [`deploy/prod/update.sh`](../../deploy/prod/update.sh).

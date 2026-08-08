# Converting a NixOS worker node to the native daemon

**Audience:** the fleet operator, holding ssh to the node with passwordless
`sudo` there, the Mini's `chuggernaut.env`, and a platform admin token. You have
a NixOS worker node whose daemon is still the `chug-worker` **container**, and
every deploy from now on fails on that node's refresh leg. This page converts
it, once, and says what to do when the unit does not come up.

**Its own page rather than a section of
[`chug-node-adoption.md`](chug-node-adoption.md).** That page is written for
whoever owns the node's NixOS configuration, adopting the modules once; its §4a
states the three-step order this procedure follows and stops there, correctly —
it is a configuration page. What is below is the operator's side of those three
steps: credentials that have to move, the one run-spec edit on the Mini that is
not optional, the deploy ordering, and the recovery when a node comes back with
no daemon. Different audience, different trigger (a failed deploy leg, not a
config change), so it is a different document. Read §4a for what the module
declares; read this to convert a node.

Not the design argument — that is
[design #440](../../design/440-native-worker-daemon.md), whose
[slice-7 correction](../../design/440-native-worker-daemon.md#correction-2026-08-07--slice-7-as-landed-job-475)
names this seam and leaves it to a runbook. For draining,
[`worker-capacity.md`](worker-capacity.md) §4.1; for converting a **mac**,
[`chug-node-adoption.md`](chug-node-adoption.md) §4b; for the platform's own
deploy story, [`deploy/prod/README.md`](../../../deploy/prod/README.md) §6.

The worked example throughout is `gumbo-nuc-0` (node name `nuc`, login user
`worksalot`), **measured on 2026-08-08 and not converted at that date**.
Substitute your own node freely: nothing here is nuc-specific except the values.

---

## 1. What conversion touches — and the toolchains it does not

Conversion removes one container and installs one systemd unit. That is the
whole blast radius on the node's filesystem: `/usr/local/bin`,
`/usr/local/lib/chuggernaut`, `/etc/chuggernaut`, and a unit path.

**The Android SDK, Flutter and JDK toolchains are unaffected.** They live on the
**host** and belong to the node's own configuration, at a stable path that is a
symlink into `/nix/store` ([`worker-kvm.md`](worker-kvm.md) §1, §7) — on
`gumbo-nuc-0`, `/var/lib/chuggernaut/toolchain/{android-sdk,flutter,jdk}`,
measured 2026-08-08. They were never mounted into the
`chug-worker` container: the daemon reads their paths out of its run spec
(`WORKER_ANDROID_SDK_DIR`, `WORKER_FLUTTER_DIR`, `WORKER_JDK_DIR`) and passes
them to **task** containers as read-only bind *sources*, which dockerd resolves
in the host's namespace — see `read_only_bind` and the three mount-path
constants in [`crates/container/src/docker.rs`](../../../crates/container/src/docker.rs).
A native daemon composes the same binds from the same host paths. Nothing in
this procedure writes to `/var/lib/chuggernaut` or to `/nix/store`, and no
`nixos-rebuild` step here bumps a toolchain. If Android jobs worked the hour
before the conversion they work the hour after.

Also untouched: the node's slot count (capacity is the Cluster page's,
[`worker-capacity.md`](worker-capacity.md) §1), its images, and any job
container running at the time — job containers are siblings on the node's docker
socket, not children of the daemon, so they survive both the container's removal
and the unit's start (`docs/spec.md` §3.1).

---

## 2. Does this apply to your node?

Two conditions: the node is **NixOS**, and its daemon is still a **container**.
Ask it:

```sh
ssh worksalot@gumbo-nuc-0 '
  uname -s
  systemctl list-unit-files | grep chug || echo "no chug unit"
  docker inspect chug-worker --format "{{.State.Status}}" 2>/dev/null || echo "no container"
  readlink /etc/systemd/system'
```

An unconverted NixOS node answers `Linux`, `no chug unit`, `running`, and
`/etc/static/systemd/system` — a symlink into the store, which is why the deploy
cannot drop a unit there. That was `gumbo-nuc-0`'s exact state on 2026-08-08,
including `sudo -n test -w /etc/systemd/system` failing.

### The two refusals you will already have seen

They are the reason this page exists, and they come from opposite ends. The
first is the node refusing to update **itself**, in a deploy leg
(`deploy/prod/worker-refresh.sh`, at swap time):

```text
worker-refresh: this daemon is running INSIDE a container, so it is a node
design #440 has not converted yet — the swap installs a binary and restarts a
supervisor unit (#440 D6) and there is no unit here; REFUSING swap (live daemon
untouched, job containers untouched, images already built). Convert the node
from the operator's laptop with 'WORKER_SSH=<user>@<node>
deploy/prod/build-worker.sh' (deploy/prod/README.md §6); until then this node is
deployed over ssh, not by self-refresh.
```

**A green refresh leg is not evidence the next one will pass.** The swap runs
the `worker-refresh.sh` **installed on the node**, so a refusal added to the
script takes effect one deploy *later* — the deploy that installs it still
passes. Deploy #488 reported `worker-refresh:nuc ok` while installing the script
that now refuses; #498 then failed on `worker-refresh:nuc`, and #499 shipped
only by taking the nuc out of `DOCKER_NODES` (§9). Until the node is converted
it fails **every** deploy.

The second is `deploy/prod/build-worker.sh` refusing to convert the node the way
the first refusal told you to:

```text
build-worker: worksalot@gumbo-nuc-0 has no usable systemd unit directory at
'/etc/systemd/system' (want `systemctl` on PATH and a directory writable by the
login user or by `sudo -n`) — on NixOS that path is a read-only symlink into the
store, where the unit is the node configuration's to declare (design #440 slice
7); REFUSING daemon restart (live daemon untouched). Point WORKER_UNIT_DIR_nuc
at a writable unit path, or declare the unit on the node itself.
```

Both refusals are correct and neither is the answer. **A NixOS node is converted
by its own configuration, never by the deploy script** — and the deploy still
has to run afterwards, for everything that is *not* the unit. That is the whole
of this page.

---

## 3. Who installs what

| Artifact | Owner | Where |
| --- | --- | --- |
| `chug-worker.service` | **the node's NixOS configuration** — `chug.node.daemon.enable` ([`nix/chug-node/nixos.nix`](../../../nix/chug-node/nixos.nix)) | `/etc/systemd/system`, via the store |
| the environment file (the whole run spec) | `build-worker.sh` | the module's `daemon.environmentFile`, whose default is the script's ([`nix/chug-node/options.nix`](../../../nix/chug-node/options.nix)) |
| the daemon binary, `chuggernaut-channel`, `worker-refresh.sh` | `build-worker.sh`, extracted from the worker image it just built (#440 D6, which holds on Linux) | `/usr/local/bin`, `/usr/local/lib/chuggernaut` |
| `worker.creds`, `worker_git`, `worker_git-cert.pub` | **you**, by hand, before the deploy runs | the credential directory, root-owned at `0700` |

The split is deliberate: the module declares the **lifecycle** and never a
`WORKER_*` value, the platform declares the **run spec** and never the unit
([`nix/chug-node/options.nix`](../../../nix/chug-node/options.nix) argues it
against design #372 §8's R3). The two halves must name **one** environment file
— if they disagree the unit fails to start naming the file it could not load,
and the module warns at build time.

**The unit cannot start early, and that is a safety property, not luck.** Its
`EnvironmentFile=` carries no `-` prefix
([`nix/chug-node/chug-worker.service.in`](../../../nix/chug-node/chug-worker.service.in)),
so between the rebuild and the deploy systemd fails the unit every 5s under
`Restart=always` rather than starting a second daemon beside the container. Loud
and bounded; §8 says how to quiet it if you have to stop half way.

---

## 4. The node's configuration

### 4.1 What the node has pinned

The module comes from the mirror `github:kasofsk/chuggernaut`, and
`chug.node.daemon.*` only exists in it since slice 7 (job #475). Read the pin:

```sh
ssh worksalot@gumbo-nuc-0 'jq -r ".nodes.chuggernaut.locked.rev" /etc/nixos/flake.lock'
```

Then, in a checkout of the platform repo, ask whether that revision carries the
unit at all:

```sh
git cat-file -e <rev>:nix/chug-node/chug-worker.service.in && echo "has slice 7"
```

A revision that predates slice 7 fails the rebuild with `The option
'chug.node.daemon.enable' does not exist` — a clean signal, at build time, that
costs nothing. Re-pin with `nix flake update chuggernaut` (on nix older than
2.19, `nix flake lock --update-input chuggernaut`), and remember the mirror lags
`main` by up to five minutes
([`chug-node-adoption.md`](chug-node-adoption.md) §3).

`gumbo-nuc-0` on 2026-08-08 already imported `chuggernaut.nixosModules.chug-node`
from that input, with `chug.node = { enable = true; user = "worksalot";
cacheDir = "/var/cache/chuggernaut/sccache"; }` and **no `daemon` block** — so
its conversion is an opt-in and a re-pin, not an adoption.

### 4.2 The edit

```nix
chug.node.daemon.enable = true;      # the three other knobs have defaults
```

That is all of it. `daemon.binary`, `daemon.environmentFile` and `daemon.path`
default to what `build-worker.sh` installs and reads; leave them alone unless
you are also overriding the deploy's side, and read
[`nix/chug-node/options.nix`](../../../nix/chug-node/options.nix) before you do
— a mismatch between `daemon.environmentFile` and `WORKER_ENV_FILE_<node>` is a
unit that will not start.

---

## 5. The run spec on the Mini — the one edit that is not optional

`WORKER_GIT_KEY` is globally `/data/keys/worker_git` in the prod
`chuggernaut.env`. That is the **container's** view of the bind mount of
`~/chuggernaut-worker/keys`, and a native daemon has no `/data/keys` at all. The
deploy refuses it rather than handing a daemon a path that does not exist:

```text
build-worker: WORKER_GIT_KEY='/data/keys/worker_git' names the container's key
mount, which a NATIVE daemon does not have (design #440 D2) — the node would
come up and every self-refresh would fail to fetch; REFUSING (live daemon
untouched).
```

**And the obvious fix is refused too.** Pointing the override at the key where
it actually sits — `/home/worksalot/chuggernaut-worker/keys/worker_git` on the
nuc, measured 2026-08-08 — hits a second guard, because that directory is the
login user's home and that user is in the `docker` group, so anything they run
can read the key (design #440 D5). `build-worker.sh` names it:

```text
build-worker: WORKER_GIT_KEY='…' is under the login user's home
('/home/worksalot') on a Linux node, outside the credential directory
'/etc/chuggernaut/keys' … REFUSING (live daemon untouched).
```

So the conversion **moves the credentials** and points the override at their new
home. On the node, before the deploy:

```sh
ssh worksalot@gumbo-nuc-0 '
  sudo install -d -o root -g root -m 0700 /etc/chuggernaut/keys
  sudo install -o root -g root -m 0600 \
    ~/chuggernaut-worker/keys/worker_git /etc/chuggernaut/keys/worker_git
  sudo install -o root -g root -m 0600 \
    ~/chuggernaut-worker/keys/worker_git-cert.pub /etc/chuggernaut/keys/worker_git-cert.pub
  sudo install -o root -g root -m 0600 \
    ~/chuggernaut-worker/keys/worker.creds /etc/chuggernaut/keys/worker.creds
  sudo ls -l /etc/chuggernaut/keys'
```

`install` copies. **Leave the originals under `~/chuggernaut-worker/keys` until
the unit is verified** — they are the only thing that makes §8's fallback
possible. `deploy/prod/README.md` §6's migration deletes them, and that step
belongs *after* this procedure, not during it.

That directory is the container's `/data/keys` bind source, so all three files
are normally in it. If `worker.creds` is not, mint a fresh one on the Mini —
`chug admin --keys-dir "$KEYS_DIR" worker-creds --node nuc`, then `scp` it to a
staging path and `install` it as above; `scp` cannot write into a `0700` root
directory, which is the point of one
([`deploy/prod/README.md`](../../../deploy/prod/README.md) §6).

Then, in `chuggernaut.env` on the Mini:

```sh
WORKER_GIT_KEY_nuc=/etc/chuggernaut/keys/worker_git
```

Per node, because the bare `WORKER_GIT_KEY` still serves nodes nobody has
converted; the air already carries its own
`WORKER_GIT_KEY_air`. The declaration matters twice — the deploy composes the
node's environment file from it, and #390's drift guard reads it as the fleet's
statement of record.

**Nothing else in the run spec is container-relative.** `NATS_CREDS` was
(`/data/keys/worker.creds`), but no operator declares it: `build-worker.sh`
resolves it to `<credential directory>/worker.creds` itself, which is why moving
the credential is the whole of that story. `WORKER_CACHE_DIR_nuc=/var/cache/chuggernaut/sccache`,
the three toolchain leaves (§1) and `WORKER_NIX_*` are all **host** paths
already, read by dockerd or by the node's nix daemon in the host's namespace,
and mean the same thing to a native daemon.

**Expect the drift guard to have opinions.** Before it replaces a live daemon,
`build-worker.sh` compares the composed run spec against the **container's own
environment** and refuses if the new spec would drop any `WORKER_*` the node is
running:

```text
build-worker: REFUSING daemon restart (live daemon untouched): the run spec
composed here drops WORKER_… which the live daemon on nuc is running.
```

That is the guard working. Anything it names is a setting that survived by
circulation rather than by declaration — declare it in `chuggernaut.env` as
`<VAR>_nuc` and re-run. Reach for `WORKER_SPEC_DROP_OK=1` only when you have
read the list and mean to lose every item on it.

---

## 6. The conversion, in order

Order is not a preference here. The node's own configuration must declare the
unit **before** the deploy writes anything, because the deploy's copy of the
unit goes to `/run/systemd/system`, which does not survive a reboot — a node
whose daemon disappears at the next power cycle is worse than a deploy that
failed, which is why `build-worker.sh` refuses instead of falling back there
itself (design #440's slice-7 correction).

A conversion is an out-of-band deploy action, so it gets the same paper trail as
one: file a `deploy` job and claim it before you start, resolve it `Pass` with
the commands you ran when you finish
([`adhoc-deploy.md`](adhoc-deploy.md) §2).

```sh
TOKEN=$(cat ~/.config/chuggernaut/token)
API=https://<api-host>

# 1. drain — note the current number, you have to put it back
curl -fsS "$API/api/v1/platform/fleet" -H "Authorization: Bearer $TOKEN" \
  | jq '.nodes[] | select(.name=="nuc") | {slots, slots_desired, occupied}'
curl -fsS -X PUT "$API/api/v1/platform/fleet/nuc/capacity" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"slots": 0}'
until [ "$(curl -fsS "$API/api/v1/platform/fleet" -H "Authorization: Bearer $TOKEN" \
  | jq '.nodes[] | select(.name=="nuc") | .occupied')" = "0" ]; do sleep 10; done

# 2. credentials into the root-owned 0700 directory (§5)

# 3. build first — this is the gate; nothing is activated
ssh worksalot@gumbo-nuc-0 \
  'sudo nixos-rebuild build --flake /etc/nixos#gumbo-nuc-0 --impure'

# 4. switch. From here the unit exists and loops on a missing environment file
ssh worksalot@gumbo-nuc-0 \
  'sudo nixos-rebuild switch --flake /etc/nixos#gumbo-nuc-0 --impure'

# 5. the runtime unit directory the deploy needs to exist to write into
ssh worksalot@gumbo-nuc-0 'sudo mkdir -p /run/systemd/system'

# 6. the deploy — from a tailnet machine that can ssh the node. The Mini cannot
#    (Tailscale blocks tagged->tagged), which is why WORKER_SSH is unset in prod
cd <chuggernaut checkout>
set -a; . deploy/prod/chuggernaut.env; set +a
WORKER_SSH=worksalot@gumbo-nuc-0 \
CHUG_WORKER_NODE=nuc \
WORKER_UNIT_DIR_nuc=/run/systemd/system \
  deploy/prod/build-worker.sh

# 7. restore the slot count
curl -fsS -X PUT "$API/api/v1/platform/fleet/nuc/capacity" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"slots": 2}'
```

Notes on the steps that are not obvious:

- **Step 1, the drain.** `chug.node.daemon.enable` alone changes no docker
  setting, so this rebuild is usually hot-safe — ask it rather than guessing,
  with `sudo nixos-rebuild dry-activate --flake … --impure` and look for
  `docker.service` in the units it would restart
  ([`worker-capacity.md`](worker-capacity.md) §4.1). Drain anyway: step 6
  removes the running daemon, and a job whose container outlives its daemon is a
  poll the dispatcher has to re-attach.
- **Step 5.** `/run/systemd/system` is a documented systemd unit search path,
  outranked by `/etc`. The deploy's unit-directory check requires the directory
  to *exist* before it will proceed, and it runs before anything is installed.
- **Step 6, `WORKER_UNIT_DIR_nuc`.** The deploy writes its own copy of the unit
  there, systemd loads the configuration's `/etc` copy ahead of it, and the copy
  is discarded at the next boot. Declare it on the command line as above rather
  than in `chuggernaut.env`: it is true of exactly one run, the conversion, and
  after that the node writes no unit again — the self-refresh swap installs a
  binary and asks systemd to restart `chug-worker.service`
  (`deploy/prod/worker-refresh.sh`).
- **`WORKER_NATS_URL` is required** and comes from the sourced env file; so do
  `WORKER_SLOTS_nuc`, `WORKER_CACHE_DIR_nuc` and `WORKER_REFRESH_GIT_URL`. Do
  not retype them ([`adhoc-deploy.md`](adhoc-deploy.md) §1b) — and source the
  **Mini's** copy of that file, which is the declaration of record; a laptop
  checkout does not carry one.

### The one step that can still stop, and how to finish it by hand

The deploy's remote install writes the binary, the channel binary,
`worker-refresh.sh` and the environment file **first**, then the unit, then
`systemctl daemon-reload`, `systemctl enable`, `docker rm -f chug-worker`, and
finally `systemctl restart` — in that order, under `set -e`
(`deploy/prod/build-worker.sh`). `systemctl enable` is the one command in that
list a NixOS node can refuse: enablement symlinks live under
`/etc/systemd/system/multi-user.target.wants`, which is read-only, and the
module already sets `wantedBy = [ "multi-user.target" ]` for exactly that reason
([`nix/chug-node/nixos.nix`](../../../nix/chug-node/nixos.nix)) — so the command
is redundant here, and if it *errors* rather than reporting the unit already
enabled, the install aborts with everything but the swap-over done. Finish it:

```sh
ssh worksalot@gumbo-nuc-0 '
  docker rm -f chug-worker
  sudo systemctl daemon-reload
  sudo systemctl restart chug-worker.service'
```

Then verify by hand (§7) — the script's own health probe did not run, so nothing
has proved the daemon came up.

---

## 7. Verify

```sh
ssh worksalot@gumbo-nuc-0 '
  systemctl is-active chug-worker.service
  systemctl show -p FragmentPath chug-worker.service      # want /etc/…, not /run/…
  systemctl cat chug-worker.service | head -20            # want the module text
  docker ps -a --filter name=chug-worker                  # want nothing
  sudo journalctl -u chug-worker.service -n 50 --no-pager | grep "worker up"'
```

Four things, and each answers a different failure:

1. **`active (running)`** — the supervisor accepted it and it stayed up.
2. **`FragmentPath` under `/etc`** — the configuration's unit won, so the node
   survives a reboot. Under `/run` means the module is not declaring it (a stale
   pin, or `daemon.enable` never applied) and you have a daemon with a deadline.
3. **No `chug-worker` container** — one daemon on this node name. Two would
   split the node into two fleet rows (#440 §1) and is the state
   `worker-refresh.sh` refuses its swap over.
4. **`worker up` in the journal** — the daemon's own proof of NATS liveness. It
   emits that line only after the connection and the worker-RPC subscription
   succeed, so it is strictly stronger than "the process is running".

Then, from the platform side, the node reporting its own capacity:

```sh
curl -fsS "$API/api/v1/platform/fleet" -H "Authorization: Bearer $TOKEN" \
  | jq '.nodes[] | select(.name=="nuc")
        | {slots, occupied, capacity_source, capacity_state, version}'
```

`capacity_source: "node"` is the one that matters — the number in force came
from the daemon rather than from the `DOCKER_NODES` seed
([`worker-capacity.md`](worker-capacity.md) §3). The `version` should be the SHA
you converted at.

---

## 8. If it does not come up

**Converting is one-way in the scripts.** #440 slice 6 deleted the `docker run`
path, so `build-worker.sh` has no way back to a container daemon and the honest
move is forward. Read the journal first — it names the cause more often than
not:

```sh
ssh worksalot@gumbo-nuc-0 'sudo journalctl -u chug-worker.service -n 100 --no-pager'
```

| What you see | What it means | What to do |
| --- | --- | --- |
| `Failed to load environment files` | the module's `daemon.environmentFile` and the deploy's `WORKER_ENV_FILE_<node>` name different files, or the deploy never ran | run §6 step 6, or point the two at one path (§3) |
| the unit loops with no environment file | expected between §6 steps 4 and 6 | finish the conversion, or stop the loop (below) |
| `Socket not found` / `backend unavailable` | the daemon dialled a docker endpoint that is not there | on Linux the default `/var/run/docker.sock` is the node's real socket; check `virtualisation.docker.enable` actually took |
| no NATS connection | `worker.creds` is unreadable or is the wrong node's | re-check §5's install; the file must be root-owned `0600` inside the `0700` directory |
| the daemon is up but never refreshes | the git key is missing at the declared path | `WORKER_GIT_KEY_nuc` (§5), and the cert beside the key |

**To stop the loop while you work**, set `chug.node.daemon.enable = false` and
rebuild — that removes the unit and leaves everything the deploy installed
alone. **To get the node serving again while you diagnose**, the pre-conversion
container is still startable *provided the old key directory still exists*
(§5's "leave the originals"): `chug-worker` is
[`adhoc-deploy.md`](adhoc-deploy.md) §1c's by-hand `docker run`, and the image
the deploy just built is on the node. Two hard rules if you do that: disable the
unit **first** — never a unit and a container on one node name — and treat the
node as unconverted again, because it is: it will refuse its own swap on the
next deploy exactly as before.

---

## 9. Order relative to a deploy, and `DOCKER_NODES`

**Convert first, deploy second.** The refresh leg is the thing the conversion
fixes, so a deploy started before it either fails on that node (unconverted) or
races the conversion (mid-conversion).

**The node must be listed in `DOCKER_NODES` or the refresh never reaches it.**
`refresh_worker_nodes` in [`deploy/prod/update.sh`](../../../deploy/prod/update.sh)
derives the fan-out from that variable — every entry whose second field is
`worker` gets a leg — and there is **no skip knob**. Deploy #499 shipped by
removing the nuc from `DOCKER_NODES`, which is a real lever and a quiet one: a
node that is out of the list keeps serving jobs, announces its capacity, and
**stops updating**, silently, until someone notices its version drifting from
the dispatcher's. So the last step of the conversion is to put it back:

```sh
# in chuggernaut.env on the Mini — the worker entry's slot field is a
# pre-observation fallback and stays 0 (spec §3.1)
DOCKER_NODES="local|unix:///…/docker.sock|0, nuc|worker|0"
```

Then run a deploy and read the node's leg. A converted node's refresh installs
the binary out of the image its build phase just made and asks systemd to
restart the unit — no unit is written, and `worker-refresh:nuc ok` now means
what it says.

---

## Related

- [`chug-node-adoption.md`](chug-node-adoption.md) — §4a, what the module
  declares and the three-step order; §4b, the same conversion on a mac (three
  node facts this page does not need); §6, build/drain/switch.
- [`worker-capacity.md`](worker-capacity.md) — §3 reading the fleet, §4.1
  draining before a rebuild.
- [`adhoc-deploy.md`](adhoc-deploy.md) — §1b `build-worker.sh` by hand, §1c the
  pre-conversion container, §2 the paper trail every out-of-band action needs.
- [`deploy/prod/README.md`](../../../deploy/prod/README.md) §6 — provisioning a
  worker node: the credential directory, the run spec, the enrollment commands.
- [design #440](../../design/440-native-worker-daemon.md) — D2 the split, D5 the
  credential boundary, D6 where the binary comes from, and the
  [slice-7 correction](../../design/440-native-worker-daemon.md#correction-2026-08-07--slice-7-as-landed-job-475)
  that names this seam.
- [`docs/spec.md`](../../spec.md) §3.1 — normative: worker nodes, self-refresh,
  node-local properties.

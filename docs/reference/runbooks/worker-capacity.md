# Worker node capacity — reading it, changing it, and where each number comes from

**Audience:** the prod operator. You want to give a node more slots, drain one
before maintenance, or work out why a node is running the concurrency it is.
This page is the reference for all three.

It is *not* the design argument (that is
[design #293](../../design/293-worker-capacity.md), including why it is shaped this
way) and not the normative text ([`docs/spec.md`](../../spec.md) §3.1, "Dynamic worker
registration"). For the standing-instance runbook — deploys, worker refresh,
backups — see [`deploy/prod/README.md`](../../../deploy/prod/README.md); for
mid-incident by-hand deploys, [the ad-hoc deploy runbook](adhoc-deploy.md).

---

## 1. The one rule

**The node owns its capacity, and the scheduler reads exactly one number per
node: the one the node itself reported.**

Everything else is either a *starting* value or a *request*. Nothing else places
work. When you want to know what a node is actually running at, read the fleet
snapshot — never the dispatcher's env.

| Number | Lives where | What it actually does |
| --- | --- | --- |
| **observed `slots`** | the daemon's live capacity cell (`crates/worker/src/capacity.rs`), reported on both the ~15s announce and every `ping` reply | **the only number placement reads** |
| **desired `slots`** (intent) | the dispatcher's `platform` bucket, key `fleet.capacity` | recorded, and commanded to the node. Never reads back into placement — the dispatcher asserts this (`crates/dispatcher/src/capacity.rs`) |
| `WORKER_SLOTS` | the node's environment file, set at node creation | the node's **first-boot value only**. Not how you change capacity |
| `WORKER_SLOTS_MAX` | the node's environment file, from `chuggernaut.env` via `build-worker.sh` | the node's **ceiling**. A request above it is refused, with a reason. Default: the node's CPU count |
| the `DOCKER_NODES` slot field | the dispatcher's `chuggernaut.env` | for a `worker` endpoint: membership seed, plus a **pre-observation fallback** that can never win once the node has reported. For a `unix://`/`tcp://` endpoint it is still the owner |
| the daemon's built-in default `4` | nobody sets it | last resort for a node brought up with no `WORKER_SLOTS` at all |

Two transports carry the observed number — the announce push and the `ping`
pull — but they are one owner and one field, ordered by a
`(capacity_epoch, capacity_generation)` pair. A `ping` reply is applied
unconditionally and resets the ordering watermark, which is what makes any
ordering anomaly self-healing rather than permanent.

---

## 2. Changing a node's capacity

**No ssh. No container rebuild. No dispatcher restart.**

### From the UI (the normal path)

Cluster page → the node's card → the capacity stepper
(`web/src/components/NodeCapacity.tsx`). Set the number; it is sent, recorded as
intent, and commanded to the node. Convergence normally shows in the next fleet
snapshot — about a second.

The stepper is bounded by the node's reported `slots_max`, and it asks for
confirmation on exactly one change: one that would take **fleet-wide** capacity
to zero. Per-node drains need no confirmation; only the last non-zero node does.

### From the API

```sh
curl -fsS -X PUT https://<api-host>/api/v1/platform/fleet/nuc/capacity \
  -H "Authorization: Bearer $(cat ~/.config/chuggernaut/token)" \
  -H 'Content-Type: application/json' -d '{"slots": 4}'
# 202 → {"node":"nuc","desired":4,"observed":2,"state":"pending"}
```

Platform admins only (spec §7.5). The reply is **202, not 200**, and that is
honest rather than sloppy: the dispatcher's actor is single-threaded and must not
block on a node RPC, so "recorded and converging" is the strongest thing it can
truthfully say at that moment. Watch the fleet snapshot for the rest.

| Status | Meaning |
| --- | --- |
| `202` | recorded; converging |
| `400` | `slots` missing, negative, fractional, or not a number |
| `403` | not a platform admin |
| `404` | unknown node |
| `409` | a docker-endpoint node — `DOCKER_NODES` still owns those |
| `422` | above the node's last reported `slots_max` (the response carries the max) |

There is no `chuggernaut admin` subcommand for this; the UI and the endpoint
above are the whole surface.

---

## 3. Reading what is actually in force

`GET /api/v1/platform/fleet` (platform admin) carries, per node:

- `slots` — **observed**, the number placement uses.
- `slots_desired` — the operator's intent, if any has ever been set.
- `slots_max` — the node's ceiling.
- `capacity_source` — `node` or `seed`.
- `capacity_observed_at` — when the node last reported.
- `capacity_state` — `converged` | `pending` | `rejected` | `unacknowledged`.
- `capacity_note` — the daemon's reason, when it refused.

`capacity_source: "seed"` is the one to care about. It means the number in force
came from `DOCKER_NODES` and **the node has never reported its own** — a number
of unknown age standing in for the node's real capacity. That exact condition ran
undetected in prod for weeks before 2026-07-26 (a missing
`event.worker.announce` publish grant made every announce silently denied), and
representing it is the reason the field exists. The cluster view renders it as a
warning chip; the dispatcher also warns in its log, three minutes after start and
then at most every fifteen (`crates/dispatcher/src/scan.rs`).

The states, and what each is telling you:

| `capacity_state` | Reading |
| --- | --- |
| `converged` | the node reports what you asked for. Nothing to do |
| `pending` | commanded, not yet observed. Normal for about a second |
| `rejected` | the node refused the value — above its `slots_max`. **Terminal**: the dispatcher stops re-pushing and waits for you. `capacity_note` carries the daemon's reason. Lower the request, or raise `WORKER_SLOTS_MAX` for the node in `chuggernaut.env` and re-run `build-worker.sh` |
| `unacknowledged` | recorded and pushed, but the node is not converging after three minutes — an old build that ignores `set_slots`, or one that adopts and reverts |

---

## 4. Draining a node

Set it to `0`. That is a full drain and it is a first-class state, not a hack.

- **Running containers are never killed to honour a cap.** Nothing in the
  lowering path touches `kill`.
- New placement is blocked while occupancy is at or above the cap. Free slots go
  non-positive and `choose_placement` skips the node (`crates/container/src/lib.rs`).
  A node at 3/2 is simply not eligible; the cluster view renders the over-cap
  cells distinctly and it reads as "finishing, taking nothing new".
- Blocked launches **queue** via the launch-capacity path (spec §3.5) and burn no
  retry budget.
- A 0-slot node never vetoes the dispatcher's startup, and a fleet of reachable
  worker nodes that all report 0 still boots — loudly warning — so a drain can
  never leave you unable to restart the thing that would undo it.

**The one caveat.** The §3.5 **maximum queue wait** (default 30 minutes) still
applies to queued launches. Draining the last node with capacity does not pause
that clock: queued jobs escalate with `no_free_slots_timeout` after the window. A
maintenance mode that pauses the queue clock is a named follow-up, not something
this behaviour already has.

### 4.1 Drain before `nixos-rebuild switch` / `darwin-rebuild switch`

Capacity is a scheduling knob, but the case that actually forces a drain is
**rebuilding the node's system closure while it is running tasks**. Do this, in
order:

```sh
TOKEN=$(cat ~/.config/chuggernaut/token)
API=https://<api-host>

# 1. drain — note the current number first, you have to put it back
curl -fsS "$API/api/v1/platform/fleet" -H "Authorization: Bearer $TOKEN" \
  | jq '.nodes[] | {name, slots, slots_desired, occupied}'
curl -fsS -X PUT "$API/api/v1/platform/fleet/nuc/capacity" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"slots": 0}'

# 2. wait for the node to go quiet — `occupied` is the number that matters,
#    not `slots`. Running containers are never killed to honour a cap (§4).
until [ "$(curl -fsS "$API/api/v1/platform/fleet" -H "Authorization: Bearer $TOKEN" \
  | jq '.nodes[] | select(.name=="nuc") | .occupied')" = "0" ]; do sleep 10; done

# 3. rebuild, on the node
ssh worksalot@gumbo-nuc-0 \
  sudo nixos-rebuild switch --flake /etc/nixos#gumbo-nuc-0 --impure

# 4. restore the slot count
curl -fsS -X PUT "$API/api/v1/platform/fleet/nuc/capacity" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"slots": 2}'
```

The Cluster page's stepper does steps 1 and 4 just as well, and the node card
shows `occupied` for step 2. Either surface is fine; the ordering is the part
that matters.

Performed twice on `gumbo-nuc-0` on 2026-08-03, uneventfully both times —
*because* it was followed.

**Why the first rebuild is the expensive one.** The `chug-node` module sets
`virtualisation.docker.daemon.settings.live-restore = true`
([`nix/chug-node/nixos.nix`](../../../nix/chug-node/nixos.nix), design
[#372](../../design/372-chug-node-modules.md) §5 A4), which keeps containers alive
across a dockerd restart. But **live-restore only protects a restart that
happens while it is already active.** Toggling it changes `daemon.settings`,
which changes the store path the docker unit's `--config-file=` names, which
restarts dockerd — *without* live-restore, because the running daemon predates
it. So the switch that adopts the setting kills every in-flight job container,
and it is exactly the switch an operator is least expecting to. Drain that one.

The same is true of any rebuild that changes the docker package or its settings,
of a **reboot** (live-restore does not survive one), and of a rebuild that
bumps a node toolchain — the new system profile stops rooting the old closure,
so the next `nix-gc` can collect a store path a running task is still using
(#372 §5 A5, and [`worker-kvm.md`](worker-kvm.md) §7 for the roots that
narrow this).

**Telling a hot-safe rebuild from one that needs the drain.** Most rebuilds
touch nothing docker-shaped and are safe to run at full capacity. Ask the
rebuild itself rather than guessing:

```sh
# on the node — builds, then prints the units it WOULD stop/restart, and
# activates nothing. Look for docker.service in that list.
sudo nixos-rebuild dry-activate --flake /etc/nixos#gumbo-nuc-0 --impure
```

The 2026-08-03 rebuild that added a toolchain symlink named only
`systemd-tmpfiles-resetup.service`, so it ran hot with no drain. `nixos-rebuild
build` followed by `nvd diff /run/current-system ./result` (or `--diff` where
your host repo wires it up) answers the coarser "did anything I care about
move" question.

**When in doubt, drain.** It costs a minute of the node's capacity, and the
fleet keeps placing on every other node meanwhile.

Two things the drain does *not* buy you, both from §4 above: queued launches
keep burning the 30-minute queue clock (only a concern if this is the last node
with capacity), and a job already running on the node keeps running — the drain
waits for it, it does not stop it. If you need the node down *now*, the honest
move is to accept the loss and let the jobs retry.

Adopting the module in the first place — flake input, the `chug.node` block,
and the one edit that fails the build if you skip it — is
[the `chug-node` adoption runbook](chug-node-adoption.md).

---

## 5. Bootstrapping a new node

Capacity at node creation is the *only* place `WORKER_SLOTS` still matters:

```sh
# WORKER_SLOTS_nuc=2 (and the rest of the node's run spec) is declared in
# chuggernaut.env on the Mini — deploy/prod/README.md §6.
set -a; . deploy/prod/chuggernaut.env; set +a
WORKER_SSH=worksalot@gumbo-nuc-0 CHUG_WORKER_NODE=nuc \
  deploy/prod/build-worker.sh
```

Set it to something the node can actually serve, then never touch it again —
after the first observation the UI owns the number. **Declare it**, per node,
rather than passing it on the command line: a value that only ever rode a
`docker run` used to survive by circulating from one daemon generation to the
next, and disappeared at the first recreation that forgot it. `build-worker.sh` refuses to replace a daemon whose `WORKER_SLOTS` the new
run would drop.

Two consequences worth knowing before they surprise you:

- **`WORKER_SLOTS` survives every self-refresh, and that is deliberate.** It is
  written into the node's environment file, which the supervisor hands the
  daemon on every start (#440 D6/D7) — the swap copies nothing forward — so a
  node whose dispatcher is down still comes back at a sane number rather than
  the default 4.
- **After a swap the node reports its boot value until the dispatcher
  reconciles.** A node you set to 2 in the UI but created with `WORKER_SLOTS=4`
  comes back from a deploy at 4 and is pushed back to 2 within one scan tick.
  The window is seconds of small over- or under-cap. If you want the boot value
  to match the steady-state number, recreate the daemon with the new
  `WORKER_SLOTS`; nothing is broken if you don't.

**The ceiling, `WORKER_SLOTS_MAX`, is declared the same way and travels the same
road.** Unset it defaults to the node's own CPU count, which is right unless the
CPU count overstates what the node can serve — dev-air's colima VM has 6 CPUs but
two concurrent Rust builds is what it actually sustains. To lower it, put
`WORKER_SLOTS_MAX_<node>=<n>` in `deploy/prod/chuggernaut.env` and run <!-- runtime -->
`build-worker.sh`: it renders the line into the node's environment file beside
`WORKER_SLOTS`, so the ceiling survives the next deploy and every self-refresh
rather than reverting to the CPU count. Changing it still is not how you change
a node's capacity — §2's stepper is — but it is not inert either: it is the
standing bound every `set_slots` is checked against, in force for the daemon's
whole life. So lowering it below what the node is running now brings the daemon
back **at or below the ceiling** (the boot `WORKER_SLOTS` is clamped to it, so
the node returns at whichever of the two is smaller —
`crates/worker/src/capacity.rs`), and the intent the dispatcher has recorded
above it is then refused **terminally** — §3's `rejected`, which it stops
re-pushing — rather than reconciled away. Lower the intent first, or expect to.

---

## 6. The `DOCKER_NODES` seed, and the sequenced `|worker|0` change

For a `worker` endpoint, the slot number in `DOCKER_NODES` is a
**pre-observation fallback**. It applies until the node's first capacity
observation and can never override one afterwards. `DOCKER_NODES` still owns
membership (a name there joins the roster at boot) and still owns capacity
outright for `unix://`/`tcp://` docker-endpoint entries.

Design #293 §7 recommends retiring the worker seeds' capacity role entirely, by
setting both worker entries to `|worker|0`:

```sh
DOCKER_NODES="local|unix:///Users/<you>/.colima/default/docker.sock|0, air|worker|0, nuc|worker|0"
```

Ping-pull then supplies the real capacity at the dispatcher's startup probe, and
the seed cannot serve capacity at all — the failure mode where a stale number
silently stands in for a node's own becomes structurally impossible for worker
nodes.

> ### ⚠️ Precondition — this change is ordered, and applying it early boots a fleet with no capacity
>
> **Do not apply it until both of these are true:**
>
> 1. **The deployed dispatcher ingests capacity from `ping`** — design #293
>    slice 3, landed on `main` as `job/298` (`61b721d`). Without it the startup
>    probe never writes an observed number and the zeroed seed is all there is.
> 2. **Every worker daemon in the fleet is on a build that reports capacity** —
>    slice 2, `job/296` (`00a6a41`). A daemon predating those fields returns no
>    `slots` on its ping at all.
>
> Applied early, the fleet comes up reachable, at zero capacity fleet-wide, and
> places nothing. It no longer crash-loops (the startup gate was narrowed so
> worker capacity can never veto a boot), so the symptom is a warning and an idle
> fleet rather than an outage — visible, but still the wrong order.

**Status as of 2026-07-30: the precondition is NOT met.** Prod is deployed at
`6fae6b5` (`job/316`), which predates both slices. Ship a `deploy` job first.

### Verifying the precondition

```sh
# 1. what is deployed
curl -fsS https://<api-host>/api/v1/health        # → {"api_sha":"…","dispatcher":"ok",…}

# 2. does that SHA contain slice 3? (in a checkout of the platform repo)
git merge-base --is-ancestor 61b721d <api_sha> && echo "slice 3 deployed"

# 3. is every worker node reporting its own capacity? THIS is the real check —
#    capacity_source "node" means the daemon is on a build that reports, and is
#    reporting. Any worker node reading "seed" means STOP.
curl -fsS https://<api-host>/api/v1/platform/fleet \
  -H "Authorization: Bearer $(cat ~/.config/chuggernaut/token)" \
  | jq '.nodes[] | {name, slots, capacity_source, capacity_observed_at}'
```

### Applying it

The value lives in the gitignored `deploy/prod/chuggernaut.env` on the Mini, so <!-- runtime -->
this is a by-hand operator step, not something a deploy carries. Keep the
precondition next to the value so the next person cannot re-derive it wrong:

```sh
# in deploy/prod/chuggernaut.env, on the Mini
# Worker slot fields are 0 on purpose (spec §3.1): capacity comes from the node
# itself, over ping at the startup probe and the announce thereafter. Do not put
# numbers back here to "fix" capacity — use the Cluster page's stepper. See
# docs/reference/runbooks/worker-capacity.md §6.
DOCKER_NODES="local|unix:///Users/worksalot/.colima/default/docker.sock|0, air|worker|0, nuc|worker|0"
```

Then restart the dispatcher and confirm real capacity arrived:

```sh
launchctl kickstart -k gui/$(id -u)/com.chuggernaut.dispatcher
# within a few seconds: every worker node reports its real slots, source "node"
curl -fsS https://<api-host>/api/v1/platform/fleet -H "Authorization: Bearer $TOKEN" \
  | jq '.nodes[] | {name, slots, capacity_source}'
```

**If capacity does not arrive**, put the previous numbers back in
`DOCKER_NODES` and restart. That is a full rollback — the seed is read only at
boot, so nothing else has to be undone.

---

## 7. Troubleshooting

| Symptom | What it means | What to do |
| --- | --- | --- |
| `capacity_source: "seed"`, `capacity_observed_at` null, node answers pings | The node's RPC works but its announce does not — the signature of a denied `event.worker.announce` publish grant. The number in force is the boot seed | Re-mint the node's creds (`chug admin --keys-dir "$KEYS_DIR" worker-creds --node <name>`) and recreate the daemon. A placement probe will also self-correct it once one happens |
| `capacity_state: "rejected"` | The value is above the node's `slots_max`. The dispatcher has stopped re-pushing it, on purpose | Lower the request, or set `WORKER_SLOTS_MAX` for the node in `chuggernaut.env` and re-run `build-worker.sh` |
| `capacity_state: "unacknowledged"` for minutes | Intent recorded and pushed, node not converging — an old daemon build that ignores `set_slots` | Refresh the node's daemon (a deploy, or `deploy/prod/build-worker.sh`) |
| Node shows `3 / 2` | Over cap, draining. Expected after lowering below live occupancy | Nothing. It takes nothing new and drops under the cap as tasks finish |
| Jobs queue with the fleet apparently idle | Every node is at 0 slots — drained, or nothing has reported yet | Check `capacity_source` per node. Raise a node from the Cluster page. Remember the 30-minute queue clock is still running |
| Lowering `DOCKER_NODES` changed nothing | Correct and by design: for a worker endpoint the seed cannot override an observation | Use the Cluster page's stepper (§2) |

---

## Related

- [`docs/spec.md`](../../spec.md) §3.1 — normative: slot source, precedence and merge,
  operator capacity control, the narrowed startup rule. §6.1 for the route.
- [design #293](../../design/293-worker-capacity.md) — why one owner and two
  transports, what was rejected, and the incident behind it.
- [`deploy/prod/README.md`](../../../deploy/prod/README.md) §6 — worker nodes:
  provisioning, self-refresh, image caching.
- [the `chug-node` adoption runbook](chug-node-adoption.md) — putting the
  NixOS/nix-darwin modules in a host repo, and why §4.1's drain is one-time
  expensive.
- [`worker-kvm.md`](worker-kvm.md) — the other node knob, and §7's GC roots.
- [ad-hoc deploy runbook](adhoc-deploy.md) — when the normal deploy path
  cannot run.

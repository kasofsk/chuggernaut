# Design #293 — Worker capacity: one source of truth, changeable from the UI

Status: IMPLEMENTED — shipped in jobs #295–#301.

All seven jobs of the [implementation plan](#implementation-plan-sliced-into-jobs)
below are merged and deployed: the spec §3.1 capacity-ownership amendment
(#295), the worker daemon's capacity cell and the dispatcher's observation,
intent and reconciliation halves (#296–#298), `PUT
/api/v1/platform/fleet/{node}/capacity` in `crates/api/src/routes.rs` (#299),
the cluster view's capacity control in `web/src/pages/Cluster.tsx` (#300), and
the ops documentation, now at
[`docs/runbooks/worker-capacity.md`](../runbooks/worker-capacity.md) (#301).
What follows is the record of the argument, not a live proposal.

Written against the tree at `a90d660`; every claim about current behavior below
was read out of the source, not inferred from the docs. Prompted by the
2026-07-26 prod incident described under [Problem](#problem).

Revised after review: the capacity ordering key now survives a daemon restart
(§1 — a counter alone would have frozen a node's capacity after every deploy),
and the fleet-level startup gate is narrowed so that worker capacity never
vetoes a boot (§5a — otherwise a drain to zero would be unrecoverable from the
UI that caused it).

Related: [spec §3.1](../../spec.md) (dispatcher backends, dynamic worker
registration), [STYLE.md](../../STYLE.md) (contract-first change rule),
[deploy/prod/README.md](../../deploy/prod/README.md) §6.

## Problem

A worker node's slot count is expressed in three places, and none of them is
authoritative on its own:

| # | Where | Set by | Read by |
| --- | --- | --- | --- |
| 1 | `DOCKER_NODES` (`air\|worker\|2`) in `chuggernaut.env` | editing env + dispatcher restart | `crates/dispatcher/src/config.rs` at boot; seeds the fleet roster |
| 2 | `WORKER_SLOTS` on the daemon container | `docker run` in `deploy/prod/build-worker.sh`, carried forward by `deploy/prod/worker-refresh.sh` | `crates/worker/src/config.rs` → the announce |
| 3 | the daemon's built-in default `4` | nobody | `parse_slots` in `crates/worker/src/config.rs` when `WORKER_SLOTS` is unset |

Spec §3.1 says the live announce beats the seed, and that is what the code
does (`Core::on_worker_announce` in `crates/dispatcher/src/core.rs`,
`WorkerFleetBackend::register_worker` in `crates/worker/src/backend.rs`). But
until 2026-07-26 both prod nodes' creds predated the
`event.worker.announce` publish grant (`crates/auth/src/nats.rs`
`worker_permissions`), so **every announce was denied** and the fleet had been
running on the boot seed alone for weeks. Re-minting creds and recreating both
daemons with `WORKER_SLOTS=2` fixed it; the fleet now reports air 2 / nuc 2
from real announcements.

Two problems remain, and they are different problems:

- **Operator experience.** Changing capacity means recreating the daemon
  container on the node — ssh, an env edit, `build-worker.sh`. There is no
  runtime control at all.
- **Provenance.** When the node's own report stops arriving, a number of
  unknown age from a different mechanism silently stands in for it, and
  nothing anywhere says so.

The second is the one that actually cost us weeks. Any design that fixes the
first without fixing the second has fixed the smaller half.

### The tension

"One source of truth = the announce" and "runtime-changeable from the UI" pull
against each other. An announce is **observed** state reported by the node; a
UI edit is **intended** state expressed by the operator. If the dispatcher
keeps the operator's number and schedules on it, that number competes with the
announce. If the dispatcher keeps nothing, capacity is not runtime-changeable.

## What is true today (verified)

Facts the design leans on, each checked in the tree:

- **The daemon does not enforce its own slot count.** It constructs its local
  backend with `slots: u32::MAX` and the comment *"The dispatcher owns slot
  policy; the worker only reports usage"* (`crates/worker/src/daemon.rs`). The
  cap is applied entirely dispatcher-side.
- **Placement math is signed.** `probe_worker` computes
  `free: slots − ping.running` as `i64` (`crates/worker/src/backend.rs`), and
  `choose_placement` skips any candidate with `free <= 0`
  (`crates/container/src/lib.rs`). A node whose cap drops below its live
  occupancy therefore goes negative-headroom and is simply skipped — **drain
  semantics already exist**; nothing kills a container to honor a cap.
- **`ping` is a live pull path that already works.** Every placement attempt
  and the startup probe call `req.worker.{node}.ping`; `PingOk` carries
  `running`, `version`, artifact hashes, and refresh state — but **not
  `slots`**. Ping rides the daemon's `subscribe req.worker.{node}.>` grant, a
  different permission from the announce publish grant that failed.
- **A ping failure is loud; an announce failure is silent.** A failed ping
  marks the node out of service and logs it (`probe_worker`). A denied publish
  is a `tracing::warn!` on the *node*, invisible to the dispatcher.
- **The heartbeat-lapse path could not have fired.** `scan_worker_heartbeats`
  (`crates/dispatcher/src/scan.rs`) iterates `announced_workers` and filters
  out anything in `seed_node_names`. Both prod nodes were `DOCKER_NODES` seeds
  and had *never* announced, so `announced_workers` was empty — there was no
  heartbeat to lapse. This is exactly what §3.1 specifies ("static seeds are
  never heartbeat-gated"); the gap is that **"never announced" is not
  represented at all**, and it is a different condition from "announced, then
  went quiet".
- **Zero-slot nodes are already first-class.** §3.1 makes a 0-slot node
  placement-inert and explicitly forbids it from blocking startup; a zero-seed
  boot is supported. So "drain a node to 0" needs no new concept.

## Principles the decision is held to

1. **The scheduler reads exactly one number per node.** Not a min, not a
   fallback chain evaluated at placement time.
2. **Observed and intended are never the same field.** Intent may be stored;
   intent never places work.
3. **No capacity path may fail invisibly.** This is the incident's real
   lesson. A mechanism whose breakage looks identical to health is a
   mechanism that will break again.
4. **Worker nodes hold no platform state.** They hold images and a read-only
   key mount. Adding a writable state dir on the node is a new class of thing
   to back up, drift, and reconcile.
5. **Single writer.** The dispatcher owns the fleet record; every mutation
   goes through the actor mailbox, like `Msg::WorkerAnnounce` does today.

## Options weighed

### Option A — Announce is truth; the UI edit is a *command* (recommended, with one addition)

The operator sets a desired slot count in the fleet view. The dispatcher sends
it to the daemon over the existing worker RPC (`req.worker.{node}.set_slots`);
the daemon validates it against what it can physically serve, adopts it, and
announces the new value. Placement keeps reading the announced number and
nothing else. The dispatcher persists the operator's number as **intent only**,
for reconciliation.

- **Buys:** one number in the scheduler path, unchanged. Desired-vs-observed
  stays honest and is displayable as such. The node keeps the right to refuse
  a number it cannot serve. The command rides the `req.worker.{node}.>`
  subscribe grant the daemon already has — *no creds change*, which is
  precisely the failure class that caused the incident.
- **Costs:** a daemon change and a worker-protocol op (cheap — every deploy
  self-refreshes the fleet, `worker-refresh.sh`). Intent does not survive as
  enforcement: if the node is stuck on an old value, the fleet runs the old
  value and the UI must say so loudly rather than pretending.
- **Gap it does not close on its own:** the announce is still push-only, so
  "node never reported" remains possible. See the addition below.

### Option B — Dispatcher-side override, applied as `min(announced, override)`

The UI writes a number into dispatcher state; placement uses the smaller of
announced and override.

- **Buys:** the smallest possible change — no worker protocol, no daemon
  change, survives a daemon restart trivially (it never left the dispatcher),
  and takes effect even when the node is unreachable or running an old build.
- **Costs:** two numbers in the scheduler path, permanently — exactly the
  shape the brief asks to eliminate. Asymmetric: an operator can lower to 2 but
  can never raise a node from 2 to 4, because the announce still caps it, so
  the control is only half a control and the UI has to explain why. And the
  fleet view must now explain "announced 4, capped at 2" forever.
- **Verdict:** rejected as the primary design, but it is the honest fallback
  if a daemon change were ever impossible. It is not: the fleet rebuilds itself
  on every deploy.

### Option C — Capacity is a platform-KV record; the announce is reduced to identity/version

The dispatcher's KV becomes the capacity of record; the node stops advertising
a number.

- **Buys:** genuinely one writer and one place. Trivially runtime-changeable,
  trivially persistent, no push path to break.
- **Costs:** it inverts ownership. A 6-CPU colima can be told to run 12
  containers and has no say. It also makes the incident's failure mode
  *permanent by design*: a number set by an operator months ago, disconnected
  from the hardware, is the number the scheduler serves — which is precisely
  what `DOCKER_NODES` was doing wrong, relocated into KV. It contradicts §3.1's
  "a worker advertises **its own** capacity", and it makes replacing a node with
  different hardware a silent misconfiguration.
- **Verdict:** rejected. Ownership should sit where the physical constraint
  sits.

### Option D — Leave it node-side; the daemon reloads a mounted file

Mount a writable file on the node; the daemon watches it and re-announces.

- **Buys:** no rebuild, persistence for free, no new dispatcher state.
- **Costs:** writing the file needs ssh or a file-sync channel to the node —
  the exact operator experience we are removing (and prod cannot reliably ssh
  air; that is why `refresh` was inverted to a pull in the first place). It
  needs a writable mount, breaking principle 4, and it adds a **fourth**
  mechanism rather than retiring any.
- **Verdict:** rejected as the control plane. It reappears below only as a
  candidate answer to the persistence question, and is rejected there too.

### Option E — Baseline: keep recreating the daemon container

- **Buys:** nothing new to build; the value is durable in the container env
  and `worker-refresh.sh` carries it across self-refreshes.
- **Costs:** the status quo the brief rejects — ssh, a rebuild, minutes, and
  it does nothing about provenance.

## Recommendation

**Option A, plus one addition: `slots` on the `ping` reply.**

The node owns its capacity. The dispatcher reads that one number, delivered
over **two transports of the same source**, and holds the operator's number
strictly as intent.

### 1. Observed capacity — one owner, two transports

The daemon holds a current slot count in memory. It is reported in *both*:

- `WorkerAnnounce` (push, ~15s) — unchanged in shape, gains `slots_max`,
  `capacity_epoch` and `capacity_generation`.
- `PingOk` (pull, every placement attempt and at startup) — gains `slots`,
  `slots_max`, `capacity_epoch`, `capacity_generation`.

Both carry the same field from the same owner. Ordering is settled by the pair
**`(capacity_epoch, capacity_generation)`**, compared lexicographically:

- `capacity_epoch` is stamped **once, at daemon start** (the process's start
  time in unix seconds, read from the node's own clock).
- `capacity_generation` is a counter from 0 that the daemon bumps on every
  adoption.

The dispatcher keeps a per-node watermark and applies an announce only when its
pair is greater than or equal to the watermark, so a stale in-flight announce
cannot undo a fresher observation.

**The restart case is the one this pair exists for.** A counter alone would be
a trap: `worker-refresh.sh` recreates the daemon container on *every* deploy
(§4), and a daemon that came back at generation 0 against a watermark of 3
would have every subsequent observation — announce *and* ping — discarded
forever, pinning the node to a pre-restart number that nothing could refresh.
That is the incident's failure mode rebuilt, and worse: the node *has* been
observed, so it would not even carry the `capacity_source: "seed"` chip, and
the §4 reconciler (which only acts on an observed disagreement) would go
silent too. With the epoch in the key, a restarted daemon's generation-0
observations carry a strictly greater epoch and land normally.

**Backstop — a pull observation always wins.** No watermark rule may be able
to permanently discard observations, so the ordering filter applies to
*announces only*. A `ping` reply is a request/reply on a connection the
dispatcher just opened; it cannot be a stale in-flight message, so it is
applied unconditionally and **resets** the watermark to the pair it carries.
This is what makes any epoch anomaly self-healing rather than terminal — a node
whose clock jumped backwards across a restart, a hand-rolled daemon that
reports a constant epoch, a bug in this rule — all converge at the next
placement probe instead of freezing.

A pre-field daemon's messages omit both numbers; treat a missing pair as
`(0, 0)`, which applies only before the node's first ordered observation.
A genuine build *downgrade* (a node rolled back to a pre-field daemon) then
stops supplying capacity at all: its pings carry no `slots`, so
`capacity_observed_at` goes stale and the §8 never-observed / stale-capacity
warning is what surfaces it. That is the correct outcome — silence about
capacity should look like silence, not like health.

Why the addition is worth it rather than announce-only:

- The incident is structurally a **push-only failure**. A pull path that is
  already exercised, already permissioned, and already fails loudly makes the
  same class of breakage self-correcting.
- Once the seed number is demoted (below), announce-only means a restarted
  dispatcher has **no capacity for up to one announce interval** and must lean
  on the zero-seed-boot path to come up. Ping-pull gives real capacity at the
  startup probe, which the dispatcher performs anyway.
- It costs one field and one assignment in `probe_worker`, which already
  writes `last_version` and `last_refresh` from the same reply.

**Load-bearing ordering, stated because it is currently implicit.**
`startup_check` (`crates/worker/src/backend.rs`) probes each worker node and
*then* reads its slot cell into the startup capacity gate. Ping-derived
capacity must therefore be applied to that cell **inside `probe_worker`**, on
the reply path, so the gate sees the observed number rather than the seed. The
existing code already has this order; job 3 must keep it, assert it, and cover
it with a test — otherwise the demotion in §7 quietly turns the startup gate
into a seed-only check.

The honest objection is that two transports look like two sources. They are
not: one owner, one field, one ordering key, and a stated precedence.
The alternative — dropping `slots` from the announce and going pull-only —
was considered and rejected because the announce is also how an *unknown* node
joins (the dispatcher cannot ping a node it does not know exists), and because
a 15s push is what makes a slot change land promptly on an idle fleet that is
not attempting placements.

### 2. Intended capacity — persisted, never scheduled on

The dispatcher writes a `fleet.capacity` key into the `platform` bucket,
beside `dispatcher.config` and `fleet.status`:

```json
{
  "nodes": {
    "air": { "slots": 2, "set_by": "operator@example.com", "set_at": "2026-07-26T23:14:02Z" }
  }
}
```

**Invariant (assert it, per [STYLE.md](../../STYLE.md) Tier 2):** no placement
path ever reads `fleet.capacity`. It feeds exactly two consumers — the
reconciler and the UI's "desired" display. This is the resolution of the
tension: intent is stored so it can be re-asserted, and is structurally
incapable of placing work.

### 3. The command path

```text
UI  ──PUT /api/v1/platform/fleet/{node}/capacity {slots}──▶  api
api ──req.fleet.capacity.set {node, slots, by}──────────▶  dispatcher (actor)
                                                            ├─ persist intent (single writer)
                                                            └─ effect: req.worker.{node}.set_slots {slots}
daemon ── validate ≤ slots_max ─▶ adopt ─▶ bump generation ─▶ reply ─▶ announce immediately
dispatcher ── observation lands ─▶ roster + fleet.status republished ─▶ UI shows converged
```

The api replies **202 Accepted**, not 200: the actor must not block on a node
RPC (single-threaded by design), and "the operator's number is recorded and
converging" is the honest status. Convergence is visible in the next fleet
snapshot, typically within a second.

### 4. Persistence across a daemon restart — the dispatcher re-pushes

The brief's three candidates:

- **Writable state dir on the node** — rejected (principle 4; and the key
  mount stays `:ro`).
- **Value intentionally does not survive** — rejected; a self-refresh swap
  (`worker-refresh.sh`) recreates the daemon container on every deploy, so a
  non-surviving value would silently revert on each deploy. That is the
  incident's shape again.
- **Dispatcher re-pushes on reconnect (recommended)** — in keeping with §3.6:
  the dispatcher already reconciles the world against its records at startup.

The reconciler runs on the existing scan tick (`crates/dispatcher/src/scan.rs`)
and on any observation whose `slots` differs from intent: if
`observed != desired`, re-send `set_slots`. Bounded, per principle 3 of
[STYLE.md](../../STYLE.md) ("everything is bounded"): at most one push per node
per scan tick, and a **rejected** value is terminal — the dispatcher stops
re-pushing a number the node refused and surfaces the rejection until the
operator changes it. Without that, a node whose max dropped would be pushed a
number it refuses forever.

Note the interaction with `worker-refresh.sh`: the swap carries `WORKER_SLOTS`
forward, so a refreshed daemon comes back on its **boot** value and the
reconciler restores the desired one within a scan tick. That window is
acceptable (it is a small over- or under-cap for seconds), and keeping the
env passthrough means a node whose dispatcher is down still boots at a sane
number rather than the default 4.

### 5. Lowering below current occupancy — drain, never kill

Already emergent from the code, ratified here as contract:

- Running containers are **never** killed to honor a cap. Nothing in the
  lowering path touches `kill`.
- New placement is blocked while `occupied >= slots`: `free` is `i64` and
  `choose_placement` skips `free <= 0`, so a node at 3/2 is simply not
  eligible.
- Launches that find no capacity queue via the §3.5 launch-capacity path (no
  retry budget burned).
- `slots: 0` is a **full drain**: the node finishes what it holds and takes
  nothing new. Placement-inert per §3.1, and it never vetoes startup *on its
  own* — but see the fleet-level gate immediately below, which is a different
  rule and the one that bites.

One honest caveat: the §3.5 **maximum queue wait** (default 30 min) still
applies to queued launches. Draining the only node with capacity to 0 will
escalate queued jobs with `no_free_slots_timeout` after that window. This
design does not change that; the UI should warn when a drain would take
fleet-wide capacity to zero, and a "maintenance mode that pauses the queue
clock" is noted as a follow-up, not smuggled in here.

#### 5a. Fleet-wide zero and the startup gate — the guard must be narrowed

"A 0-slot node cannot block startup" is a **per-node** rule. The rule that
actually decides whether the dispatcher boots is **fleet-level** and stricter:
`evaluate_startup` (`crates/worker/src/backend.rs`) requires *at least one
reachable node with `slots > 0` anywhere in the fleet*, `startup_check`
bypasses it **only when the node list is empty** — not when it is
configured-but-all-zero — and `crates/dispatcher/src/run.rs` propagates the
error with `?`, so it is fatal. Two consequences follow directly from this
design and must be handled, not discovered in prod:

1. **The §7 `|worker|0` seed recommendation.** Once both seeds read 0, startup
   depends entirely on ping-derived capacity having landed in the slot cell
   before the gate reads it. During the job-2 rollout window a daemon on the
   pre-field build returns no `slots` (the `Option` fields are
   `#[serde(default)]`), both nodes evaluate to 0, and the dispatcher
   hard-fails with *"no node has slots > 0"* where today the seed carries it.
2. **An operator drain to zero.** Once the seed is zeroed, an operator who
   drains every node to 0 from the new UI control leaves a dispatcher that
   cannot restart — and the dispatcher is the only thing that can raise the
   number back. The recovery path would be ssh and an env edit: precisely the
   procedure §7 claims to delete.

The second consequence is the decisive one. **Recommendation: narrow the
fleet-level hard-fail so that worker-endpoint *capacity* never vetoes startup.**
Concretely, the gate becomes fatal only when the fleet's capacity is both zero
*and* unchangeable without a restart:

> The dispatcher refuses to start only if **no worker-endpoint node is
> reachable** and no reachable docker-endpoint node has `slots > 0`. A fleet
> with at least one reachable worker node starts — with a loud warning when its
> total capacity is zero — and launches queue via the §3.5 NoCapacity path
> until capacity is observed or commanded.

The asymmetry is principled, not a loophole: a docker-endpoint node's slot
count is static config that only a restart can change, so zero there really is
a fatal misconfiguration and the crash-loop guard should keep catching it. A
worker node's capacity is *observed*, arrives after boot, and is now
operator-changeable at runtime — zero there means "not yet known, or
deliberately drained", and neither warrants refusing to boot. It also makes
the existing empty-list bypass a special case of one rule rather than a
carve-out: zero seeds and zero-slot seeds now behave identically, which is
what §3.1's zero-seed boot already implies.

**The narrowing is scoped to capacity; the reachability half of the gate is
deliberately kept.** The tempting simplification is to key the rule on the mere
*presence* of a worker-endpoint node ("a worker fleet never hard-fails"), which
would additionally make a fleet whose every daemon is down boot successfully.
That is rejected. Whole-fleet-unreachable is the one condition
[spec](../../spec.md) §3.6 reserves for fail-fast, and it is the deploy-time
catcher for exactly the class of bug behind this job's incident — bad
credentials, a wrong `NATS_URL`, a node that never came back. A design whose
thesis is *make the silent failure loud* should not spend the loudest signal it
already has. The cost of keeping it is honest and accepted: a dispatcher that
restarts in the window where every daemon is also down still crash-loops until
one dials in, exactly as today. It is not a new failure, its recovery is
automatic (`probe_worker` rejoins a node without a dispatcher restart, and the
container restart policy retries the boot), and a dispatcher with zero reachable
nodes could not have placed anything anyway.

Re-deriving the decisive drain case against the stricter form: an operator who
drains every node to 0 leaves nodes that are still **reachable** — their daemons
answer `ping`, they simply report no capacity — so the dispatcher boots, the UI
comes up, and the operator raises the number back. A drain therefore can never
create the unrecoverable state, which was the whole point of §5a. The only
remaining hard-fail needs *every* node unreachable, a condition a drain does not
produce and that an undrained fleet reaches identically.

Implementation note for job 3: `NodeCapacity` (the struct `evaluate_startup`
decides on) carries only `slots` and `reachable` today; it needs the node's
transport so the rule can be expressed. Blast radius on the existing tier-1
tests is one case: `all_zero_slot_reachable_fails`
(`crates/worker/src/backend.rs`) splits into an all-docker case (still fatal)
and a worker-present case (now starts, warns). Everything else holds its
premise unchanged, and that is a deliberate check on the narrowing rather than
a coincidence — `zero_slot_docker_plus_dead_worker_fails` (reachable 0-slot
docker + unreachable worker) and its tier-2 twin
`no_reachable_capacity_fails_startup` (`crates/worker/tests/nats_backend.rs`)
both pin the reachability half and must keep failing startup after the change.
Had the rule been keyed on presence instead of reachability, both would have
inverted; job 3 should treat either one turning green as a signal the rule was
widened too far.

Even with the guard narrowed, the `|worker|0` seed change stays **sequenced**:
apply it only after job 3 is deployed *and* every daemon in the fleet is on the
job-2 build, so that observed capacity is actually arriving. Until then a
zeroed seed would boot fine and simply place nothing — visible, but pointless.

### 6. Ceiling — the node is the authority, the operator is trusted below it

The daemon reports `slots_max` and **rejects** a `set_slots` above it, with a
reason the UI shows. `slots_max` defaults to
`std::thread::available_parallelism()` (the node's CPU count) and is
overridable with a new `WORKER_SLOTS_MAX` env for nodes that know better
(air's colima VM is a good example: 6 CPUs, but 2 concurrent Rust builds is
what it can actually serve).

Below the ceiling the operator is trusted — no heuristics about memory or disk.
`slots_max` is advisory to the UI (bounds the stepper) and enforced only at the
daemon, so the enforcement point is the only place that actually knows.

### 7. What gets demoted or deleted

"Single source of truth" has to mean something concrete is retired:

- **`DOCKER_NODES` slot field, for `worker` endpoints: demoted to a
  pre-observation fallback that is labelled as such.** It is used only until
  the node's first observation, after which it can never win. The fleet
  snapshot reports `capacity_source: "node" | "seed"` per node, so a node
  running on a seed number is *visible* rather than indistinguishable from a
  healthy one. Recommended prod follow-up, **conditional and sequenced**: set
  both worker entries to `|worker|0` once job 3 is deployed and every daemon is
  on the job-2 build, so ping-pull supplies real capacity at the startup probe
  and the seed cannot serve capacity at all. Doing it earlier boots a fleet
  with no capacity (see §5a for why that is now a warning rather than a
  crash-loop, and why it is still the wrong order). Entries for
  `unix://`/`tcp://` docker-endpoint nodes are unaffected — `DOCKER_NODES`
  remains their owner, and the single-source claim is scoped to worker nodes.
- **`WORKER_SLOTS`: demoted to the node's first-boot value only.** It stops
  being the way an operator changes capacity; the docs that say otherwise are
  listed below. It is deliberately kept (not deleted) so a fresh node has a
  sane starting number before any operator intent exists.
- **The daemon's default `4`: kept**, as the value of last resort for a node
  brought up with no env at all.
- **Deleted outright:** the *procedure*. No path to changing capacity requires
  ssh, a container recreate, or a dispatcher restart after this lands.

### 8. Failure visibility — the incident must not be able to recur silently

Three mechanisms, in increasing strength:

1. **Provenance in the snapshot.** `capacity_source` and
   `capacity_observed_at` per node, surfaced in the fleet view. Tonight's
   failure would have read *"air — 2 slots from boot seed, node never
   reported"* on the cluster page from the first minute.
2. **A dispatcher warning for the exact signature.** A worker-endpoint node
   that answers pings but has never been observed for capacity within a few
   minutes of the dispatcher's start is warned about, at a bounded cadence.
   That signature — RPC works, announce does not — *is* the denied-publish bug.
3. **Ping-pull makes it self-correcting.** Even with the announce denied, the
   node's real number arrives on the next placement probe.

**Should seed nodes now be heartbeat-gated?** No — and this is worth stating,
because it looks like the obvious fix. Had §3.1 gated seeds on announce
freshness, the denied publish grant would have marked **the entire fleet
unschedulable** — a total outage instead of a stale number. Availability-wise
the current rule is right. The defect is not that we failed to treat a missing
announce as fatal; it is that we had no representation for "the number I am
serving was never confirmed by the node". Fix the representation, keep the
gating rule.

### 9. Authorization and audit

- **Who:** `platform_admin` only, per [spec §7.5](../../spec.md) ("Platform-level
  config"). The api gates it exactly like `platform_config_get` /
  `platform_fleet_get` in `crates/api/src/routes.rs`.
- **Audit:** the `fleet.capacity` record carries `set_by` and `set_at`, so the
  UI can show *"2 slots — set by alice@example.com, 23:14"*. Honest limitation:
  that is last-writer-only, not a history. Spec §10.3 names `job.events.>` as
  the audit log, and it is job-scoped — a capacity change belongs to no job, so
  folding it in there would be a category error. A small retained
  `platform.events` stream is the right home and is proposed as a **follow-up
  job**, not smuggled into this one. Until then, the dispatcher also emits a
  structured log line per change, which is retained by the platform's log
  collection.

### 10. UI surface

On the cluster view's node card (`web/src/pages/Cluster.tsx`), attached to the
slot widget it already renders:

| State | Display |
| --- | --- |
| Converged | plain slot count; edit control (stepper, bounded by `slots_max`) |
| In flight | `2 → 4` with a pending affordance; the number the scheduler uses stays visually primary |
| Rejected | the observed number plus a badge carrying the daemon's reason (`node max is 2`); does not clear until the operator changes the request |
| Unacknowledged | `desired 4, node reporting 2 for 3m` — intent recorded, node not converging |
| Over cap (draining) | `3 / 2` with the over-cap cells rendered distinctly; reads as "finishing, taking nothing new" |
| Seed-sourced | warning chip: *capacity from boot seed — node never reported* |

`web/src/components/CapacityWidget.tsx` renders one cell per slot and currently
assumes `occupied <= slots`; it must tolerate the over-cap case rather than
clipping or negative-counting.

One confirm-step, not a sixth state: a change that would take **fleet-wide**
capacity to zero warns before it is sent — nothing new will be placed anywhere,
queued jobs still burn the §3.5 30-minute queue clock and escalate with
`no_free_slots_timeout`, and (until §5a's narrowed startup gate ships) a
dispatcher restart in that window would fail outright. Per-node drains need no
confirmation; only the last non-zero node does.

## Spec §3.1 amendment

To be applied by the first implementation job (this design does not edit the
normative spec). Under **Dynamic worker registration**, replace the *Slot
source* and *Precedence and merge* bullets and add two:

> - **Slot source — the node owns its capacity, and the scheduler reads exactly
>   one number per node.** A worker daemon holds a current slot count and
>   reports it over two transports of the same source: the `WorkerAnnounce`
>   push (~15s) and the `ping` reply (pulled at the startup probe and at every
>   placement attempt). Both carry `slots`, `slots_max`, a `capacity_epoch`
>   stamped once at daemon start, and a `capacity_generation` the daemon bumps
>   on every change. The dispatcher orders observations by the pair
>   `(capacity_epoch, capacity_generation)` and applies an **announce** only
>   when that pair is at least the last one it applied for the node, so a stale
>   in-flight announce cannot undo a fresher observation; because the epoch
>   advances on every daemon restart, a restarted daemon's generation-0
>   observations are accepted rather than discarded. A **`ping` reply is applied
>   unconditionally and resets that watermark** — it is a request/reply on a
>   live connection and so cannot be stale, and this guarantees no ordering
>   anomaly can permanently freeze a node's capacity. The `ping` path also
>   matters because it is *pulled*: a failure there marks the node out of
>   service (loud), whereas a denied announce publish is silent on the
>   dispatcher side.
> - **Precedence and merge.** `DOCKER_NODES` remains the supported static seed
>   for *membership*. For a `worker` endpoint its slot number is a
>   **pre-observation fallback only**: it applies until the node's first
>   capacity observation and can never override one afterwards. The fleet
>   records report `capacity_source` (`node` | `seed`) and
>   `capacity_observed_at` per node, so a node still serving a seed number is
>   visible as such rather than indistinguishable from a healthy one. Merge by
>   node name is otherwise unchanged: an unknown name joins as a new worker
>   node, a name held by a docker-endpoint seed is refused, and newly-observed
>   capacity re-drains the §3.5 launch queue on the same actor turn.
> - **Operator capacity control (runtime, no restart, no rebuild).** A platform
>   admin sets a node's **desired** slot count from the operator UI. The
>   dispatcher persists it as intent in the `platform` bucket (key
>   `fleet.capacity`, `{ slots, set_by, set_at }` per node) and sends it to the
>   daemon as a command on `req.worker.{node}.set_slots` — a subject already
>   covered by the daemon's existing subscribe grant, so no credential change is
>   required. The daemon validates the value against `slots_max` (default: the
>   node's CPU count, overridable with `WORKER_SLOTS_MAX`), adopts or rejects
>   it, bumps its capacity generation, and announces immediately. **Intent is
>   never read by placement** — the scheduler reads only the observed value —
>   and the dispatcher re-pushes intent when an observation disagrees with it,
>   bounded to one push per node per scan tick, with a rejected value treated as
>   terminal until the operator changes it. A daemon restart or self-refresh
>   swap therefore reverts to its boot `WORKER_SLOTS` and is reconciled back
>   within a scan tick; worker nodes hold no capacity state of their own.
> - **Lowering below occupancy drains; it never kills.** Reducing a node's cap
>   below its live occupancy leaves running containers alone: free slots
>   (`slots − running`) go non-positive and placement skips the node until
>   occupancy falls under the new cap, with blocked launches queued via the §3.5
>   capacity queue (no retry budget consumed). `slots: 0` is a full drain — the
>   node is placement-inert, as any 0-slot node already is, and never vetoes
>   startup on its own account (the fleet-level rule below governs the case
>   where *every* node is at zero). The §3.5 maximum queue wait still applies to
>   queued launches, so a fleet-wide drain still escalates queued jobs.
> - **Startup capacity, narrowed: worker capacity never vetoes the boot.** The
>   fleet-level startup rule (§3.6) becomes: the dispatcher refuses to start
>   only if **no worker-endpoint node is reachable** and no reachable
>   docker-endpoint node has `slots > 0`. A fleet with at least one reachable
>   worker node starts with a loud warning when its total capacity is zero, and
>   launches queue via the NoCapacity path until capacity is observed or
>   commanded. The asymmetry follows from ownership: a docker-endpoint node's
>   slot count is static config that only a restart can change, so zero there
>   remains a fatal misconfiguration, whereas a worker node's capacity is
>   observed and runtime-changeable — zero means "not yet reported, or
>   deliberately drained", and refusing to boot on it would make an
>   operator-commanded drain unrecoverable from the UI that caused it. Only
>   *capacity* is narrowed: **reachability is not**, so a fleet with no reachable
>   node of either transport still fails fast. The zero-seed boot rule is a
>   special case of this one. The gate is evaluated **after** each node's startup
>   probe has applied any ping-reported capacity.

Three existing normative sentences are superseded by that last bullet and must
be updated in the same edit:

- §3.6's *"the dispatcher starts iff at least one reachable node has slots > 0,
  regardless of whether that node is a docker-endpoint or a worker"* — replaced
  wholesale by the bullet above.
- §3.6's *"In a mixed **worker** fleet, docker-endpoint nodes and worker nodes
  are treated symmetrically at startup"* — no longer true, and the bullet above
  is explicitly an asymmetry, so the guarantee must be restated in the direction
  that now holds: *"In a mixed **worker** fleet every node is probed and marked
  in/out of service identically, and the 'no live capacity' hard-fail is still
  applied **once** over the whole fleet — never per-node and never
  per-sub-backend. The transports are deliberately asymmetric in **what** that
  one check demands of them: a docker-endpoint node must be reachable *with
  `slots > 0`*, a worker node need only be reachable, because its capacity is
  observed after boot and operator-changeable at runtime (§3.1)."*
- §3.1's **zero-seed boot** bullet — *"only a configured fleet with no reachable
  node with slots > 0 is still a fatal misconfiguration"* — narrowed the same
  way.

The neighbouring §3.6 clause *"It fails fast only when the **whole** fleet is
unreachable"* stays **true and unedited** under this rule, and that is the point
of scoping the narrowing to capacity (see §5a): it remains the deploy-time
catcher for a credentials or `NATS_URL` misconfiguration.

And in the same section's `WORKER_SLOTS` mention: it is the node's **first-boot
value only**, not the way capacity is changed.

One line in the §6 route list ([spec §6.1](../../spec.md)):

> `PUT /api/v1/platform/fleet/{node}/capacity  body: { slots } → 202; platform admins only. Sets the node's desired slot count (§3.1); 404 unknown node, 409 for a docker-endpoint node, 422 above the node's reported maximum.`

## API and wire shape

**HTTP** (`crates/api/src/routes.rs`):

```text
PUT /api/v1/platform/fleet/{node}/capacity
  body:  { "slots": 2 }
  202 →  { "node": "air", "desired": 2, "observed": 4, "state": "pending" }
  400 →  slots not a number
  403 →  not a platform admin
  404 →  unknown node
  409 →  docker-endpoint node (DOCKER_NODES owns those)
  422 →  above the node's last reported slots_max (carries the max)
```

`GET /api/v1/platform/fleet` gains, per node: `slots_desired`, `slots_max`,
`capacity_source`, `capacity_observed_at`, `capacity_state`
(`converged` | `pending` | `rejected` | `unacknowledged`), and
`capacity_note` (the daemon's rejection reason, when any).

**NATS:**

- `req.fleet.capacity.set` — api → dispatcher, `{ node, slots, by }`, replies
  with the 202 body or the `{"error": {...}}` envelope
  (`crates/dispatcher/src/handlers/`, new `fleet` family; needs a
  `store::subjects` helper and a `MODULES.md` row).
- `req.worker.{node}.set_slots` — dispatcher → daemon,
  `{ slots }` → `{ accepted, slots, slots_max, epoch, generation, note }`
  (`crates/types/src/worker.rs`, `crates/worker/src/daemon.rs`).

**Types** (`crates/types/src/worker.rs`): `WorkerAnnounce` and `PingOk` each
gain `slots_max: Option<u32>`, `capacity_epoch: Option<u64>` and
`capacity_generation: Option<u64>`; `PingOk` gains `slots: Option<u32>`. All
`#[serde(default)]`, so a pre-field daemon's messages stay decodable through
the version-skew window §3.1 already tolerates — and a missing pair reads as
`(0, 0)`, which per §1 applies only before the node's first ordered
observation.

## Implementation plan (sliced into jobs)

Per the [STYLE.md](../../STYLE.md) contract-first rule, each job names the
contract it changes.

| # | Job | Contract changed | Depends on |
| --- | --- | --- | --- |
| 1 | `docs` — apply the §3.1 amendment above (plus the §6.1 route line) | spec §3.1 capacity ownership | — |
| 2 | `code` — worker protocol + daemon: `set_slots` op, `slots`/`slots_max`/`capacity_epoch`/`capacity_generation` on `PingOk` and `WorkerAnnounce`, `WORKER_SLOTS_MAX`, immediate re-announce on adopt, bounds rejection | new `Msg`-equivalent worker op; two wire records | 1 |
| 3 | `code` — dispatcher observation: ingest capacity from both transports with `(epoch, generation)` ordering and the ping-resets-watermark backstop, applied inside `probe_worker` *before* the startup gate reads slots; the §5a narrowed `evaluate_startup` rule; `capacity_source` / `capacity_observed_at` on the roster and `fleet.status`; seed demoted to pre-observation fallback; the never-observed warning | `WorkerAnnounce` postcondition; `FleetNode` record; §3.6 startup rule | 2 |
| 4 | `code` — intent + reconciliation: `fleet.capacity` record, `req.fleet.capacity.set` handler, the bounded re-push, the "placement never reads intent" invariant + assertion | new `Msg::SetNodeCapacity`; new subject family | 3 |
| 5 | `code` — api: the `PUT` endpoint, platform-admin gate, snapshot field passthrough | §6.1 route | 4 |
| 6 | `web` — cluster-view control and the six display states; `CapacityWidget` tolerates over-cap | none (display) | 5 |
| 7 | `docs` + ops — deploy/prod/README.md §6, `.claude/skills/chug-ops/SKILL.md`, `deploy/prod/env.example`, `deploy/prod/chug-install.sh` guidance; and the sequenced `DOCKER_NODES` `|worker|0` change from §7, valid only once job 3 is deployed and every daemon is on the job-2 build | none | 6 |

Jobs 2 and 3 are independently useful: after job 3, tonight's failure mode is
*visible and self-correcting* even with no UI control at all. That is the
natural place to stop if the rest slips.

Test placement, per [testing.md](../../testing.md): the ordering rule and the
`slots_max` bound are pure functions → tier 1 (beside `parse_slots` in
`crates/worker/src/config.rs` and the announce merge in
`crates/dispatcher/src/core.rs`). The ordering tests must include the cases
that motivated the rule:

- a stale announce (lower generation, same epoch) is discarded;
- **a daemon restarts, its generation resets to 0, and its observations still
  land** because the epoch advanced — the case that a counter-only rule fails;
- a ping observation is applied and resets the watermark even when its pair is
  lower than the last applied one (the anti-freeze backstop);
- an observation with no epoch/generation applies before the first ordered
  observation and never after.

The narrowed startup rule sits beside the existing `evaluate_startup` tests in
`crates/worker/src/backend.rs`. Exactly one inverts:
`all_zero_slot_reachable_fails` splits into an all-docker case that still fails
and a worker-present case that starts. Two must be re-asserted *unchanged*,
because they are what pins the narrowing to capacity rather than to transport —
`zero_slot_docker_plus_dead_worker_fails` and, at tier 2,
`no_reachable_capacity_fails_startup` (`crates/worker/tests/nats_backend.rs`)
both keep failing startup. Add two: a reachable worker reporting 0 slots starts
(the drain-and-restart case §5a turns on), and a worker's ping-reported slots
reach the gate. The
set/adopt/announce round trip is tier 2 against a real `nats-server`
(`crates/worker/tests/nats_backend.rs`,
`crates/dispatcher/tests/dynamic_fleet.rs`); drain-below-occupancy belongs with
the existing `choose_placement` unit tests in `crates/container/src/lib.rs`.

## What this makes wrong elsewhere

Documentation that is already wrong today, or that this design falsifies:

- **`deploy/prod/README.md` §6** — states *"Prod: air runs 2 …, nuc 4"*.
  Reality tonight is **air 2 / nuc 2**. Its whole "Capacity is set on the node,
  not in `DOCKER_NODES`" paragraph, including the `build-worker.sh` recipe,
  becomes the *bootstrap* story; changing capacity moves to the UI.
- **`.claude/skills/chug-ops/SKILL.md`** — records
  `DOCKER_NODES="air|worker|4, nuc|worker|2"`, wrong on both numbers, and
  presents the seed as the capacity statement.
- **`deploy/prod/env.example`** — the `WORKER_SLOTS` and `DOCKER_NODES`
  comments describe the seed/announce precedence as the operator's control
  surface.
- **`deploy/prod/chug-install.sh`** — tells the operator to add the node to
  `DOCKER_NODES` *"then restart the dispatcher"*; after this, membership
  arrives by announce and capacity by command, so the restart instruction goes.
- **`deploy/prod/build-worker.sh` / `deploy/prod/worker-refresh.sh`** — the
  `WORKER_SLOTS` passthrough **stays** (it is the boot value), but the comments
  claiming it is where a node's concurrency is set need rewording: after a swap
  the node reports its boot value until the dispatcher reconciles. Their tests
  (`deploy/prod/build-worker.test.sh` case 2c,
  `deploy/prod/worker-refresh.test.sh` case 3e) keep passing unchanged.

## Risks and open questions

- **Transport skew.** Two transports carrying capacity is the design's main
  complexity. Mitigated by the `(epoch, generation)` pair; the failure mode if
  the ordering is wrong is a *flapping* slot count — visible rather than silent
  — and never a frozen one, because a ping always resets the watermark. The
  epoch depends on the node's own clock across its own restarts, never on
  cross-node comparison; a backwards clock jump costs at most one placement
  probe of staleness.
- **Reconcile loop.** A node that adopts, then reverts (an old build that
  ignores `set_slots`) would be pushed every scan tick forever. Bounded by
  one-push-per-tick and by treating an explicit rejection as terminal; a node
  that silently ignores the op shows as `unacknowledged` in the UI.
- **Drain to zero and the queue clock.** Covered above: the §3.5 30-minute
  queue wait still escalates. A maintenance mode that pauses that clock is a
  separate design.
- **Narrowing the startup gate trades a crash for a warning.** §5a means a
  worker fleet whose daemons answer but report no capacity now boots and
  silently places nothing, where today it crash-loops. That is the right trade
  only because §8 makes the condition loud (`capacity_source`, the
  never-observed warning, and a zero-capacity start that logs at warn); without
  §8 it would be a regression, so the two must land together — both are in
  job 3. The trade is bounded to capacity: a fleet with no reachable node at all
  still fails fast, so the check that would have caught a credentials or
  `NATS_URL` misconfiguration is not spent on this.
- **Audit history.** Only the last change is retained until a platform event
  stream exists. Named as a follow-up rather than papered over.
- **Docker-endpoint nodes keep `DOCKER_NODES`.** The single-source claim is
  scoped to worker nodes; a `unix://`/`tcp://` entry (such as the 0-slot local
  placeholder deployments carry) is unaffected, and a capacity edit against one
  is a 409, not a silent no-op.

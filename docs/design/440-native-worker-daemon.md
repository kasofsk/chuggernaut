# Design — the natively-supervised worker daemon

Status: PROPOSED — the prerequisite #309 P0 named and left unowned. Slices 1 and
2 have landed; 3–8 have not started. [D3](#decisions) is **proven on both
platforms**: its **macOS** mechanism firsthand on 2026-08-06 against the
mechanism itself — see [the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux) —
and its **Linux** mechanism on 2026-08-06 **through the shipped code path**, on
a real systemd node, both of its assertions passing together — see
[the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456).
That run needed `XDG_RUNTIME_DIR` set in the invoking environment; whether a
daemon under a supervisor has one is [slice 7](#slices)'s provisioning question
and this run does **not** settle it. [D8](#decisions) is a separate claim that
rides on the same mechanism and is **still unverified in execution**: its one
test has never reached its assertion, because it failed in its own staging, and
**why that staging failed is still unidentified** — the Linux execution records
which candidates are ruled out and how the fixture now names the answer.

Slice 2's Linux mechanism was corrected by job #451 to the scope an unprivileged
daemon can actually create — see
[the correction](#correction-2026-08-06--the-scope-an-unprivileged-daemon-can-create-job-451) —
and by job #453 to give that scope's `systemd-run` client the bus variables it
needs, which is why the Linux assertion had still never run — see
[the correction](#correction-2026-08-06--the-bus-the-client-needs-job-453). Its
assertions were then fixed twice: job #455 for a membership check that raced the
manager's start job — see
[the first execution](#correction-2026-08-06--the-first-execution-of-d3s-linux-tests-job-455) —
and job #456 for an escapee fixture that reported a failed setup as a silent
timeout, and whose staging had never executed on any machine.

Written against the tree at `1030704`. Every claim about current behavior below
was read out of the source or out of [`docs/spec.md`](../spec.md) in this tree, not
carried over from the brief or from a sibling design; where the brief and the
tree disagree, the tree wins. Two things are relied on **secondhand** and are
marked where they are load-bearing: the operator's `macos-runner` host
configuration (not checked out in this workspace, so nothing here re-derives it,
the way [#361](./361-per-run-placement.md) and [#362](./362-binary-artifacts.md)
mark theirs), and `launchd`'s documented process-group teardown semantics, which
no file in this repo states and which [slice 2](#slices) must therefore prove
rather than assume.

This document decides **one** thing: what a `chuggernaut worker` daemon that is
not itself a container looks like. It does not touch #309's phase list, does not
delete #401's `runtime.mode: host` refusal, and designs neither capability
advertisement nor device leases. Those stay #309's.

Related: [#309](./309-host-native-execution.md) §2, §6, §8, §10 and its
2026-08-05 P0 correction (finding 6); [#372](./372-chug-node-modules.md) §6, §8;
[#322](./322-macos-native-runtime.md) §1, §6;
[`docs/spec.md`](../spec.md) §3.1 (backends, self-refresh, the drain guarantee),
§3.6 (restart reconciliation); [`docs/reference/style.md`](../reference/style.md);
[`docs/reference/testing.md`](../reference/testing.md).

---

## Decisions

| # | Decision | One-line rationale |
| --- | --- | --- |
| **D1** | **One daemon per node, run natively, serving both modes.** There is never a second daemon process on a node. | A native daemon reaches the host's docker socket directly, so container mode is unchanged and the container-vs-native question becomes a deployment detail; two daemons would split one machine into two fleet rows with no one summing their slots. |
| **D2** | **Linux: a systemd unit declared by `chug.node`**, amending that module's "owns no lifecycle" charter. **macOS: a `launchd` agent in the login user's GUI domain**, the same shape `deploy/prod/install-launchd.sh` already installs for the dispatcher and api. | #372 §8's four reasons for refusing to declare the container are artifacts of the container swap; three dissolve when the daemon is native — R4 because a unit over a binary has no tag to be missing — and R3, the strongest, is answered by splitting lifecycle (nix) from run spec (the platform's env file). |
| **D3** | **Host tasks run in their own supervision unit, not the daemon's** — Linux: a transient systemd scope per task; macOS: the process group `spawn_task` already creates — **proven on both, 2026-08-06**: macOS 26.5.1 against the mechanism itself ([the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux)), and Linux through the shipped path on `gumbo-nuc-0`, with `XDG_RUNTIME_DIR` set in the invoking environment ([the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456)). | It is the same mechanism #309 §6, §7 and §2 each independently need, and it is the only way `systemctl restart chug-worker` can stop killing in-flight work. |
| **D4** | **And the daemon declines a `refresh` while any host task is live**, naming the task — evaluated **twice: at accept, and again at the swap boundary** beside `RefreshGate::drained`. | D3 covers a unit restart; it does not cover a reboot or a rebuild that restarts more than the unit, and the self-refresh is the only restart the platform performs *automatically* — a loud refusal there is cheap and unconditional. The accept check is the fast, informative one; the swap-boundary check is the one that is actually load-bearing, because the build phase runs between them. |
| **D5** | **Credentials move to a root-owned `0700` directory named by the unit, not the login user's home.** `chuggernaut admin worker-creds` is unchanged; the install step in `deploy/prod/README.md` §6 changes. | The login user is in the `docker` group and is who `build-worker.sh` ssh's in as, so a creds file under that user's home is readable by anything that user runs — a strictly worse boundary than the one the mount was pretending to give. |
| **D6** | **`build-worker.sh` renders and installs a unit + environment file; `worker-refresh.sh`'s swap collapses to "install the binary, ask the supervisor to restart".** The daemon binary is extracted from the worker image the build phase already produces. | Every mount, device and `docker inspect` carry-forward in the swap phase exists only because the daemon is a container that must be re-composed; extracting the binary keeps its build environment byte-identical to today's and needs no host Rust toolchain. |
| **D7** | **#390's drift guard keeps its meaning and gains reach**: presence-decides-refusal over the same `WORKER_*` key set, comparing the live unit's environment against the composed environment file. | The comparison was never about docker — it is about what a recreate would drop — and a declaration that is a file on the node is legible without `docker inspect`. |
| **D8** | **Of #309 P0's three known holes, two get worse and one gets better.** Environment inheritance (§10) and `/proc/<pid>/environ` (§8) get worse and stop being P3; the `setsid()` escape (§2) is closed on Linux for free by D3 — **not for free** ([the correction](#d8-is-confirmed-on-linux-and-it-needed-one-line-of-code)), and **still unverified in execution**: the one test asserting it has never reached its assertion on any machine, having failed in its own staging for a reason job #456 did not identify and did not run ([the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456)). | Blast radius is what changes: a task inheriting a *native* daemon's environment inherits the node, not a container that happens to hold a socket. |

## Slices

| # | Slice | Contract changed | Depends on | State |
| --- | --- | --- | --- | --- |
| 1 | `code` — `spawn_task` calls `env_clear()`; a host task's environment is exactly the dispatcher's launch env plus the two exit-status paths | `HostBackend` launch env (`crates/container/src/host.rs`) | — | **Landed** (job #442), plus a two-name floor the slice line does not mention — see [the correction](#correction-2026-08-05--slice-1-as-landed) |
| 2 | `code` — launch each host task into a transient supervision unit; refuse to advertise `host` when the node cannot create one. Includes the macOS proof: assert a task survives `launchctl kickstart -k` of the daemon | `HostBackend::launch` / `kill` | 1 | **Landed** (job #447), and **proven for D3 on both platforms**: the macOS proof PASSED on 2026-08-06 ([the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux)), and D3's two Linux assertions PASSED through `HostBackend` on `gumbo-nuc-0` the same day ([the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456)). Its third assertion, D8's escapee, is still unexecuted — see [the correction](#correction-2026-08-05--slice-2-as-landed), amended by job #451 for [the scope an unprivileged daemon can create](#correction-2026-08-06--the-scope-an-unprivileged-daemon-can-create-job-451), by job #453 for [the bus that scope's client needs](#correction-2026-08-06--the-bus-the-client-needs-job-453) and by job #455 for [a membership check that raced the manager](#correction-2026-08-06--the-first-execution-of-d3s-linux-tests-job-455) |
| 3 | `code` — the daemon declines `refresh` while any host task is live, with the task id in the reason: a precondition in `refresh` **and** a re-check in `run_refresh` after `quiesce`, beside the `drained` wait, failing the refresh at the `drain` stage | worker `refresh` op precondition and swap-boundary gate (`crates/worker/src/daemon.rs`) | 2 | Proposed |
| 4 | `deploy` — `chug-worker` unit + environment-file templates; `build-worker.sh` renders and installs them instead of composing `docker run`; #390's guard compares the environment file | the node run spec (`deploy/prod/build-worker.sh`) | — | Proposed |
| 5 | `deploy` — creds and the node-local artifacts move to a root-owned directory; `deploy/prod/README.md` §6 install step | node credential layout | 4 | Proposed |
| 6 | `deploy` — `worker-refresh.sh` swap phase: extract the binary from the built worker image, install, ask the supervisor to restart; delete the detached swapper and every mount/device carry-forward | spec §3.1 self-refresh | 4, 5 | Proposed |
| 7 | `code` — `nix/chug-node/` gains the unit and the `chug.node` charter amendment; the macOS plist template and its opt-in installer | `chug.node` option surface | 4, 6 | Proposed |
| 8 | `docs` — `docs/spec.md` §3.1's drain guarantee narrowed to say what survives a *native* daemon restart and what does not | spec §3.1 | 2, 3 | Proposed |

**The ordering between 2–3 and 4–6 is load-bearing**, not a preference: flipping
a node to a native daemon before the drain mechanism lands means the first deploy
after the flip kills that node's in-flight host work. It binds only on a node
that declares `host`, which today is none.

Test placement per [`docs/reference/testing.md`](../reference/testing.md): `env_clear` and the
refresh-precondition predicate are pure and belong beside the existing
`crates/container/src/host.rs` and `crates/worker/src/config.rs` unit tests
(**tier 1**); the scope escape and the survives-a-restart assertions extend
`crates/container/tests/host_backend.rs`'s simulated-restart case (**tier 2**,
and like the rest of that file it needs neither Docker nor NATS). Slices 4–6 are
shell and their tests are `deploy/prod/build-worker.test.sh` and
`deploy/prod/worker-refresh.test.sh`, which `.chug/tasks/ci.sh` already
discovers. Nothing in this repo's CI evaluates `nix/chug-node/` (#372 §2.3) and
slice 7 does not change that.

---

## What is true today

| Fact | Where | State |
| --- | --- | --- |
| The daemon is a container: `docker run -d --restart=always --name chug-worker` with the host's docker socket and `keys` bind-mounted | `deploy/prod/build-worker.sh:524` | Shipped |
| So it is docker-out-of-docker: task containers are **siblings on the host**, and container mode is correct | same, plus `docs/spec.md` §3.1 | Shipped |
| `HostBackend` spawns a task with `process_group(0)` and `.envs(&config.env)` — and **no `env_clear()`**, so the task inherits the daemon's whole environment | `crates/container/src/host.rs` (`spawn_task`) | **Superseded** by slice 1 (job #442): the environment is composed, not inherited |
| A host task's exit status is written by the task's own wrapper, not by the daemon, so the daemon need not be alive when a task exits | `crates/container/src/host.rs` (`supervised_cmd`); #309 correction finding 2 | Shipped |
| The swap runs a **detached `docker:cli` sibling** that removes `chug-worker` and re-composes `docker run` from mounts and devices recovered by `docker inspect` of the live container | `deploy/prod/worker-refresh.sh` (`swap`) | Shipped |
| `nix/chug-node/` prepares the host and deliberately declares **no** unit supervising the daemon | `nix/chug-node/options.nix` charter; #372 §8 | Shipped |
| The Mini already runs the dispatcher and api **natively under launchd**, rendered from templates | `deploy/prod/install-launchd.sh`, `deploy/prod/launchd/` | Shipped |
| `WORKER_CACHE_DIR` is env-only — a host path the daemon passes to sibling containers, never mounted into the daemon | `deploy/prod/worker-refresh.sh` swap comment; `crates/worker/src/config.rs` | Shipped |
| `WORKER_CHANNEL_BINARY` and `WORKER_REFRESH_SCRIPT` default to `/usr/local/lib/chuggernaut/…` — paths that are *inside the image* today but are shaped like host paths | `crates/worker/src/config.rs` | Shipped |
| `chuggernaut admin worker-creds` writes the `.creds` at mode `0600` on the dispatcher host; the operator `scp`s it into the node login user's `chuggernaut-worker/keys/` | `crates/cli/src/admin.rs`, `crates/cli/src/keygen.rs`, `deploy/prod/README.md` §6 | Shipped |
| Prod's nodes only ever self-refresh — `WORKER_SSH` is unset for both, so `build-worker.sh` no-ops on every deploy | `deploy/prod/README.md` | Shipped |

**The consequence #309 P0 finding 6 records, restated in one line:** a
`HostBackend` inside `chug-worker` spawns processes in the *daemon container's*
namespace, with its filesystem and its `/workspace` — "not host-native execution;
it is a second, worse container runtime." Host mode is settable since #439 and
unusable, and nothing else in #309's phase list can be proven until this is
decided.

---

## 1. One daemon or two

**Decision: one, native, serving both modes.**

The native daemon opens `unix:///var/run/docker.sock` — the same socket the
container gets by bind mount, at the same path, which is already
`WORKER_DOCKER_ENDPOINT`'s default in `crates/worker/src/config.rs`. So
`DockerBackend` is unchanged, task containers stay siblings on the host, the
labels and the two §3.6 sweeps see exactly what they see today, and container
mode does not become a migration. What changes is that `HostBackend`'s processes
land on the node instead of inside a container, which is the entire point.

**Two daemons on a mixed-mode node, priced honestly:**

- **The fleet record doubles, because the node name is the key.** A worker seed
  is `{name}|worker|{slots}` and `WorkerAnnounce` carries `node`
  (`crates/worker/src/backend.rs`); merge is by name, and per `docs/spec.md` §3.1 a
  name held by a docker-endpoint seed is refused outright. Two daemons need two
  names, so one machine becomes two rows in `fleet.status`, two heartbeats and
  two version numbers.
- **Nothing sums their slots.** `probe_worker` computes `free = slots − running`
  per row and `choose_placement` skips `free <= 0`. Two rows of 2 on a two-core
  box is a four-way over-commit by construction, and the operator's capacity
  button has to be pressed twice to drain one machine.
- **`worker-refresh` doubles on one docker daemon.** The deploy fans refresh out
  per node and fails for any node that does not confirm (`docs/spec.md` §3.1), so one
  machine gets two legs, two build phases contending for the same BuildKit cache,
  and a disk pre-flight sized for one generation guarding two.
- **Two run specs, and #390 has two live things to compare.**
- **The sweeps get ambiguous.** §3.6 step 7 kills running managed containers that
  no live task owns, per node. Two backends on one docker socket means each
  daemon's `list_managed_running` can see the other's containers.

**What the losing option genuinely buys**, and it is not nothing: host mode could
be adopted on a node without touching the proven container daemon at all, with
rollback being `rm` of one unit. The answer is that the same property comes from
the run spec instead — the flip is one unit install plus `docker rm -f
chug-worker`, and the rollback is `build-worker.sh` at the previous tag, which is
an act the operator already performs. Buying incremental rollout by permanently
doubling the fleet record is the expensive way to get it.

## 2. Supervision, per platform

### Linux (NixOS)

**A new unit, declared by `chug.node`** — which amends that module's charter.
`nix/chug-node/options.nix` says today that the module "does NOT declare the
`chug-worker` container or any unit supervising it", and #372 §8 gives four
reasons. Take them in order against a *native* daemon:

- **R1 — a unit would race the swapper.** Dissolves. The race is that the
  swapper's `docker rm -f chug-worker` is indistinguishable from a crash to a
  supervisor, which then collides with the swapper's own `docker run` on the same
  `--name`. A native swap has no `rm -f` and no second starter: it writes a
  binary and asks the supervisor to restart the unit. The supervisor *is* the
  starter.
- **R2 — two supervisors.** Dissolves. `--restart=always` stops existing; there
  is exactly one `Restart=always`.
- **R3 — two sources of truth for the run spec.** **Survives, and must be
  answered.** #372 §8 verified it as the strongest of the four: the swap
  composes the run spec from inspected mounts plus a dozen carried-forward
  `WORKER_*` values, each with its own recorded reason, and a nix-declared unit
  would hold a second static copy that a reboot would resurrect.
- **R4 — image delivery.** Dissolves, and for a different reason than R1 and R2.
  #372 §8 already marks it "weakened but not load-bearing" per its C4
  correction, and its residue — that a `pull = "never"` unit "fails hard the
  moment the tag is missing, which is the exact state the prune incident
  produced" — is an objection to a unit that runs an *image*. A native unit runs
  an installed binary, so there is no tag to be missing and no pull policy to
  set. #372 §8 closes by naming the precondition for the change it refused —
  "after image delivery moves to a registry … it is not a side effect of adding
  a module, and this design does not propose it" — and the consequence is worth
  stating plainly: **this design does not propose it either.** What it proposes
  is nix owning a *unit over a binary*, which is not the container §8 refused,
  and D6 keeps the image node-built exactly as today — the binary is extracted
  from it, so the registry precondition is never triggered.

The answer to R3 is this design's own, and it is to **split lifecycle from
configuration**. The unit is a **machine fact** and nix declares it: the binary
path, `User`, `Restart=always`, the `EnvironmentFile=` path, the kill semantics.
The **run spec** stays the platform's, in the environment file that
`build-worker.sh` renders from `deploy/prod/env.example`'s per-node variables —
the same declaration, in the same place, with the same owner.

The prior support for that split is #372 §6's closing paragraph, which
anticipates this document exactly: "the drain answer changes if **host mode**
ships … a future `chug-node` module *would* own [it], because on a host-mode
node the daemon is a unit the module declares." Note what is *not* support: the
three-clocks table in #372 §7 assigns the worker daemon to the **platform**
clock and marks it "must not touch" for the module. D2 crosses that line deliberately,
and the split is the reason it is crossable — the module takes the lifecycle,
which is clock 1, and touches no part of the run spec, which stays clock 2. §7's
own operational test is satisfied: two projects on the same node cannot want
different values for `Restart=always` or a binary path.

That keeps `chug.node` free of any platform credential or dispatcher address
(#372 §6's reason for refusing a drain hook), keeps the run spec where #265's
silent-revert findings put it, and gives R3 a real answer rather than a denial.
The unit runs as **root**, because it needs the docker socket and because #309
§8's per-task user pool is only reachable from a process that can drop
privilege — a coherence worth naming, not a coincidence.

`chug.node` gains `enable`-scoped options for the unit rather than a second
module: the daemon's binary path, the environment-file path, and whether the node
serves host mode at all. The `virtualisation.docker` assertions in
`nix/chug-node/nixos.nix` are untouched and stay correct — a native daemon still
drives dockerd, so `live-restore`, `enableOnBoot` and the prune exclusion all
still say what they say.

### macOS

**A `launchd` agent in the login user's GUI domain.** The shape is first-hand and
already in this repo: `deploy/prod/install-launchd.sh` renders
`deploy/prod/launchd/*.plist.template` and `launchctl bootstrap`s them into
`gui/$(id -u)`, which is how the Mini runs the dispatcher and api today. A worker
plist is one more template of the same shape. The GUI domain rather than a system
daemon is forced, not chosen: per #322, CoreSimulator and the keychain are
per-user-session services, so a `LaunchDaemon` would be in the wrong session.

Two consequences, one of them a real finding:

- **`install-launchd.sh` globs its template directory**, so dropping a worker
  template beside the others would install a worker agent on the Mini — the
  control-plane host, whose own colima node sits at 0 slots precisely so heavy
  builds cannot starve it (`deploy/prod/README.md`). The worker plist therefore
  needs its own opt-in installer or a name the glob excludes; it cannot simply
  join the directory.
- **The daemon runs as the task user, not root**, so macOS cannot have D5's
  root-owned credential boundary or #309 §8's per-task user pool. That asymmetry
  is stated rather than papered over: it is the same platform gap #322 §7 already
  lists under "what the recommendation gives up".

**Secondhand:** where the plist and the node's environment file live in the
operator's `macos-runner` configuration is not verifiable from this workspace.
The claim this design makes about it is only that it is the natural home, in the
same way the NixOS unit's home is the consuming host repo.

## 3. The drain guarantee — the crux

`docs/spec.md` §3.1 guarantees that a worker swap does not interrupt in-flight work:
`wait` is implemented dispatcher-side as an inspect poll "so worker restarts are
transparent (containers keep running; the poll re-attaches)", and
`worker-refresh.sh`'s swap comment states the mechanism — `docker rm -f` hits
only `chug-worker`, so job containers survive.

**Start from the fact that the guarantee is already broken for host mode, today,
on a deployed node.** With the containerized daemon and `WORKER_MODES` naming
`host`, a host task is a process *inside* `chug-worker`, so the swapper's
`docker rm -f chug-worker` kills it. Going native does not break the guarantee.
It is the first opportunity to fix it.

The reason it is a crux is that the guarantee changes **kind**. In container mode
it is *structural*: the daemon has no mechanism by which it could end a sibling
container's life, so the property holds without anyone maintaining it. A native
daemon whose children are host processes can only have a *conditional*
guarantee — one that holds while a mechanism keeps working. Degrading a
structural invariant to a conditional one is the real cost of going native, and
`docs/spec.md` §3.1 should say so rather than keep a sentence that is true of one mode
only (slice 8).

### The mechanism (D3)

**Linux: a transient systemd scope per task**, `systemd-run --scope
--unit=chug-task-{id}`. A scope is its own cgroup, so `systemctl restart
chug-worker` — which is what a `nixos-rebuild switch` performs — kills the
daemon's cgroup and not the task's; the task is reparented to pid 1 and keeps
running. This is #309 §6's own recommendation, and #309 calls it "the single most
load-bearing implementation detail in this document on Linux" because §7 needs
the same scope for `CPUQuota`/`MemoryMax` and §2 needs it to kill a `setsid()`
escapee. Three requirements, one mechanism.

It composes with what P0 already shipped rather than replacing it. The exit
status is written by the task's own wrapper (`supervised_cmd`), so the daemon
holding a `Child` handle it can no longer `wait` on costs nothing — the handle
was already only a backstop, which is exactly why #309 correction finding 2
called that the load-bearing decision. `inspect` stays a pure function of the
task directory.

**macOS: the process group `spawn_task` already creates.** `launchd`'s teardown
is process-group scoped, not cgroup scoped, and `spawn_task` calls
`process_group(0)` — so a host task is plausibly *already* outside the daemon
agent's teardown set, and macOS may need no new mechanism at all. That is a claim
about `launchd` semantics that no file in this repo states, so slice 2 **proves
it with a test** — kill the daemon agent, assert the task is still live and still
lands its `exit_code` — rather than shipping it as an assumption. If it does not
hold, the fallback is #322 §6's second mitigation, one `launchd` job per task,
and that is more machinery than this design should pre-commit.

### What the mechanism does not cover, stated plainly

The scope escape covers a **unit restart**. It does not cover:

- a **reboot** — nothing survives one, on either platform, and #322 §1's boot
  generation is how a surviving task directory is correctly read as dead;
- a `nixos-rebuild switch` that restarts more than the daemon's unit, or a
  `nix.gc` that collects a store path a running task is still using (#372 §6's
  third drain case);
- **macOS**, until slice 2's proof lands.

So the mechanism is not sufficient on its own, and D4 is the backstop: **the
daemon declines a `refresh` while any host task is live**, naming the task. This
is #322 §6's phase-1 answer, generalized off macOS, and the argument for it is
the same — failing fast and loud beats silently killing a forty-minute build.

**Where the check lives matters more than the check.** A refresh is minutes
long, and `refresh` in `crates/worker/src/daemon.rs` accepts and returns *before*
`run_refresh` runs the build phase; `RefreshGate` only stops admitting launches
when `quiesce` opens the swap window, after the build, and `drained` then waits
only on the accept→container-exists window, not on running tasks. So a host task
accepted after an accept-time precondition passed and still running at the swap
is exactly the case a single check misses. The condition is therefore evaluated
**twice**:

- **At accept**, as a precondition in `refresh` — cheap, and it turns the common
  case into an immediate, informative refusal rather than a wasted build.
- **At the swap boundary**, in `run_refresh` after `quiesce` and beside the
  `drained` wait. This is the one that actually holds the guarantee. It needs no
  new machinery: `RefreshGate`'s existing handshake is where it hangs, and the
  failure path already exists — the drain-timeout branch records a failure at
  the `drain` stage and calls `abort()`, so a live host task at the swap takes
  the same branch with a different reason.

**A host task that appears mid-build aborts the refresh; it does not wait.**
Waiting is the tempting option and it is wrong here: the existing drain waits
30s, while a host task is bounded only by its job type's `task_timeout`, which
across `.chug/jobs/` runs from `10m` to `2h`. So a waiting
swap would either need a second, far longer timeout or would hold the swap
window open — refusing every launch on the node — for the length of a build.
Aborting costs the build phase's work and one deploy leg, and the deploy retries
against a node that is by then likely idle. The one real cost is that on a busy
host-mode node a refresh can be starved by successive tasks; that is the same
argument for scheduled draining made below, not a reason to wait.

The self-refresh is the only restart the
platform performs automatically; every other one is an operator at a keyboard,
who has `slots: 0` (`docs/reference/runbooks/worker-capacity.md`) and is already told to
drain before a rebuild by `nix/chug-node/options.nix`'s own header.

**The cost of the refusal, priced:** a deploy fails for a node that is running
host work, because `docs/spec.md` §3.1 fails the deploy for any node that does not
confirm onto the target SHA. That converts "the deploy silently killed a build"
into "the deploy failed and said why", which is the right trade, and it is
bounded by `task_timeout` rather than being open-ended. It is worse for a node
running long host work continuously — which is an argument for draining that node
on a schedule, not for dropping the refusal.

**What in-flight host work still does not survive:** a reboot, and a host rebuild
that restarts the world. Both are operator acts with a documented drain step in
front of them, and neither is something the platform can make lossless.

## 4. Credentials

Today the container gets `keys` by a read-only bind of the node login user's
`chuggernaut-worker/keys`, and `NATS_CREDS` names `/data/keys/worker.creds`
inside the container. The mount's `:ro` bit is doing less than it looks: the bind
*source* is a directory in the login user's home, so the boundary was never
"only the container can read this".

**A native daemon reads the file off the host, and the layout changes with it:**

- **Owner and mode.** A root-owned directory at `0700` holding `worker.creds` and
  `worker_git` at `0600`, outside any user's home. On Linux the unit runs as
  root, so the daemon can read them and the task-user pool cannot — which is what
  makes the credential boundary real rather than nominal for the first time. On
  macOS the daemon runs as the task user and this reduces to `0600` in that
  user's home, i.e. the status quo; #322 §7 already lists cross-task secret
  isolation on macOS as given up.
- **No `$HOME` in the path.** `worker-refresh.sh` records the bug this avoids:
  the swapper runs with `HOME=/root`, so re-deriving `$HOME` there would bind an
  empty directory and strand the daemon without NATS creds. A unit's environment
  is not a login shell's, so a home-relative credential path is the same trap one
  supervisor over.
- **The node-local artifacts move with them.** `WORKER_CHANNEL_BINARY` and
  `WORKER_REFRESH_SCRIPT` default to `/usr/local/lib/chuggernaut/…` — image paths
  today, but already *shaped* like host paths. Materializing that same layout on
  the host makes both defaults correct with **no change to
  `crates/worker/src/config.rs`**.

**`chuggernaut admin worker-creds` does not change.** It already mints at `0600`
and prints the path (`crates/cli/src/admin.rs`, `crates/cli/src/keygen.rs`); it
is local-only, signs with the account seed, and knows nothing about how the node
consumes the file. What changes is the *install* step in `deploy/prod/README.md`
§6: `scp` to a staging path the login user owns, then `install -o root -m 0600`
into the daemon's directory. Rotation is unchanged — re-mint, re-install,
restart the unit — and the restart is now a supervisor command instead of a
container recreate.

One thing the design must forbid rather than assume: the daemon's own credential
path rides in its environment, and `spawn_task` does not `env_clear()`, so today
a host task inherits `NATS_CREDS` pointing at the node's identity. Slice 1 is
what closes that, and D5's file mode is what makes the leak harmless if a future
launch path reintroduces it. Two independent guards, deliberately.

## 5. What the two scripts become

### `build-worker.sh`

It stops composing a `docker run` line and becomes a **node-join and
spec-apply** script: render the environment file from the same per-node
`WORKER_*` variables it reads today, install it, install the unit (Linux) or the
plist (macOS), and ask the supervisor to (re)start. Its pre-flight guards keep
their meaning verbatim, because every one of them is about the **node**, not
about docker — the image label check, the disk pre-flight, and the refusals it
already prints with the live daemon untouched.

Three things simply disappear, and they are the ones that only existed to give a
*container* a view of the host: the KVM `--device` flag, the read-only nix mount
arguments, and the keys bind. A native daemon has the node's devices, the node's
`/nix`, and the node's filesystem by construction. The KVM shape check in
`build-worker.sh` — that the toolchain path is a direct symlink into the store
under a real parent, because the daemon container mounts the parent and resolves
the symlink itself — is a *mount* constraint, so it can be deleted rather than
ported. That is a real simplification of the hardest guard in the file.

### `worker-refresh.sh`

The **build** phase is nearly unchanged: the node still runs container tasks, so
it still builds the agent images, still verifies labels, still retag-swaps and
still prunes. The daemon image keeps being built too — which is D6's mechanism:
the swap **extracts the binary from the image it just built** (`docker create` +
`docker cp`) rather than compiling on the host. That keeps the daemon's build
environment byte-identical to today's, needs no Rust toolchain as a node machine
fact, and leaves the pinned Dockerfile as the single definition of how the binary
is produced. Compiling natively on the node was the obvious alternative and is
rejected on exactly that ground: it would make the toolchain a per-node fact that
can drift, on a path where the failure mode is a node that will not come back.

The **swap** phase collapses. Gone: the detached `docker:cli` swapper, the
`KEYS_SRC`/`SOCK_SRC` recovery, the KVM-device carry-forward, the nix-mount
carry-forward, the `RUN_NEW` composition, and the retained `chug-worker-swap`
transcript — which existed because "the daemon that reports to the dispatcher is
the very thing being replaced", a problem the supervisor's own log
(`journalctl -u chug-worker`, or the agent's log path on macOS) already solves.
What remains: install the extracted binary and the refresh script, then ask the
supervisor to restart. The daemon cannot `docker rm -f` itself today and it
cannot `systemctl restart` itself synchronously either, so the restart is
requested and the process exits — which is what `Restart=always` and `KeepAlive`
are for.

The **environment carry-forward goes away entirely, and that is the largest
change in the file.** Every `*_ARGS` block in the swap phase exists because
inheritance is how a value survives a container recreate; with an environment
file on disk, the value survives because it is written down. That deletes
the #55/#82 silent-revert class at its root rather than defending against it
eleven times.

### #390's drift guard

It keeps meaning, and gains. The guard's shape is presence-decides-refusal:
enumerate the live daemon's `WORKER_*` environment, and refuse if the composition
about to be applied would drop one, with the value comparison informational only.
Natively that becomes: enumerate the live unit's environment, compare against the
composed environment file, same key set, same refusal, same `WORKER_SPEC_DROP_OK`
escape hatch.

It gains reach in one direction and loses it in another, and both should be
stated. It **gains** because the declaration becomes a file on the node that an
operator can read without `docker inspect` — which matters on prod, where
`WORKER_SSH` is unset for both nodes and `build-worker.sh` no-ops on every
deploy, so the node's spec is currently only visible in the refresh's own stdout
report. It **loses** the mounts-and-devices half of the comparison, which is not
a loss: those flags stop existing, so there is nothing left to drop.

## 6. What P0's known holes become

The #309 P0 correction lists three, all P3 in that document's phase list. A
native daemon moves two of them.

**§10 — the task inherits the daemon's environment, including a reachable docker
socket. Worse, and no longer P3.** Verified: `spawn_task` calls
`.envs(&config.env)` with no `env_clear()`. Today the inherited environment is a
container's, and it already includes a mounted `/var/run/docker.sock`, which is
what #309 §10 calls "effectively root on the node". Natively the task
inherits a *node* process's environment: the same socket without the indirection,
plus the node's filesystem, plus whatever the supervisor put in the unit's
environment — including `NATS_CREDS`. The power was already total; what grows is
the surface and the number of ways to reach it. Slice 1 makes it a prerequisite
of shipping native rather than a hardening step, because "the launch environment
is exactly what the dispatcher specified" is the only version of this that is
checkable.

**§8 — secrets readable via `/proc/<pid>/environ`. Worse.** Today the readers are
processes inside `chug-worker`, which is the daemon and its own tasks. Natively
the readers are every process on the node running as the same uid — and the node
login user is who `build-worker.sh` ssh's in as and who runs `docker build`. On a
node pinned to one project this is #309 §8 option (a), accept-and-document, and
stays P3. On a node serving more than one project it is a cross-project leak,
so the #309 §8 per-task user pool stops being hardening and becomes the
condition for advertising `host` on a multi-project node — #309 §8's own rule ("the
daemon does not advertise `host` unless the user pool is provisioned"), now with
a sharper reason to hold it.

**§2 — a `setsid()` task escapes `kill`. Better, on Linux, for free.** The
transient scope D3 mandates is a cgroup, and killing a cgroup catches an escapee
that a process-group signal misses. So the mechanism the drain problem forces is
the same one that closes this hole — which is #309 §2's own observation that
three requirements share one mechanism, now with the drain as the requirement
that makes it non-optional. **On macOS it is unchanged and still leaks**, since
the mechanism there is the process group and a `setsid()` call is precisely what
leaves one.

---

## Risks and open questions

- **The macOS half is the weakest, and it is the half that motivates host mode.**
  D3's macOS mechanism is a claim about `launchd` that this repo does not state,
  the daemon cannot run as root there so D5's boundary does not port, and the
  plist's home in the operator's `macos-runner` configuration is secondhand.
  Slice 2's proof is what converts the first of those from assumption to fact;
  the other two are stated as gaps.
- **Amending `chug.node`'s charter is a one-way door.** #372 §8 is a carefully
  argued refusal and this document reverses part of it. R1, R2 and R4 dissolve
  cleanly; R3's answer — nix owns the unit, the platform owns the environment
  file — is a *split*, and splits drift. The mitigation is that #390's guard
  moves to the environment file with it, so a drift between the two halves is
  detected on the same path that detects a dropped knob today.
- **The flip is a node-down risk on a path with no fast rollback.** Prod's nodes
  cannot be ssh'd from the Mini, so a native daemon that refuses to start leaves
  a node that only the operator's laptop can reach. Slice 4 should carry the same
  refuse-with-the-live-daemon-untouched discipline `build-worker.sh` already
  applies to the KVM and nix guards, and the first flip should be a node that is
  reachable.
- **This design does not prove host execution works on a real node.** #309's P0
  correction lists that as still unverified, and it stays unverified after this:
  what this decides is the daemon that would make the proof *possible*. The proof
  itself is a later job, in the shape of `.chug/jobs/android-proof.yaml` and
  `.chug/jobs/gcp-proof.yaml`, and per #308 H.4 against a boring cache-heavy
  build.
- **Nothing here changes the fleet, the wire, or any schema.** No node change, no
  env change on a live node, no `runtime.mode: host` edit, no epoch. That is
  deliberate — the slices are all node-local or deploy-local, so each can land
  and be reverted without a version-skew window.

## What this makes wrong elsewhere

- **`docs/spec.md` §3.1's drain guarantee** is written for a containerized daemon and
  becomes mode-specific; slice 8 narrows it.
- **`nix/chug-node/options.nix`'s charter** says the module "owns no lifecycle"
  and "does NOT declare the `chug-worker` container or any unit supervising it".
  Slice 7 amends both lines and must cite this document beside #372 §8.
- **#372 §6's closing paragraph** already anticipates this: "the drain answer
  changes if **host mode** ships … a future `chug-node` module *would* own
  [it]". This is that document; the paragraph should point here.
- **`deploy/prod/README.md` §6's worker-join steps** describe a `scp` into the
  login user's home and a `docker run`; slices 4–6 replace both.
- **#322 §6's self-refresh collision** is resolved for the general case here.
  Its phase-1 refusal is adopted as D4 rather than superseded, and its phase-2
  per-task `launchd` job stays the macOS fallback if slice 2's proof fails.

---

## Correction, 2026-08-05 — slice 1 as landed

Appended by job #442, which implemented [slice 1](#slices). Nothing above is
edited except that slice's State cell and the `env_clear()` row of
[what is true today](#what-is-true-today); this section records the floor the
slice line does not mention and what measuring the parity target found.

### The floor is two names, carried from the daemon

`spawn_task` clears the environment and then composes it in three layers: the
floor, the dispatcher's launch env, the two exit-status paths. In that order, so
a launch declaring a floor name wins — the same precedence a container's env has
over its image's.

| Variable | Why it is in the floor |
| --- | --- |
| `PATH` | The bootstrap clones with `git` and `ssh`, which #309 §9 calls machine facts on a host node: on a nix or a macOS node they sit on the daemon's `PATH` and in no fixed system directory. A daemon carrying none falls back to the value docker gives a container whose image declares none. |
| `HOME` | Docker sets one for every container from the image's user, and `git`, `ssh`, `cargo` and the agent harness all key per-user state off it. A daemon carrying none leaves the task carrying none — there is no correct value to invent, and each tool's own passwd-entry fallback is the honest answer. |

Nothing else. `TMPDIR`, `LANG` and `HOSTNAME` were weighed and refused: a
container is given no `TMPDIR` or `LANG` by either the image or the runtime, so
setting them here would be a *divergence* from the parity target rather than
parity, and a host task's hostname is the node's whether or not a variable says
so.

**Measured rather than assumed**, on this repo's own Debian agent image: a
process spawned with a cleared environment sees exactly one variable, the `PATH`
the shell supplies from its own compiled-in default, and no `HOME`. So "nothing
visibly broke without a floor" was true here and beside the point — what has to
reach a host task is the *node's* `PATH`, and a shell's built-in default is the
undocumented version of the hardcoding this slice removes.

### The parity target, and the two differences that remain

Docker composes a task's environment from three sources: the image's baked
`ENV`, the runtime's own additions, and `config.env`. A host task has no image,
so two differences are structural rather than oversights:

- **The image's `ENV` has no host analogue.** `deploy/prod/Dockerfile.agent-rust`
  bakes `CARGO_TARGET_DIR` and `IS_SANDBOX`, and its `rust:1.96-bookworm` base
  bakes `CARGO_HOME`, `RUSTUP_HOME` and a `PATH` reaching cargo's bin directory.
  A host task gets none of them, which is #309 §9's own answer — a host node's
  toolchain is `runtime.env` (P1) or machine configuration, never a value this
  backend invents. It does mean an agent job on a host node would run without
  the sandbox marker its image sets; that is P1's to answer, and no node runs
  host work today.
- **`HOSTNAME` is a docker fact.** A container's is its id; a host task shares
  the node's, and `hostname` answers without an environment variable.

Everything else now has the same shape in both backends: the launch env last-wins
over what the runtime supplies, and the backend's own variables come after it.
The docker backend was read and **not** changed.

### What this does not close

`env_clear` narrows *what* is exposed, never *to whom*. #309 §8 —
`/proc/<pid>/environ` readable by every process of the same uid — is untouched
and stays exactly what [D8](#decisions) says it is. What changed is that the
environment a reader finds there is the dispatcher's launch env and nothing
else, rather than that plus everything the daemon was started with.

---

## Correction, 2026-08-05 — slice 2 as landed

Appended by job #447, which implemented [slice 2](#slices). Nothing above is
edited except that slice's State cell. This section records what the mechanism
became, what was measured, and — most importantly — **what is still unproven**.

### The mechanism, as built

`HostBackend` carries a `Supervision` (`crates/container/src/host.rs`), named by
its constructor rather than discovered per launch:

| Variant | Launch becomes | Proven |
| --- | --- | --- |
| `Scope` | `systemd-run --scope --quiet --collect --unit=chug-task-{task}.scope -- {the task's argv}` | Asserted at tier 2, **skipped** wherever no scope can be created — see below |
| `ProcessGroup` | the argv unchanged; the mechanism is the `process_group(0)` `spawn_task` already sets | **No.** The procedure is `docs/reference/runbooks/macos-host-supervision-proof.md` and nobody has run it |

`--scope` runs the command from `systemd-run` itself rather than handing it to
pid 1, so the composed environment [slice 1](#correction-2026-08-05--slice-1-as-landed)
built, the cwd and the two log fds are inherited exactly as they were without
it — the scope is a cgroup change and nothing else. `--collect` is what reclaims
the unit when the task's last process exits, so a node that runs for months does
not accumulate dead scopes. The unit name is recorded in `meta.json`, so it
survives a daemon restart the way the pid and start time already do.

**The refusal.** `probe_supervision()` asks the node to create one throwaway
scope at daemon start; `enforce_host_supervision` in `crates/worker/src/daemon.rs`
turns a refusal into a `Config` error beside `enforce_host_capacity`, so the boot
fails naming the node and carrying the probe's own reason. Where #434's refusal
is about a value the operator set, this one is about a capability the machine
either has or does not — but the shape is the same, and it is the shape #309 §7
demands: a node never advertises what it cannot serve.

**Both `systemd` calls are bounded**, by `SYSTEMD_BOUND` (10s). `systemd-run`
waits on the manager's own start job, so an unbounded wait would turn this
slice's point — refuse loudly, by name — into a daemon that hangs at boot saying
nothing, on nodes the risk list above notes cannot be reached from the Mini.
Expiry is therefore a named refusal reason like any other, and in `kill` a
`systemctl` that does not answer in time is a logged failure naming what was
*not* reached (the escapee) rather than a blocked task.

### D8 is confirmed on Linux, and it needed one line of code

The `setsid()` escape (#309 §2) **does** close on Linux, and it is not free the
way D8's "for free" suggests — the scope is what makes closing it *possible*, but
`kill` has to actually address the cgroup. So `HostBackend::kill` now signals the
process group **and** the scope (`systemctl kill --signal=… chug-task-{task}.scope`),
in that order, at both SIGTERM and the SIGKILL escalation. The group signal alone
misses an escapee by construction; the scope signal catches it, because leaving a
process group does not leave a cgroup. Both are sent rather than one chosen, so a
node whose `systemctl` is unusable still gets today's behavior instead of none —
and the group goes **first** because that signal cannot block, so a slow bus
delays only the half that reaches the escapee. On macOS this is unchanged and
**still leaks**, exactly as D8 says.

### The Linux proof runs, or announces that it did not

Three tier-2 tests in `crates/container/tests/host_backend.rs`:
`a_host_task_runs_in_its_own_supervision_unit` (the backend puts the task in a
cgroup that is not inside the launcher's),
`a_host_task_survives_the_teardown_of_the_launching_unit` (a stand-in daemon
scope launches a task through the shipped composition; `systemctl kill --signal=SIGKILL`
of the daemon's unit kills the daemon and leaves the task running), and
`a_kill_reaches_a_setsid_escapee_through_the_scope` (the D8 claim itself: a
`setsid()` child, verified to be out of the task's process group and inside its
cgroup, dies with a `HostBackend::kill` — which only the scope signal can have
done). The argv both scope signals become is asserted at tier 1 by
`a_killed_task_is_signalled_through_its_scope_at_both_stages`, so the SIGTERM and
SIGKILL spellings are covered on every machine even where the send is not.

**Measured, on the evaluator this job ran in: it cannot.** There is no
`systemd-run` on `PATH`, pid 1 is `sh`, and `/proc/self/cgroup` reads `0::/`.
All three tests therefore self-skip, printing the reason and the words "is NOT
covered by this run" — the `docker_available()` precedent. A systemd host still
running cgroup v1 self-skips the same way rather than failing the crux assertion
over a hierarchy it cannot read back, and so does a machine with no usable
`setsid` to stage the escapee with. That is the honest state:
**the Linux assertion is written and unexecuted in CI.** It runs the first time
anyone points the suite at a systemd host, which is any prospective host-mode
node, and that is where it is worth running.

### What this does not do

No node advertises `host`, no deploy path changed, and slice 3's `refresh`
precondition is untouched — D3 covers a unit restart and D4 is still the backstop
for everything else, unbuilt. `docs/spec.md` §3.1's drain guarantee is still
written for a containerized daemon; narrowing it is slice 8's, and slice 8 should
not be read as blocked on the macOS proof — it is blocked on the proof having an
*answer*, which is one operator command away.

---

## Proofs, 2026-08-06 — D3 on macOS and on Linux

Appended by job #452, a `docs` job that changed no code and no gate. The
operator ran both proofs on 2026-08-06 against the tree at `c8a8354`. Nothing
above is edited except the head: the `Status:` line, [D3](#decisions)'s cell and
[slice 2](#slices)'s State cell.

**This section is what retires the `launchd` half of the preamble's
secondhand marking.** The preamble stands as written, because a design doc's body
is append-only ([#415](./415-knowledge-architecture.md) D2); what changed is that
the process-group teardown semantics it says "no file in this repo states" are
now stated *here*, from a run on a named OS version rather than from
documentation nobody in this repo has read. The other secondhand item — the
operator's `macos-runner` host configuration — is untouched and still secondhand.

The two results are not equal, and the asymmetry is the finding:

| Platform | Node | What was exercised | Verdict |
| --- | --- | --- | --- |
| macOS 26.5.1 (Darwin) | `gumbo-air-0` | the shipped proof script, restarting a real `launchd` agent with the same `launchctl kickstart -k` a native `worker-refresh.sh` would use | **PASS** — firsthand, against the real mechanism |
| NixOS (systemd, cgroup v2) | `gumbo-nuc-0` | a hand-built transient **`--user`** scope whose parent process was `SIGKILL`ed — not `HostBackend`, not `probe_supervision`, not the test suite | the *mechanism* holds; the *shipped path* is still unexercised |

### macOS — PASSED, against the mechanism itself

On `gumbo-air-0` (Darwin 26.5.1), in a checkout of this repo:

```sh
sh deploy/prod/macos-host-supervision-proof.sh
```

```text
task pid=89504 pgid=89504 ; stand-in daemon pid=89502 pgid=89502
kicking: launchctl kickstart -k gui/501/com.chuggernaut.host-supervision-proof
PASS: the task survived 'launchctl kickstart -k' of the agent that launched it and landed exit code 7
      => design #440 D3's macOS mechanism (the process group) holds on 26.5.1
```

- **Both halves of the assertion landed, and both are load-bearing.** The task
  survived the restart *and* wrote its own exit code afterwards. A survivor that
  loses its status would still be a broken drain guarantee, because the exit
  status is what `supervised_cmd`'s wrapper exists to deliver and what the
  daemon reads back on `inspect`.
- **It restarted the agent, not a stand-in for the restart.** Step 3 of the
  procedure is `launchctl kickstart -k` of the agent that launched the task —
  the same command a native `worker-refresh.sh` would issue at the swap. The
  daemon is stood in for; the teardown event is not.
- **What it settles.** The `Proven` cell for `ProcessGroup` in
  [the slice-2 correction](#correction-2026-08-05--slice-2-as-landed) read
  "**No.** … nobody has run it". For macOS 26.5.1 that is now yes, and #322 §6's
  per-task `launchd` job stays the fallback it was — unadopted, and now with one
  fewer reason to be adopted.
- **What it does not settle.** [D8](#decisions)'s `setsid()` leak on macOS is
  untouched: a process that leaves its group leaves the only mechanism macOS
  has. Nothing here survives a reboot. And no node runs a native daemon, so what
  is proven is the mechanism such a daemon would rely on, not a deployment. The
  answer is also a property of *that host's OS version* — re-run the script on
  any macOS node before it is considered for `host` mode.

### Linux — confirmed by hand only, and it is the weaker of the two

Job #451 is why it was by hand: on `gumbo-nuc-0` the three tier-2 tests in
`crates/container/tests/host_backend.rs` could not run, so D3's Linux half was
tested outside the suite. The run, in three steps:

1. A parent shell created a transient user scope around a task built to outlive
   it: `systemd-run --user --scope -- sh -c 'sleep 45; echo done > /tmp/d3-exit'`.
2. At t+3 the parent process was killed with `SIGKILL`.
3. `/tmp/d3-exit` was written at **t+45**.

So the task outlived its launcher and landed its result, which is the shape D3
claims. **Three limits, and every one of them has to be carried with the
result:**

1. **It killed a parent process, not a unit.** What `worker-refresh.sh` does —
   and what the macOS proof actually did — is restart a *supervision unit*.
   Killing the shell that ran `systemd-run` is the weaker event, and the scope
   surviving it is the weaker claim.
2. **It used a `--user` scope; the shipped code asks for a system one.**
   `scope_args` in `crates/container/src/host.rs` passes
   `--scope --quiet --collect --unit=…` and no `--user`, so `probe_supervision`
   asks for a system scope — which polkit denies an unprivileged caller. That is
   the defect job #451 owns. This run therefore proves the *mechanism*, not the
   code path that would use it.
3. **`XDG_RUNTIME_DIR` was unset** in the ssh session where the `--user` scope
   worked. Whether that holds for a daemon running under a supervisor is #451's
   question, not this one's.

**So D3 is not proven on Linux, and nothing should be written as if it were.**
The mechanism is confirmed; the shipped path is unexercised. What closes it is
the three assertions running on `gumbo-nuc-0` after #451 —
`a_host_task_runs_in_its_own_supervision_unit`,
`a_host_task_survives_the_teardown_of_the_launching_unit` and
`a_kill_reaches_a_setsid_escapee_through_the_scope` — which is exactly the "it
runs the first time anyone points the suite at a systemd host" the slice-2
correction named, still outstanding.

**Superseded the same day.** Those three assertions did run on `gumbo-nuc-0`,
and after two fixes D3's two of them pass through the shipped path — see
[the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456).
Everything above stands as the record of what the hand run alone established.

### What this lifts, and what it does not

- **Slice 2 is half proven, not proven.** Its macOS half is verified against the
  real mechanism; its Linux half is code written, self-skipped in CI, and
  corroborated only by the hand run above.
- **Slices 3–8 were gated on D3 and that gate is only partly lifted.** Nothing
  here starts them; when they start is the operator's call.
- **Slice 8 gains its macOS answer.** [`docs/spec.md`](../spec.md) §3.1's drain
  guarantee is still written for a containerized daemon, and the "what survives a
  native daemon restart" half of the narrowing now has a measured answer on
  macOS and an unexecuted one on Linux.
- **The runbook is corrected.**
  [`docs/reference/runbooks/macos-host-supervision-proof.md`](../reference/runbooks/macos-host-supervision-proof.md)
  said nobody had run the proof; that is no longer true, and it now carries the
  result and points here.
- **No code, no gate, no test changed** in the job that recorded this. The
  `--user`/system scope defect is #451's, and `probe_supervision`, `scope_or_skip`
  and the suite were left exactly as job #447 shipped them.

---

## Correction, 2026-08-06 — the scope an unprivileged daemon can create (job #451)

Appended by job #451, which fixes a defect in [slice 2](#slices) as landed —
the one [the proofs section](#proofs-2026-08-06--d3-on-macos-and-on-linux)
records as its Linux limit 2. Nothing above is edited except that slice's State
cell. Slice 2 asked for a **system** transient scope unconditionally. polkit
denies that to an unprivileged user, so on the one real candidate node the
mechanism refused itself. Both sections were written against the same tree and
landed in the other order: #452 recorded the operator's two runs, and this one
decides what the code asks for and reads the same Linux run for what it does and
does not establish about [D3](#decisions).

### What was measured on `gumbo-nuc-0`

NixOS, systemd, cgroup v2, at `c8a8354`, as the unprivileged `worksalot` over a
non-login ssh:

| Asked | Answer |
| --- | --- |
| `systemd-run --scope --quiet true` | **exit 1** — `Failed to start transient scope unit: Access denied`, "as the requested operation requires interactive authentication" |
| `systemd-run --user --scope --quiet true` | **exit 0** — `systemctl --user is-system-running` reports `running` |
| `sudo -n systemd-run --scope --quiet true` | **exit 0** — passwordless sudo is available on that node |

So all three of D3's Linux tests self-skipped on a node fully capable of running
them. The guard behaved exactly as designed: it named the failure, printed "is
NOT covered by this run", and refused to certify the mechanism vacuously. It is
kept as it is. What was wrong is one flag in the thing it was measuring, and the
consequence was not cosmetic — a real node would refuse to advertise `host` at
all (job #447's `enforce_host_supervision`) for a defect rather than a missing
capability.

### D3's Linux claim is confirmed, by hand, outside the suite

Run by the operator on the same node, and the first evidence D3's Linux half
holds anywhere — the run
[the proofs section](#linux--confirmed-by-hand-only-and-it-is-the-weaker-of-the-two)
also records: a parent process created a `--user` scope running `sh -c "sleep
45; echo done > /tmp/d3-exit"`, and was SIGKILLed at t+3. `/tmp/d3-exit` was
written at **t+45**.

**What it establishes.** A task in a transient scope survives the death of the
process that launched it, keeps running to completion, and lands its exit
status — the property slices 3–8 rest on and spec §3.1's drain guarantee needs
in host mode. It also establishes it for a `--user` scope specifically, which is
the mode this correction ships for an unprivileged daemon.

**What it does not.** It killed a **process**, not a **unit**. The thing D3
actually promises is that `systemctl restart chug-worker` — a *unit* teardown,
which kills a cgroup rather than a pid — leaves the task running, and a pid kill
does not exercise that at all: a scope's independence from its launcher's
process tree is weaker than its independence from its launcher's cgroup. Only
`a_host_task_survives_the_teardown_of_the_launching_unit` asserts the real
thing, and it is still unexecuted. The hand proof raises D3 from *unproven* to
*plausible with a mechanism demonstrated*; it does not close it.

### The decision: the manager follows the daemon's privilege

`manager_for(euid)` in `crates/container/src/host.rs` picks the **system**
manager for a root daemon and the user's own `systemd --user` for anything else.
There is no fallback between them. Three options were on the table:

- **`--user` unconditionally** — rejected. [D2](#decisions) runs the Linux daemon
  as **root**, and a root daemon's "user manager" is `user@0.service`, which a
  NixOS node has no reason to be running. That would take the one configuration
  this design actually ships and hand it the weaker of the two mechanisms, on the
  strength of a measurement taken as a different user.
- **system, then user on failure** — rejected. It works, but it makes the
  mechanism's identity depend on which attempt won, and the two are **not**
  equivalent (next section). A node must know which one it got and an operator
  reading a log must not have to guess; and the fallback's first leg prints the
  same `Access denied` on every boot of every unprivileged node, which is the
  line this job exists because someone read.
- **`sudo -n systemd-run --scope`** — rejected outright. It works on that node
  and is the wrong shape: a daemon that escalates to create a scope has an
  undeclared dependency on a sudoers rule, gets root-owned cgroups it cannot
  clean up as itself, and buries a privilege boundary inside a launch path.
  If a node wants system scopes it should run the daemon as root, which is what
  D2 already says.

Selection by euid gives prod (root, system unit) the stronger mechanism and a
hand-run test suite the one that works, with one `systemd-run` call and a verdict
that is explainable without reading the log of what was tried.

### What a `--user` scope means for D3's lifecycle

A `--user` scope is a unit in the invoking user's manager, so:

- **A restart of the daemon's own unit does not touch it.** The scope is a
  *sibling* unit, not a child — the same relationship a system scope has to a
  system unit. This is the D3 property, and it holds in both managers.
- **A teardown of the user *manager* does.** `systemctl stop user@$UID.service`,
  the last session ending without lingering, or a logout takes the whole user
  slice — scopes included. A system scope has no equivalent: only pid 1 is above
  it. So the user mode's guarantee is conditional on a lifecycle the system mode
  does not have, which is exactly why the two are not fallbacks for each other.
- **Which the deployed node never depends on.** On a host-mode node the daemon is
  D2's root `chug.node` system unit, so it takes the system manager and this
  paragraph does not apply to it. The user mode is what a developer and the
  proof-run get.

`TaskMeta` records which manager the scope was created in, because `kill` has to
signal the same one: a `--user` scope is not a unit the system `systemctl` can
see, so an unaddressed kill reports "not loaded" and leaves the `setsid()`
escapee running — [D8](#decisions)'s single failure mode wearing a success.

### `XDG_RUNTIME_DIR`, answered

It was **unset** in the ssh where `--user` worked, which is the interesting part.
`sd-bus` finds the user bus at `$DBUS_SESSION_BUS_ADDRESS`, else at
`$XDG_RUNTIME_DIR/bus`, else at `/run/user/$UID/bus` computed from the uid — the
last is the path that carried the measurement. What actually has to exist is the
**user manager for that uid**, not the variable.

That matters because of how slice 1 composes a launch: `spawn_task` calls
`env_clear()` and gives the child exactly `PATH`, `HOME` and the two exit-status
paths — and with `--scope` the child *is* `systemd-run`, so the launcher gets that
environment too. `XDG_RUNTIME_DIR` therefore cannot reach `systemd-run` on a
launch, and cannot be made to without leaking it into the task and breaking slice
1's "nothing the dispatcher did not declare".

The probe used to inherit the daemon's whole environment, so it could have found
a bus through a variable no launch would ever carry — a node that boots green and
then fails every task. It now runs `systemd-run` under the same `env_clear` +
`floor_env` the launch uses, so the probe measures the launch. If a node's user
bus is reachable only through `XDG_RUNTIME_DIR`, that node is refused at boot,
loudly, rather than at the first task.

### What is operator provisioning, and stops here

A daemon running as a non-root user with **no session and no lingering** has no
user manager, so `/run/user/$UID` does not exist and the probe fails. The remedy
is `loginctl enable-linger` for that user — or running the daemon as root, which
is what D2 says. Both are node provisioning, which is [slice 7](#slices)'s and
not this job's; the refusal names `enable-linger` so the operator is not left to
derive it. No polkit rule, no NixOS module change and no node config was written
here, deliberately.

### Verification — stated, and unverified from this workspace

The evaluator has no `systemd-run` at all, so the three tests skip here exactly
as they did for job #447. On a systemd host:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | grep -i skipping
```

Silence is the pass: it means `probe_supervision` returned a scope and all three
D3 tests ran. On `gumbo-nuc-0` as `worksalot` the expected mode is
`Scope(User)`. **Nobody has run this** — it is the one command
[the proofs section](#linux--confirmed-by-hand-only-and-it-is-the-weaker-of-the-two)
leaves outstanding, and it now asks for a mechanism that node has been measured
to grant rather than one polkit denies it.

---

## Correction, 2026-08-06 — the bus the client needs (job #453)

Appended by job #453, which fixes a defect in the interaction between
[slice 1](#correction-2026-08-05--slice-1-as-landed) and
[slice 2](#slices) as landed. Nothing above is edited except that slice's State
cell. Slice 1 is **not** wrong and nothing here loosens it; what was wrong is
that the same `env_clear()` was applied to a process that is not the task.

### What was measured on `gumbo-nuc-0`

Same host, same second, as `worksalot` (NixOS, systemd, cgroup v2) against the
tree at `e7f7c0f`, with `XDG_RUNTIME_DIR=/run/user/1000` in the session:

```
systemd-run --user --scope --quiet /bin/sh -c ":"                   -> exit 0
env -i PATH=… systemd-run --user --scope --quiet /bin/sh -c ":"     -> Failed to connect to user
    scope bus via local transport: $DBUS_SESSION_BUS_ADDRESS and $XDG_RUNTIME_DIR not defined
```

The second form is what `probe_supervision` ran: `.env_clear()` plus the
two-name floor, neither of which is a bus variable. So `--user` could not work in
**any** environment, the probe reported a node incapability that was not one, and
all three D3 tests self-skipped on a node whose mechanism the
[proofs section](#linux--confirmed-by-hand-only-and-it-is-the-weaker-of-the-two)
had already measured working.

### `XDG_RUNTIME_DIR`, answered again — the earlier answer was wrong

[The #451 section](#xdg_runtime_dir-answered) says `sd-bus` finds the user bus at
`$DBUS_SESSION_BUS_ADDRESS`, else at `$XDG_RUNTIME_DIR/bus`, else at
`/run/user/$UID/bus` computed from the uid, and that the last is what carried the
hand measurement. **There is no such fallback**, as the error above says in its
own words: what carried the hand measurement was the ssh session's own
`XDG_RUNTIME_DIR`, which `sudo`-free interactive logins get from
`pam_systemd`. The variable is not incidental to the user bus; it is one of the
only two names that locate it.

### Two environments, and only one of them is #309 §10's

| | Composed of | Rule |
| --- | --- | --- |
| The **task's** environment | the two-name floor, the dispatcher's launch env, the two exit-status paths | #309 §10 / slice 1, exhaustive, unchanged by this job |
| The **client's** — the `systemd-run` invocation that creates the scope | the same floor, plus `XDG_RUNTIME_DIR` and `DBUS_SESSION_BUS_ADDRESS` read from the daemon by name | it is a bus client; without them it cannot reach the manager |

`--scope` execs the task from `systemd-run` itself, so the client's environment
*is* the task's unless something removes the difference between them. That
something is the task wrapper's `unset`, emitted only for names actually
borrowed: bus variables before the exec, gone after it. Getting that ordering
backwards silently re-opens §10, so it is asserted at tier 1 by running the
wrapper and reading the environment the task actually got
(`the_task_sheds_what_the_client_borrowed`), beside the composition assertions in
`the_client_gets_the_bus_and_the_task_never_sees_it`.

The floor is **not** widened to carry the bus names, and `task_env` is untouched.
A name the launch config declares is never borrowed and never shed, so the
dispatcher's value still wins over the daemon's.

### The refusal, corrected

`loginctl enable-linger` is the remedy for exactly **one** of the three ways a
`--user` scope fails. The refusal now distinguishes them, because the wrong one
costs an operator a node change that fixes nothing — which is what this defect
did:

| The failure | What the refusal says |
| --- | --- |
| The daemon holds neither bus variable | it cannot address a bus at all; a live session, or `loginctl enable-linger` **and** `XDG_RUNTIME_DIR` in the daemon's environment, is [slice 7](#slices)'s provisioning. Refused before the probe runs — there is nothing to measure |
| A bus was addressed and nothing answered | this uid has no running user manager: a live session or `loginctl enable-linger` for it |
| The manager answered and refused (polkit, a unit that would not start, a command that would not exec) | the failure itself, and **no** provisioning advice |

Which of the last two a failure is gets read from `sd-bus`'s own opening phrase,
`Failed to connect`, and never from the errno it carries. `No such file or
directory` is how an unreachable bus reads *and* how a missing `/bin/sh` fails —
and `--scope` execs the command from `systemd-run` itself, so that second one is
a manager that answered. Keying on the errno would answer it with
`enable-linger`, which is the wrong-advice class this section exists to close.

The daemon-holds-neither case refuses honestly rather than falling back to a
system scope polkit would deny or to the daemon's own cgroup, which is the silent
lie #309 §7 rejects.

### Verification — stated, and unverified from this workspace

Unchanged from #451's, and still the outstanding one. On `gumbo-nuc-0` as
`worksalot`:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | grep -i skipping
```

Silence is the pass: `probe_supervision` returned `Scope(User)` and all three D3
tests ran. This job could not run it — the evaluator has no `systemd-run` — and
claims only that the cause of the last skip is removed.

---

## Correction, 2026-08-06 — the first execution of D3's Linux tests (job #455)

The three assertions [the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux)
called "still outstanding" ran, on `gumbo-nuc-0` (NixOS, systemd, cgroup v2,
`XDG_RUNTIME_DIR` set), against the tree at `9c0ccd0`:

| Test | 2026-08-06 |
| --- | --- |
| `a_host_task_survives_the_teardown_of_the_launching_unit` | ok |
| `a_host_task_runs_in_its_own_supervision_unit` | **FAILED** |
| `a_kill_reaches_a_setsid_escapee_through_the_scope` | **FAILED** |

**The one green line is not D3 confirmed.** It is a run in which the crux
assertion of the three failed:

```text
panicked at crates/container/tests/host_backend.rs:519:
  the task runs in its own unit: /user.slice/user-1000.slice/session-c340.scope
```

`session-c340.scope` is the ssh session's own scope — the launcher's.

### The mechanism was never in question, and the launch is not the fault

Measured by the operator on the same host in the same session, `--user --scope`
creates a real per-invocation scope under `app.slice` while the shell it was
launched from stays in `session-c341.scope`. So the question was only why the
shipped path put the task somewhere else. Four candidates were named; three are
answered by the code and the run itself.

| Candidate | Answer |
| --- | --- |
| `manager_for` resolves `Scope(User)` for the probe and something else at launch | Out. The same run asserted `meta.unit` and `meta.scope`, and both passed: the launch resolved `Scope(User)` and `supervised_launch` has exactly one argv for it |
| `systemd-run` runs but the task is spawned beside it rather than as its command | Out by inspection: the task's argv follows `--`, and `--scope` execs it from `systemd-run`'s own process |
| The scope is created and the task re-parented out of it | Out. Nothing moves a process out of a scope, and the escapee assertion in the third test reads the task's own cgroup after the exec |
| The `env_clear`/floor composition at the **launch** call site drops what job #453's fix keeps at the probe | Out, and this was the first suspect because it is one call site over from #453's. The launch client's environment is the task's environment plus the borrowed bus variables, which is `probe_env`'s superset by construction — now named `launch_env` and asserted at tier 1 by `the_launch_client_gets_everything_the_probe_measured`, on every machine, so it is a red test rather than an argument |

### What it was: the assertion raced the manager's start job

`systemd-run --scope` registers **its own pid** with the manager, waits for the
start job to complete, and only then execs the command. Until that job completes
the pid is still in the cgroup it was forked into — the daemon's, or on a hand
run the ssh session's. `spawn_task` returns as soon as the client is forked, so
the test read `/proc/<pid>/cgroup` about a millisecond later, before any correct
systemd could have moved it. `session-c340.scope` is precisely what an unmoved
pid reads.

So the assertion could only ever have passed by luck, and it has never passed on
any machine. It now polls for scope entry under a 10s bound, and when entry
never comes it reports what the pid was in instead, whether it is still live,
the task's own log — `systemd-run`'s stderr goes there, `--quiet` silencing only
the informational line — and the exit code. That is enough for one more run to
separate a launch that never reached `systemd-run` from a scope that was merely
slow.

**This diagnosis is reasoned from the code and the mechanism, not measured**:
the workspace this job ran in has no `systemd-run`, pid 1 is `sh`, and
`/proc/self/cgroup` reads `0::/`. What settles it is the command below.

### The launch's own window, which is not closed here

`HostBackend::launch` returns before the scope exists. For those tens of
milliseconds the task's pid really is in the daemon's cgroup, so a daemon
restart landing inside the window takes the task with it — D3's guarantee is
eventual, not immediate. Nothing here changes that: it is recorded rather than
fixed, because closing it means blocking every launch on a bus round trip and
the window is not what failed.

### The teardown test, strengthened whether or not that diagnosis holds

It never *was* the vacuous pass the brief describes — it asserted the stand-in's
task was in `chug-proof-task-….scope` before tearing anything down, which is why
it did not fail the way the crux test did. But it was weak in two ways that
matter as much:

- **It proves nothing about the shipped launch path.** Its stand-in spawns
  `supervised_launch`'s argv directly, inheriting the whole test process's
  environment, so it exercises the argv and never `HostBackend::launch`'s
  `env_clear` composition. The first suspect above is invisible to it by
  construction.
- **It asserted one membership, not the relation.** The stand-in daemon's own
  unit went unchecked, and so did the one thing the teardown is *about*: that
  the task's cgroup is not inside the launcher's.

Both are now asserted, through the same bounded membership check, **before** the
teardown. If no scope is created the test panics there, naming the cgroup it
found and the daemon's, and the teardown never runs — so a `HostBackend` that
supervised nothing fails at the membership check instead of being certified by a
task trivially outliving a `systemctl kill` that reached nothing.

### D8 is neither confirmed nor retracted, and that is the honest state

`a_kill_reaches_a_setsid_escapee_through_the_scope` failed and no output from it
was carried back. Under the diagnosis above it should have passed: both of its
cgroup reads happen after the escapee has written its pid, which is after the
exec and therefore after the scope exists, so neither of them was racing. That
leaves the D8 claim itself — the escapee outliving `kill` — or the exit status
after it, and nothing in hand distinguishes them. So the "closed for free" cell
now carries *unconfirmed in execution* rather than a retraction: retracting a
claim needs evidence as much as confirming one does. The failure path is
instrumented for the next run with the escapee's cgroup and what the manager
says about the scope, which separates "the signal did not reach the cgroup" from
"the scope was already gone when it was sent".

### Verification — stated, and unverified from this workspace

On `gumbo-nuc-0` as `worksalot`, in a checkout of this branch:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | tail -40
```

Three outcomes and their meanings:

| What it prints | What it means |
| --- | --- |
| all three `ok`, and no "skipping" line | D3's Linux half is proven through the shipped path, and D8 with it |
| `pid N never entered chug-task-….scope within 10s …` | the diagnosis above is wrong. The line carries the cgroup the pid was in, whether it is live, and the `systemd-run` client's own stderr out of the task's log — which is the missing measurement |
| any "skipping" line | nothing was covered; the reason is on the line |

---

## Correction, 2026-08-06 — D3 is proven on Linux through the shipped path (job #456)

The two assertions [D3](#decisions) rests on passed together, on `gumbo-nuc-0`
(NixOS, systemd, cgroup v2) against the tree at `186beeb`, run by the operator
with `XDG_RUNTIME_DIR=/run/user/1000` in the invoking environment:

```sh
cargo test -p container --test host_backend -- --nocapture
```

| Test | 2026-08-06 |
| --- | --- |
| `a_host_task_runs_in_its_own_supervision_unit` | **ok** |
| `a_host_task_survives_the_teardown_of_the_launching_unit` | **ok** |
| `a_kill_reaches_a_setsid_escapee_through_the_scope` | **FAILED**, in its own setup |

**The two that passed are the whole of D3**, and they had to pass *together* to
mean anything. [Job #455](#correction-2026-08-06--the-first-execution-of-d3s-linux-tests-job-455)
made the teardown test assert both units' membership **before** it tears
anything down; on the run before that, it passed while the task sat in the ssh
session's own scope, which is a task outliving a `systemctl kill` that reached
nothing. With membership first, the pair says what the design claims: the
backend puts a host task in a unit of its own, and tearing down the unit that
launched it leaves it running. So D3 holds **on Linux, through
`HostBackend`, on a real node** — and on macOS against the real `launchctl
kickstart -k` mechanism ([the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux)).
The design's load-bearing claim is proven on both platforms.

### The qualifier that has to travel with the result

The run had `XDG_RUNTIME_DIR` **set in the invoking environment**, which is what
`cargo test` in an interactive login gets from `pam_systemd` and what job #453's
client borrows to reach the user bus. A daemon under a supervisor has no login,
so whether it holds that variable is [slice 7](#slices)'s provisioning question —
`loginctl enable-linger` plus `XDG_RUNTIME_DIR` in the unit's environment, or
running as root and taking the system manager, which is what [D2](#decisions)
says. **This run settles none of that.** It proves the mechanism through the
shipped code on a node whose environment already satisfies the precondition;
`host_refusal` is what a node that does not gets, loudly, at boot.

### D8 is still unverified — the failure was in the staging, and its cause is not identified

`a_kill_reaches_a_setsid_escapee_through_the_scope` never reached an assertion:

```text
panicked at crates/container/tests/host_backend.rs:515:
  the stand-in daemon never recorded a task pid at
  /tmp/chug-host-escapee-…/escapee.pid
```

That is the staging step — a `setsid()` child recording its own pid — and it is
setup, not the claim. So [D8](#decisions) is **neither confirmed nor
retracted**, exactly as it was: the escapee, the scope and the kill have still
never been executed anywhere.

**The cause of that staging failure is not identified, and job #456 did not
identify it.** What it did was rule two candidates out against the tree and
against the same run, narrow the third, and make the next run name the answer
instead of timing out. The record below is the elimination, not a diagnosis.

**Candidate 1 — `setsid` was resolved in the wrong environment — is ruled
out.** The guard ran `setsid true` with the environment `cargo test` carries and
the escapee runs under [slice 1](#correction-2026-08-05--slice-1-as-landed)'s
two-name floor, which looks like the #451/#453/#455 defect and is not one: the
floor's two names are `PATH` and `HOME`, and `floor_env` copies the daemon's
`PATH` verbatim. In a tier-2 test the "daemon" **is** the `cargo test` process,
so the two are the same string — which this suite already asserts of a launched
task, `PATH is the daemon's`. And the guard **passed** on `gumbo-nuc-0`: the run
reached `launch` rather than skipping, so `setsid` was runnable through the very
`PATH` the task was given. The guard passing is itself the measurement.

**Candidate 2 — the scope sees a different `/tmp` — is ruled out by the same
run.** `scope_args` passes `--scope --quiet --collect --unit=` and nothing else,
so no unit property that could namespace a mount is ever set; and both passing
tests write and read files under their own `/tmp/chug-host-*` root from inside a
transient scope. The teardown test's stand-in daemon records its task's pid
there, and `a_host_task_runs_in_its_own_supervision_unit`'s task polls a gate
file there through the shipped composition and exits 0.

**Candidate 3 — the launch composition changed under #451/#453/#455 — is
narrowed, not ruled out.** The same passing scope test runs a task shell to a
clean exit inside a `--user` scope through `spawn_task`, the wrapper and
`supervised_launch`, so the composition as a whole executes the task's command.
And the new staging test below runs that exact escapee script through the same
composition under `Supervision::ProcessGroup` and passes on the evaluator, so
the script itself is not broken. What is left unmeasured is the intersection:
the escapee script *inside a scope*, which is the one combination no green test
covers and the one that failed.

Alongside that, one real defect **in the fixture** is fixed regardless of the
cause, in `crates/container/tests/host_backend.rs`: **nothing observed whether
the staging ran.** The task's stderr goes to the task log, which the fixture
never read, so every way the staging can fail — an unresolvable `setsid`, a
failed exec, a failed redirect — read identically as a silent timeout, and the
test then waited out the task's own `sleep` on a precondition. It now reports
the task's log, its exit code and the launcher's cgroup on that path, kills the
task before panicking, and says in the message that the fixture's setup is what
failed. `setsid` is also now resolved through `container::host::task_path()` and
handed to the script as an **absolute** path: that changes nothing today,
because the two `PATH`s are equal, and it is **hardening** — the guard follows
the floor rather than the caller, so it stays correct if the floor stops
carrying `PATH`, and it already differs for a non-UTF-8 `PATH`, which
`daemon_floor` drops to `PATH_FALLBACK`.

### The staging is asserted on every machine, which is why it had never been

`a_setsid_escapee_is_staged_outside_the_task_process_group` is new: it stages
the escapee through the shipped launch composition under
`Supervision::ProcessGroup`, asserts the escapee left the task's process group,
and asserts it **survives** the task's `kill` — D8's premise, that a group
signal cannot reach it, measured rather than argued. None of that needs systemd,
so it runs on the evaluator and on every machine. That is the reason this bug
lived: the only test that staged an escapee could run on exactly one host in the
fleet, so its setup had never executed anywhere, and a fixture nothing runs is
not a fixture. The scope test still asserts the group escape itself, alongside
the two things only a scope adds — the escapee's cgroup, and the kill that
reaches it.

### Verification — stated, and unrun from this workspace

On `gumbo-nuc-0` as `worksalot`, with `XDG_RUNTIME_DIR` set, in a checkout of
this branch:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | tail -40
```

| What it prints | What it means |
| --- | --- |
| the three scope tests and the staging test all `ok`, no "skipping" | D8 is confirmed in execution: the escapee left the process group, stayed in the scope's cgroup, and only the scope signal could have ended it |
| `no escapee recorded a pid at … NOTHING about design #440 D8 was exercised` | the staging is still the failure, and the line now carries the task's log, its exit code and the launcher's cgroup — which is the measurement this job could not take |
| the escapee outlived a kill | D8 is **refuted** on this node, and the line carries the escapee's cgroup and what the manager says about the scope |
| any "skipping" line | nothing was covered; the reason is on the line |

The three D3-and-D8 scope tests are unchanged in what they assert. Nothing here
touches `probe_supervision`, the macOS path or the two tests that passed.

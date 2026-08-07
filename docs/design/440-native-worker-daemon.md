# Design — the natively-supervised worker daemon

Status: IMPLEMENTED — all eight slices landed, **corrected on 2026-08-07 for the platform D6 assumed away, on 2026-08-08 for the one artifact that correction over-reached onto, and again for the docker steps both corrections asked of every node rather than of a container-capable one** (job #487), and no node runs a native daemon from this tree: nothing was applied, and no node advertises `host` (#309 P1 made `runtime.mode: host` a legal declaration in job #478; placing by it is P2).

**The first conversion of a real node, on 2026-08-06, found two things this
design got wrong for macOS** — [D6](#decisions)'s extracted binary is an ELF file
a mac cannot exec, and `WORKER_DOCKER_ENDPOINT` was never rendered by anything,
so the daemon dialled a socket that is not there. Both are
[#309](./309-host-native-execution.md) P0 finding 6 again, and both are fixed in
[the correction](#correction-2026-08-07--d6-holds-on-linux-only-and-the-endpoint-was-never-rendered-job-476):
a Darwin node **compiles** its own daemon from a declared toolchain, the
endpoint is derived from the node's own docker context, and every staged binary
must prove it runs on the node before it is installed. `gumbo-air-0` is running
a daemon an operator built **by hand** and is not converted by this tree.

**That correction then generalised one artifact too far.** It sent all three
staged artifacts down the native-build path on Darwin, and
`chuggernaut-channel` is the *inverse* case: it never runs on the mac, it is
injected into every agent **container**, so a Mach-O is what breaks it. Jobs #477
and #478 paid for that in four "produced no output" escalations before the air
was drained —
[the 2026-08-08 correction](#correction-2026-08-08--the-correction-above-generalised-over-two-binaries-with-opposite-platforms-job-480)
takes the channel binary out of the worker image on both platforms and asks each
binary the question **its own executor** asks.

**The run spec carries `WORKER_SLOTS_MAX` since job #477** —
[the correction](#correction-2026-08-07--worker_slots_max-is-forwarded-now-job-477)
— which supersedes slice 6's parenthetical calling it the one knob no script
forwards. The swap still copies nothing forward; the ceiling survives because it
is written down, which is [D7](#decisions) working as intended.

`IMPLEMENTED` is a claim about the slices and nothing more
([`docs/reference/docs.md`](../reference/docs.md)), and it is worth saying what it does
not claim. Every slice is in the tree; the daemon is buildable and supervisable
natively on both platforms; and the fleet is exactly where it was — two nodes
still running the containerized daemon, no `WORKER_MODES` naming `host`, and no
job type declaring `runtime.mode: host` (which #309 P1 has since made legal to
declare, in job #478, without making it routable). The last slice's own half is the sharpest
case of this: `nix/chug-node/` declares the unit, **nothing in this repo's CI
evaluates that module** (#372 §2.3), and no node has ever been given it. The
slices landed as — 3 as
[the refusal at both checks](#correction-2026-08-06--slice-3-as-landed-job-460),
8 as
[the narrowed guarantee](#slice-8-2026-08-06--the-guarantee-narrowed-in-the-spec-job-470),
4 as [a unit, an agent and an environment file](#correction-2026-08-06--slice-4-as-landed-job-469)
that **no node has yet been given**, 5 as
[a root-owned directory and four refusals](#correction-2026-08-06--slice-5-as-landed-job-472)
over it, 6 as
[install-and-restart, with the detached swapper and every carry-forward deleted](#correction-2026-08-06--slice-6-as-landed-job-473),
and 7 as
[a shared unit template, an amended charter and an installer no glob reaches](#correction-2026-08-07--slice-7-as-landed-job-475)
— the nix half of which is **unevaluated by construction**.
[D3](#decisions) and [D8](#decisions) are **both
proven on Linux, through the shipped code path**: on `gumbo-nuc-0` (NixOS,
**systemd 260 (260.2)**, cgroup v2) on 2026-08-06, all thirteen tests in
`crates/container/tests/host_backend.rs` passed with **no skips** at tree
`692656e` under `cargo test -p container --test host_backend` — see
[D8 in execution](#proof-2026-08-06--d8-in-execution-thirteen-of-thirteen-job-466).
D3 is proven on **macOS** as well, its mechanism exercised firsthand on
2026-08-06 — see [the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux) —
and its Linux half had already passed through the same path at trees `186beeb`
and `af9f74e` — see
[the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456)
and [the re-run](#correction-2026-08-06--d3-holds-on-both-platforms-and-the-escapee-staging-is-narrowed-job-457).
So #309 §2's `setsid()` escape **is** closed on Linux by the scope, and it was
not closed for free: it took `--expand-environment=no` in `scope_args`
(`crates/container/src/host.rs`), the fix job #462 diagnosed out of systemd's own
source and this run confirms — see
[the client rewriting the command](#correction-2026-08-06--the-scopes-client-was-rewriting-the-tasks-own-command-job-462).

**Two qualifiers travel with that result and neither is settled by it.** Every
Linux run needed `XDG_RUNTIME_DIR=/run/user/1000` set in the invoking
environment, because every one of them ran unprivileged and an unprivileged
daemon can only create a `systemd --user` scope. [Slice 7](#slices) answers that
by construction rather than by measurement: the unit it declares runs as `root`,
whose scopes are **system** scopes on a bus at a fixed socket path, so the
daemon borrows nothing from its environment — read off `scope_manager` and
`borrowed_bus` in `crates/container/src/host.rs`, on a node that does not exist.
And the defect the flag fixes is
**systemd-version dependent**: v258 turned `--expand-environment=` on by default
for `--scope`, so a client below v258 never rewrote a task's argv and the flag
is a no-op on v254–v257 — and below v254 an unknown option, which makes the node
refuse `host` outright
([the version table](#the-systemd-version-dependency-plainly)). The proving node
runs 260.2, past that cutover — which is why the bug was reachable at all, and
why it read as environment-specific for the five attempts (#455–#459) that
chased it.

Slice 2's Linux mechanism was corrected by job #451 to the scope an unprivileged
daemon can actually create — see
[the correction](#correction-2026-08-06--the-scope-an-unprivileged-daemon-can-create-job-451) —
and by job #453 to give that scope's `systemd-run` client the bus variables it
needs, which is why the Linux assertion had still never run — see
[the correction](#correction-2026-08-06--the-bus-the-client-needs-job-453). Its
assertions were then fixed three times: job #455 for a membership check that
raced the manager's start job — see
[the first execution](#correction-2026-08-06--the-first-execution-of-d3s-linux-tests-job-455) —
job #456 for an escapee fixture that reported a failed setup as a silent
timeout, and whose staging had never executed on any machine, and job #457 for
the membership check the D8 test itself never had, so that its staging budget is
no longer shared with the manager's start job — an asymmetry read off the code
and **not** measured, and the same change makes the next run name the step that
fails.

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
| **D3** | **Host tasks run in their own supervision unit, not the daemon's** — Linux: a transient systemd scope per task; macOS: the process group `spawn_task` already creates — **proven on both, 2026-08-06**: macOS 26.5.1 (Darwin) on `gumbo-air-0` against the mechanism itself, `sh deploy/prod/macos-host-supervision-proof.sh` at tree `c8a8354` ([the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux)), and Linux through the shipped path on `gumbo-nuc-0` (NixOS, systemd, cgroup v2), `cargo test -p container --test host_backend` at trees `186beeb` and `af9f74e`, with `XDG_RUNTIME_DIR=/run/user/1000` set in the invoking environment ([the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456), [the re-run](#correction-2026-08-06--d3-holds-on-both-platforms-and-the-escapee-staging-is-narrowed-job-457)). | It is the same mechanism #309 §6, §7 and §2 each independently need, and it is the only way `systemctl restart chug-worker` can stop killing in-flight work. |
| **D4** | **And the daemon declines a `refresh` while any host task is live**, naming the task — evaluated **twice: at accept, and again at the swap boundary** beside `RefreshGate::drained`. | D3 covers a unit restart; it does not cover a reboot or a rebuild that restarts more than the unit, and the self-refresh is the only restart the platform performs *automatically* — a loud refusal there is cheap and unconditional. The accept check is the fast, informative one; the swap-boundary check is the one that is actually load-bearing, because the build phase runs between them. |
| **D5** | **Credentials move to a root-owned `0700` directory named by the unit, not the login user's home.** `chuggernaut admin worker-creds` is unchanged; the install step in `deploy/prod/README.md` §6 changes. | The login user is in the `docker` group and is who `build-worker.sh` ssh's in as, so a creds file under that user's home is readable by anything that user runs — a strictly worse boundary than the one the mount was pretending to give. |
| **D6** | **`build-worker.sh` renders and installs a unit + environment file; `worker-refresh.sh`'s swap collapses to "install the binary, ask the supervisor to restart".** The daemon binary is extracted from the worker image the build phase already produces — **on Linux only**: the image is a Linux container, so a **Darwin** node compiles its own from a declared `WORKER_CARGO`, and both platforms must prove the staged binary runs on the node before installing it ([the correction](#correction-2026-08-07--d6-holds-on-linux-only-and-the-endpoint-was-never-rendered-job-476), measured on `gumbo-air-0` 2026-08-06). The split is **per artifact, by who execs it**: `chuggernaut-channel` rides out of the image on *both* platforms, because agent containers exec it and a mac never does ([the 2026-08-08 correction](#correction-2026-08-08--the-correction-above-generalised-over-two-binaries-with-opposite-platforms-job-480)). | Every mount, device and `docker inspect` carry-forward in the swap phase exists only because the daemon is a container that must be re-composed; extracting the binary keeps its build environment byte-identical to today's and needs no host Rust toolchain — an argument whose premise is that the image's platform *is* the node's, which is false on a mac and buys nothing there. |
| **D7** | **#390's drift guard keeps its meaning and gains reach**: presence-decides-refusal over the same `WORKER_*` key set, comparing the live unit's environment against the composed environment file. | The comparison was never about docker — it is about what a recreate would drop — and a declaration that is a file on the node is legible without `docker inspect`. |
| **D8** | **Of #309 P0's three known holes, two get worse and one gets better.** Environment inheritance (§10) and `/proc/<pid>/environ` (§8) get worse and stop being P3; the `setsid()` escape (§2) is closed on Linux by D3's scope — **proven in execution on 2026-08-06**, on `gumbo-nuc-0` (NixOS, systemd 260 (260.2), cgroup v2) at tree `692656e` under `cargo test -p container --test host_backend`, where `a_kill_reaches_a_setsid_escapee_through_the_scope` reached and passed its assertion for the first time on any machine, alongside all twelve of its siblings and with no skips ([D8 in execution](#proof-2026-08-06--d8-in-execution-thirteen-of-thirteen-job-466)). **Not for free**, twice over: `kill` has to address the cgroup and not only the process group ([the correction](#d8-is-confirmed-on-linux-and-it-needed-one-line-of-code)), and `scope_args` has to pass `--expand-environment=no`, without which a systemd v258-or-later client rewrites the task's own argv before exec'ing it ([the client rewriting the command](#correction-2026-08-06--the-scopes-client-was-rewriting-the-tasks-own-command-job-462)). Its premise is separately asserted, and **passing**, by `a_setsid_escapee_is_staged_outside_the_task_process_group`, which measures the *opposite* half — that a process-group signal cannot reach the escapee — so the two are complementary and both are needed; `a_setsid_escapee_is_staged_under_a_scope_as_well` is that same premise under a scope. On **macOS** the hole is unchanged and still leaks. | Blast radius is what changes: a task inheriting a *native* daemon's environment inherits the node, not a container that happens to hold a socket. |

## Slices

| # | Slice | Contract changed | Depends on | State |
| --- | --- | --- | --- | --- |
| 1 | `code` — `spawn_task` calls `env_clear()`; a host task's environment is exactly the dispatcher's launch env plus the two exit-status paths | `HostBackend` launch env (`crates/container/src/host.rs`) | — | **Landed** (job #442), plus a two-name floor the slice line does not mention — see [the correction](#correction-2026-08-05--slice-1-as-landed) |
| 2 | `code` — launch each host task into a transient supervision unit; refuse to advertise `host` when the node cannot create one. Includes the macOS proof: assert a task survives `launchctl kickstart -k` of the daemon | `HostBackend::launch` / `kill` | 1 | **Landed** (job #447), and **every assertion it carries is now executed and passing**: the macOS proof PASSED on 2026-08-06 ([the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux)), and on `gumbo-nuc-0` (NixOS, systemd 260 (260.2), cgroup v2) all three Linux assertions PASSED through `HostBackend` at tree `692656e` — D3's two (`a_host_task_runs_in_its_own_supervision_unit`, `a_host_task_survives_the_teardown_of_the_launching_unit`), already passing at `186beeb` and `af9f74e` ([the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456), [the re-run](#correction-2026-08-06--d3-holds-on-both-platforms-and-the-escapee-staging-is-narrowed-job-457)), and D8's escapee (`a_kill_reaches_a_setsid_escapee_through_the_scope`), for the first time, in a thirteen-of-thirteen run with no skips ([D8 in execution](#proof-2026-08-06--d8-in-execution-thirteen-of-thirteen-job-466)) — see [the correction](#correction-2026-08-05--slice-2-as-landed), amended by job #451 for [the scope an unprivileged daemon can create](#correction-2026-08-06--the-scope-an-unprivileged-daemon-can-create-job-451), by job #453 for [the bus that scope's client needs](#correction-2026-08-06--the-bus-the-client-needs-job-453), by job #455 for [a membership check that raced the manager](#correction-2026-08-06--the-first-execution-of-d3s-linux-tests-job-455), by job #457 for [the membership check the D8 test itself never had](#correction-2026-08-06--d3-holds-on-both-platforms-and-the-escapee-staging-is-narrowed-job-457), by job #458 for [a trace of the escapee's own](#correction-2026-08-06--the-escapees-own-trace-and-three-differences-ruled-out-job-458), by job #459 for [the one fixture variable never varied](#correction-2026-08-06--the-one-variable-four-attempts-never-changed-job-459), and by job #462 for [the client that was rewriting the task's own command](#correction-2026-08-06--the-scopes-client-was-rewriting-the-tasks-own-command-job-462) |
| 3 | `code` — the daemon declines `refresh` while any host task is live, with the task id in the reason: a precondition in `refresh` **and** a re-check in `run_refresh` after `quiesce`, beside the `drained` wait, failing the refresh at the `drain` stage | worker `refresh` op precondition and swap-boundary gate (`crates/worker/src/daemon.rs`) | 2 | **Landed** (job #460), with "live" decided against the exited-but-unremoved window and the tests placed at tier 1 for a reason the slice line does not mention — see [the correction](#correction-2026-08-06--slice-3-as-landed-job-460) |
| 4 | `deploy` — `chug-worker` unit + environment-file templates; `build-worker.sh` renders and installs them instead of composing `docker run`; #390's guard compares the environment file | the node run spec (`deploy/prod/build-worker.sh`) | — | **Landed** (job #469), and **no node has been converted** — the script changes, nothing was applied. Three things the slice line does not mention: the guard keeps a `docker inspect` path *for the conversion itself*, the nix toolchain-shape guard was **ported rather than deleted**, and two knobs were added the design did not name — see [the correction](#correction-2026-08-06--slice-4-as-landed-job-469) |
| 5 | `deploy` — creds and the node-local artifacts move to a root-owned directory; `deploy/prod/README.md` §6 install step | node credential layout | 4 | **Landed** (job #472), on **Linux only** and with the migration left to the operator's hands — two things the slice line does not mention, plus a third: the node-local *artifacts* had already moved in slice 4, so what this changed is the credentials and the guard over them — see [the correction](#correction-2026-08-06--slice-5-as-landed-job-472) |
| 6 | `deploy` — `worker-refresh.sh` swap phase: extract the binary from the built worker image, install, ask the supervisor to restart; delete the detached swapper and every mount/device carry-forward | spec §3.1 self-refresh | 4, 5 | **Landed** (job #473), and **no node has been converted**, so every un-converted node's self-refresh now REFUSES — the cost is named, not hidden. What the slice line does not mention: install is by rename (ETXTBSY, and this script truncating itself) and escalates to `sudo -n`, refusals guard against a second daemon on one node, and §3.1's host-task-across-a-unit-restart case becomes true for the first time — see [the correction](#correction-2026-08-06--slice-6-as-landed-job-473) |
| 7 | `code` — `nix/chug-node/` gains the unit and the `chug.node` charter amendment; the macOS plist template and its opt-in installer | `chug.node` option surface | 4, 6 | **Landed** (job #475), **unevaluated by construction** on the nix half and with nothing applied to any node. Three things the slice line does not mention: the unit is a **shared template** rather than a second rendering, so the two halves cannot drift textually; the macOS installer is opt-in three times over and refuses a control-plane mac; and a NixOS node that declares the unit leaves `build-worker.sh` a seam that this slice documents rather than closes — see [the correction](#correction-2026-08-07--slice-7-as-landed-job-475) |
| 8 | `docs` — `docs/spec.md` §3.1's drain guarantee narrowed to say what survives a *native* daemon restart and what does not | spec §3.1 | 2, 3 | **Landed** (job #470) — four cases, the reboot residue explicit, and both live qualifiers stated; see [the narrowed guarantee](#slice-8-2026-08-06--the-guarantee-narrowed-in-the-spec-job-470) |

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
| The daemon is a container: `docker run -d --restart=always --name chug-worker` with the host's docker socket and `keys` bind-mounted | `deploy/prod/build-worker.sh` | **Superseded** by slice 4 (job #469) in the script — it now installs a unit or agent over an environment file — and **still true of every node**, because nothing has run it against one |
| So it is docker-out-of-docker: task containers are **siblings on the host**, and container mode is correct | same, plus `docs/spec.md` §3.1 | Shipped |
| `HostBackend` spawns a task with `process_group(0)` and `.envs(&config.env)` — and **no `env_clear()`**, so the task inherits the daemon's whole environment | `crates/container/src/host.rs` (`spawn_task`) | **Superseded** by slice 1 (job #442): the environment is composed, not inherited |
| A host task's exit status is written by the task's own wrapper, not by the daemon, so the daemon need not be alive when a task exits | `crates/container/src/host.rs` (`supervised_cmd`); #309 correction finding 2 | Shipped |
| The swap runs a **detached `docker:cli` sibling** that removes `chug-worker` and re-composes `docker run` from mounts and devices recovered by `docker inspect` of the live container | `deploy/prod/worker-refresh.sh` (`swap`) | **Superseded** by slice 6 (job #473): the swap extracts the binary from the built image, installs it and asks the supervisor to restart; an un-converted node refuses |
| `nix/chug-node/` prepares the host and deliberately declares **no** unit supervising the daemon | `nix/chug-node/options.nix` charter; #372 §8 | Shipped |
| The Mini already runs the dispatcher and api **natively under launchd**, rendered from templates | `deploy/prod/install-launchd.sh`, `deploy/prod/launchd/` | Shipped |
| `WORKER_CACHE_DIR` is env-only — a host path the daemon passes to sibling containers, never mounted into the daemon | `crates/worker/src/config.rs`; the node's environment file | Shipped, and since slice 6 it is declared in that file rather than carried by the swap |
| `WORKER_CHANNEL_BINARY` and `WORKER_REFRESH_SCRIPT` default to `/usr/local/lib/chuggernaut/…` — paths that are *inside the image* today but are shaped like host paths | `crates/worker/src/config.rs` | Shipped |
| `chuggernaut admin worker-creds` writes the `.creds` at mode `0600` on the dispatcher host; the operator `scp`s it into the node login user's `chuggernaut-worker/keys/` | `crates/cli/src/admin.rs`, `crates/cli/src/keygen.rs`, `deploy/prod/README.md` §6 | **The minting is unchanged** and stays exactly this. The *install* was superseded by slice 5 (job #472) on Linux: `scp` to a staging path, then `install -o root -m 0600` into a root-owned `0700` directory. Still true of macOS, and of every node until it is converted |
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
[the Linux execution](#correction-2026-08-06--d3-is-proven-on-linux-through-the-shipped-path-job-456),
re-run at tree `af9f74e` in
[the re-run](#correction-2026-08-06--d3-holds-on-both-platforms-and-the-escapee-staging-is-narrowed-job-457).
So the heading above is the record of the hand run alone, and "D3 is not proven
on Linux" is no longer true of the tree: **D3 is proven on both platforms**, on
`gumbo-air-0` (Darwin 26.5.1) and on `gumbo-nuc-0` (NixOS, systemd, cgroup v2),
through the shipped code on each. Everything above stands as what the hand run
alone established.

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

---

## Correction, 2026-08-06 — D3 holds on both platforms, and the escapee staging is narrowed (job #457)

The run that settles the head, on `gumbo-nuc-0` (NixOS, systemd, cgroup v2),
against the tree at `af9f74e`, by the operator with
`XDG_RUNTIME_DIR=/run/user/1000` in the invoking environment:

```sh
cargo test -p container --test host_backend
```

| Test | 2026-08-06, tree `af9f74e` |
| --- | --- |
| `a_host_task_runs_in_its_own_supervision_unit` | **ok** |
| `a_host_task_survives_the_teardown_of_the_launching_unit` | **ok** |
| `a_setsid_escapee_is_staged_outside_the_task_process_group` | **ok** |
| `a_kill_reaches_a_setsid_escapee_through_the_scope` | **FAILED**, in its own staging |

9 passed, 1 failed. **The first two are the whole of [D3](#decisions) on Linux**,
and they pass through `HostBackend` rather than beside it, with the membership
check [job #455](#correction-2026-08-06--the-first-execution-of-d3s-linux-tests-job-455)
put **before** the teardown — which is what makes the second one mean anything.
With [the macOS proof](#proofs-2026-08-06--d3-on-macos-and-on-linux) on
`gumbo-air-0` (Darwin 26.5.1) against the real `launchctl kickstart -k`, **D3
holds on both platforms, through the shipped code on each**. This is the second
consecutive Linux run of the pair, after `186beeb`.

**The qualifier travels with the result, unchanged.** Both Linux runs had
`XDG_RUNTIME_DIR` set in the invoking environment, which is what an interactive
login gets from `pam_systemd` and what job #453's client borrows to reach the
user bus. A daemon under a supervisor has no login, so whether it holds that
variable is [slice 7](#slices)'s provisioning question — `loginctl
enable-linger` plus `XDG_RUNTIME_DIR` in the unit's environment, or running as
root and taking the system manager per [D2](#decisions). **Neither run settles
any of that.** `host_refusal` is what a node that does not satisfy the
precondition gets, loudly, at boot.

### The staging test's first execution on a systemd node, and why it is not D8

`a_setsid_escapee_is_staged_outside_the_task_process_group` is job #456's, and
this is the first time it has run anywhere but the evaluator. It passes on
`gumbo-nuc-0`, which is the measurement that matters most here: **the same
`escapee_script`, through the same `spawn_task`, `supervised_cmd` and
`supervised_launch`, stages an escapee on that host** — under
`Supervision::ProcessGroup`.

It asserts the *opposite* premise on purpose, and its own message says so: `the
process-group signal reached the escapee, so D8's premise — that only the
scope's cgroup can — is not what this backend does`. So it is not a substitute
for the scope test and never becomes one. The two are complementary: one
measures that a group signal **cannot** reach the escapee, the other that the
scope's signal **can**. Both are needed and only one of them runs today.

### What the launch composition is now measured to do

The two escapee tests differ in exactly one argument — `Supervision::ProcessGroup`
against the probed `Supervision::Scope` — and everything the scope adds is in
three places: the `systemd-run --user --scope --quiet --collect --unit=… --`
prefix `supervised_launch` puts in front of the argv, the `unset` of the
borrowed bus variables `shed_borrowed` puts at the head of the wrapper, and
those two variables in the client's own environment.

Measured this job **on the evaluator**, with a `systemd-run` shim on `PATH` that
consumes options up to `--` and `exec`s the rest: `probe_supervision` returns
`Scope(System)`, the launch goes through `supervised_launch`'s scope argv, and
**the escapee stages** — the test reaches the escapee's cgroup assertion, which
only a real scope can satisfy, instead of timing out in `staged_escapee`. So the
argv shape, the `--`, the wrapper's `"$@"` handoff, the composed environment,
the task-directory cwd, the log fds and `process_group(0)` are ruled out **by
execution rather than by argument**. What that shim run does *not* cover is the
`unset` prefix, which `borrowed_bus` emits only for `Scope(User)` and the
evaluator's root euid resolves to `Scope(System)`.

**What is left is what only a real `systemd-run --scope` does**: the bus round
trip, the manager's start job, and the cgroup move — none of which this
workspace can perform (`systemd-run` is absent, `/proc/self/cgroup` reads
`0::/`, and `/sys/fs/cgroup` is read-only).

### What actually differed: the D8 test paid for the start job out of its setup budget

`systemd-run --scope` execs the task's command only **once the manager's start
job has completed** — job #455's finding, and the reason `SCOPE_ENTRY` exists at
all. `HostBackend::launch` returns as soon as the client is forked, so under
`Supervision::Scope` there is a window between the launch returning and the
task's first instruction that the process-group path does not have.

`a_kill_reaches_a_setsid_escapee_through_the_scope` started its **10s** staging
clock at that moment and waited for a file the task could not write until the
window closed, while `a_host_task_runs_in_its_own_supervision_unit` is allowed
that same 10s for the window **alone**. The two were never comparable: the D8
test was strictly the tighter of the two, on the one step neither of them
controls. It now waits for the task's own scope membership through the same
bounded `supervised_cgroup` check the other two tests use, **before** it waits
for the escapee — membership first, exactly as job #455 ordered the teardown
test — so the staging budget buys staging and nothing else.

**This is the asymmetry, stated from the code; it is not a measured cause.** If
the next run still fails, it fails somewhere the message now names, because the
same change decomposes that failure:

- the task's shell echoes `host-task-shell-running` before it does anything and
  `host-task-forked-escapee` with the escapee's pid after the fork, both to the
  task's own log, so a staging that never finishes says *which* step never ran;
- between the second marker and a written pidfile lie only `setsid` and the
  redirect, whose own errors go to that same log;
- `launch_diagnosis` now carries the task pid's cgroup and whether it is live,
  beside the launcher's;
- and `staged_escapee` prints the diagnosis on its own lines **before** it
  panics, so a truncated paste of the panic still carries the measurement.

### D8 is recorded as unverified, and the command that settles it is unrun

[D8](#decisions) has never once executed. It is **not** proven and **not**
refuted here, and nothing in this job changed what its test asserts: the
assertion was not weakened, and the test was not switched to
`Supervision::ProcessGroup` to make it pass — that would delete the only test of
the claim.

On `gumbo-nuc-0` as `worksalot`, with `XDG_RUNTIME_DIR` set, in a checkout of
this branch:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | tail -60
```

| What it prints | What it means |
| --- | --- |
| all four `ok`, no "skipping" | D8 is confirmed in execution: the escapee left the process group, stayed in the scope's cgroup, and only the scope signal could have ended it |
| `the task's own command has not started …` | the manager's start job is what the D8 test was waiting out, and the line carries the cgroup the task pid was in, whether it is live, and the client's own stderr from the task log |
| `no escapee recorded a pid at …` | the task entered its scope and still staged nothing; the markers in the log above the panic say whether its shell ran and whether it forked |
| the escapee outlived a kill | D8 is **refuted** on this node, and the line carries the escapee's cgroup and what the manager says about the scope |
| any "skipping" line | nothing was covered; the reason is on the line |

Nothing here touches `probe_supervision`, the macOS path, `supervised_launch`,
or the two tests that pass. No production code changed in this job.

## Correction, 2026-08-06 — the escapee's own trace, and three differences ruled out (job #458)

Job #457's run on `gumbo-nuc-0` at tree `af53f49` narrowed the staging failure to
three unreplicated differences between a hand-built scope, which stages the
escapee fine, and `HostBackend::launch`, which does not: the task-directory cwd,
the task's stdout/stderr redirected to its log file, and the full composed
environment on top of the two-name floor.

**All three are ruled out as *sole* causes by that same run**, without a new
measurement — none of them is ruled out as *half* of one. The
suite holds ten tests and nine of them passed; the ninth is
`a_setsid_escapee_is_staged_outside_the_task_process_group`, and it runs the
**identical** `escapee_script` through the **identical** `HostBackend::launch`
and `spawn_task` — which is what sets the cwd to the task directory, opens
`output.log` and hands the child both fds, and composes the environment out of
`task_env`. The two tests are constructed from the same `backend`-shaped
`with_workspace` call and the same `cfg`; the sole argument that differs is
`Supervision`. So an escapee staged under `Supervision::ProcessGroup` on that
host, at that tree, in that same run, already carried all three differences and
wrote its pidfile anyway. So none of them can be the cause **by itself**, and
what remains is the scope combined with one of them — which is the one place
still worth looking, not a place the elimination closes off.

### What that leaves, and why it still cannot be named

The residue is what job #457 already stated from the code and is unchanged: the
`systemd-run --user --scope --quiet --collect --unit=… --` prefix, the `unset` of
the borrowed bus variables at the head of the wrapper, and those two variables in
the client's own environment. The operator hand-ran that residue — `scope_args`
verbatim, shed and all — and it staged the escapee. So the cause is an
**interaction** between the scope and one of those same three: the cwd, the log
fds and the composed environment are precisely what neither the hand tests nor
the residue carried, which is why each is eliminated alone and none of them is
eliminated as the scope's other half. No argument available from this workspace
picks out which: three attempts have now each ruled out every difference they
could name **on its own** and left the failure standing.

### The one bit no attempt has ever measured

Between the fork and the missing pidfile, the observed evidence is silence: the
task's log holds both markers and no error. That is consistent with two
incompatible stories — the escapee's shell ran and its write failed, or it never
got that far — and the fixture could not tell them apart, because the escapee
inherits the task's fds and an error into a redirected fd is exactly as invisible
as no error at all.

The escapee's first instruction is now `exec 2>` an explicit path of its own,
`escapee_trace`, followed by a marker carrying its pid:

| What the run leaves at `escapee.trace` | What it means |
| --- | --- |
| the file is absent | the escapee's shell never ran at all — `setsid` did not exec it, or it died first; the forked pid's own facts follow on the same line |
| `host-escapee-shell-running <pid>` alone | the shell ran and the pidfile write is what failed, silently |
| that marker and an error line | the same, with the write's own errno |
| the marker and a written pidfile | the staging succeeded and D8's assertions were reached |

`escapee_diagnosis` prints that file, the pidfile, and the forked pid's liveness,
process group, cgroup and `cmdline` beside `launch_diagnosis`'s existing lines.
`a_failing_escapee_write_reports_into_the_escapees_own_trace` is the
discrimination's regression test: it launches the shipped composition with the
pidfile path pre-created as a **directory**, so the write cannot succeed, and
asserts the trace holds the marker and the error anyway. It needs no systemd, so
it covers the probe wherever `setsid` is.

### D8 stays unverified, and this is the last attempt

[D8](#decisions) is neither proven nor refuted. No assertion was weakened, no
test was switched to `Supervision::ProcessGroup`, `probe_supervision` and the
macOS path are untouched, and no production code changed in this job.

On `gumbo-nuc-0` as `worksalot`, with `XDG_RUNTIME_DIR=/run/user/1000` set, in a
checkout of this branch:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | tail -80
```

| What it prints | What it means |
| --- | --- |
| all eleven `ok`, no "skipping" | D8 is confirmed in execution: the escapee left the process group, stayed in the scope's cgroup, and only the scope signal could have ended it |
| `no escapee recorded a pid at …` | the staging failed again, and the two `STAGING FAILED` lines above the panic now carry the trace, which settles which of the two stories it is |
| the escapee outlived a kill | D8 is **refuted** on this node, and the line carries the escapee's cgroup and what the manager says about the scope |
| any "skipping" line | nothing was covered; the reason is on the line |

If that run does not settle it, **stop**. D8 is a secondary claim — #309 §2's
`setsid` escape, otherwise P3 work — [D3](#decisions) is proven on both
platforms, and the cost of chasing it has passed its value. "Unverified, and here
is exactly where it stops" is where it should be left.

## Correction, 2026-08-06 — the one variable four attempts never changed (job #459)

Job #458 was to be the last attempt, and this job is not a fifth repetition of
it: it changes the **fixture**, not the instrumentation. Reading the three
escapee tests together shows one difference nobody had varied.

| Test | Supervision | Result to date |
| --- | --- | --- |
| `a_setsid_escapee_is_staged_outside_the_task_process_group` | `ProcessGroup` | passes |
| `a_failing_escapee_write_reports_into_the_escapees_own_trace` | `ProcessGroup` | passes |
| `a_kill_reaches_a_setsid_escapee_through_the_scope` | **`Scope`** | never staged, on any run |

So **no escapee has ever been staged under a scope**, and the two green tests
cannot detect that: neither creates one. Jobs #455–#458 each added tracing,
markers, a membership check and a trace of the escapee's own around the one
failing test; none of them moved this variable.

### The experiment: put a scope on a test that passes

`a_setsid_escapee_is_staged_under_a_scope_as_well` in
`crates/container/tests/host_backend.rs` is
`a_setsid_escapee_is_staged_outside_the_task_process_group` with the single
argument changed — same `escapee_script`, same `cfg`, same `staged_escapee`, same
process-group assertion, `Supervision::Scope` from `scope_or_skip` in place of
`Supervision::ProcessGroup`. It is guarded by `scope_or_skip` **and**
`setsid_or_skip`, so a machine with no scope to test says so rather than passing
vacuously.

It stops at the staging. What a `kill` then reaches is D8's own claim and is
already asserted by `a_kill_reaches_a_setsid_escapee_through_the_scope`; carrying
the process-group test's post-kill `alive(escapee)` assertion across would assert
the **opposite** of D8 under a scope. It also omits the D8 test's
`supervised_cgroup` wait on the task pid, because the point is the simplest
fixture that can hold a scope — and `staged_escapee`'s panic already reports the
task pid's cgroup, its liveness and the task log, so a launch that never entered
its scope is still named.

### What this job did not do

No assertion was weakened. No test was switched to `Supervision::ProcessGroup`.
`probe_supervision`, `supervised_launch`, the macOS path and every existing
assertion are untouched, and **no production code changed** — the diff is one
test and this record.

### The command, and what each outcome settles

The experiment is **unrun**: this job's container has no `systemd-run` and no
unified hierarchy, so the new test self-skipped with `this machine cannot create
a transient systemd scope` — the guard working, not a result. On `gumbo-nuc-0` as
`worksalot`, with `XDG_RUNTIME_DIR=/run/user/1000` set, in a checkout of this
branch:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | tail -80
```

| What it prints | What it means |
| --- | --- |
| all twelve `ok`, no "skipping" | D8 is confirmed in execution: the escapee left the process group, stayed in the scope's cgroup, and only the scope signal could have ended it |
| `no escapee recorded a pid at …` for **both** `…_under_a_scope_as_well` and `a_kill_reaches_…` | the defect reproduces in the simplest fixture that has a scope, so a `setsid` child does not survive `HostBackend`'s scoped launch at all and D8's test is merely its first victim — the next measurement belongs on the scoped staging test, which has no membership wait and no cgroup assertions in the way |
| `…_under_a_scope_as_well` **`ok`** and `a_kill_reaches_…` still staging-failed | the scope is not sufficient to cause it; the cause is something the D8 test does that the staging test does not, and that list is short and enumerable — the `supervised_cgroup` wait before the staging wait, and the ten extra seconds of task lifetime it spends |
| the escapee outlived a kill | D8 is **refuted** on this node, and the line carries the escapee's cgroup and what the manager says about the scope |
| any "skipping" line | nothing was covered; the reason is on the line |

### D8's record after this job

[D8](#decisions) is still **neither proven nor refuted**, and this job records no
outcome it did not execute. What is new is that the failure now has a second,
simpler fixture to fail in — or a discriminator that narrows it in one run. The
stopping rule from job #458 stands: if this command does not settle D8, record
what it showed and leave the claim unverified.

---

## Correction, 2026-08-06 — slice 3 as landed (job #460)

Appended by job #460, which implemented [slice 3](#slices). Nothing above is
edited except the head and that slice's State cell. This section records what
"live" was decided to mean, what the two refusals look like on the wire, and why
the tests are at tier 1 rather than where the test-placement note put half of
them.

### The two checks, as built

Both live in `crates/worker/src/daemon.rs` and share one predicate, so the two
sites cannot drift into refusing in different words:

| Where | Shape | What the caller sees |
| --- | --- | --- |
| `refresh`, before `begin_refresh` | `RefreshOk { accepted: false, skipped: Some(reason) }` | `admin worker-refresh` prints `refresh SKIPPED — {reason}`; no build starts and the node's refresh slot is not consumed |
| `run_refresh`, after `quiesce` and after the `drained` wait | `record_refresh_failure(.., "drain", ..)` then `abort()` | the same terminal shape the drain timeout already produces: `RefreshResult::Failed { stage: "drain" }`, launches reopened, node left on its old images |

The accept refusal rides in `skipped` rather than as an RPC error because
`skipped` already means exactly this — the node could not even *attempt* a
refresh — and [`update.sh`](../../deploy/prod/update.sh) fails the deploy leg for
any node that does not print `refresh OK:`, so the refusal is loud on the path
that matters without a second reply shape. The boundary refusal invents nothing:
it is the drain-timeout branch with a different reason, which is what
[D4](#decisions) asked for.

**The boundary check runs after the drain wait, not beside it.** A launch that
was accepted before `quiesce` may not have created its task directory yet, so
the listing is only complete once `drained()` holds — checking first would read
"no host work" for a task that is about to exist. The build phase is what makes
the second check load-bearing at all; the drain wait is what makes it *correct*.

### "Live" means running — not launched-and-not-removed

`list_managed_running` is the source, so a task counts as live while its status
is `Running`: no `exit_code` written, and either this daemon's own live set or
the #309 §2(b) pid-identity rule still claims it. The **exited-but-unremoved**
window — one of the P1 gaps job #434's report named — is deliberately on the
*permissive* side of the line, for two reasons:

- **A swap cannot lose an exited task's result.** The exit status is written by
  the task's own wrapper, not by the daemon (`supervised_cmd`, #309 correction
  finding 2), and `inspect` is a pure function of the task directory. The daemon
  being replaced under an exited-but-unremoved task costs nothing, which is
  exactly what D4 exists to prevent and precisely what does not happen here.
- **The window has no upper bound.** It closes when the dispatcher disposes of
  the task, so a crash between exit and `remove` leaves a directory that is never
  reclaimed by anything but the next launch's `reclaim_workspace`. Counting it as
  live would turn one leaked directory into a node that can never be deployed to
  again — trading a bounded, self-clearing refusal for a permanent one.

So the refusal is bounded by `task_timeout` exactly as [§3](#3-the-drain-guarantee--the-crux)
prices it, and no other state on the node can extend it.

**A backend that cannot answer refuses.** `list_managed_running` failing is not
read as "nothing is running": a daemon that cannot tell whether a swap would
kill a task takes the same refusal, naming the backend error. That is the only
answer that cannot lose work, and on a host node the listing fails only when the
worker-owned root is unreadable.

### A container node is untouched, and that is asserted

The check reads a `host_mode` flag taken from `backend_kind(&config.modes)` at
construction, so a node that does not name `host` never calls the backend at
all — not "calls it and ignores the answer".
`a_container_node_refreshes_without_asking_what_is_running` is that assertion in
its sharpest available form: the fake backend is scripted to **fail** the
listing, and the refresh still reaches its swap. If the query ever leaked onto
the container path, that failure would refuse the refresh and the test would go
red.

### Why the tests are at tier 1, both of them

The [test-placement note](#slices) puts the pure precondition at tier 1 and the
swap-boundary re-check at tier 2 in `crates/worker/tests/nats_backend.rs`. The
boundary test is at tier 1 instead, and the reason is not convenience:

- **A host-mode daemon cannot boot in CI.** `run` refuses `WORKER_MODES=host`
  when `probe_supervision` finds no scope mechanism, which is every machine
  without systemd — including this repo's evaluator, where
  `host_mode_without_a_supervision_unit_refuses_to_start` asserts that refusal
  today. A tier-2 test driving the real RPC would self-skip everywhere it is
  ever run.
- **Staging a real host task through the daemon would delete the checkout.**
  `local_backend` builds `HostBackend::new`, which pins the workspace to the
  literal `/workspace` (only `with_workspace` avoids it), and `launch` calls
  `reclaim_workspace` — `remove_dir_all` — when no managed task owns it. On any
  machine where that path is a working tree, a suite that launched a host task
  through a real daemon would remove it. `crates/container/tests/host_backend.rs`
  is safe from this because every backend it builds names its own workspace; a
  daemon-level test cannot.

What the tier-1 tests exercise is not a re-implementation of the checks: they
call the shipped `refresh` and `run_refresh` against a scripted backend and a
stand-in refresh script, so both refusals are driven through the real code.
`a_host_task_started_during_the_build_is_refused_at_the_swap_boundary` is the
one that distinguishes this design from the naive one — the accept check passes
on an idle node, the task appears while the build blocks on a release-file
handshake, and the boundary refuses at the `drain` stage. Each check was deleted
in turn and only its own test went red, which is the independence
[D4](#decisions) claims, measured rather than asserted.

### What this does not do

No deploy path changed — `build-worker.sh` and `worker-refresh.sh` are slices
4–6, and the [ordering note](#slices) is why they stay untouched.
`probe_supervision`, the scope path and D8's test are untouched. No node config,
no `WORKER_MODES` change, no schema and no epoch: the only wire-visible effect is
that `RefreshOk::skipped` now carries a second reason, which an N-1 dispatcher
already prints verbatim. `docs/spec.md` §3.1's drain guarantee is still written
for a containerized daemon; narrowing it is still [slice 8](#slices)'s, and this
slice is the mechanism that narrowing will describe. The
[slice-2 correction](#correction-2026-08-05--slice-2-as-landed)'s line that
"slice 3's `refresh` precondition is untouched" is superseded here and stands as
the record of what was true at job #447.

---

## Correction, 2026-08-06 — the scope's client was rewriting the task's own command (job #462)

Job #459's command ran on `gumbo-nuc-0` at tree `e0b6570`. Ten tests passed;
the two under a scope failed, at the same line, in staging:

| Test | Supervision | Result |
| --- | --- | --- |
| `a_setsid_escapee_is_staged_outside_the_task_process_group` | `ProcessGroup` | `ok` |
| `a_setsid_escapee_is_staged_under_a_scope_as_well` | **`Scope`** | staging failed |
| `a_kill_reaches_a_setsid_escapee_through_the_scope` | **`Scope`** | staging failed |

So the experiment answered its own question: the defect reproduces in the
simplest fixture that has a scope, and D8's test was its first victim rather
than its cause.

### The cause: `systemd-run --scope` expands the command line itself

With `--scope`, `systemd-run` does not hand the command to the manager — it
`execvpe`s it in its own process, and immediately before that exec it runs the
argv through systemd's `${VARIABLE}` substitution. **This is a diagnosis, not a
measurement**: none of the mechanism below was executed here, and the operator
run at the end of this section is what would settle it. Two facts are what it is
read from, both out of systemd's own tree rather than argued from this one:

- `systemd-run(1)` on `--expand-environment=`: *"If enabled (the default),
  environment variables specified as `${VARIABLE}` will be expanded in the same
  way as in commands specified via `ExecStart=` in units. With `--scope`, this
  expansion is performed by `systemd-run` itself"*, and *"Disabling variable
  expansion is useful if the specified command includes or may include a `$`
  sign."*
- systemd's `NEWS` for **v258**: *"systemd-run's `--expand-environment=` switch,
  which was disabled by default when combined with `--scope`, has been changed
  to be enabled by default."* `gumbo-nuc-0` runs a systemd new enough to have
  taken that change; the flag itself exists from **v254**.

That substitution is the unit-file one, not a shell's, so `$$` is its escape for
a literal dollar and braceless `$NAME` is left alone. Applied to the exact argv
this backend ships, it is almost invisible — which is why five jobs did not see
it:

| Token in the shipped composition | What the client does to it | What that looked like |
| --- | --- | --- |
| `"$CHUG_HOST_EXIT_TMP"`, `"$CHUG_HOST_EXIT"` in the exit wrapper | untouched — they are **braceless** | the wrapper kept working and the task kept recording its exit status |
| `"$@"`, `$?`, `$s` in that wrapper, `"$!"` in the fixture | untouched — braceless names are not expanded | the task ran, and its log carried both markers **and a real forked pid** |
| `${CHUG_ENV_PATH:-}` in `bootstrap_cmd`'s runtime-env prelude | skipped — the `:` makes it syntax systemd does not support | the only braced token the composition has, and the client leaves it alone |
| `"$$"` in the escapee's own script | **collapsed to `"$"`** | `printf %s "$" > escapee.pid` wrote a literal `$`; `recorded_pid` cannot parse it, so the staging timed out with no error anywhere |

**In production the client was rewriting nothing at all, and that is luck rather
than design.** The shipped composition holds no braced token except the single
`${CHUG_ENV_PATH:-}` in `bootstrap_cmd`'s runtime-env prelude
(`crates/container/src/lib.rs`), and the `:` inside those braces is syntax
systemd does not support, so it is skipped. Every other `$` the composition
writes is braceless: `bootstrap_cmd`'s `"$JOB_BRANCH"`, `"$REPO_URL"` and
`$PATH`, and the exit wrapper's `"$CHUG_HOST_EXIT_TMP"` and `"$CHUG_HOST_EXIT"`,
which `supervised_cmd` builds through a `format!` capture whose `$` is literal
and whose name is unbraced. So `--expand-environment=no` cannot change the text
of any launch this backend ships today, and the escapee fixture's `$$` is the
only token the client has ever rewritten. The luck is that a job type whose
command grew a `$$` or a plain `${VAR}` would have been rewritten silently, and
the escapee fixture is the thing that grew one first.

Every observation the four earlier attempts collected is exactly this and
nothing else: a task that runs, a fork that happens, a marker with a pid, an
escapee whose shell reaches its own trace, and a pidfile that never holds a
number. The eliminations were all sound and none of them could reach it — the
process-group sibling never invokes `systemd-run` at all, job #457's evaluator
shim `exec`d the rest of the argv verbatim, and a hand-built scope typed at a
shell has the **shell** consume `$$` before `systemd-run` ever sees it.

### The fix, and the version window it opens

`scope_args` in `crates/container/src/host.rs` now passes
`--expand-environment=no`, so the argv `supervised_launch` composes reaches the
task byte for byte. It is one flag on the client and changes nothing about the
scope, the cgroup, the environment or the kill path.

The flag is systemd **v254** and later. A client older than that refuses the
option rather than the scope, and because `probe_supervision` creates its
throwaway scope through the same `scope_args`, such a node fails the probe at
boot and refuses to advertise `host` — with a refusal that names the option and
the version instead of sending the operator after a bus that was never
addressed. Nothing is lost by that: the versions that reject the flag are
exactly the versions that never expanded a scope's argv.

### What this job executed, and what it did not

Executed here: the unit tests, on a machine with no systemd —
`a_scope_hands_the_task_the_dollars_the_dispatcher_wrote` (the flag is in the
argv, ahead of the `--`, and the task's command after it is unchanged) and
`a_client_too_old_to_leave_the_command_alone_is_named_as_such` (the probe's
refusal for a client that predates it).

**Not** executed here: the mechanism itself. This container has no
`systemd-run` and no unified hierarchy, so every scope test still self-skips.
The cause above is read from systemd's source, man page and `NEWS`, and from
the failure signature the node reported — it is a diagnosis, and the run below
is what turns it into a measurement.

`crates/container/tests/host_backend.rs` gains
`a_scoped_task_is_handed_the_dollars_its_command_was_written_with`: a scoped
task whose whole command is `printf %s "$$" > …`, asserting the file holds a
pid. No `setsid`, no kill, no cgroup — if the diagnosis is right and the flag
were dropped, that is the test that goes red, and it names the cause in its own
message.

### The command

On `gumbo-nuc-0` as `worksalot`, with `XDG_RUNTIME_DIR=/run/user/1000` set, in a
checkout of this branch:

```sh
cargo test -p container --test host_backend -- --nocapture 2>&1 | tail -80
```

| What it prints | What it means |
| --- | --- |
| all thirteen `ok`, no "skipping" | the diagnosis is confirmed and **D8 is confirmed in execution**: the escapee left the process group, stayed in the scope's cgroup, and only the scope signal could have ended it |
| `a_scoped_task_is_handed_the_dollars_…` red, naming what the shell was handed | the expansion is real and the flag did not stop it — the client is older than v254, or the manager's own expansion applies where this one reasoned it does not |
| the two escapee tests still failing in staging while the verbatim test is `ok` | the argv is now faithful and the staging failure is something else; the `STAGING FAILED` lines carry the escapee's trace, and D8 stays unverified |
| the escapee outlived a kill | D8 is **refuted** on this node, and the line carries the escapee's cgroup and what the manager says about the scope |
| `unrecognized option '--expand-environment=no'` | this node's systemd predates v254 and now refuses `host` at the probe; that refusal is the deliberate half of the fix |

### What this job did not do

No assertion was weakened, no test was switched to `Supervision::ProcessGroup`,
and the two scoped escapee tests are untouched. `probe_supervision`'s shape, the
D3 record, the macOS path and slices 4–8 are unchanged.

---

## Proof, 2026-08-06 — D8 in execution, thirteen of thirteen (job #466)

Appended by job #466, a `docs` job that changed no code, no test and no gate.
The operator ran job #462's own command on `gumbo-nuc-0`, and it passed. That
turns #462's diagnosis into a measurement and closes the last unverified claim
in [slice 2](#slices). Nothing above is edited except the head: the `Status:`
paragraph, [D8](#decisions)'s cell and [slice 2](#slices)'s State cell.

### The run

| | |
| --- | --- |
| Host | `gumbo-nuc-0` — NixOS, cgroup v2 |
| systemd | **260 (260.2)** |
| Tree | `692656e` |
| Invoking environment | `XDG_RUNTIME_DIR=/run/user/1000` |

```sh
cargo test -p container --test host_backend -- --nocapture
```

```text
a_scoped_task_is_handed_the_dollars_its_command_was_written_with ... ok
a_setsid_escapee_is_staged_under_a_scope_as_well                 ... ok
a_kill_reaches_a_setsid_escapee_through_the_scope                ... ok
test result: ok. 13 passed; 0 failed
```

**Thirteen passed and nothing skipped**, and the second half of that sentence is
the load-bearing one: the scope tests self-skip through `scope_or_skip`
(`crates/container/tests/host_backend.rs`) on a machine that cannot create a
scope, so a green run *with* skips would have said nothing at all — which is
precisely what every CI run of this file has said since job #447.
`a_kill_reaches_a_setsid_escapee_through_the_scope` is [D8](#decisions)'s only
test, and this is the first time it has reached its assertion on any machine.

### What is proven, stated exactly

- **D8 is proven on Linux, through the shipped path.** The escapee left the
  task's process group, stayed inside the scope's cgroup, and died on a
  `HostBackend::kill` — which only the scope signal can have done, because the
  process-group signal misses an escapee by construction. So #309 §2's
  `setsid()` escape **is** closed on Linux by D3's scope.
- **D3 and D8 are both proven on Linux**, on the same host, in the same run,
  through the same code. D3 is additionally proven on **macOS 26.5.1**
  (`gumbo-air-0`, job #452,
  [the proofs](#proofs-2026-08-06--d3-on-macos-and-on-linux)), where D8 is
  untouched and still leaks: a process that leaves its group leaves the only
  mechanism macOS has.
- **Not "for free."** [D8](#decisions) as first written said the escape closed
  for free, and §6 below still says so; two changes were needed. `kill` signals
  the scope as well as the process group (job #447,
  [the correction](#d8-is-confirmed-on-linux-and-it-needed-one-line-of-code)),
  and `scope_args` passes `--expand-environment=no` (job #462).

### The cause, confirmed rather than inferred

Job #462 read the five-attempt staging failure out of `systemd-run(1)`, systemd's
`NEWS` and its source rather than out of a measurement: with `--scope` the client
`execvpe`s the command itself and runs the argv through systemd's `${VARIABLE}`
substitution first, in which `$$` is the escape for a literal dollar. So the
escapee fixture's `printf %s "$$"` reached the shell as `printf %s "$"`, the
pidfile never held a number, and the staging timed out with no error anywhere.

**The node's version is what closes the argument.** 260.2 is past **v258**, the
release that turned that expansion on by default for `--scope`. The version on
which the fix was needed is therefore the version that exhibits the defect, which
is what a diagnosis read off release notes could not establish.
`--expand-environment=no` in `scope_args` (`crates/container/src/host.rs`) is the
fix, and `a_scoped_task_is_handed_the_dollars_its_command_was_written_with` is
its regression test — a scoped task whose whole command is `printf %s "$$"`,
which passed in the same run.

It also explains every earlier observation at once, which no partial explanation
managed:

- the fixture staged fine in every hand-built rehearsal — a shell performs no
  systemd expansion, and consumes `$$` before `systemd-run` ever sees it;
- it failed only under `Supervision::Scope` — the `ProcessGroup` path never
  invokes `systemd-run`, and job #457's evaluator shim `exec`d the argv verbatim;
- the escapee forked correctly and wrote nothing usable — the fork was never the
  fault, and the missing pidfile was one collapsed character.

### The systemd-version dependency, plainly

`--expand-environment=` exists from **v254**; **v258** made it default-on for
`--scope`. So the same source tree behaves three ways:

| Client version | What it does to a task's argv | What that means here |
| --- | --- | --- |
| below v254 | no expansion, and the option is unknown | the defect cannot appear — and this node fails `probe_supervision`, refusing to advertise `host` with a reason naming the option and the version (`scope_verdict` in `crates/container/src/host.rs`) |
| v254–v257 | no expansion by default with `--scope` | the flag is a no-op and the defect would never have appeared |
| v258 and later | expands `${VARIABLE}` and collapses `$$` unless told not to | the flag is load-bearing; `gumbo-nuc-0` is here, at 260.2 |

That is why the defect read as environment-specific for five attempts: a tree
that is correct on a v257 node silently rewrites a task's command on a v258 one,
and nothing in this repo pins or reports the client's version.

### What this does not settle

- **`XDG_RUNTIME_DIR` remains the qualifier.** The run had it set in the
  invoking environment, as every Linux run of these tests has. A `--user` scope
  needs it to locate a manager — `bus_refusal` in
  `crates/container/src/host.rs` refuses ahead of the attempt when the daemon
  holds neither bus variable — and whether a daemon under a supervisor is given
  one is [slice 7](#slices)'s provisioning question, unanswered here.
- **macOS is untouched.** D8 leaks there and this changes nothing about it.
- **No node runs a native daemon.** What is proven is the mechanism such a
  daemon would rely on, not a deployment; slices 4–8 are unstarted and when they
  start is the operator's call.
- **Nothing survives a reboot**, on either platform, and the scope covers a unit
  restart only — [what the mechanism does not cover](#what-the-mechanism-does-not-cover-stated-plainly)
  is unchanged, and [D4](#decisions) is still the backstop for the rest.
- **CI still covers none of this.** The evaluator has no `systemd-run` and no
  unified hierarchy, so all five scope tests self-skip there exactly as job
  #447 recorded. These assertions run when someone points the suite at a systemd
  host, which is what happened here.

---

## Slice 8, 2026-08-06 — the guarantee narrowed in the spec (job #470)

Appended by job #470, a `docs` job that changed no code, no test and no gate.
Nothing above is edited except the head's `Status:` line and
[slice 8](#slices)'s State cell.

[`docs/spec.md`](../spec.md) §3.1's drain guarantee now says what a **native**
daemon restart covers, immediately under the paragraph it qualifies, in four
cases: container tasks (unconditional, however the daemon is deployed), host
tasks across a restart of the daemon's own supervision unit ([D3](#decisions),
with the 2026-08-06 proof pointers for both platforms), host tasks across the
platform's self-refresh (refused rather than risked, [D4](#decisions)), and a
reboot or any broader restart (**not** guaranteed, said plainly rather than left
as the unstated exception the sentence carried before). Both live qualifiers are
stated there rather than buried: `XDG_RUNTIME_DIR` in the invoking environment,
[slice 7](#slices)'s unanswered provisioning question, and the systemd-version
window in which `--expand-environment=no` is load-bearing
([the version table](#the-systemd-version-dependency-plainly)).

Two things it deliberately does not do. It restates **no mechanism** this
document owns — the scope, the process group, and where the two refusal checks
hang stay here, and the spec links ([#415](415-knowledge-architecture.md) D1,
D5). And it claims **no availability**: the spec's new text says the host half is
written because the mode is designed, not because it is offered, and #401's
`runtime.mode: host` refusal is untouched and still cited in §1.1.

### What this does not do

No code, no gate, no deploy path, no test. Slices 4–7 are unstarted and their
State cells are as they were; `crates/container/src/host.rs`,
`crates/worker/src/daemon.rs` and `deploy/prod/build-worker.sh` are untouched.
Whether a real node ever advertises `host` remains #309 P1's, not this slice's.

---

## Correction, 2026-08-06 — slice 4 as landed (job #469)

Appended by job #469, which implemented [slice 4](#slices). Nothing above is
edited except that slice's State cell, the `Status:` line and the first row of
[what is true today](#what-is-true-today). **No node was touched**: this changes
what `build-worker.sh` *would* install, and prod's two nodes are unreachable
from the Mini, so both still run the container the row above describes.

### What the script installs

| Piece | Linux | macOS |
| --- | --- | --- |
| supervision | `chug-worker.service` in `WORKER_UNIT_DIR` (default `/etc/systemd/system`), `User=root`, `Restart=always` | `com.chuggernaut.worker.plist` in `~/Library/LaunchAgents`, bootstrapped into `gui/$(id -u)`, `KeepAlive` |
| the run spec | `WORKER_ENV_FILE` (default `/etc/chuggernaut/worker.env`), read by `EnvironmentFile=` | the same file under `~/chuggernaut-worker/`, **sourced** by the agent's `sh -c` — `launchd` has no `EnvironmentFile` |
| the binaries | `/usr/local/bin/chuggernaut`, `/usr/local/lib/chuggernaut/{chuggernaut-channel,worker-refresh.sh}`, `docker cp`'d out of the worker image the same run built | the same paths |

Every value in the environment file is **single-quoted**, and a value carrying a
single quote of its own is refused rather than escaped. Both readers are
shell-like and their quoting rules agree, so `WORKER_MODES='container, host'` is
one value to systemd and one value to `. <file>`; escaping it two ways would be
a guess, and a wrong guess is a daemon that will not boot.

The plist deliberately does **not** live in `deploy/prod/launchd/`. §2's finding
stands: `install-launchd.sh` globs that directory, so a template there would
install a worker agent on the Mini.

### A container-only node ends up equivalent, and here is the argument

Every node in the fleet is container-only, so this is the claim that decides
whether the change is safe. Taken piece by piece against the `docker run` it
replaces:

| What the container had | What the unit gives | Same? |
| --- | --- | --- |
| `-e WORKER_NODE`, `NATS_URL`, `RUST_LOG`, `WORKER_REFRESH_GIT_URL`, and every optional `WORKER_*` | the same names, same values, one line each in the environment file — asserted as a whole-file golden in `deploy/prod/build-worker.test.sh` | yes |
| `--restart=always` | `Restart=always` / `KeepAlive` | yes |
| `-v /var/run/docker.sock:/var/run/docker.sock` | nothing — the daemon opens `unix:///var/run/docker.sock` on the node, which is `WORKER_DOCKER_ENDPOINT`'s default already | yes, and see below |
| `-v $HOME/chuggernaut-worker/keys:/data/keys:ro`, `NATS_CREDS=/data/keys/worker.creds` | `NATS_CREDS` naming the **host** path under `WORKER_KEYS_DIR` | equivalent, path changed |
| `WORKER_GIT_KEY` defaulting to `/data/keys/worker_git` | the host path beside the creds | equivalent, path changed |
| `--device /dev/kvm` when `WORKER_KVM` is on | nothing — the daemon's own view is the node's | yes |
| four nix bind mounts | nothing — the node's `/nix`, profiles and daemon socket are already at those paths | yes |
| `WORKER_CACHE_DIR` as env only, host dir provisioned first | unchanged | yes |
| the daemon binary, built in the pinned Dockerfile | the same bytes, `docker cp`'d out of the same image | yes |

**The docker socket is the sharpest case and it gets *more* permission, not
less.** The container reached the socket because the socket was bind-mounted
into it and the container ran as root. The unit runs as `User=root` on the node,
so it reaches `/var/run/docker.sock` whatever that socket's group is — the login
user's membership of `docker` (which is how `build-worker.sh` runs `docker build`
over ssh at all, and what [D5](#decisions) cites) stops being load-bearing for
the daemon. On macOS the agent runs as the login user, which is the same uid
that owns the colima socket today, so nothing changes there either. What changes
is stated plainly: on Linux the daemon is now root on the node rather than root
in a container that held a socket — [D8](#decisions)'s "blast radius is what
changes", exactly.

**Two paths genuinely move**, and both are refused rather than silently wrong: a
`worker.creds` the daemon cannot read fails the deploy before the live daemon is
touched, and a `WORKER_GIT_KEY` still naming `/data/keys/...` is refused with the
host path to declare instead. `/data` only ever existed inside the container, so
a declaration naming it is a conversion trap with no correct interpretation.

**`WORKER_CACHE_DIR`'s row says "unchanged", and the reason underneath it did
change.** The deploy still provisions the host directory before the daemon
starts, but not for the reason it used to. Containerized, the daemon's own
`create_dir_all` never reached the host at all, so the engine refused every
launch whose bind source was missing (#379). Natively it *does* reach the host,
and runs in `local_backend` before the daemon serves anything — so the gap that
argument closed is gone, and a different one opens: it creates only where that
process may, and how far that reaches is the supervisor's, not the daemon's. The
macOS agent runs as the login user in their GUI domain, so a path under a
root-owned parent is a `Config` refusal at start that `KeepAlive` then loops on.
The Linux unit is `User=root` (above), so its gap is the narrower one — a
read-only or otherwise unwritable path, not merely a parent it does not own. The
deploy's own `mkdir` is the login user's on both, which is why the fallback is
`sudo -n`. Provisioning first forecloses the lot, which is why the row is
"unchanged" rather than "no longer needed"; the script's refusal names both
failure modes, and `docs/spec.md` §3.1 states the prerequisite per view.

### The drift guard keeps a `docker inspect` path, on purpose

[D7](#decisions) says the guard compares the live unit's environment against the
composed environment file, and that is what it does — the live side is
`cat $WORKER_ENV_FILE`, legible on the node with no `docker inspect`. But a node
that has never been converted **has no such file**, and the conversion is
precisely the recreate the guard exists to police: read nothing there and the
one deploy that replaces the container is the one deploy that drops every
setting silently. So the guard falls back to the container's environment and
says which side it read. The `docker inspect` half retires when the last node is
converted, not when this slice lands.

The comparison itself is unchanged in shape — presence over the same `WORKER_*`
key set decides the refusal, values are informational, `WORKER_SPEC_DROP_OK=1`
is still the way to drop one on purpose — with one addition: the live side is
unquoted before it is compared, so a converted node does not report
`live ''2'' -> declared '2'` on every run.

**The read is tri-state, and that is what keeps the guard from degrading to a
pass.** "The file is absent" and "the file is there and I cannot read it"
produce the same empty output. Collapse them and a converted node whose
environment file the login user cannot `cat` falls through to a `docker inspect`
that the same node also answers emptily — and the run prints the *fresh-node*
line while overwriting the node's whole run spec unchecked. That is #390's
failure mode restored, invisibly, on the one path [D7](#decisions) exists to
cover. So the node answers which case it is, and **cannot read REFUSES**, with
the live daemon untouched. The environment file is installed `0644` for the same
reason: it carries paths, URLs and settings and no secret — it *names* the
credential, it does not hold it — and the guard must be able to read it back as
the login user on every later deploy. A root-only mode there would have made
every converted Linux node silently unguarded from its second deploy onward.

### The health probe had to be bounded to *this* start

`docker logs --tail 50 chug-worker`, read after a `docker rm -f` and a
`docker run`, could only ever show the new container's output — the bound was
free. A unit's journal and a launchd agent's `StandardOutPath` both span **every
generation the node has ever run**, so the same 50 lines are no longer a
statement about this start. The failure that opens: a converted, idle node is
redeployed, the new daemon cannot reach NATS and crash-loops; under
`Type=simple` + `Restart=always` the unit is `active (running)` on most 3s
polls, and the last 50 journal lines still hold the *previous* generation's
`worker up` because a quiet node logs little between deploys. The probe would
print HEALTHY over a daemon that never connected — the silent "deployed" #207
built this block to make impossible.

So each platform gets a real bound. Linux reads
`_SYSTEMD_INVOCATION_ID`, which systemd mints fresh per start: exact, and immune
to the clock skew a `--since` would inherit. A systemd too old to report one
yields no verdict rather than a false pass, so the deploy times out loudly.
macOS has no such handle, so the install **truncates the agent's log between
`bootout` and `bootstrap`** — the one window where the old agent is gone and the
new one has not started — and the `tail` can only see the new agent.

### Both platforms pre-flight the paths they write

The Linux unit directory had a check from the start ([slice 7](#slices)'s NixOS
case demanded it). macOS needed the same courtesy and did not have it: only the
environment file and the agent live in the login user's tree, while
`/usr/local/bin` and `/usr/local/lib/chuggernaut` are root's on **both**
platforms — and on a stock Apple-Silicon mac (which the plist's own
`/opt/homebrew` PATH assumes) `/usr/local` is root-owned and often absent.
`install-launchd.sh`, the precedent [D2](#decisions) names, writes only under
`$HOME`, so this is the first thing on that platform to need it. Without the
check the operator gets a bare `sudo: a password is required` from inside the
install; with it, the same shape of named refusal the unit-directory case gives,
before anything is extracted.

Separately, **every `ssh` that reads nothing now reads `< /dev/null`.** The
drift check already said why for its own two calls — `update.sh` runs this whole
script over an ssh session whose stdin it must not swallow — and it is true of
all of them. The health probe is the one that turns it into a hang, being a
loop.

### The nix toolchain-shape guard was ported, not deleted

[§5](#build-workersh) reads that guard as a mount constraint and says it "can be
deleted rather than ported". **That is half right, and the half it misses is
load-bearing.** Two things were entangled in one check:

- *A direct symlink under a real parent* — a mount constraint, because
  `mount(2)` resolves a bind source host-side and would flatten the symlink.
  Gone with the mount, correctly.
- *Resolves into the store* — **not** a mount constraint. `store_target`
  (`crates/worker/src/nix.rs`) canonicalizes the realise target in the daemon's
  own boot check and refuses anything landing outside the store dir. A native
  daemon runs that same check against the node's filesystem, so a plain
  directory still refuses the boot and the supervisor still loops it.

So the check became `readlink -f` resolving under `WORKER_NIX_STORE_DIR`: it
asks exactly what the daemon asks, and it is genuinely looser — a NixOS
`environment.etc` entry, disqualified before because of its `/etc/static` hop,
now qualifies. Deleting it outright would have reintroduced the node-down class
this file is built around.

### Three knobs the design did not name

`WORKER_UNIT_DIR`, `WORKER_ENV_FILE` and `WORKER_KEYS_DIR` (plus `WORKER_PATH`
for the PATH the supervisor gives the daemon, which is also the PATH
[slice 1](#slices)'s launch floor carries into a host task). They resolve
per node like every other `WORKER_*`, and each has a default that needs no
declaration.

`WORKER_UNIT_DIR` is the one that is not cosmetic. **On NixOS
`/etc/systemd/system` is a read-only symlink into the store**, so a unit cannot
be dropped there at all — which is the state [slice 7](#slices) exists for. The
script refuses, with the live daemon running, naming both the remedy (point the
knob at a writable unit path) and the owner (declare the unit on the node). It
does not silently fall back to `/run/systemd/system`: that path does not survive
a reboot, and a node whose daemon disappears on the next power cycle is worse
than a deploy that failed.

### What this does not do

- **It converts nothing.** No `chuggernaut.env` edit, no node run, no restart.
  An operator converts a node by running `build-worker.sh` against it.
- **`worker-refresh.sh` is untouched**, and that is the ordering hazard to hold:
  its swap still removes and re-creates a *container*. A converted node asked to
  self-refresh does **not** get a second daemon, though — the swap reads
  `KEYS_SRC` from `docker inspect chug-worker` and, finding no container, exits
  1 with `no /data/keys mount on chug-worker; refusing swap (would strand
  creds)` (`deploy/prod/worker-refresh.sh`) **before `RUN_NEW` is composed**. So
  the real behaviour is a loudly refused swap surfaced as a node-refresh
  warning: the node stops updating itself and says so, which is better news than
  a second daemon on one node name and is the note [slice 6](#slices) should be
  read against. `build-worker.sh` prints the same thing at the moment it
  converts a node, so an operator learns it there rather than from a failed
  deploy leg. The detached swapper and every mount and device carry-forward are
  deliberately left in place for slice 6 to delete in one piece.
- **[Slice 5](#slices) is not started**: the creds stay under the login user's
  home, so [D5](#decisions)'s root-owned `0700` boundary is still nominal. The
  `WORKER_KEYS_DIR` default is the one line that moves when it lands.
- **`nix/chug-node/` is untouched** ([slice 7](#slices)), so the unit is the
  platform's for now and the charter still says the module declares none.
- **`docs/spec.md` §3.1's drain guarantee** was narrowed by [slice 8](#slices)
  (job #470), which landed in this branch's base while it was in review — so
  the guarantee already distinguishes container tasks, host tasks under a unit
  restart, host tasks under the self-refresh, and the reboot residue. Nothing
  here changes it.

### Verification — stated, and what was not run

`deploy/prod/build-worker.test.sh` (50 cases, all passing) is the whole of it,
and it is the tier the change can be expressed at: the script's output is a
remote command string, and the test drives it with a fake `ssh` and `git` that
log their argv. What is asserted: the whole environment file and the whole unit
as goldens for a container-only node, the binary extraction, the drift guard
firing on a declared-but-unforwarded key, `WORKER_MODES` round-tripping
byte-identically, the macOS branch producing a plist and never a unit, and every
refusal reaching neither the supervisor nor the install.

Five of those cases are the ones this correction's later half exists for, and
each was **checked against the un-fixed code**, not merely written beside the
fix: an unreadable environment file (`rc=0`, and the fresh-node line printed,
before the tri-state read), a stale `worker up` on Linux (`rc=0` before the
InvocationID bound) and on macOS (`rc=0` before the truncation), the
`/usr/local` refusal, and the fresh-node contrast that keeps the first from
being a blanket refusal.

**Not run, and it cannot be from here:** the composed script has never executed
on a node. `systemctl`, `launchctl`, `plutil` and `docker cp` are all faked. The
rendered unit was read against systemd's syntax rather than loaded by it; the
plist was parsed as a property list and the environment file was sourced by
`sh`, both outside the test. The first real execution is an operator running
this against a node they can reach — which the [risk list](#risks-and-open-questions)
already says should be a reachable node, not a prod one.

---

## Correction, 2026-08-06 — slice 5 as landed (job #472)

Appended by job #472, which implemented [slice 5](#slices). Nothing above is
edited except that slice's State cell, the `Status:` line and the
`worker-creds` row of [what is true today](#what-is-true-today). **No node was
touched**: this changes where `build-worker.sh` *expects* the credentials to be
and what it refuses without them. Prod's nodes are still unreachable from the
Mini and still run the container.

### What moved, and what did not

| Piece | Before | After |
| --- | --- | --- |
| `WORKER_KEYS_DIR` default, **Linux** | `$HOME/chuggernaut-worker/keys` | **`/etc/chuggernaut/keys`**, required to be `root`-owned at mode `700` |
| `WORKER_KEYS_DIR` default, **macOS** | `$HOME/chuggernaut-worker/keys` | unchanged — the agent runs as the login user, so the boundary has nobody to exclude |
| `NATS_CREDS`, `WORKER_GIT_KEY` | under that default | under that default; both follow it with no separate knob |
| `chuggernaut admin worker-creds` | mints at `0600`, prints the path | **unchanged**, exactly as [D5](#decisions) requires — it is local-only and knows nothing about how a node consumes the file |
| the node-local *artifacts* | — | **already moved in slice 4**: the binary, the channel binary and the refresh script land at `/usr/local/…`, the paths `crates/worker/src/config.rs` already defaults to. The slice line names them; there was nothing left to do |

`/etc/chuggernaut/keys` rather than a new tree of its own because
`/etc/chuggernaut/worker.env` is already the unit's directory: the run spec and
the credential it names sit beside each other, at modes that differ for the
reason they exist — `0644` on the file that carries no secret so [D7](#decisions)'s
guard can read it back as the login user, `0700` on the directory that does.

### Why it is a finding and not tidiness, restated from the code

The `:ro` bind the container had made the boundary look like "only the daemon
can read this", and the bind *source* was a directory in the login user's home.
That user is in the `docker` group — which is how `build-worker.sh` runs
`docker build` over ssh at all — and is who every deploy authenticates as. So
the pre-slice arrangement is not merely no better than the mount: going native
with it would leave the credential readable by everything that user runs, which
is a **lower** boundary than the one being replaced, on a change whose whole
argument ([D1](#decisions)) is that container mode is not degraded by it.

The Linux unit is `User=root` (slice 4), so root-owned `0700` is a boundary the
daemon can be on the right side of and nothing else on the node can. That is the
first time this credential has had a real one.

### Four refusals, because one message would have been wrong three times

A daemon that cannot read its credential does not come up degraded — it fails to
start, and `Restart=always` loops the failure on a node the operator has just
converted, which prod cannot reach from the Mini. So the whole of it is asked in
**one ssh round trip, before the image build**, with the live daemon running:

| State | Refusal names |
| --- | --- |
| directory absent | `sudo install -d -o root -g root -m 0700 <dir>`, and README §6 for the rest |
| wrong owner or mode | **what it found**, the expected `root`/`700`, the `chown`+`chmod`, and `WORKER_KEYS_DIR_<node>` |
| `worker.creds` not in it | mint on the Mini, `scp` to staging, `sudo install -o root -g root -m 0600` |
| **cannot look** | that it could not see the file, and passwordless `sudo` as the remedy |

The fourth is the one worth arguing for. Inside a root-owned `0700` directory
the login user cannot `test -r` the file **at all**, so "not there" and "not
allowed to look" are the same failed test — and collapsing them tells an
operator to re-mint a credential that is already installed correctly. That is
the same tri-state shape [D7](#decisions)'s run-spec read needed for the same
reason, and it is why the probe returns owner, mode and a credential *state* as
data rather than a verdict.

Moving the check **before** the build is new and is deliberate. Nothing it asks
needs an image, the platform probe already sets the precedent ("a node this
script cannot supervise refuses in seconds rather than after a ten-minute image
build"), and a wrong-mode directory is a failure this slice introduces — making
the operator pay ten minutes to discover it would be a poor trade. The
`WORKER_GIT_KEY` `/data/keys` refusal moved up with it, since it decides the same
directory.

### The git key is the same move, and it was the half left behind

The credential and the git key sit in the same directory and follow the same
default, so "where the creds live" is really two files — and the second one
carries the move differently. A missing `worker.creds` **stops the daemon**, so
the guard above catches it on the next deploy. A `WORKER_GIT_KEY` naming the
wrong path does not: the daemon starts, serves jobs, and only *self-refresh*
fails, weeks later and quietly. So the git key needs its refusal stated where the
credential's failure states itself.

Two places said the old path and only one of them is a script:

- **`build-worker.sh`** now refuses, on Linux, a `WORKER_GIT_KEY` that resolves
  under the node's `$HOME` and **outside** the credential directory. Both harms
  are real and independent — the `docker` group can read a key in that home
  ([D5](#decisions)'s own argument), and §6's migration `rm -f`s exactly that
  file, after which the run spec names a key that is gone. A path *inside* the
  credential directory is exempt however it was reached: the owner-and-mode guard
  has already vouched for that directory, so a node whose root-owned `0700`
  `WORKER_KEYS_DIR` happens to sit under a home is served rather than refused for
  a boundary it satisfies. macOS is exempt entirely, for the reason the whole
  boundary is.
- **`chuggernaut admin worker-git-key`** printed `install both under the node's
  ~/chuggernaut-worker/keys/`. The **minting** is unchanged — [D5](#decisions)
  requires that of `worker-creds` and the same reasoning holds here — but the
  *hint* is the install step, and it is what an operator reads at the moment they
  have the two files in hand. It now names the credential directory, the `sudo
  install -o root -g root -m 0600` that `scp` cannot substitute for, macOS's own
  answer, and §6.

The second is a `code` edit inside a `deploy`-shaped slice. It is in scope
because the alternative is a CLI that contradicts the runbook in the same commit,
and because nothing else would have caught it: the pre-build guard checks
`worker.creds`, never the git key's presence, so following the printed hint
yields a green deploy and a daemon that starts.

### The migration is manual, and the runbook owns it

`build-worker.sh` does **not** move an existing node's keys. Two reasons, both
about what a deploy script should not do to a node on its own: the move is
privileged and irreversible, and it is only half done until the copy in `$HOME`
is **deleted** — leaving it makes the new layout with the old boundary, which is
the worst of the three states. So the script refuses and prints the commands,
and [`deploy/prod/README.md`](../../deploy/prod/README.md) §6 carries the
procedure: create the directory, `install` each file into it, verify, then
remove the originals, *before* the run that converts the node.

§6 carries one more step than the file moves, and it is the one a procedure
would naturally omit: **drop the `WORKER_GIT_KEY` line that named the old path.**
§6 step 3 used to instruct exactly that declaration, so a node adopted before
this slice is likely still carrying it on the Mini, pointing at the file the
migration deletes. The refusal above is the belt to the runbook's braces —
whichever the operator reaches first, the outcome is not a node that keeps
serving jobs and silently stops updating.

### A mixed fleet still works, and the reason is that nothing else changed

`worker-refresh.sh` is untouched — its swap still recovers `KEYS_SRC` from
`docker inspect` and re-creates a container, so a node that has **not** been
converted refreshes itself exactly as it did before this slice.
[Slice 6](#slices) owns collapsing that, and the 4→5→6 ordering is why it is not
collapsed here. The drift guard's `docker inspect` fallback is likewise
untouched, so the one run that converts a node still compares against the
container it is replacing and reports the two paths that move.

### Verification

`deploy/prod/build-worker.test.sh` — 56 cases, all passing. Nine are this
slice's: the default landing at `/etc/chuggernaut/keys` for both the credential
and the git key and nowhere under `$HOME`; wrong owner and two wrong modes
refusing with what was found and the remedy; an absent directory refusing with
the *create* rather than the `chown`; the cannot-look state refusing distinctly
from a missing file; `WORKER_KEYS_DIR_<node>` moving the directory **and** the
guard; a `WORKER_GIT_KEY` under the login user's home refusing with the `install`
that moves it and the declaration to drop; macOS keeping the home path, never
being asked GNU `stat`, and being told the boundary does not port; and a node
still running the container converting without its own `/data/keys` values
refusing anything. A tenth pins the *exemption* rather than a refusal — a git key
inside a vouched-for `WORKER_KEYS_DIR` that sits under a home is served — and is
green against the un-fixed script by construction, which is what a
regression pin is for.

**Each of the nine was run against the un-fixed script and observed red** —
six of them because it proceeded to a daemon restart (`rc=0`) on a node whose
credential directory nothing had checked. Not run, and it cannot be from here:
the composed script has still never executed on a node, so `stat -c`, `sudo -n`
and `install -d` are all faked. The first real execution is an operator
converting a node they can reach.

---

## Correction, 2026-08-06 — slice 6 as landed (job #473)

Appended by job #473, which implemented [slice 6](#slices). Nothing above is
edited except that slice's State cell, the `Status:` line and the swap row of
[what is true today](#what-is-true-today). This section records what the swap
became, what each deleted carry-forward was for, what an **un-converted** node
does now, and five things the slice line does not mention.

### The swap, as built

`deploy/prod/worker-refresh.sh`'s `swap` phase is now, in order: report the run
spec, refuse if anything makes a restart unsafe, extract the three artifacts
from `chuggernaut/worker:$TAG` (`docker create` + three `docker cp`, the same
extraction [slice 4](#correction-2026-08-06--slice-4-as-landed-job-469) runs
over ssh), install each, ask the supervisor to restart. 341 lines out, 234 in.

The **build** phase is untouched — the node still runs container tasks, so it
still builds all three images, verifies the label, retag-swaps and prunes.

### What was deleted, and what each one was for

| Deleted | What it was for | Why the native path does not need it |
| --- | --- | --- |
| the detached `docker:cli` swapper (`docker run -d --name chug-worker-swap`) | a container cannot `docker rm -f` itself mid-swap, so the replacement had to be composed by a *third* process with its own lifecycle | a supervisor restarting its own unit is one act by a process that is not being replaced — #372 §8's R1 dissolves, and `--no-block` / `kickstart -k` is the whole of it |
| `KEYS_SRC` / `SOCK_SRC`, recovered by `docker inspect` | the replacement needed the **literal host** bind sources, because re-deriving `$HOME` inside a swapper running as root bound an empty directory and stranded the daemon without NATS creds | a native daemon opens `/var/run/docker.sock` and reads `/etc/chuggernaut/keys/worker.creds` off the node it is running on ([D5](#decisions)); there is no bind to get wrong |
| the KVM `--device` carry-forward and its refusal | a device is a `docker run` flag, so it could not ride the environment; dropping it while keeping `WORKER_KVM` took the **node down** (the replacement refuses to boot, `--restart=always` loops it) | the daemon's own view *is* the node's, so `/dev/kvm` is there iff the node has one. The hazard the refusal guarded cannot arise from a refresh: nothing in the swap changes what the daemon will see |
| the five nix mounts, carried by destination with their read-only bits | same shape one mount over, and #373's node-down hazard: no roots dir, client or socket in the replacement's own view and it refuses to start | same answer — the node's `/nix` is the daemon's `/nix`. The old refusal was checking that a **copy** was faithful; with no copy, the daemon that restarts reads the same filesystem the live one already booted against |
| the `*_ARGS` environment carry-forwards — cache dir, the two disk knobs, `WORKER_SLOTS`, `WORKER_MODES`, the KVM leaves, the nix settings — plus the `-e` flags composed straight onto the `docker run` line (`WORKER_NODE`, `NATS_URL`, `NATS_CREDS`, `WORKER_REFRESH_GIT_URL`, `WORKER_GIT_KEY`, `RUST_LOG`) | inheritance was the only way a value survived a container recreate, and #265 reason 3 found four of them living **only** inside the container | every one of them is a `spec_line` in `build-worker.sh`'s environment file, which the unit's `EnvironmentFile=` (or the agent's `. $ENV_FILE`) loads on every start. A value survives because it is written down — the #55/#82 class deleted at its root ([D6](#decisions), [D7](#decisions)) |
| the retained `chug-worker-swap` transcript (#270) | "the daemon that reports to the dispatcher is the very thing being replaced", so the only record of a failed replacement had to live on the node | the supervisor's own log is that record: `journalctl -u chug-worker`, or the agent's `StandardOutPath`. One fewer container to bound, name and force-remove |

That environment set is derivable rather than tallied, from any `<base>`
predating this slice, and the derivation names its own scope — `*_ARGS`
variables (base `deploy/prod/worker-refresh.sh:424-601`) and the flags written
directly on the `docker run` line (base 673-676) alike:

```sh
git show <base>:deploy/prod/worker-refresh.sh \
  | grep -oE '\-e (WORKER_[A-Z_]+|RUST_LOG|NATS_[A-Z_]+)' | sort -u
```

**No carry-forward turned out to have a second purpose**, and that was checked
rather than assumed: every `WORKER_*` and `NATS_*` value the swap composed is
written by a `spec_line` call in `build-worker.sh` (`WORKER_SLOTS_MAX` is the
one knob no script forwards, and it was never in the swap either).

### An un-converted node refuses, and the cost is named

The fleet is mixed until an operator converts each node, so the question is not
rhetorical: **a node still running the `chug-worker` container refuses its own
swap**, loudly, with the live daemon serving, the job containers untouched and
the freshly built images already retag-swapped. The refusal names the conversion
(`WORKER_SSH=<user>@<node> deploy/prod/build-worker.sh`). It is decided by
docker's own `/.dockerenv`, which is overridable (`WORKER_SWAP_CONTAINER_MARKER`)
because the shell test runs inside a container itself.

The price is stated plainly, because it is real: **prod's two nodes only ever
self-refresh** — `WORKER_SSH` is unset for both, so `build-worker.sh` no-ops on
every deploy — so from this commit until an operator converts them, their
`worker-refresh:{node}` legs **fail** and the deploy fails with them. That is
the loud half of the trade the alternative loses: keeping the container swap
beside the native one would leave the node updating itself under a design nobody
is maintaining, which is the "stranded between two designs" failure #440 warns
about. Converting a node is now the act that puts it *back* on the self-refresh
path, and `build-worker.sh`'s closing NOTE says so at the moment it happens.

### Five things the slice line does not mention

- **Install is `install` + `mv`, never a write in place.** Writing over
  `$DAEMON_BIN` while the daemon is executing it is `ETXTBSY`, and writing over
  `worker-refresh.sh` truncates the file the running shell is reading by byte
  offset — it would be fed the tail of a different script. Both are avoided by
  installing to `<path>.chug-new` beside the target and renaming over it: a
  rename swaps the directory entry and leaves the open inode alone. The design
  did not anticipate this; it is the sharpest edge in the slice.
- **Two refusals the design did not name, both about not making things worse.**
  A supervisor that is not reachable (`command -v systemctl` / `launchctl`)
  refuses before anything is installed, because a node with a new binary on disk
  and an old daemon running says nothing about itself. And a unit that is **not
  active** refuses, because restarting a unit this process does not belong to
  would leave *two* daemons on one machine — the fleet-record split
  [§1](#1-one-daemon-or-two) prices and #372 §8's R2. An extraction that yields
  an empty file refuses on the same principle.
- **Four knobs, all with correct defaults.** `WORKER_DAEMON_BIN`,
  `WORKER_UNIT`, `WORKER_AGENT_LABEL` and `WORKER_SWAP_CONTAINER_MARKER` default
  to exactly what `build-worker.sh` installs; `WORKER_CHANNEL_BINARY` and
  `WORKER_REFRESH_SCRIPT` are the *existing* `crates/worker/src/config.rs`
  defaults, read here rather than re-hardcoded, so a node that moved them
  installs where it reads from.
- **The install escalates, and refuses first if it cannot.** `/usr/local` is
  root's on both platforms, and on macOS the daemon is a GUI-domain agent
  running as the *login user* — so "the daemon cannot write where it must
  install" is a real node. Each of the three writes is unprivileged-first with
  `sudo -n` as the fallback, exactly `build-worker.sh`'s `chug_dir`/`chug_put`,
  and a pre-flight in the validate-first block asks that same question of the
  nearest existing ancestor of each target *before* the extraction. Without it
  the operator got a bare `EACCES` from half-way through an install, on a node
  whose images the build phase had already retag-swapped.
- **The staging directory is removed before the restart, not by the `EXIT`
  trap.** `systemctl restart` kills the cgroup this shell is in, and POSIX `sh`
  runs no `EXIT` trap when it is killed by a signal — the same fact the build
  phase's `TERM` handler exists for. Left to the trap, every *successful*
  refresh stranded tens of MB of extracted binaries on a node whose docker-disk
  headroom is what half this script defends. The trap stays as the failure-path
  backstop.

### Slice 3's gate and §3.1's four cases

**Slice 3's refusal is untouched**, and it had to be: it lives in
`crates/worker/src/daemon.rs`, not in this script. `run_refresh` still calls
`host_work_check` after `quiesce` and beside the `drained` wait, still fails the
refresh at its `drain` stage, and still does so *before* `begin_swap` — so the
script this slice rewrote is never invoked on a node with live host work. This
job changed no Rust.

[`docs/spec.md`](../spec.md) §3.1's four cases all still hold, and one of them
improves:

| Case | Under the new swap |
| --- | --- |
| container tasks, any daemon restart | holds, and more simply: nothing removes a container at all now |
| host tasks, a unit restart ([D3](#decisions)) | holds, and is **true for the first time**. The old swap's `docker rm -f chug-worker` killed a host task, because on a containerized node the task is a process *inside* the daemon. `systemctl restart` kills the unit's cgroup, and the task's transient scope is not in it |
| host tasks, the self-refresh | holds — refused, by the check above, unchanged |
| a reboot | unchanged, and still not covered |

§3.1 step 3 is rewritten to describe install-and-restart, since it described the
detached sibling in normative prose.

### Verification

`deploy/prod/worker-refresh.test.sh` — 29 cases, all passing; thirteen are this
slice's, replacing the sixteen that asserted the carry-forwards. They cover: the
extract-install-restart sequence; **no** `docker run`, no `chug-worker-swap` and
no `docker rm -f chug-worker` anywhere; nothing carried forward and the live
daemon never inspected, with every deleted knob set at once; install-by-rename;
the un-converted refusal installing nothing and reaching no docker mutation; the
inactive-unit refusal; an empty extraction; the launchd path;
a node that cannot reach its supervisor; an install path this node cannot write,
refused before any extraction; an install the daemon's own user cannot do,
escalated to `sudo -n`; the staging dir reclaimed before a restart that kills
the caller outright; and the three phase markers.
`build-worker.test.sh` — 56 cases, all passing.

The count is the suite's own `ok:` lines, as `build-worker.test.sh`'s 56 is.
This section first said 24, which was wrong when written — the file held 26 —
and is corrected here rather than merely bumped by the three cases the rework
added.

**Every new case was run against the un-fixed script and observed red**, except
the "no detached process" one: the old script refuses earlier, on a fixture that
no longer answers its `docker inspect` for `/data/keys`, so it passes there for
the wrong reason. That case was verified instead against a **mutant** of the new
script that re-adds a `docker run -d --name chug-worker-swap` line, which it
catches. **Nothing was run against a node**: no refresh, no restart, no
`chuggernaut.env` edit. The first real execution of this path is a deploy leg on
a node an operator has converted, and no node has been.

---

## Correction, 2026-08-07 — slice 7 as landed (job #475)

Appended by job #475, which implemented [slice 7](#slices) and closed the
design. Nothing above is edited except that slice's State cell, the `Status:`
line and the head's `XDG_RUNTIME_DIR` qualifier. **No node was touched, and no
node has ever run the unit this slice declares.**

### The honest limit, first, because a green job does not carry it

**Nothing in this repo's CI evaluates `nix/chug-node/`.** [#372](./372-chug-node-modules.md)
§2.3 says so, the [test-placement note](#slices) above says slice 7 does not
change that, and it did not: there is no nix stage in `.chug/tasks/ci.sh`, no
agent image carries `nix`, and a `nix/`-only diff runs neither the cargo stage
nor the web one. So the nix half of this slice is **unverified by construction**
— not "verified by tests that happened to pass", and not "unverified because
nobody wrote a test". The only thing that has ever evaluated the module is a
consuming host repo's `nixos-rebuild build`, and no such repo has been pointed
at this branch. That is exactly [#415](./415-knowledge-architecture.md) M7's
class — a gate that did not run reading as a gate that passed — so it is stated
in the module's own header, in the adoption runbook, in
[`docs/reference/crates.md`](../reference/crates.md), and in the commit message.

What CI *does* run is `nix/chug-node/chug-worker-unit.test.sh`, and its reach is
narrow on purpose: it is **text over text**. It proves the unit template and
`deploy/prod/build-worker.sh` render the same unit and that the module's option
defaults are that script's defaults. It cannot tell you whether the nix
evaluates, whether `systemd.units."chug-worker.service"` is spelled the way
nixpkgs expects, or whether the substituted text loads on a machine.

The macOS half is different in kind and the difference is worth naming. It can
be reasoned about against `deploy/prod/install-launchd.sh`, which exists and
works on the Mini today:

| Checked against `install-launchd.sh` | Not checked, and why |
| --- | --- |
| the domain (`gui/$(id -u)`), `bootout`-then-`bootstrap`, `plutil -lint` before loading, `@…@` placeholders substituted by `sed`, the plist landing in `$HOME/Library/LaunchAgents` | whether `launchctl bootstrap` accepts this plist on a real mac — `launchctl` and `plutil` are stubbed in the suite, and no mac has run the installer |
| the agent's shape, byte-for-byte against the plist `build-worker.sh` renders (the suite diffs them) | whether the daemon it launches comes up: that needs a node with a run spec, a credential and a binary, and there is none |

### One shape, and it is one file

The brief's preference was a shared template over two renderings, and that is
what landed: `nix/chug-node/chug-worker.service.in` is the unit, with `@NODE@`,
`@ENV_FILE@`, `@PATH@` and `@BINARY@` substituted by `builtins.replaceStrings`
in `nixos.nix`. `systemd.units."chug-worker.service".text` rather than
`systemd.services.chug-worker` for exactly that reason — the text *is* the
artifact, so there is no attribute-set-to-unit-file translation for a reader to
audit.

**`build-worker.sh` was not touched**, which is why the sharing is one-directional:
the script keeps its own heredoc, and slices 4 and 6 stay byte-identical and
verified. What keeps the two in step is mechanical rather than editorial —
`chug-worker-unit.test.sh` extracts the unit out of the script, renders the
template with the script's own variable names left unexpanded, and **diffs
them**; then reads the script's `WORKER_ENV_FILE`, `WORKER_PATH` and `BIN_DIR`
defaults and compares each against the matching `chug.node.daemon.*` default. A
divergence in shape or in a default fails a normal Chuggernaut job. Each of the
three was checked against a mutated tree before it was trusted.

Two things it deliberately does not equalize:

- **The `Description=` parenthetical.** The script substitutes the *fleet node
  name*; nix substitutes `config.networking.hostName`. The fleet name is run
  spec — it is `WORKER_NODE` in the environment file — and putting it in a nix
  option would be #372 §8's R3 reintroduced for a human-readable string. The
  diff compares the script's `$NODE` against the template's `@NODE@`, so the
  shape is pinned and the value is allowed to differ.
- **`wantedBy`.** The unit text carries `[Install] WantedBy=multi-user.target`
  because the script's does, but NixOS makes its own enablement symlinks and
  never runs `systemctl enable` over `/etc/systemd/system` — so the module sets
  `wantedBy` as an attribute too, and the suite asserts it. Without that the node
  would hold a unit that exists and never starts at boot.

### The charter, amended against #372 §8's four reasons

In `nix/chug-node/options.nix`, where an operator editing the module meets it.
R1 and R2 dissolve (there is no `docker rm -f` for a supervisor to read as a
crash, and `--restart=always` is gone); R4 dissolves for its own reason (a unit
over a binary has no tag to be missing, so §8's registry precondition is never
triggered, and this design still does not propose that move); **R3 survives and
is answered by the split** — nix owns the lifecycle, the platform's environment
file owns the run spec, and no `WORKER_*` value is a nix option.

The split's failure mode is named rather than assumed: if the module's
`environmentFile` and the deploy's `WORKER_ENV_FILE_<node>` name different
files, the unit **fails to start** saying which file it could not load. The
module also warns at build time whenever `environmentFile` is not the deploy's
default. That is the answer to the [risk list](#risks-and-open-questions)'s
"splits drift" — the drift is loud on both sides.

**The option surface is three knobs and an `enable`, not the four
[§2](#linux-nixos) sketched.** `chug.node.daemon.{enable,binary,environmentFile,path}`
landed; "whether the node serves host mode at all" did **not**, and refusing it
is the same argument as R3. `WORKER_MODES` is run spec: it is read from the
environment file by `crates/worker/src/config.rs`, the daemon refuses to
advertise `host` when the node cannot create a scope, and a second declaration
in nix could only be a copy that a reboot resurrects. #309 P1's
`runtime.mode: host` refusal is untouched, and nothing here enables `host`
anywhere.

### The provisioning question, answered by construction

The head has carried an open question since [job #451](#correction-2026-08-06--the-scope-an-unprivileged-daemon-can-create-job-451):
every Linux proof needed `XDG_RUNTIME_DIR` in the invoking environment, and
whether a daemon under a supervisor gets one was slice 7's. The answer is that
it does not need one. `scope_manager()` picks `System` for euid 0 and
`borrowed_bus` returns an empty map for a system scope — the system bus is a
fixed socket path — so a `User=root` unit borrows nothing. `loginctl
enable-linger` and `XDG_RUNTIME_DIR=/run/user/$UID` are an **unprivileged**
daemon's provisioning, and this module declares no way to run one. Read off
`crates/container/src/host.rs`; **not measured**, because measuring it needs a
node running the unit.

### The macOS half, and the hazard it refuses to create

`deploy/prod/launchd-worker/com.chuggernaut.worker.plist.template` plus
`deploy/prod/install-worker-launchd.sh`. Job #467 recorded the hazard —
`install-launchd.sh` globs `deploy/prod/launchd/*.plist.template` and installs
what it finds, so a worker template dropped there arrives on the **Mini**, whose
own colima node sits at 0 slots precisely so heavy builds cannot starve the
control plane. Three independent locks, each asserted by
`deploy/prod/install-worker-launchd.test.sh`:

1. the template is in a **different directory**, which that glob cannot reach;
2. **nothing calls the installer** — the suite greps every tracked `*.sh`,
   `*.yaml` and hook for its name and fails if anything does;
3. the installer **refuses a mac that runs the dispatcher or api agent**,
   checking both the plist on disk and `launchctl print`, and names
   `CHUG_WORKER_ON_CONTROL_PLANE=1` as the deliberate override.

It installs a lifecycle and never a run spec, symmetrically with the nix half: a
missing or unreadable environment file is a refusal here rather than a
boot-loop under `KeepAlive` on the node, and so is a missing daemon binary.
`uninstall` boots the agent out and removes the plist, leaving the environment
file, the binary and the keys alone.

It also removes a containerized `chug-worker` before bootstrapping the agent —
announced rather than silent, because the agent claims the same `WORKER_NODE`
and two daemons on one node name is the state `worker-refresh.sh` refuses its
swap over (§1). That is what `build-worker.sh` does at its own bootstrap. The
existence question is `docker inspect`'s, **not** `docker rm -f`'s exit status:
under `--force` the CLI reports a missing container and still exits 0, so a
status-driven removal would announce one on every node that never had one. And a
docker that cannot be asked at all — absent, or its daemon down — is a
**refusal** before anything is written rather than a shrug, because a stopped
`--restart=always` container is invisible to a check that cannot run and comes
back the moment dockerd does; `CHUG_WORKER_SKIP_DOCKER_CHECK=1` is how an
operator asserts this mac never ran one. `build-worker.sh` can afford `|| true`
at its own removal because it has already driven docker on that node in the same
run; nothing in the installer has.

**Not** a nix-darwin declaration, and `darwin.nix` now *asserts*
`chug.node.daemon.enable` is false with a message pointing at the installer.
Two reasons: the option declares a systemd unit, which darwin has none of; and
the plist's home in the operator's `macos-runner` configuration remains
**secondhand** — no such configuration is checked out here, so the module
declares no agent rather than a plausible one. A mac's own configuration may
declare `launchd.user.agents` from the same template, and that claim is marked
secondhand wherever it is made.

### The seam this slice documents rather than closes

On NixOS `/etc/systemd/system` is a read-only symlink into the store, so
`build-worker.sh` — which installs the binary, the environment file *and its own
copy of the unit* — refuses such a node before it builds anything. Declaring the
unit in nix does not by itself make that script succeed, because the script
still wants a writable unit directory; **it was deliberately not changed**
(slices 4 and 6 are verified and the brief scopes them out). The documented
sequence is in
[the adoption runbook](../reference/runbooks/chug-node-adoption.md) §4a: declare
and switch first, then run the deploy with
`WORKER_UNIT_DIR_<node>=/run/systemd/system`, whose copy systemd outranks with
the configuration's `/etc` unit and which is discarded at the next boot. After
that the node is on the self-refresh path and no unit is written again — the
swap installs a binary and asks the supervisor to restart (D6). The narrower
fix, teaching `build-worker.sh` a "the node declares its own unit" mode, is a
follow-up and is named here so it is not rediscovered.

### Verification — stated, and what was not run

- `sh nix/chug-node/chug-worker-unit.test.sh` — 5 cases, passing. Each of the
  three drift checks was also run against a mutated tree (a changed
  `RestartSec`, a renamed placeholder, a changed option default) and fails
  naming the divergence.
- `sh deploy/prod/install-worker-launchd.test.sh` — 6 cases, passing, driving
  the real installer against stubbed `uname`, `launchctl`, `plutil` and `docker`
  in a throwaway `$HOME`; the docker stub carries real docker's shape, where
  `rm --force` exits 0 whether or not the container was there, and the
  docker-less case runs against a `PATH` built of symlinks with no docker on it.
  Also checked against mutants: a template moved into the globbed directory, a
  caller added to another deploy script, `docker rm -f`'s status used as an
  existence oracle, and the `docker info` gate deleted — all four are caught.
- **Not run, and it cannot be from here:** any nix evaluation, any `launchctl`
  on a real mac, any node. `nix` is not on this image and neither is macOS. The
  first real execution of either half is an operator on a node they can reach,
  which the [risk list](#risks-and-open-questions) already says should not be a
  prod node.

---

## Correction, 2026-08-07 — D6 holds on Linux only, and the endpoint was never rendered (job #476)

The first real conversion of a node happened on 2026-08-06, against
`gumbo-air-0`, and it produced two failures this design did not predict. Both
are the same mistake in two places, and the mistake is
[#309](./309-host-native-execution.md) P0 finding 6 for the third time:
**macOS was reasoned about for supervision and inherited from the container for
everything else.** [D2](#decisions) was designed for that platform and its proof
passed. The binary's *provenance* and the engine's *address* were not
re-derived at all — they were carried over from a daemon that was a Linux
container holding a bind-mounted socket, where both were true by construction.

### What happened, in order

`WORKER_SSH=worksalot@dev-air.tail20c474.ts.net CHUG_WORKER_NODE=air
deploy/prod/build-worker.sh`, on an idle fleet:

1. **It refused, correctly.** `WORKER_GIT_KEY=/data/keys/worker_git` names the
   container's mount, and [slice 5](#correction-2026-08-06--slice-5-as-landed-job-472)'s
   guard caught it. Nothing here changes that.
2. **It installed the agent, and the agent crash-looped.**
   `/usr/local/bin/chuggernaut` was an `ELF 64-bit LSB pie executable, ARM
   aarch64` — a **Linux** binary — and launchd reported `cannot execute binary
   file` on repeat. The health probe timed out at 60s and the script reported
   FAILED, with the container daemon already removed.
3. **After a native rebuild by hand, a second failure:** `backend unavailable:
   Socket not found: /var/run/docker.sock`.

The air now runs a Mach-O daemon built with its own `cargo` and a
`WORKER_DOCKER_ENDPOINT` line appended to its `worker.env` by hand. Neither step
was reproducible by any script in this tree, which is what this correction fixes.

### D6's sentence, split by platform

> the daemon binary is extracted from the worker image the build phase already
> produces … so its build environment stays byte-identical to today's and needs
> no host Rust toolchain.

That argument is sound **on Linux**, where the image's platform and the node's
platform are the same thing, and it is unsalvageable **on Darwin**, where the
image is a Linux container and the host is not. It is not a bug in the sentence;
it is a premise that silently stopped holding. The correction is therefore
narrow: D6 is a **Linux** decision, and a Darwin node needs its own answer.

**Every place that sentence is asserted is split with it**, including the one
place it is said *to an operator*: the `NOTE` `build-worker.sh` prints at the
moment a node is converted, which is where someone learns what their node will
do on the next deploy. On Darwin it now names the compile, its cargo and its
build directory; on Linux it is unchanged and still cites D6. A claim corrected
in the docs and left standing in the console is corrected for the wrong reader.

### The three ways to a Mach-O daemon, and why the node compiles

| Option | What it costs | Verdict |
| --- | --- | --- |
| **Compile on the node** | D6's "no host Rust toolchain" promise dies on that platform, the build environment becomes the node's rather than the pinned image's, and build time moves onto the node | **Chosen** |
| **Cross-compile on the builder** | keeps the promise, and needs a Darwin target, the macOS SDK and a Darwin linker on the builder — i.e. a mac. The builder is the node itself here (`build-worker.sh` builds *over ssh, on the node*), so this is the chosen option with extra steps unless a second mac appears | Refused |
| **Ship a prebuilt artifact** | keeps the promise and needs somewhere to put it — a registry or artifact store, which is [#313](./313-workload-identity-image-builds.md) gap 11 and does not exist | Refused, for now |

The deciding argument is that only the first needs nothing the platform does not
have. It is also the option the third row eventually replaces: when #313 gap 11
lands somewhere to publish a signed artifact, a mac fetching one is strictly
better than a mac compiling one, and this becomes the fallback rather than the
path.

**The cost is stated rather than hidden.** A Darwin node's daemon is built by
*that node's* cargo, not by `deploy/prod/Dockerfile.worker`'s pinned
`rust:1.96-bookworm`, so two nodes in one fleet can be built by two compilers.
Three things narrow it: the build is `--locked`, so the dependency graph is the
tree's `Cargo.lock` and not whatever resolves that day; `CHUG_GIT_SHA` is passed
exactly as the image passes it, so the fleet's version column keeps meaning the
same thing; and the node is refused outright when it has no cargo the deploy can
reach, rather than being given a binary from the wrong platform. What is *not*
claimed is byte-identity: it is gone on that platform, and no wording recovers it.

### Where the toolchain is declared, and why the run spec carries it

`build-worker.sh` asks the node for its toolchain in the **same round trip**
that asks `uname -s` and `$HOME` — before anything is built, so a mac that
cannot compile refuses in seconds with the live daemon untouched, naming
`WORKER_CARGO_<node>` as the declaration to add. The ssh shell is not the
interactive one an operator sees, and a nix-darwin or rustup cargo is routinely
absent from it; that is why the override exists and why the refusal quotes the
exact question it asked.

**The toolchain is a directory, not a binary, and `command -v cargo` is not the
question the compile asks.** Cargo resolves `rustc` through `PATH` — `RUSTC`,
then `build.rustc`, then a plain lookup — and an absolute `WORKER_CARGO` is
declared *precisely because* the bare name is not on the `PATH` in question. So
a guard that asks only `command -v "$WORKER_CARGO"` passes on the exact node it
was written for and the compile then dies half-way, after three image builds.
Three answers come back instead of one, and each is its own named refusal:
where `cargo` is, whether `rustc` resolves **with cargo's own directory
prepended**, and whether that cargo runs at all (a rustup shim with no default
toolchain is on `PATH`, execs, and compiles nothing). Everything that compiles
then runs with that directory prepended — the remote build, and
`worker-refresh.sh`'s own.

`WORKER_CARGO` and `WORKER_BUILD_DIR` ride **in the run spec**, and the
toolchain's **directory** rides in the launchd agent's `PATH`. That pair is what
keeps the node self-refreshing: `worker-refresh.sh` runs as the daemon, whose
`PATH` is the agent's, so the declaration alone buys nothing once cargo goes
looking for its compiler. With both, a converted mac's refresh compiles in its
build phase (where minutes are affordable, and before the retag-swap) and its
swap installs what it compiled. A Linux node gets neither line, no `PATH`
change, and keeps extracting from the image.

`build-worker.sh`'s own compile also writes `native.sha` into the staging
directory, as `worker-refresh.sh`'s does. Two writers share that directory and
the swap refuses when the marker disagrees with the image's `chug.git.sha`
label; leaving it stale after a conversion would make that second opinion true
only by call ordering rather than by construction.

### `WORKER_DOCKER_ENDPOINT` — a setting nothing had ever rendered

`crates/worker/src/config.rs` has read `WORKER_DOCKER_ENDPOINT` since long
before this design, defaulting to `unix:///var/run/docker.sock`. **Nothing in
this repo ever wrote it into a run spec**, and nothing needed to: the daemon was
a container with that exact socket bind-mounted in, so the default was true by
construction. This design's own equivalence table says so in the row that reads
*"nothing — the daemon opens `unix:///var/run/docker.sock` on the node, which is
`WORKER_DOCKER_ENDPOINT`'s default already"* — and that row is right about Linux
and wrong about a mac, where colima listens at `~/.colima/default/docker.sock`.

It is now rendered on `WORKER_MODES`'s terms: forwarded, per-node overridable,
**unset stays unset** so a Linux node's environment file is byte-identical to
what it was. Two guards, because the two failures are different:

- **An unsupported scheme is refused** — `DockerBackend::new` rejects anything
  that is not `unix://` or `tcp://` / `http://`
  (`crates/container/src/docker.rs`), which is a start-time error the supervisor
  would loop.
- **A `unix://` socket that is not there is refused** — that one is *not* a
  daemon that fails to boot. It is a node that comes up, announces its slots,
  and fails every launch with `backend unavailable: Socket not found`. The air
  spent a window in exactly that state.

### Deriving it, and what a changed context does

On a Darwin node with nothing declared, the value is **derived** from
`docker context inspect --format '{{.Endpoints.docker.Host}}'` on the node —
the same engine that just built the node's images, asked of the same CLI. A mac
therefore needs no declaration at all in the ordinary case, and the derived
value is announced on the deploy leg rather than applied quietly.

**A derived value is a snapshot, and this is what happens when the context
changes under a running node:** nothing, until the daemon next starts — at which
point it reads the environment file it was given and dials the socket that was
true at conversion time. A node whose colima was replaced by Docker Desktop (or
whose colima profile was renamed) keeps announcing itself healthy and fails
every launch, exactly as the air did. That is the same durability every other
line in that file has, and the same remedy: re-run `build-worker.sh`, or edit
the file and kickstart the agent. It is not made self-healing on purpose — a
daemon that re-derived its engine at each start would silently follow an
operator's `docker context use` onto a different machine's engine, which is a
larger surprise than a loud failure. A **derived** value equal to the daemon's
own default is dropped rather than written, so deriving cannot give a node a
line it never chose.

### The generalisation, because there will be a fourth instance

The class is: **a fact that was true by construction inside the container, still
being assumed after the daemon left it.** Both failures here are that, and the
container was doing the work in both cases — it made the binary's platform match
the node's, and it made the socket path correct. Slice 4 audited the `docker
run` flags one by one (the equivalence table above) and that audit was *good*;
what it could not see was the two premises that were never flags.

The check that generalises is in both scripts now, on both platforms: **the
staged binary must run on this node before it is installed** (`chuggernaut
--version`, before the first `install`). It is not a mac special case. The next
instance is predicted, unmeasured, and follows from the same reasoning: the
worker image is `debian:bookworm-slim`, and **a NixOS node has no
`/lib64/ld-linux-x86-64.so.2`** — so D6's extracted binary may well fail to exec
on `gumbo-nuc-0` too, which is the node slice 7 was written for. Nobody has
tried; the point is that when it happens it is now a named refusal with the live
daemon untouched, rather than a crash loop and a 60s health-probe timeout.

### Restore-ability: there is no way back, and that is the trade

[Slice 6](#correction-2026-08-06--slice-6-as-landed-job-473) deleted the
`docker run` path, so when the air's native daemon failed there was no scripted
way back to a container daemon and the node was down for the window. That is a
deliberate consequence of [D1](#decisions) — one daemon per node, and a second
supported shape is a second thing to keep working — and this correction does not
reverse it. What it does is make the window much harder to enter: every failure
above is now a refusal *before* the live daemon is touched, and the one
remaining unguarded window is between the supervisor bootout and the health
probe.

It is acceptable **only because an operator is told before they convert**, which
is the half that was missing. `deploy/prod/README.md` §6 and
[the adoption runbook](../reference/runbooks/chug-node-adoption.md) §4b now say
it in the place a conversion is read from: converting a node is one-way, a
failed conversion strands it, and the way back is forward — fix what the
refusal names and re-run. Drain the node first (`worker-capacity.md` §4.1) and
convert one you can afford to lose for the length of a build.

### The air must be re-converted BEFORE the next prod deploy, not at leisure

This is a precondition, not a preference, and nothing in this change can enforce
it. **The daemon executes the `worker-refresh.sh` installed on the node** —
`resolve_refresh_script` in `crates/worker/src/config.rs` resolves
`/usr/local/lib/chuggernaut/worker-refresh.sh`, the copy the 2026-08-06
conversion wrote — not this tree's. That copy **predates this correction**: its
build phase has no Darwin branch, and its swap is the unconditional `docker
create` + `docker cp` out of `chuggernaut/worker:$TAG`.

So the first prod deploy after this merges asks the air to self-refresh, its
build phase succeeds happily on colima, and its swap renames the **ELF** binary
over the operator's Mach-O one and kickstarts launchd — undoing the hand fix and
taking the node out of the fleet, which is the 2026-08-06 outcome again. A
re-conversion is what replaces that installed script; until it happens the air
is one deploy away from the failure this correction describes.

The remedy is the operator verification below, run **first**. Draining the node
and booting its agent out is the alternative if a conversion cannot be scheduled
before the next deploy. `deploy/prod/README.md` §6 and
[the adoption runbook](../reference/runbooks/chug-node-adoption.md) §4b carry
the same warning beside their drain-first instruction.

### What this does not do

- **Nothing was applied to any node.** The air is untouched by this change: it
  is healthy and serving on the operator's hand-built daemon. It is *not*
  indefinitely safe, for the reason directly above — the deadline is the next
  prod deploy, not the operator's leisure. The nuc is untouched and unconverted.
- **No `WORKER_MODES` change and no #309 P1.** `runtime.mode: host` is still
  refused.
- **The git-key guard is unchanged.** It did its job.
- **`--locked` is a new way for a Darwin build to fail** that a Linux node does
  not have: a `Cargo.lock` that no longer resolves the manifests fails the
  compile rather than silently resolving a different graph. That is the intended
  trade, and it fails loudly before anything is installed.

### Verification — stated, and what was not run

- `sh deploy/prod/build-worker.test.sh` — 66 cases, passing, including the nine
  added here: a Darwin node compiles rather than extracts; a mac with no
  reachable cargo refuses before any image is built and names
  `WORKER_CARGO_<node>`; a mac with cargo but **no `rustc`** on the compile's
  own `PATH`, and one whose cargo does not exec, each refuse there too; the
  toolchain **directory** reaches both the remote compile and the launchd
  agent's `PATH` (and is not prepended twice when it is already there), and the
  conversion leaves `native.sha` describing what it built; the endpoint is
  derived into the run spec and a Linux node's file is unchanged; a declared
  endpoint rides per node; a derived value equal to the default is dropped; an
  unparseable scheme and an absent socket each refuse with the live daemon
  untouched; the staged binary must exec before it is installed; and the
  conversion `NOTE` names the binary's provenance **per platform**. The macOS
  run spec is asserted as a **whole-file golden**, beside the Linux one slice 4
  added.
- `sh deploy/prod/worker-refresh.test.sh` — 35 cases, passing, including: a
  Darwin build phase compiles before the retag-swap, under a `PATH` carrying the
  toolchain directory; a node whose cargo cannot find `rustc`, and one whose
  cargo does not run, each refuse **before any docker mutation** rather than
  mid-compile; a failed compile leaves the live images alone; a converted mac
  with no cargo refuses the build by name; a Linux node is never asked for one;
  the macOS swap installs the node's own build and not the image's; a staging
  directory from another SHA refuses; and a staged binary that cannot exec
  refuses on either platform.
- `sh deploy/prod/install-worker-launchd.test.sh` — 7 cases, passing, with one
  extended and one added: the opt-in macOS installer runs `--version` on the
  daemon it is about to supervise, so a binary that is present, `0755` and
  unable to exec — precisely what the air held — is refused instead of
  bootstrapped into a `KeepAlive` loop; and it reads `WORKER_CARGO` out of the
  run spec it is handed so the agent it writes carries the same toolchain
  directory `build-worker.sh`'s does. Its "no executable daemon" message no
  longer says the binary is extracted from the image, which on the only platform
  that installer runs on is false.
- `sh nix/chug-node/chug-worker-unit.test.sh`, `sh
  deploy/prod/update-refresh.test.sh` — unchanged and passing.
- **Not run, and it cannot be from here:** any of it on a mac. No node was
  converted, no `cargo` ran on Darwin, no launchd agent was bootstrapped, and no
  `docker context inspect` answered a real question. The first real execution is
  the operator re-converting the air with
  `WORKER_SSH=worksalot@dev-air.tail20c474.ts.net CHUG_WORKER_NODE=air
  deploy/prod/build-worker.sh` from the fetched `chuggernaut.env`, on an idle
  fleet — which is the same command that produced the measurement above. **Run
  it before the next prod deploy**, for the reason in "The air must be
  re-converted" above: the air still holds a pre-correction
  `worker-refresh.sh`, and the next deploy that asks it to refresh installs the
  Linux binary over the working daemon. Expect it to derive
  `WORKER_DOCKER_ENDPOINT` from the node's own context and announce the value,
  to compile with the node's cargo (declared as `WORKER_CARGO_air`, whose
  directory then leads the agent's `PATH`), and to end at `worker up node=air
  slots=2`.

## Correction, 2026-08-08 — the correction above generalised over two binaries with opposite platforms (job #480)

The 2026-08-07 correction narrowed D6 from "the binary comes out of the image"
to "the binary comes out of the image **on Linux**". It got the scope right for
the *daemon* and then applied the same verdict to everything `build-worker.sh`
stages, in one sentence:

> On DARWIN all three come out of the tree compiled above, for the reason stated
> where that build is: an image built for Linux holds a binary a mac cannot exec.

Three artifacts, one verdict. But `chuggernaut-channel` is not the daemon's
platform problem in miniature — it is its **inverse**. The daemon runs **on** the
node. The channel binary never does: the worker reads it at startup and injects
it into every agent **container** (`crates/worker/src/daemon.rs`,
`FileSource::LocalArtifact`), which is Linux on both platforms. So on the air a
`Mach-O 64-bit executable arm64` was installed at
`/usr/local/lib/chuggernaut/chuggernaut-channel` and shipped into `linux/arm64`
containers. Right architecture, wrong OS.

### The failure had no symptom of its own

Claude Code launches that binary as the `chuggernaut-channel` MCP server, the
exec fails, and the server stays `status: "pending"` forever — so
`mcp__chuggernaut-channel__update_status` and `mcp__chuggernaut-channel__submit_eval`
simply **do not exist** in the agent's toolset. Nothing errors. Observed in
jobs #477 and #478: every task placed on the air reported
`"mcp_servers":[{"name":"chuggernaut-channel","status":"pending"}]` while #478's
tasks on the nuc reported `"connected"`. The evaluators behaved correctly —
searched for the tool by name and by keyword, got `"matches":[]`, and wrote the
verdict in prose. The platform recorded four "exited without producing any
output" failures and escalated both jobs. **Work tasks limp** (they lose
`update_status`; the diff still lands); **agent evaluators fail outright**, every
time, on that node. The air was drained to 0 slots as mitigation.

### The rule that was missing

The platform question an artifact answers is not *which machine staged it* but
**which kernel execs it**:

| Artifact | Executed by | Darwin source | Linux source |
| --- | --- | --- | --- |
| `chuggernaut` | the node, under its supervisor | the native tree build | the worker image |
| `chuggernaut-channel` | every agent **container** | **the worker image** | the worker image |
| `worker-refresh.sh` | `/bin/sh`, either | the source file | the worker image |

The right bytes were already on the node. `build-worker.sh` builds
`chuggernaut/worker:$TAG` unconditionally on both platforms using the **node's
own** Docker — colima on the air — so the image's
`/usr/local/lib/chuggernaut/chuggernaut-channel` is already
`linux/<the node's container arch>`. The Darwin branch now takes it out of that
image with the same `docker create` / `docker cp` / `docker rm` the Linux branch
uses, and keeps the daemon on the native-build path. `worker-refresh.sh`'s
launchd swap did the same thing on the self-refresh path and got the same
treatment; its Linux swap is unchanged.

### Each binary is now asked the question its own executor asks

The 2026-08-07 correction's pre-flight — *the staged binary must run on this
node* — is right for the daemon and would be exactly **backwards** for the
channel binary on Darwin, where the correct answer is that it does **not** run
here. So the guard is split, and it refuses before anything is installed:

- **Linux**, both binaries: it must exec on the node. `chuggernaut-channel` takes
  no `--version` (it is an MCP server that reads its job context out of the
  environment), so what is asked is whether the kernel would load it at all —
  exit 126/127 is the refusal.
- **Darwin**, the channel binary: it must be a Linux ELF for the architecture
  **this node's docker reports**, read as ELF magic plus the `e_machine` at file
  offset 18. The architecture is derived from
  `docker version --format '{{.Server.Arch}}/{{.Server.Os}}'` rather than assumed
  from the mac — a `linux/amd64` colima would be just as wrong and just as
  silent — and a docker that cannot answer is a refusal, not a guess.

The refusal names the node's container platform and the whole chain the operator
would otherwise have to reconstruct, because there is no other signal: this
surfaces four job escalations later as "the evaluator produced no output".

### Where it is checked

- `sh deploy/prod/build-worker.test.sh` — five cases added and one narrowed: the
  Darwin path stages the channel binary from the image and the daemon from the
  tree; the refresh script stays on the source-file path; the guard's own two
  shell functions are lifted out of the rendered install script and run here
  against ELF and Mach-O fixtures, in both architectures; the refusal names
  `arm64/linux` and precedes the first `chug_put`; an underivable container
  platform refuses before anything is installed; and the Linux staging is
  asserted unchanged, with no platform probe on that path. The case that read
  "a Darwin node must not `docker create`" now reads "must not extract its
  *daemon*" — the Darwin path does create a container, for the other artifact.
- `sh deploy/prod/worker-refresh.test.sh` — four cases added: the launchd swap
  extracts the channel binary from the image and reports its separate
  provenance; a Mach-O refuses it, naming `arm64/linux`; an arm64 ELF refuses on
  a node whose docker runs `amd64/linux`, and an unanswerable docker refuses too;
  and on Linux a channel binary that cannot exec refuses that swap, with the
  three-artifact extraction otherwise untouched.
- Every one of those cases was run against the **unfixed** scripts and fails
  there.
- **No Rust changed.** `crates/worker` is correct: it injects the bytes it is
  given.
- **Not run, and it cannot be from here:** anything on a mac, and any real
  `docker`. The claim that the image carries a `linux/arm64` channel binary rests
  on `deploy/prod/Dockerfile.worker` (one `cargo build --release --bin
  chuggernaut --bin chuggernaut-channel` in a `rust:1.96-bookworm` stage, copied
  to `/usr/local/lib/chuggernaut/chuggernaut-channel`), on `build-worker.sh`
  building that image over ssh with the node's own Docker and no `--platform`,
  and on the nuc — where the same image feeds the same injected artifact and
  #478's tasks reported `"connected"`. The new pre-flight is what turns that
  reasoning into a check on the node itself.

---

## Correction, 2026-08-07 — `WORKER_SLOTS_MAX` is forwarded now (job #477)

[Slice 6's correction](#correction-2026-08-06--slice-6-as-landed-job-473) argued
that no carry-forward the swap deleted had a second purpose, and named
`WORKER_SLOTS_MAX` as the one knob no script forwards. The argument holds — it
was never in the swap, and the swap still copies nothing forward. **The
parenthetical does not.** Job #477 renders `WORKER_SLOTS_MAX` through
`spec_line` beside `WORKER_SLOTS`, so a node's ceiling is declared in
`deploy/prod/chuggernaut.env` (bare or as `WORKER_SLOTS_MAX_<node>`) and written <!-- runtime -->
into the environment file the supervisor hands the daemon — which is what makes
it survive a deploy and a self-refresh, exactly as [D7](#decisions) intends.

Unset still renders nothing, so a node that declares no ceiling produces a
byte-identical run spec and gets the daemon's own default, its CPU count.

The knob that motivated it is [#309](./309-host-native-execution.md) P0's: a
`host` node must boot at `WORKER_SLOTS=1` **and** `WORKER_SLOTS_MAX=1`
(`enforce_host_capacity`), and until this the deploy could only check the first
and tell the operator to add the second to the node by hand. `build-worker.sh`
now checks both and refuses the deploy, live daemon untouched, naming whichever
is wrong — an unset ceiling included, since the daemon then defaults it to a CPU
count nothing in the deploy can read.

The drift guard is unchanged and still needed: the daemon reads more `WORKER_*`
than the run spec composes (`WORKER_HOST_ROOT`, `WORKER_CHANNEL_BINARY`,
`WORKER_REFRESH_SCRIPT`), and a live daemon carrying one of those is still a
refusal.

## Correction, 2026-08-07 — the docker requirements are the *container-capable* node's, not the mac's (job #487)

The two corrections above are written as "what a mac node needs": a docker
endpoint the daemon can dial, and a *running* docker to stage
`chuggernaut-channel` out of the worker image. Both are true of a mac that
serves container launches, and neither is true of a node that names no container
runtime at all — the deploy scripts asked them of every node, so a mac that only
runs host tasks could not be deployed to even though the daemon it would run
needs nothing from docker.

### The daemon already served this shape; the scripts did not

`local_backend` (`crates/worker/src/daemon.rs`) builds the host backend and
**returns** when `!serves_container`, so `docker_backend` is never reached and
`WORKER_DOCKER_ENDPOINT` is never read. `serves_container` is
`contains(Container) || !serves_host(modes)` — a node names containers if it
says `container`, or if it says nothing at all, which is every node in the fleet
today.

`deploy/prod/build-worker.sh` and `deploy/prod/worker-refresh.sh` now branch on
that same rule, derived once per script from `WORKER_MODES` in the daemon's own
spelling. A second spelling was the thing to avoid: a deploy that disagrees with
the node it is converting either refuses a legal node or hands a docker-less one
a spec it cannot serve.

### What a host-only node skips, and why each is safe

- **The docker socket check** — nothing dials it.
- **`chuggernaut/agent` and `chuggernaut/agent-rust`** — a job type resolving to
  `runtime.mode: host` cannot declare an `image:`
  (`crates/types/src/job_type.rs`), so no launch on that node carries one.
- **The container-platform probe** — its only purpose is judging the injected
  binary, and there is none.
- **`chuggernaut-channel`, which is installed nowhere on such a node.** It is
  injected by exactly one function, `Core::channel_mcp`
  (`crates/dispatcher/src/exec.rs`), with two callers: the `WorkType::Agent` arm
  and the agent evaluator (`crates/dispatcher/src/eval.rs`). The command path
  composes its launch in `Core::command_launch_config`
  (`crates/dispatcher/src/launch_queue.rs`), whose `files:` is inline ssh
  credentials and nothing else — no artifact reference. And host mode serves
  `work.type: command` alone, enforced twice: `validate_host_serves_commands_only`
  and `HostBackend::admit`, which also refuses an image-carrying launch. A
  missing channel binary is already tolerated by the daemon — it warns and
  leaves its artifact map empty, and that map is consulted only on the
  `FileSource::LocalArtifact` branch a command-only node never reaches.
  **Installing nothing is the honest default**: a binary nothing on the node can
  use is a thing a later reader has to work out.
- **The refresh disk pre-flight** — it exists to protect an image build. This is
  the immediate operational payoff: it refused deploy #486 on dev-air over
  headroom no build was going to consume.
- **The native compile's `--bin chuggernaut-channel`** — the mac's own copy was
  always discarded (the 2026-08-08 narrowing), and on this node there is no
  container to inject one into either.

### D6 is why "needs no docker" is a Darwin sentence

On Linux the worker image is the only place that node's daemon binary comes
from, so a host-only **Linux** node still builds it and still needs a docker to
build and extract with. It skips the agent images and the channel binary and
nothing else. On Darwin the daemon is compiled natively, so skipping the image
leaves nothing behind that wants a runtime — which is exactly the asymmetry the
2026-08-07 correction introduced, read forward.

### What this does not do

- **No Rust change.** The daemon was already correct.
- **No node was converted.** `WORKER_MODES` in `deploy/prod/chuggernaut.env` is <!-- runtime -->
  unchanged and no node declares `host`. This makes the conversion possible;
  performing it is an operator step.
- **No host guard was weakened.** `WORKER_SLOTS=1`/`WORKER_SLOTS_MAX=1`, the
  host-root probe and the supervision probe all still refuse with the live
  daemon untouched, and a host-only node with the wrong capacity is a case in
  the suite.
- **The staged-generation check DEGRADES on a host-only mac rather than being
  dropped.** The swap compares `native.sha` against the worker image's
  `chug.git.sha` label; with no image there is no label, and the swap phase is
  handed a tag rather than a SHA, so there is nothing else on the node to
  compare with. It now requires the staging directory to exist and to name a
  SHA, and reports which one it is installing. That is strictly weaker than the
  container path and is said out loud rather than left to be discovered.
- **The disk pre-flight's `DISK_PATH` is still wrong on a container-capable
  mac**, and is deliberately left open. Since this design made the daemon
  native, `/` is the boot volume while docker lives in a VM: dev-air measured
  7.2GB free on `/` against 76.3GB inside colima. Host-only sidesteps it by
  running no build; a dual-mode or container mac still meets it. Fixing it means
  asking docker for its own free space, i.e. running a container on every
  refresh of every node — a new failure mode on the whole fleet's hot path, for
  a guard that is deliberately fail-open — and re-deriving the 30GB floor, which
  was measured against `/` semantics on a Linux node. It wants its own change
  and its own measurement. `WORKER_REFRESH_DISK_FREE_GB_MIN_<node>=0` turns the
  guard off for one node in the meantime.

### Verification

`sh deploy/prod/build-worker.test.sh` and `sh deploy/prod/worker-refresh.test.sh`,
both green, with nine cases added between them: a host-only mac deploys and
refreshes with no docker call at all and still installs its daemon, writes its
run spec and restarts; its swap installs two artifacts rather than three, and
refuses when nothing is staged; a host-only Linux node builds only the worker
image; and a host-only node with the wrong capacity still refuses. Each of those
fails against the unfixed scripts — verified by running the new suites against
`HEAD`'s copies. The last case in each suite is the acceptance bar rather than
new behaviour: it diffs the docker calls a `container` and a `container, host`
node issues against an **unset** node's, in both phases, and therefore passes
against the unfixed scripts too — which is the point.

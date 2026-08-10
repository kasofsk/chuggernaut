# Design #537 — Per-project unix users on a macOS host node

Status: PROPOSED — nothing below is built, and slice 0 is a measurement.

Written against the tree at `4674210` (2026-08-10). Every claim about current
behaviour was read out of the source named beside it rather than out of a sibling
design doc; where a sibling and the tree disagree, the disagreement is recorded
in the corrections section at the end. The measurement that opens this design was
taken by the operator on `gumbo-air-0` on 2026-08-10 and is reproduced verbatim
in the first section, with a line drawn under what it licenses and what it does
not.

## Current state

*This section is the mutable head: it is rewritten to current truth whenever
anything below it changes. Everything after the horizontal rule is append-only —
the argument and its dated corrections, never edited
([#415](415-knowledge-architecture.md) D2).*

| Fact | Where | State |
| --- | --- | --- |
| A host task runs as the **daemon's own uid** — the login user on a Mac. Nothing in the launch path names a uid at all | `spawn_task` in `crates/container/src/host.rs`: `env_clear`, `envs`, `process_group(0)`, `spawn` | True, and it is what this design changes |
| The macOS daemon is a `launchd` **agent in the login user's GUI domain** | `deploy/prod/install-worker-launchd.sh` bootstraps into `gui/$(id -u)` | True |
| A host task inherits exactly `PATH` and `HOME` from the daemon | `INHERITED` / `floor_env`, `crates/container/src/host.rs` | True, so a task's `$HOME` **is** the login user's home |
| A host node is single-tenant, enforced at the node and fail-closed | `HostTenancy` read in `HostBackend::admit`, `crates/container/src/host.rs`; parsed in `crates/worker/src/config.rs` | True. The list already accepts **several** projects — nothing in the parse or the admit forbids two |
| One host task at a time | `enforce_host_capacity`, `crates/worker/src/daemon.rs`, plus the running-task exclusion in `HostBackend::admit` | True |
| The task directory is created `0700` by the daemon, and every wire path is rebased into it | `create_private_dir` and `rebase_path`, `crates/container/src/host.rs` | True |
| `remove` deletes the recorded files, the task directory, and this task's MCP-log subtree **under the daemon's own `HOME`** | `reclaim_agent_cache` reads `HOME` out of the daemon's environment, `crates/container/src/host.rs` | True, and it breaks the moment the task's home is not the daemon's |
| The cross-project secret boundary is **absent** on a Mac, accepted by job #526 | [#322](322-macos-native-runtime.md)'s 2026-08-09 correction | True today; this design is what replaces that decision |

## Decisions

| # | Decision | One-line rationale |
| --- | --- | --- |
| **D1** | **One unix user per project, `chug-{project}`, not a per-task pool.** | It maps one-to-one onto a list the node already declares and already fails closed, and it keeps within-project persistence — which [#309 §10](309-host-native-execution.md#10-trust-and-tenancy) calls the feature — while restoring the boundary between projects. |
| **D2** | **The daemon keeps its GUI-domain agent shape and escalates per launch through a node-provisioned `sudo` binding.** A root daemon is recorded as the endpoint, with three triggers, and is not this design's first cut. | The escalation is exactly the mechanism the measurement used; a root daemon would dissolve most of the work below but rewrites the one Mac's supervision two weeks before it carries the operator's iOS work. |
| **D3** | **The binding is told to the backend, never discovered by it** — a resolved `{project → uid, home}` map handed to `HostBackend::new` beside `Supervision`, `AgentCapability` and `HostTenancy`. | It is the pattern those three already establish in `crates/container/src/host.rs`: the daemon discovers node facts, the backend is told them and is testable without a node. |
| **D4** | **`launch`, `kill` and *every delete* escalate — including the two the **daemon** performs with no task left to ask, `spawn_reaper`'s teardown repeat and `sweep_detached`'s boot sweep. The read family does not, and the daemon's own exit-code write rides the group.** | A non-root uid cannot signal or unlink another uid's work at all, so the deletes are not a preference; reads and the exit-code write are the only places a permission bit can carry it, and job/181's rule says an unreadable result is an error and never an empty one. |
| **D5** | **No new boot gate. `WORKER_HOST_PROJECTS` becomes the roster.** The deploy refuses a listed project whose user does not resolve on the node; the daemon refuses that project's *launch* by name. | [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)'s do-not-advertise rule and the tenancy list would otherwise be two gates over one fact; the list already enumerates exactly the set of users that must exist. |
| **D6** | **The user name is derived, `chug-{project}` from the slug's second component, and a derivation collision is a hard parse error.** | A silent collision between `a/beacon` and `b/beacon` would hand two projects one uid — precisely the failure this design exists to prevent — and `parse_projects` already refuses a repeated entry in the same shape. |
| **D7** | **`WORKER_HOST_ROOT` moves out of the login user's home** to a node-wide root — `0711`, owned by the daemon's uid, **created by the operator with root** at provisioning, and traversable but not listable by every project user. | A root under `/Users/worksalot` is unreachable to another uid if that home is `0700`, and the whole task directory hangs off it; the daemon creates its root at boot and cannot create one outside its own home, so the operator has to hand it one that already exists. |
| **D8** | **Signing is not designed here.** A per-project file keychain unlocked per task is the recorded direction; beacon's real setup is **operator input required** before any certificate is installed on the node. | The node has **zero** valid signing identities in any session today ([#322](322-macos-native-runtime.md), job #526's rung table), so nothing is lost by moving to a session-less user and nothing is known well enough to design against. |
| **D9** | **Provisioning is the operator's, by runbook, and it is not symmetric.** Creation over ssh works; directory-services deletion is refused (`eDSPermissionError -14120`). Every procedure is idempotent against a stale account record. | A platform that assumed it could delete a user would be assuming an access path the operator measured as absent. |
| **D10** | **[#534](309-host-native-execution.md#phasing)'s deferred cache namespacing is retired by construction, and so is the *placement* half of its eviction slice. The ceiling and the LRU are not.** | Per-user homes make the collision impossible rather than avoided, and they move the caches out of the operator's own home — which was the stated reason placement had to precede eviction. |
| **D11** | **Host-mode docker stops being ambient.** [#517](517-docker-access-for-jobs.md) D1 is untouched — jobs may use docker — but the host half's default inverts from granted-for-free to unreachable-unless-granted. | That is #517 D4's own S6 arriving early: per-task users were named there as the **only** mechanism that can withhold host-mode docker, and a uid change withholds it whether or not anyone intended it to. |

## Slices

| # | Slice | Contract changed | Depends on | State |
| --- | --- | --- | --- | --- |
| 0 | `human` — the measurements M1–M8 below, on `gumbo-air-0`, with no platform change: M1 and M2 ride an existing `mode: host` job type's own script | none | — | Proposed |
| 1 | `code` — resolve the per-project binding at boot and hand it to `HostBackend`; `launch` escalates; the task's `HOME` and task directory follow the task user | `HostBackend::new` signature, the host launch path (`crates/container/src/host.rs`) | 0 (M1, M2, M3) | Proposed |
| 2 | `code` — every delete escalates: `kill`, `remove`, **and the daemon-side pair a task's exit already runs without it** — `spawn_reaper`'s `reclaim_credentials` + `reclaim_agent_cache`, and `sweep_detached` at boot; `reclaim_agent_cache` follows the **task** user's home rather than the daemon's | `ContainerBackend::kill` / `remove` on the host backend, and the reaper's teardown repeat | 1 | Proposed |
| 3 | `deploy` — `build-worker.sh` refuses a listed project whose user does not resolve on the node; `WORKER_HOST_ROOT` guidance and `deploy/prod/env.example` follow D7, including that the root is now an operator precondition on macOS | the node run spec | 1 | Proposed |
| 4 | `docs` — a provisioning runbook (create the user, the group, the home, the `sudoers` line and **the `WORKER_HOST_ROOT` root itself**; verify; decommission; and the deletion asymmetry); `docs/reference/runbooks/worker-host-projects.md` §2 stops arguing single-tenancy as the boundary | runbook set | 3 | Proposed |
| 5 | `design` — amend [#322](322-macos-native-runtime.md)'s job #526 correction and [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)/§10 with what this replaces | design record | 1 | Proposed |
| 6 | deferred — signing, once D8's operator input exists | — | 0 (M5) | Deferred |
| 7 | deferred — the cache ceiling and LRU eviction inherited from #534(b) | node cache policy | 1 | Deferred |

**Slice 0 gates every other row**, and the ordering inside it matters: M1 is the
only one that can fail in a way that sends this design to its rejected
alternative.

## What must be measured first

None of these is answerable from this workspace. Each names what it decides, so
a failing row changes a decision rather than producing a note.

| # | Measurement | Decides |
| --- | --- | --- |
| **M1** | A task **spawned by the daemon's own launch path** — not by ssh — drives CoreSimulator as `chug-probe`. Ride an existing `mode: host` job type: its script runs `sudo -u chug-probe -H` and reports `launchctl managername`, `simctl list devices`, `create`, `boot`, `bootstatus -b`. No platform change is needed to run it | D2. A failure here is what sends this design to the rejected shared-uid alternative |
| **M2** | Does `sudo` succeed non-interactively from that path (no tty, `NOPASSWD`), and **does the composed environment survive it**? `sudo` resets the environment by default | Whether the task environment must be handed over as a `0600` file the wrapper sources — [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)'s option (c) arriving as a requirement rather than a follow-up |
| **M3** | The mode of `/Users/worksalot` and of the daemon's own state under it: can `chug-probe` read the worker environment file, the NATS creds it names, or the login keychain? | Whether the boundary is real. If the daemon's own home is world-traversable the uid buys nothing until D7 moves that state |
| **M4** | Are installed simulator **runtimes** shared (under `/Library`) or per-user? | The provisioning cost per project — tens of GB per user if they are per-user, and a slice of its own if so |
| **M5** | Does a fresh uid build with Xcode without a per-user first launch — `xcodebuild -version`, then a simulator build | Whether provisioning a project user is one command or a procedure |
| **M6** | Can one project user read another's **argv** via `ps`? | Whether any secret may ever ride argv. Assume yes until measured; it is why M2's answer must not be "pass it on the command line" |
| **M7** | Is colima's docker socket reachable to `chug-probe`? | D11's default, and whether beacon needs an explicit grant on day one |
| **M8** | Can `chug-probe` exec the agent CLI the daemon discovered on its own `PATH` (`/Users/worksalot/.local/bin/claude`, [#490](490-agent-work-on-a-mac.md) D3/M3)? | Whether agent host work survives the uid change, or the CLI has to move to a node-wide path the way the channel binary already did |

---

## 1. The measurement, and the line under it

A user `chug-probe` (uid 502) was created on `gumbo-air-0` and has **never**
logged in at the console. Under `sudo -u chug-probe -H`, over an ssh session:

| Rung | Result |
| --- | --- |
| `launchctl managername` | `Background` |
| `launchctl print gui/502` | not reachable |
| `launchctl print user/502` | reachable |
| `xcrun simctl list devices` | works, and returns **different UDIDs from uid 501's** — its own device set |
| `simctl create` / `boot` / `bootstatus -b` | ok / rc 0 / `Finished` |
| `simctl launch com.apple.Preferences` | pid 55860 |
| `simctl spawn <udid> launchctl list` | the simulator's own daemons |
| `security list-keychains` | **only** `/Library/Keychains/System.keychain` — no login keychain, because there is no session to unlock one |

The test device was deleted and the home removed afterwards. The account
**record** persists, because directory-services deletion is refused over ssh
(`eDSPermissionError -14120`, even under `sudo`) — which is D9.

**What it shows.** A uid with no Aqua session and no GUI launchd domain drives
simulators, in a device set of its own. That is the exact question
[#322](322-macos-native-runtime.md) deferred per-task users on, and the
confound that spoiled the 2026-08-09 attempt — `worksalot` was logged in at the
console, so a Background session may have been talking to a GUI-domain
`CoreSimulatorService` — is gone: uid 502 has no GUI domain at all, and its
device set is demonstrably not uid 501's.

**What it does not show.** That the worker daemon's own launch path can spawn as
that uid. `sudo -u` from an interactive ssh session is a **proxy** for that, not
a proof: the daemon is a `launchd` agent in a different domain, with a different
environment, no tty, and a `sudo` invocation nobody has run from there. That gap
is M1, and it is the one measurement that can send this design to its rejected
alternative. It is cheap: a `mode: host` job type's own script can run the probe,
because a host task **is** spawned by the daemon's launch path, so the
measurement needs no platform change at all.

**One thing the measurement also settles quietly**: `security list-keychains`
returning only the system keychain is consistent with *there being no login
keychain*, not with *keychains being unavailable*. A file keychain created and
unlocked explicitly is a different mechanism and is untested — section 5.

## 2. Per-project, not per-task

[#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host) proposed a
**fixed pool** of per-task users, `chug-task-0 … chug-task-{slots-1}`, sized to
the slot count. Three things decide against it here, and the third is the one
that would decide it even if the first two were free.

- **The pool has one member.** `enforce_host_capacity`
  (`crates/worker/src/daemon.rs`) refuses to boot a host-capable node whose
  `WORKER_SLOTS` and `WORKER_SLOTS_MAX` are not both 1, and `HostBackend::admit`
  refuses a second concurrent task as `NoCapacity`. A pool sized to the slots is
  therefore a pool of one, and the only task a second user could separate this
  one from is **its own predecessor** — which [#490](490-agent-work-on-a-mac.md)'s
  M5 fork already argued a teardown handles at a fraction of the cost.
- **It maps onto nothing the node declares.** `WORKER_HOST_PROJECTS` names
  projects, is parsed in `crates/worker/src/config.rs`, is read in
  `HostBackend::admit` and nowhere else, and is fail-closed. A per-task pool
  needs a second declaration with its own gate; a per-project user needs the
  list that is already there, which is what makes D5 possible.
- **A per-task user would destroy the thing host mode exists for.** Persistence
  across a project's own tasks — a warm derived-data tree, a warm package cache,
  a resolved simulator — is what [#309 §10](309-host-native-execution.md#10-trust-and-tenancy)
  calls *the feature*, and a fresh uid per task discards it every time. The
  per-project user keeps it **inside** a project and removes it **between**
  projects, which is the boundary that was actually missing.

The honest cost of choosing per-project: two tasks of the *same* project still
share a uid, so a leaked credential from one is readable by the next until the
teardown runs. That is the residue #526 already named, unchanged in kind and
narrowed in reach — it is no longer shared with a different project or with the
operator's own login session.

## 3. How the daemon spawns a task as another uid

This is the question everything else waits on. Four candidates, and what each
costs the tree as it stands.

### C1 — a `sudo` binding from the existing agent (recommended)

The daemon stays a `launchd` agent in the login user's GUI domain; a node-side
`sudoers` entry grants that user `NOPASSWD` execution as exactly the project
users, and the launch becomes `sudo -u chug-{project} -H …` in front of the
command `supervised_cmd` already wraps.

- **What it needs:** a `sudoers` file, installed once by the operator with root,
  naming the login user and the project users. Nothing else.
- **What privilege the daemon holds:** the ability to become exactly those uids,
  and nothing more. The escalation is one-way — a task running as
  `chug-beacon` has no rule of its own, so it cannot become `chug-chuggernaut`
  or the login user.
- **Why it is the recommendation:** it is the mechanism the measurement used, so
  M1 is the only gap rather than a stack of them; and it leaves
  [#440](440-native-worker-daemon.md) D2, [#490](490-agent-work-on-a-mac.md) D2
  and D3, `deploy/prod/install-worker-launchd.sh` and the self-refresh path all
  exactly as they are, on the one Mac that is about to carry the work.
- **What it costs, and it is not small:** a non-root uid cannot `chown` a
  directory to another user, cannot signal another user's process group, and
  cannot unlink files in a directory it does not own. So the task directory, the
  environment file above, the `kill` path, the `remove` path **and the three
  deletes the daemon runs after the task is gone** all change shape — section 7
  is that argument, and it is the price of not taking C2.

### C2 — a root daemon (`LaunchDaemon`), spawning with `setuid`

The daemon becomes a system-domain `LaunchDaemon` running as root and spawns
with `uid()`/`gid()` and `initgroups` before `exec`.

- **What it buys:** every problem in section 7 disappears. `chown` is free, so
  the task directory can be `0700`-owned by the task user — *stronger* than the
  group-shared `0770` C1 needs — and the environment file above becomes `0600`
  owned by the task user rather than `0640` shared with a group. `kill`,
  `remove` and the reaper's deletes are free. And
  [#440](440-native-worker-daemon.md) D5's root-owned credential directory,
  landed on Linux only, becomes available on macOS.
- **What it costs:** it converts the macOS daemon out of the GUI domain, which
  #440 D2 called *forced, not chosen* — on two premises, CoreSimulator and the
  keychain, of which **the measurement has just falsified the first**. That is
  the honest state: the argument for C2 is stronger today than it was yesterday,
  and it is still a rewrite of the supervision shape on the single Mac that
  serves host work, plus a `LaunchDaemon` installer, plus the `WORKER_CARGO`
  self-compile running as root, plus [#490](490-agent-work-on-a-mac.md) D3's CLI
  discovery moving off the login user's `PATH`.
- **And it has an unmeasured risk of its own.** `CoreSimulatorService` is a
  per-user Mach service. A process spawned from a system-domain daemon may land
  in a bootstrap namespace where the lookup fails — the classic
  `Service is disabled` shape — which is what `launchctl asuser` exists to fix.
  So C2 does not skip M1; it needs M1 asked of a different launch path.

**C2 is recorded as the endpoint, not rejected.** Three triggers move it from
recorded to scheduled: M1 failing under C1 while succeeding under `asuser`;
section 7's ownership scheme proving fragile in practice (a tool that creates
`0700` directories inside the task tree is enough); or the platform needing to
withhold something from the login user, which only root can do.

### C3 — `launchctl asuser <uid> …`

Runs the command in the target uid's per-user domain, which is the most
*correct* placement available — and `user/502` was measured reachable. It
requires root, so it is C2 plus a step, and its necessity is unmeasured (M1
answers whether the plain escalation suffices). Recorded as the repair for a
specific failure rather than as a candidate on its own.

### C4 — a per-task `launchd` domain, `launchctl bootstrap user/<uid>`

macOS's nearest equivalent of the transient systemd scope
[#440](440-native-worker-daemon.md) D3 uses on Linux, and it would upgrade
`Supervision::ProcessGroup` — whose macOS half is asserted by an operator
procedure rather than by the mechanism — into a real supervision unit. It needs
root, a rendered plist per task, and a reaping story. **Deferred, and worth
naming**: it is the only candidate that answers a question this design does not
otherwise touch, so it belongs to a future #440 slice rather than to this one.

### Rejected outright: a setuid helper

A small `4755` root-owned binary the daemon execs. Rejected: it is a
hand-written version of what `sudo` already does with an audited configuration
file, and the failure mode of getting it wrong is node root.

### The environment must not cross on the command line

Whatever the mechanism, one rule falls out of M2 and M6 together. `sudo` resets
the environment by default, and the obvious repair — `sudo -u X env VAR=… cmd` —
would put every injected secret into **argv**, which is readable from the process
table. That is strictly worse than the `environ` exposure #526 accepted.

So the composed environment is handed over as a file inside the task directory,
which the wrapper `supervised_cmd` already generates sources before `exec`. **Its
mode is decided by the candidate, and C1 cannot deliver the obvious one.** A
`0600` file is owned by whoever wrote it, and under C1 the writer is the
non-root daemon, which cannot `chown` it to the task user — so `0600` would make
the wrapper's `.` of it fail and every launch die. Under C1 the file is
therefore `0640`, `chgrp`'d to the project group: the same move section 7 makes
for the task directory, and the same one a non-root owner is permitted as a
member of the target group. The residue, stated rather than glossed: it is
readable to every member of that group — which is the daemon and that one
project's user — and to root. Under C2 it becomes `0600` owned by the task user,
which is strictly better and is one more line on C2's ledger. This is
[#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)'s option (c)
— file-based injection — arriving as a **requirement** of the escalation rather
than as the follow-up §8 filed it as. It composes exactly as §8 predicted: the
secrets stop riding `environ` into the shell that spawns the task's own children,
while still reaching the task itself.

## 4. What the uid restores, and what stays shared

### #526's three bounds, restated against the new shape

Job #526 leaned the security story on three bounds, of which single-tenancy was
the only fully real one. Each moves:

**1. Single-tenancy stops being the boundary and becomes the roster.** This is
the sharpest change and it must be said plainly: a node serving two projects is
**not single-tenant**, so a bound the tree currently leans on is being withdrawn
and replaced, not supplemented. What `WORKER_HOST_PROJECTS` decides afterwards is
*which* projects the node serves and therefore *which* users must exist — real
work, and no longer a security control. Anything else resting on single-tenancy
has to be re-checked; section 6 finds one such thing and
`docs/reference/runbooks/worker-host-projects.md` §2 is another (slice 4).

**2. Exit-time deletion keeps its *shape* on the task's side, changes shape on
the daemon's, and improves in reach.** The wrapper still empties the mapped
credential tree the moment the command returns, sparing `AGENT_STATE_DIR` for the
harvest, and `remove` still reclaims the task directory — both unchanged, because
both are the task's own uid or an escalation. **The daemon's own half is not
unchanged**: `spawn_reaper` repeats that teardown from the daemon's uid on every
exit, and section 7 is where that is fixed. What it never covered — *everything outside the task directory*,
which #526 enumerated as `~/Library/Developer/CoreSimulator`, the shared
derived-data tree, `~/.docker/config.json` and the login keychain — is not
reclaimed,
but it now lands in a **per-project home** rather than in the home the daemon and
the operator share. The residue is confined to one project instead of pooled
across the node.

**3. Short credential TTLs are unchanged, and the open item is narrowed rather
than closed.** What the platform mints is TTL-bounded; what it forwards
(`work.secrets`, evaluator `secrets`, project `vars`, the reserved
`global/agents` credentials) still carries no TTL — `crates/dispatcher/src/exec.rs`
and `crates/auth/src/nats.rs` are unchanged by anything here. What changes is who
can read a forwarded secret out of a running process: one project's uid and root,
rather than every process of the login user. Still an open item, and this design
does not close it.

### What is genuinely restored

The cross-project secret boundary [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)
lost. Concretely: beacon's App Store credentials, its git deploy key and its
minted NATS creds stop being readable by a chuggernaut task on the same Mac, and
the reverse. Under C1 that boundary is enforced by uid separation plus the
absence of a `sudoers` rule for the project users — not by policy, and not by a
list somebody has to keep correct.

Per-user device sets come with it, measured rather than assumed: uid 502 saw
different UDIDs from uid 501's. That is [#322](322-macos-native-runtime.md) §5's
per-task device set arriving **per project** for free, so a project cannot
`simctl delete all` another project's devices even by accident.

### What stays shared, and stays lost

- **Root**, and therefore everything. Unchanged, and it is the bound nothing
  short of a VM moves.
- **The machine.** CPU, memory and disk are unbounded on a host task
  ([#490](490-agent-work-on-a-mac.md) D7), so one project can still starve or
  fill the node. A uid is not a resource limit.
- **The node's single host slot, and this is the operational cost of the brief's
  own goal.** `enforce_host_capacity` pins a host-capable node to
  `WORKER_SLOTS = WORKER_SLOTS_MAX = 1`, and this document leans on that three
  times as an *argument* (section 2, D1, section 10). Turned around, it is a
  price: beacon and chuggernaut host work now **serialize on one slot**, so a
  long iOS build blocks chuggernaut host work behind it — the queue
  [#490](490-agent-work-on-a-mac.md#d4--one-host-task-per-node-stays) named as
  D4's own revisit trigger, arriving because a second project arrived. This
  design does not raise the slot count, and D4 owns that decision. What it does
  do is remove one of the two reasons it could not be raised: per-project users
  make two concurrent tasks of **different** projects safe from each other,
  leaving only CoreSimulator's global device state — #322 §5's per-task device
  set and [#309 §5b](309-host-native-execution.md)'s leases — as the blocker. A
  follow-on this unblocks, not one it takes.
- **The immutable shared tree** — the macOS analogue of `/nix/store`:
  `/Applications/Xcode.app`, `/opt/homebrew`, and the node's own
  `/usr/local/lib/chuggernaut/chuggernaut-channel-host`. Root-owned, additive,
  and genuinely bounding, exactly as §10's table says of the store.
- **The simulator runtimes**, if M4 confirms they live under `/Library`. Shared
  is the *wanted* answer here: per-user runtimes would mean tens of GB per
  project.
- **The process table.** `ps` shows other users' processes; whether it shows
  their argv is M6, and the design assumes it does.
- **The docker socket** — but not for free any more, which is section 8.
- **The daemon's own uid**, which can enter every project user by construction.
  That is not a leak, it is the trust model: the daemon composes every task's
  secrets already. It does mean the boundary protects projects from each other
  and protects nothing from the login user.

## 5. Signing

The measurement found no login keychain for a session-less user, which is
expected and not a failure: a login keychain is created and unlocked by a login,
and there was none.

**What this design does not do is guess.** beacon's actual signing setup — manual
versus automatic signing, whether the identity arrives as a `.p12` plus a
provisioning profile or through an App Store Connect API key, whether device
builds or only simulator builds are wanted first — is not in this repo and cannot
be read out of it. **Operator input is required before beacon installs
certificates on the node**, and that input is what slice 6 waits on.

**The direction, recorded so it is not re-derived:** a per-project **file**
keychain, created once at provisioning (`security create-keychain`), unlocked per
task from a `work.secrets` value, added to the task's search list, and given a
partition list so `codesign` can use the key without a UI prompt. This is the
ordinary CI shape and it does not depend on a session — which is why the
measurement's `security list-keychains` result is not evidence against it. It is
also untested here, so it is a direction and not a decision.

**What the per-project user costs signing today: nothing.** #526's rung table
measured **zero valid signing identities on the node in any session**, so there
is no working setup to break. The uid change is free right now and gets more
expensive the longer it waits — which is an argument for taking it before beacon
installs anything, not after.

**One thing it makes structurally better**, worth stating because it is the whole
reason to prefer a per-project keychain over a shared one: a signing identity in
`chug-beacon`'s keychain is unreadable to a chuggernaut task even when unlocked.
Under the current shared-uid shape it would not be.

## 6. Provisioning, and the two-gates smell

### Who creates the users

**The operator, by runbook (slice 4).** Not the deploy, and not the daemon.
`deploy/prod/build-worker.sh` runs over ssh as the login user; creating a user
needs root, and — given D9's asymmetry — it is an act the operator cannot fully
undo through the same access path. A deploy script that creates accounts it
cannot delete is the wrong shape.

Five root-requiring acts, per node, and the last is easy to forget because
nothing today asks for it:

1. The user `chug-{project}`, with a home.
2. The group `chug-{project}`, with the daemon's login user as a member and no
   other project user.
3. The `sudoers` line granting the login user `NOPASSWD` execution as exactly
   that user.
4. The per-project file keychain, once D8's operator input exists (slice 6).
5. **The `WORKER_HOST_ROOT` directory itself** — `0711`, owned by the daemon's
   uid — because D7 moves it somewhere the login user cannot create and
   `HostBackend::new` creating its own root is what the old placement relied on.
   Section 7 has the argument.

The asymmetry is a design constraint, not a footnote:

- **Creation** over ssh works.
- **Deletion** of the directory-services record is refused
  (`eDSPermissionError -14120`), even under `sudo`, over that path.
- Therefore the account record is **durable**, and the platform must never depend
  on removing one. Decommissioning a project from a node means removing its home
  and its `sudoers` line — both of which *are* reachable over ssh — and leaving
  the record.
- Therefore provisioning must be **idempotent against a stale account**: a
  project re-added to a node six months later meets a record that still exists,
  possibly with a home that still exists. The runbook says how to reset it; the
  daemon's boot-time resolution must accept an existing uid rather than assuming
  it created one.

The eventual home for this is the operator's own `macos-runner` configuration, in
the same way the NixOS unit's home is the consuming host repo
([#372](372-chug-node-modules.md), [#440](440-native-worker-daemon.md) D2).
Whether nix-darwin can declare a macOS user reliably is not verifiable from this
workspace and is recorded as unknown rather than as a plan.

### Two gates over one fact

[#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host) makes it a
hard rule that *the daemon does not advertise `host` unless the user pool is
provisioned*. `WORKER_HOST_PROJECTS` is already a fail-closed gate over an
overlapping fact. Two gates saying similar things is the smell the brief names,
and the resolution is that **there is only one fact**: the list of projects a
node serves *is* the list of users that must exist.

So D5 collapses §8's rule into the list rather than adding a gate beside it:

- **At the deploy**, `build-worker.sh` checks — over the ssh it already has —
  that each listed project resolves to a usable user, and refuses with the live
  daemon untouched. This is where the loud failure belongs, and it is the shape
  the script already uses for an empty tenancy, a malformed entry and a
  `WORKER_SLOTS` that is not 1.
- **At the launch**, a project whose user does not resolve is a hard
  `BackendError::Launch` naming the project, the expected user and the node —
  never `NoCapacity`, for the reason the tenancy refusal already gives: it cannot
  clear without a change on the node.
- **At boot**, a warning and nothing else, matching what an empty tenancy already
  does. A boot refusal here would brick a node's daemon under `KeepAlive` the
  moment an operator adds a project to the list before creating its user — and
  the deploy gate is what stops that reaching the node in the first place.

§8's rule survives in substance — an unprovisioned node runs no host work for the
project in question — while the mechanism stays the one gate that is already
there. What §8 gets *wrong* for macOS, and this replaces, is the granularity: the
pool was sized to slots, and the roster is sized to projects.

### Naming

D6 derives `chug-{project}` from the second component of the `owner/project`
slug. It is the shortest thing an operator can be asked to create and the only
thing the daemon can check without a second declaration. The failure it must not
have is silent: `a/beacon` and `b/beacon` on one node derive one name, which
would hand two projects a single uid — the exact outcome this design exists to
prevent. So a derivation collision is a **hard parse error** in
`crates/worker/src/config.rs`, the same treatment a repeated entry already gets.

If a node ever must serve two same-named projects from different owners, the
answer is an explicit `owner/project:user` mapping in the shape
`WORKER_DOCKER_GRANTS` already uses. Recorded, not built — adding it now would be
the second declaration D5 just avoided.

## 7. Task directories, ownership, and every call site that changes

This is where C1's cost is paid, and it is worth reading before agreeing to C1.

**A non-root daemon cannot:** `chown` a directory to another user; signal another
user's process; or unlink an entry from a directory it neither owns nor has write
permission on. `crates/container/src/host.rs` does all three today, and gets away
with it only because the task is the same uid.

### The directory

`create_private_dir` makes the task directory and its two wire-path children
`0700`, owned by the daemon. Under C1 that is unusable: the task could not write
its own workspace. The scheme that works without root:

- **A per-project group**, created beside the user; the daemon's uid is a member
  of every project group, and no project user is a member of another's.
- The daemon creates the task directory as itself, `chgrp`s it to the project's
  group — permitted for a non-root owner who is a member of the target group —
  and modes it `0770`.
- The task's wrapper sets `umask 007`, so what the task creates inside is
  group-readable and group-writable. On macOS a new file's group is inherited
  from its parent directory, so the group propagates down the tree without a
  setgid bit.
- `chug-beacon` can write; `chug-chuggernaut` cannot read; the daemon can do
  both. That is the boundary, and it is a strictly weaker directory mode than
  C2's `0700`-owned-by-the-task — stated rather than glossed.

**And D7 falls out of it.** The whole tree hangs off `WORKER_HOST_ROOT`, which
`deploy/prod/env.example` documents as a path *the login user owns* and which the
air declares under that user's home. If `/Users/worksalot` is `0700` (M3), no
project user can traverse to a task directory at all. So the root moves to a
node-wide path outside that home — `0711`, owned by the daemon's uid, so every
project user can traverse to its own task directory and none can list the root or
create in it.

**Who creates it is the half that cannot be left implicit, because the daemon's
boot path turns silence here into a crash loop.** `HostBackend::new`
(`crates/container/src/host.rs`) does `create_dir_all(&root)` and maps a failure
to `BackendError::Unavailable`; `deploy/prod/env.example` documents the
consequence in terms — *a root the login user cannot create is a boot failure the
supervisor loops* — and that is exactly why the default `HOST_ROOT_DEFAULT`,
`/var/lib/chuggernaut/host-tasks`, is unusable on macOS. D7's replacement is by
construction outside the login user's home, so on macOS it is very likely a path
the login user also cannot create (`/usr/local` is root-owned on Apple Silicon),
and a daemon handed one would walk straight into the failure `env.example`
already names.

So the root becomes an **operator provisioning step**, listed in section 6 and
scoped into slice 4's runbook: created with root, `chown`ed to the daemon's uid,
moded `0711`. Against a root that already exists, `create_dir_all` returns
success without touching it — which is what makes the move safe, and it is the
whole mechanism: the daemon's boot path is unchanged, it simply no-ops. One
honest consequence for slice 3: `HostBackend::new`'s own doc comment currently
asserts the root is *worker-owned node state, not an operator precondition*, and
on macOS that stops being true. The line changes with the guidance.

### The methods that escalate

- **`launch`** escalates (section 3). One further note: `output.log` needs no
  permission at all, because the daemon opens it and passes the **fd** to the
  child. File descriptors cross a uid change; that half is free.
- **`kill`** escalates. `signal_group(meta.pgid, SIGTERM)` from the login user to
  a process group owned by `chug-beacon` is `EPERM` — a silent failure to kill,
  which is worse than a loud one. The `SIGKILL` follow-up in the spawned grace
  task has the same problem. Both go through the escalation.
- **`remove`** escalates. Deleting a subtree the task created requires write
  permission on each directory in it; the group scheme above delivers that *as
  long as* no tool inside the task created something with a tighter umask, and
  tools do. So the recursive delete runs as the owner rather than relying on a
  bit — one path, always exercised, no class of leftovers that appears only when
  a particular tool ran. `remove` already reports a failed reclaim as leaked disk
  and returns an error rather than swallowing it; that behaviour is what makes
  this safe to get wrong loudly. It is also the method that reclaims the agent
  CLI's `chuggernaut/claude` tree — the one subtree exit-time teardown
  deliberately spares for the harvest, written by the CLI as the **task** user
  ([#490](490-agent-work-on-a-mac.md) D6, and its job #497 correction) — so under
  this design `remove` is the only thing standing between that tree and a leak on
  every task.

### The daemon's own deletes, which no task is left to perform

`remove` is the *loud* case, and it is not the dangerous one. Three deletes run
from the **daemon's** uid with the task already gone, and every one of them can
cross the boundary this design introduces. **They do not all cross it equally,
and the ordering matters** — two cross by construction, one is defensive:

- **`sweep_detached`**, called from `HostBackend::new` at boot, `remove_dir_all`s
  the `.removing-*` trees `remove` renames before deleting. That is a whole task
  tree, task-user-owned by construction — so this delete crosses the boundary
  unconditionally, every time it has anything to do, and its failure is an
  `error!` line at boot.
- **`spawn_reaper` → `reclaim_agent_cache`** is held to
  `crates/worker/src/nix.rs`'s reaper charter — it leaks disk rather than ever
  failing a job, and returns nothing a caller could fail on. It therefore cannot
  report a permission failure *at all*, only log one. It is already the subject
  of the `HOME` break below; the escalation is the other half of the same fix.
  After slice 2 the cache it sweeps sits under the **task** user's home, so it
  crosses unconditionally too.
- **`spawn_reaper` → `reclaim_credentials`** (`crates/container/src/host.rs`) is
  the **defensive** one, and its bound is genuinely narrower than the other two.
  It runs on every task exit and deletes each entry directly under the task's
  `chuggernaut/` tree, sparing `AGENT_STATE_DIR` — so the agent CLI's tree is
  `remove`'s problem, above, and not this function's. In the ordinary case every
  entry it *does* touch is **daemon-materialized**: `materialize` writes the
  injected files and `create_private_dir` makes their parents, both from the
  daemon's uid before the task is ever spawned, so unlinking them from the
  daemon's uid succeeds and needs no escalation at all. What it cannot
  necessarily delete is **content the task itself created under `chuggernaut/`**
  outside `claude`, if the tool that made it used a umask tighter than the
  wrapper's `007`: the daemon owns the `chuggernaut/` directory and so may unlink
  its direct entries, but `remove_dir_all` must *descend*, and a `0700`
  task-owned subdirectory refuses it.

  That residue is small precisely because the task's own wrapper normally empties
  it — the `find … -exec rm -rf` in `supervised_cmd` runs as the task, where no
  permission question arises — so it survives in exactly one case: **a wrapper
  killed before it got there**, which is the case the reaper's repeat exists to
  cover. Escalating here is therefore insurance on the covering path rather than
  a fix for the common one. It is still worth taking, because the failure is
  silent.

**That silence is what makes this subsection worse than the `remove` case**, and
it is the property all three share rather than anything about how often each one
crosses: a failed reclaim here is an `error!` line and the task completes.
Nothing is returned and nothing fails, so the leak is invisible at the job that
caused it.

**The daemon's one *write* into a task directory does not escalate**, and it is
worth saying why so D4's two families are not read as covering everything.
`write_exit_code` writes a temp file into the task directory and renames it over
`exit-code`; both operations need write permission on the **directory**, which
the `0770` group mode gives the daemon, and neither needs ownership of the target
(the root is not sticky). It is also already guarded — the reaper writes only
when `read_exit_code` finds none, i.e. when the wrapper never got there. So it
rides the group like the read family, deliberately.

### The read family, and one concrete break

`copy_file`, `find_file`, `logs` and `inspect` stay unescalated and read through
the group. A task that deliberately tightens a mode on its own `eval-result.json`
makes it unreadable to the daemon; that is a project breaking itself, and it must
fail loud — job/181's rule — rather than read as a task that produced nothing.

**One thing breaks outright and must be fixed in slice 2.**
`reclaim_agent_cache` resolves the agent CLI's MCP-log cache against `HOME` **read
out of the daemon's own environment** (`crates/container/src/host.rs`). With a
per-project task user the cache lands under `/Users/chug-beacon/Library/…`, and
the sweep would look in the daemon's home, find nothing, and log a debug line. The
subtree [#490](490-agent-work-on-a-mac.md) D6 exists to reclaim would leak on
every task. The sweep must follow the **task** user's home, and — since it is a
delete — through the escalation.

The same `HOME` question is the launch-side half: `INHERITED` carries `PATH` and
`HOME` from the daemon into the task, and `HOME` must stop being the daemon's.
`PATH` is a separate question that M8 answers: if the agent CLI lives under the
login user's home and that home is `0700`, agent host work stops working until
the CLI moves to a node-wide path — the shape `CHANNEL_PATH_HOST`
(`/usr/local/lib/chuggernaut/chuggernaut-channel-host`) already has for the
channel binary, and for the same reason.

## 8. Docker, and what this does to #517

[#517](517-docker-access-for-jobs.md) D1 — *jobs may use docker, and the
escalation to node root is accepted* — is **not reopened here**. What changes is
mechanical and unavoidable: a unix socket owned by the login user is not
reachable by another uid unless its mode or an ACL says so. #517's own table says
host-mode docker is *inherited from the uid the task runs as* and that to deny it
you would have to *change the task's uid* — which is precisely what this design
does.

So D11: the host half's default inverts from **granted for free** to
**unreachable unless granted**, and #517 D4's deferred S6 — *per-task users are
where withholding becomes possible* — arrives as a side effect of a design that
was not aiming at it. That is a gain for auditability and a real, immediate cost
for any host job type that quietly used docker.

The consequences to carry:

- **M7 measures it** rather than assuming either answer.
- **A project that needs the socket gets an explicit grant** — a group, an ACL or
  a per-user colima instance, node-side, in the shape
  `docs/reference/runbooks/worker-docker-grant.md` already documents for
  container launches. #517 D2's rule holds unchanged: node-side, never a job-type
  field the platform honours on request.
- **`NodeCapabilities`' advertised docker reachability
  (`crates/types/src/worker.rs`) becomes per-user on a host node**, and a single
  node-level boolean would be a lie the moment one project has the socket and
  another does not. Flagged for #517's slice rather than decided here.

## 9. What this does to #534's cache work

[#534's correction to #309 §9](309-host-native-execution.md#9-environment-and-state)
split the declared-cache work into three parts. Checked against this design
rather than assumed:

**(a) Namespacing — retired by construction.** The `{root}/{owner}/{project}/{purpose}`
scheme existed so two projects could not collide on `~/.gradle`. With a
per-project home there is no `~/.gradle` to collide on: `chug-beacon`'s is
`/Users/chug-beacon/.gradle`. #534 wrote the trigger as *namespacing becomes live
when a host node serves more than one project*, and noted the dependency is on
job #525's single-tenancy policy rather than on a property of the caches. This
design fires that trigger and answers it in the same breath — with a uid instead
of a path scheme. **What survives:** caches that do **not** live under `$HOME`
still collide, and `WORKER_CACHE_DIR` is one of them — a single node path shared
by every container the node launches, deliberately, because sccache's cache is
content-addressed and carries no job state. It is unaffected and stays that way.

**(b) Placement and eviction — the placement half goes, the eviction half
stays.** #534 argued eviction could not be built alone because *the caches are in
the login user's home, which the platform does not own*, and a daemon deleting
from the operator's own home is a different and worse act. Per-project homes
dissolve exactly that objection: `/Users/chug-beacon` exists only because the
platform's operator provisioned it for the platform, so it is a root the node may
bound. The relocation step — moving caches into a platform-owned root and
exporting each tool's own variable — becomes unnecessary work.

**What is still owed is the ceiling and the LRU**, unchanged and still mandatory
(`docs/reference/style.md` Tier 2 rule 3, everything is bounded). It is slice 7,
and it is now a smaller slice than #534 scoped: a ceiling per project home and a
sweep, with no relocation and no variable injection.

**(c) The declaration site — untouched.** #534 recorded its intended answer as a
file in the project repo naming caches from a node-known vocabulary. Nothing here
bears on it.

## 10. The rejected alternative: one shared `worksalot` uid

This was the live plan for several hours on 2026-08-10, and it is recorded with
its reasoning intact because **it is the fallback if M1 fails**.

**The shape:** both projects run host tasks as the node's existing login user;
the cross-project exposure is accepted, exactly as job #526 accepted it for one
project; and #534(a)'s cache namespacing is built after all, because two projects
on one uid is precisely the trigger it was deferred behind.

**What was genuinely good about it:**

- **Nothing in the tree changes.** No escalation, no ownership scheme, no
  `sudoers`, no `kill`/`remove` rework — sections 3 and 7 of this document simply
  do not exist. `WORKER_HOST_PROJECTS` gains a second entry and that is the whole
  platform change.
- **It keeps the daemon's shape**, and with it [#440](440-native-worker-daemon.md)
  D2, [#490](490-agent-work-on-a-mac.md) D3's CLI discovery, and ambient docker
  under [#517](517-docker-access-for-jobs.md) D1.
- **It is honest about what it gives up**, in the way #526 was: the exposure is
  accepted and documented rather than mitigated by something that does not work.

**Why it loses, now:**

- **Its premise expired.** #526 accepted the absent boundary because per-task
  users were believed unavailable on macOS — CoreSimulator needing a session was
  the load-bearing reason, in #322 §5, in #526's ratification, and in #490's M5
  fork. That belief is now measured false. Accepting an exposure because it
  cannot be avoided is a decision; continuing to accept it after it can be
  avoided is a different one, and it has not been argued.
- **The exposure it accepts is larger than the one #526 accepted.** One project
  sharing a uid with the login user is the operator's own code on the operator's
  own machine. Two projects sharing a uid means beacon's credentials are readable
  by a chuggernaut agent task and the reverse — and chuggernaut jobs run
  arbitrary agent-authored code by design.
- **It buys work rather than saving it.** It requires #534(a)'s namespacing,
  which per-project users retire; it leaves #534(b)'s placement objection
  standing; and it leaves #517's host half permanently ambient.

**The condition that revives it:** M1 failing — a task spawned by the daemon's
own launch path cannot drive CoreSimulator as another uid, under C1 *and* under
C3. In that case the shared uid is the answer, `WORKER_HOST_PROJECTS` takes both
projects, #534(a) is un-deferred, and this document becomes the record of why.

## What this does not decide

- **Whether the daemon should become root.** C2 is recorded with triggers, not
  scheduled. Taking it would delete most of section 7 and is the right endpoint;
  it is not the right first move.
- **Signing.** D8, waiting on operator input.
- **Whether agent host work survives the uid change** without moving the CLI —
  M8, and the fix if it does not is a slice of its own.
- **The per-user docker grant's mechanism.** D11 names the inversion and hands
  the mechanism to [#517](517-docker-access-for-jobs.md).
- **Anything about a Linux host node.** [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)'s
  per-task pool with `systemd-run --uid=` stands unweakened there, and no Linux
  host node exists.
- **Resource bounds.** A uid is not a limit; [#490](490-agent-work-on-a-mac.md)
  D7 still owns that.

## What this makes wrong elsewhere

Listed so slice 5 has a work list, and so a reader of those documents is not
misled in the meantime:

- **[#322](322-macos-native-runtime.md) §5**, *"Secrets and users: where #309 §8
  does not port"* — its three collisions are the argument this measurement
  overturns for CoreSimulator, and leaves standing only for the keychain. Its
  recommendation of *one dedicated task user with a login session* is superseded:
  no login session is needed.
- **[#322](322-macos-native-runtime.md)'s job #526 correction** — its decision
  stands as what the node does today and its premise no longer holds. It is
  amended, not deleted, and the amendment names this design.
- **[#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)** — the
  do-not-advertise rule is satisfied by `WORKER_HOST_PROJECTS` rather than by a
  gate of its own on macOS, and the pool's granularity is wrong for this node.
- **[#309 §10](309-host-native-execution.md#10-trust-and-tenancy)** and
  **`docs/reference/runbooks/worker-host-projects.md`** §2 — both argue
  single-tenancy as the boundary. It becomes the roster.
- **[#490](490-agent-work-on-a-mac.md)'s M5 fork** — its second reason (*there is
  no concurrency for a second user to isolate*) is still correct and still the
  reason per-**task** users lose. Its first reason (*the daemon inhabits a domain
  a per-task user cannot*) was about the **daemon's** domain and remains true; it
  does not carry to the **task's** domain, which is what the measurement tested.
- **[#517](517-docker-access-for-jobs.md)'s host-mode table** — the *default:
  granted, for free* row stops being true on a node with per-project users.

## Corrections (verified against the tree)

Three claims a sibling doc or the brief makes that do not survive contact with
the source.

1. **`WORKER_HOST_PROJECTS` does not need to change to hold two projects.** The
   brief and #534 both read as though multi-project host serving were a new
   capability. It is not: `parse_projects` (`crates/worker/src/config.rs`) parses
   a comma-separated list, refuses a malformed or repeated entry, and `HostTenancy`
   (`crates/container/src/host.rs`) admits any listed project. The
   `crates/container/tests/host_backend.rs` tenancy case constructs a two-entry
   list already. What is missing is the boundary, not the list.

2. **The brief lists the docker socket among what "stays shared". That is not
   automatic.** A socket owned by the login user is reachable to another uid only
   if a mode or an ACL says so, so per-project users *withhold* it by default —
   which is [#517](517-docker-access-for-jobs.md)'s own H4/D4 outcome arriving
   unrequested. Recorded as D11 and M7 rather than assumed either way; #517 D1's
   decision that jobs may use docker is untouched.

3. **[#440](440-native-worker-daemon.md) D2's macOS reasoning is now half
   false.** It records the GUI domain as *forced, not chosen*, on two premises —
   *"CoreSimulator and the keychain are per-user-session services, so a
   `LaunchDaemon` would be in the wrong session"*. The CoreSimulator half is
   falsified by the measurement in section 1: uid 502 has no GUI domain, no
   session, and its own working device set. The keychain half is untested and is
   D8. This matters because it is the load-bearing objection to C2, and half of
   it is gone.

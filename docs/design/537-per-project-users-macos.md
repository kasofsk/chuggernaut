# Design #537 — Per-project unix users on a macOS host node

Status: IMPLEMENTED IN PART — slice 0's measurements landed (jobs #557, #561), slice 8 moved the agent CLI to a node-wide path (job #571), slice 1's launch path landed (job #563), slice 2's teardown path landed (job #565), and slice 5 amended the siblings it supersedes (job #567). It is **inert until a node declares a binding**: `WORKER_HOST_USERS` is off everywhere, the deploy does not forward it yet (slice 3), and no node has the users (slice 4).

Written against the tree at `4674210` (2026-08-10). Every claim about current
behaviour was read out of the source named beside it rather than out of a sibling
design doc; where a sibling and the tree disagree, the disagreement is recorded
in the corrections section at the end. The measurement that opens this design was
taken by the operator on `gumbo-air-0` on 2026-08-10 and is reproduced verbatim
in the first section, with a line drawn under what it licenses and what it does
not.

**One of the two things that measurement left open has since been answered by the
operator rather than by a measurement.** Signing does not use the login keychain
section 1 found absent — real builds are signed by fastlane from ordinary
secrets — so **D8 is closed and slice 6 with it**, and the file-keychain
direction §[5](#5-signing) records is retired. What that changes here, what it
changes in [#322](322-macos-native-runtime.md), and the one thing it hands to
[#529](529-secret-handling.md) are in
[the 2026-08-10 correction](#correction--2026-08-10-job-558-signing-is-answered-fastlane-from-secrets-so-d8-closes-and-slice-6-with-it).

**The other one is now measured, and M1 passes.** A `mode: host` task the
**daemon's own launch path** spawned drove CoreSimulator as `chug-probe` through
`sudo -n -u chug-probe -H`, in a device set of its own — so this design is **not**
sent to [its rejected shared-uid alternative](#10-the-rejected-alternative-one-shared-worksalot-uid),
and C1 is viable. M1–M8 and what each answered are in the measurement table
below; the record, the two things the measurements *change* rather than confirm,
and the one question they leave open are in
[the 2026-08-10 slice-0 correction](#correction--2026-08-10-job-561-slice-0-is-measured-m1-passes-and-the-staff-primary-group-is-load-bearing-in-two-directions).
Read that question before treating slice 0 as closed: M1 passed in a session
inherited from the login user's **console** session, and the headless case is
untested.

## Current state

*This section is the mutable head: it is rewritten to current truth whenever
anything below it changes. Everything after the horizontal rule is append-only —
the argument and its dated corrections, never edited
([#415](415-knowledge-architecture.md) D2).*

| Fact | Where | State |
| --- | --- | --- |
| A host task runs as the **daemon's own uid** — the login user on a Mac — on every node in the fleet | `spawn_task` in `crates/container/src/host.rs`, whose escalation is reached only through a `HostUsers` binding, and no node declares one | True today. The launch path names a uid since job #563: `WORKER_HOST_USERS` on a host node resolves `chug-{project}` per listed project and `launch` escalates to it with `sudo -n -u … -H` |
| The macOS daemon is a `launchd` **agent in the login user's GUI domain** | `deploy/prod/install-worker-launchd.sh` bootstraps into `gui/$(id -u)` | True |
| A host task inherits exactly `PATH` and `HOME` from the daemon | `INHERITED` / `floor_env`, `crates/container/src/host.rs` | True unbound, so a task's `$HOME` **is** the login user's home. A bound task's `HOME` is its own user's, taken from that user's passwd entry and re-exported through the environment file (job #563) |
| A host node is single-tenant, enforced at the node and fail-closed | `HostTenancy` read in `HostBackend::admit`, `crates/container/src/host.rs`; parsed in `crates/worker/src/config.rs` | True. The list already accepts **several** projects — nothing in the parse or the admit forbids two |
| One host task at a time | `enforce_host_capacity`, `crates/worker/src/daemon.rs`, plus the running-task exclusion in `HostBackend::admit` | True |
| The task directory is created `0700` by the daemon, and every wire path is rebased into it | `create_task_dir` and `rebase_path`, `crates/container/src/host.rs` | True unbound. A bound task's directory is `0770` with the project user's own group, which is §7's scheme: the daemon still writes `meta.json` and reads the results, and no other project can reach it (job #563) |
| `remove` deletes the recorded files, the task directory, and this task's MCP-log subtree — under the home of the user the task **ran as** | `agent_cache_root` and `remove_all_as`, `crates/container/src/host.rs` | True since job #565. It used to read `HOME` out of the daemon's environment, which reclaimed nothing the moment the task's home was not the daemon's |
| Every delete and every signal a teardown performs escalates to the task user, the daemon-side pair a task's exit runs (`spawn_reaper`) and the boot sweep of a crashed `remove` included | `remove_all_as` / `signal_group_as` / `tree_user`, `crates/container/src/host.rs` | True since job #565. The escalation deletes and the daemon's own delete is the verdict; an unbound node is byte-identical to what it always did |
| The daemon's own state on a Mac lives **under the login user's home** — `worker.env`, and `keys/worker.creds` / `keys/worker_git` beside it | the macOS branch of `deploy/prod/build-worker.sh` (`$NODE_HOME/chuggernaut-worker/…`); the launchd plist's `ENV_FILE` in `deploy/prod/install-worker-launchd.sh` | True. Measured 2026-08-10: the home is `0750` group `staff`, `worker.env` is `0644`, and a second uid whose **primary group is `staff`** reads it. The credentials at `0600` do not follow — the protection is the per-file mode, not the home |
| A host task execs the agent CLI by **bare name off the `PATH` it inherits**, and that `PATH`'s CLI directory is now **node-wide** | `AgentCli::discover_on` (`crates/worker/src/agent_cli.rs`) resolves it only to advertise the capability; the `PATH` is `AGENT_PATH`, rendered **twice** — in `deploy/prod/install-worker-launchd.sh` (hand-run) and in `deploy/prod/build-worker.sh`'s own macOS plist (what the deploy reaches a node with), both now defaulting to `…:/usr/local/lib/chuggernaut/bin`, with `deploy/prod/install-worker-launchd.test.sh` comparing the two defaults whole | True since slice 8 (job #571). It used to be `…:$HOME/.local/bin`, which a second uid reached **only** through the `0750`+`staff` traversal D12 removes. **The platform half only**: placing the CLI at that path is the operator's, and it must happen before a project user leaves `staff` |
| The cross-project secret boundary is **absent** on a Mac, accepted by job #526 | [#322](322-macos-native-runtime.md)'s 2026-08-09 correction | True today; this design is what replaces that decision |

## Decisions

| # | Decision | One-line rationale |
| --- | --- | --- |
| **D1** | **One unix user per project, `chug-{project}`, not a per-task pool.** | It maps one-to-one onto a list the node already declares and already fails closed, and it keeps within-project persistence — which [#309 §10](309-host-native-execution.md#10-trust-and-tenancy) calls the feature — while restoring the boundary between projects. |
| **D2** | **The daemon keeps its GUI-domain agent shape and escalates per launch through a node-provisioned `sudo` binding.** A root daemon is recorded as the endpoint, with three triggers, and is not this design's first cut. | The escalation is exactly the mechanism the measurement used, and **M1 has now run it from the daemon's own launch path** (2026-08-10) rather than by proxy; a root daemon would dissolve most of the work below but rewrites the one Mac's supervision two weeks before it carries the operator's iOS work. |
| **D3** | **The binding is told to the backend, never discovered by it** — a resolved `{project → uid, home}` map handed to `HostBackend::new` beside `Supervision`, `AgentCapability` and `HostTenancy`. | It is the pattern those three already establish in `crates/container/src/host.rs`: the daemon discovers node facts, the backend is told them and is testable without a node. |
| **D4** | **`launch`, `kill` and *every delete* escalate — including the two the **daemon** performs with no task left to ask, `spawn_reaper`'s teardown repeat and `sweep_detached`'s boot sweep. The read family does not, and the daemon's own exit-code write rides the group.** | A non-root uid cannot signal or unlink another uid's work at all, so the deletes are not a preference; reads and the exit-code write are the only places a permission bit can carry it, and job/181's rule says an unreadable result is an error and never an empty one. |
| **D5** | **No new boot gate, and no second roster: `WORKER_HOST_PROJECTS` is the roster.** The deploy refuses a listed project whose user does not resolve on the node; the daemon refuses that project's *launch* by name. What job #563 added beside it is an **on/off declaration**, `WORKER_HOST_USERS`, in `WORKER_KVM`'s shape — the roster is unchanged, and off is what makes the slice inert on a fleet with no users provisioned ([the record](#correction--2026-08-12-job-563-slice-1-lands-and-the-file-modes-c1-was-argued-to-be-stuck-with-are-not-the-ones-it-got)). | [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)'s do-not-advertise rule and the tenancy list would otherwise be two gates over one fact; the list already enumerates exactly the set of users that must exist. |
| **D6** | **The user name is derived, `chug-{project}` from the slug's second component, and a derivation collision is a hard parse error.** | A silent collision between `a/beacon` and `b/beacon` would hand two projects one uid — precisely the failure this design exists to prevent — and `parse_projects` already refuses a repeated entry in the same shape. |
| **D7** | **`WORKER_HOST_ROOT` moves out of the login user's home** to a node-wide root — `0711`, owned by the daemon's uid, **created by the operator with root** at provisioning, and traversable but not listable by every project user. **Necessary, and measured not sufficient** (M3): it is D12 that makes it a boundary. | The whole task directory hangs off that root, and the daemon creates its root at boot but cannot create one outside its own home, so the operator has to hand it one that already exists. The premise §[7](#7-task-directories-ownership-and-every-call-site-that-changes) argued it from — *the home is `0700`, so no project user can traverse* — is measured **false**: `/Users/worksalot` is `0750` group `staff`, and the move relocates state the project user could read anyway until D12 lands. |
| **D8** | **Signing needs nothing from this design** (operator, 2026-08-10). Real builds are signed by **fastlane** from keys supplied as ordinary **secrets**; local work uses ad-hoc debug keys. The per-project file keychain this row used to record as the direction is **retired**, and slice 6 closes with it. | `match` installs certificates into a keychain fastlane creates and unlocks itself, and an Android upload keystore arrives inline as base64 — so nothing reads the login keychain section 1 measured absent, and a session-less task user is not disadvantaged. [The record](#correction--2026-08-10-job-558-signing-is-answered-fastlane-from-secrets-so-d8-closes-and-slice-6-with-it). |
| **D9** | **Provisioning is the operator's, by runbook, and it is not symmetric.** Creation over ssh works; directory-services deletion is refused (`eDSPermissionError -14120`). Every procedure is idempotent against a stale account record. | A platform that assumed it could delete a user would be assuming an access path the operator measured as absent. |
| **D10** | **[#534](309-host-native-execution.md#phasing)'s deferred cache namespacing is retired by construction, and so is the *placement* half of its eviction slice. The ceiling and the LRU are not.** | Per-user homes make the collision impossible rather than avoided, and they move the caches out of the operator's own home — which was the stated reason placement had to precede eviction. |
| **D11** | **Host-mode docker stops being ambient.** [#517](517-docker-access-for-jobs.md) D1 is untouched — jobs may use docker — but the host half's default inverts from granted-for-free to unreachable-unless-granted. | That is #517 D4's own S6 arriving early: per-task users were named there as the **only** mechanism that can withhold host-mode docker, and a uid change withholds it whether or not anyone intended it to. |
| **D12** | **A project user's primary group is not `staff`, and the agent CLI moves to a node-wide path in the same act.** One decision, two directions: §[6](#6-provisioning-and-the-two-gates-smell)'s provisioning sets `chug-{project}` as the **primary** group rather than adding it beside `staff`, and the CLI moves the way `chuggernaut-channel-host` already did ([#490](490-agent-work-on-a-mac.md) D2 — the node-local path, not D3's discovery of it). **The CLI half landed first** (slice 8, job #571): the path is `/usr/local/lib/chuggernaut/bin`, and the group half is still slice 4's to write. | The shared `staff` primary group is load-bearing in **opposite** directions: it is what lets a project user read `worker.env` (M3, the thing this design exists to stop) and what lets it exec `/Users/worksalot/.local/bin/claude` (M8, the thing agent host work needs). Fixing M3 alone silently breaks agent host work, so the two are one decision or neither. |

## Slices

| # | Slice | Contract changed | Depends on | State |
| --- | --- | --- | --- | --- |
| 0 | `human` — the measurements M1–M8 below, on `gumbo-air-0`, with no platform change: M1 and M2 ride an existing `mode: host` job type's own script | none | — | **Landed** (job #561). M1–M8 were **taken by job #557** on 2026-08-10 from a `mode: host` task the daemon spawned; that job merged nothing, so this row carries the number of the job that recorded it. **M1 passes** and C1 is viable; M2 and M3 changed a decision each ([the record](#correction--2026-08-10-job-561-slice-0-is-measured-m1-passes-and-the-staff-primary-group-is-load-bearing-in-two-directions)). One question is left open and is **not** closed by this row: M1 has not been taken with no console session |
| 1 | `code` — resolve the per-project binding at boot and hand it to `HostBackend`; `launch` escalates **and hands the composed environment over as a file, not as environment** (M2); the task's `HOME` and task directory follow the task user | `HostBackend::new` signature, the host launch path (`crates/container/src/host.rs`) | 0 (M1, M2, M3) | **Landed** (job #563). `WORKER_HOST_USERS` is the node's declaration and D5's roster stays `WORKER_HOST_PROJECTS`; the environment file is the task user's own `0600` because the injected files are written **through** the escalation, which §3 assumed C1 could not reach ([the record](#correction--2026-08-12-job-563-slice-1-lands-and-the-file-modes-c1-was-argued-to-be-stuck-with-are-not-the-ones-it-got)). Inert until a node declares a binding |
| 2 | `code` — every delete escalates: `kill`, `remove`, **and the daemon-side pair a task's exit already runs without it** — `spawn_reaper`'s `reclaim_credentials` + `reclaim_agent_cache`, and `sweep_detached` at boot; `reclaim_agent_cache` follows the **task** user's home rather than the daemon's | `ContainerBackend::kill` / `remove` on the host backend, and the reaper's teardown repeat | 1 | **Landed** (job #565). One escalation shape for all of them, resolved out of each tree's own `meta.json` where no launch is left to ask; the escalation deletes and the daemon's own delete is the verdict, so a leak names both the path and the escalation ([the record](#correction--2026-08-12-job-565-slice-2-lands-and-the-listing-is-a-delete-too)). Inert until a node declares a binding |
| 3 | `deploy` — `build-worker.sh` refuses a listed project whose user does not resolve on the node; `WORKER_HOST_ROOT` guidance and `deploy/prod/env.example` follow D7, including that the root is now an operator precondition on macOS | the node run spec | 1 | Proposed |
| 4 | `docs` — a provisioning runbook (create the user, the group **as the user's primary group** per D12, the home, the `sudoers` line and **the `WORKER_HOST_ROOT` root itself**; verify; decommission; and the deletion asymmetry); `docs/reference/runbooks/worker-host-projects.md` §2 stops arguing single-tenancy as the boundary | runbook set | 3, 8 | Proposed |
| 5 | `design` — amend [#322](322-macos-native-runtime.md)'s job #526 correction and [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)/§10 with what this replaces | design record | 1 | **Landed** (job #567). Both siblings carry the supersession and both say it is a supersession **in design**: of #526's three bounds one is replaced, one moves its enforcement site to slice 2 — which that amendment recorded as unlanded and which has since landed — and one is untouched; §8's option (c) is recorded as a requirement and §10's list as the roster ([the record](#correction--2026-08-12-job-567-slice-5-the-siblings-carry-the-supersession-and-one-composition-claim-does-not-survive-the-landed-code)) |
| 6 | signing — formerly *deferred, once D8's operator input exists* | none | — | **Closed** (2026-08-10). D8 is answered and no platform work survives it; nothing smaller is left to do. [The record](#correction--2026-08-10-job-558-signing-is-answered-fastlane-from-secrets-so-d8-closes-and-slice-6-with-it) |
| 7 | deferred — the cache ceiling and LRU eviction inherited from #534(b) | node cache policy | 1 | Deferred |
| 8 | `deploy` — D12's other half: the agent CLI moves to a node-wide path and the daemon's rendered `PATH` follows it, so a project user outside `staff` can still exec it. **Must land before slice 4's provisioning**, or agent host work breaks the moment a project user stops being a member of `staff` | the node run spec — `AGENT_PATH` in **both** `deploy/prod/install-worker-launchd.sh` and `deploy/prod/build-worker.sh`'s macOS plist, plus the one-`PATH` assertion in `deploy/prod/install-worker-launchd.test.sh`, which pinned the login user's `~/.local/bin` | — | **Landed** (job #571). Both renderings tail at `/usr/local/lib/chuggernaut/bin` and neither carries a home directory, so the defaults are one string and the suite compares them whole. **The operator must place the CLI there before a project user is taken out of `staff`** ([the record](#correction--2026-08-12-job-571-slice-8-the-agent-cli-is-node-wide-and-the-move-is-an-ordering-not-just-a-path)) |

**Slice 0 gated every other row**, and it is the one that could have failed in a
way that sent this design to its rejected alternative. It did not: slice 1 is
unblocked, and slices 3 and 4 carry D12 with them. What slice 0 did **not**
settle is M1 under no console session — recorded as an open question rather than
as a row, because it is the same measurement asked of a state of the machine.

## What was measured first, and what it answered

None of these was answerable from this workspace. Each names what it decides, so
a failing row changes a decision rather than producing a note — and two of them
did. **All eight were taken by job #557 on `gumbo-air-0`, 2026-08-10**, from a
`mode: host` task the daemon spawned; the record — what each answer changes, and
the one it leaves open — is
[below](#correction--2026-08-10-job-561-slice-0-is-measured-m1-passes-and-the-staff-primary-group-is-load-bearing-in-two-directions).

| # | Measurement | Decides | Answer (job #557, 2026-08-10) |
| --- | --- | --- | --- |
| **M1** | A task **spawned by the daemon's own launch path** — not by ssh — drives CoreSimulator as `chug-probe`. Ride an existing `mode: host` job type: its script runs `sudo -u chug-probe -H` and reports `launchctl managername`, `simctl list devices`, `create`, `boot`, `bootstatus -b`. No platform change is needed to run it | D2. A failure here is what sends this design to the rejected shared-uid alternative | **Passes.** `sudo -n -u chug-probe -H` drove `simctl` to a device of its own; uid 501 and uid 502 each list 11 devices and share **0** UDIDs; boot reached `Finished`, and the device was deleted and verified gone. **Open, and it is M1's own**: the session was **Aqua**, inherited from the login user's console session — the headless case is untested |
| **M2** | Does `sudo` succeed non-interactively from that path (no tty, `NOPASSWD`), and **does the composed environment survive it**? `sudo` resets the environment by default | Whether the task environment must be handed over as a `0600` file the wrapper sources — [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)'s option (c) arriving as a requirement rather than a follow-up | **Succeeds; the environment does not survive.** `env_reset` strips `CHUG_*` and `DEVELOPER_DIR`; `-H` sets `HOME` and `env_keep` preserves `PATH`. Option (c) is a **requirement**, not a follow-up |
| **M3** | The mode of `/Users/worksalot` and of the daemon's own state under it: can `chug-probe` read the worker environment file, the NATS creds it names, or the login keychain? | Whether the boundary is real. If the daemon's own home is world-traversable the uid buys nothing until D7 moves that state | **The home is `0750` group `staff`, and `chug-probe`'s primary group is `staff`** — so it traverses, and reads `worker.env` (`0644`). The `0600` credentials and the login keychain (behind `Library` at `0700`) are denied. Today's protection is the per-file mode; **D7 is necessary and not sufficient**, which is D12 |
| **M4** | Are installed simulator **runtimes** shared (under `/Library`) or per-user? | The provisioning cost per project — tens of GB per user if they are per-user, and a slice of its own if so | **Shared.** `/System/Library/AssetsV2`, ~7.9 G, `root:wheel`; only the device set is per-home. No runtime slice, and provisioning is not tens of GB per project |
| **M5** | Does a fresh uid build with Xcode without a per-user first launch — `xcodebuild -version`, then a simulator build | Whether provisioning a project user is one command or a procedure | **No first launch and no licence prompt.** `xcodebuild -version` reports Xcode 26.5 and a compile for `arm64-apple-ios18.0-simulator` succeeds. **Caveat**: no full `xcodebuild` project build was run, and `-version` does not hit the licence gate |
| **M6** | Can one project user read another's **argv** via `ps`? | Whether any secret may ever ride argv. Assume yes until measured; it is why M2's answer must not be "pass it on the command line" | **Yes.** `chug-probe` read `worksalot`'s full argv, including an injected marker. The assumption was correct, and it is what makes M2's file answer right rather than convenient |
| **M7** | Is colima's docker socket reachable to `chug-probe`? | D11's default, and whether beacon needs an explicit grant on day one | **Not reachable.** The socket is `0600` owned by `worksalot`, and `docker version` from uid 502 is denied. **D11 holds**, and beacon needs an explicit node-side grant on day one |
| **M8** | Can `chug-probe` exec the agent CLI the daemon discovered on its own `PATH` (`/Users/worksalot/.local/bin/claude`, [#490](490-agent-work-on-a-mac.md) D3/M3)? | Whether agent host work survives the uid change, or the CLI has to move to a node-wide path the way the channel binary already did | **Yes — and only via the traversal M3 wants removed.** It execs it (2.1.198, rc 0) through the same `0750`+`staff` path. Tighten the home or drop `staff` and the CLI must move; that is **D12**, and it is one decision with M3 rather than two findings |

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

## Correction — 2026-08-10, job #558 (signing is answered: fastlane from secrets, so D8 closes and slice 6 with it)

Appended by the job recording an **operator decision on signing**, taken on
2026-08-10 against the tree at `4bbdb52`. Nothing above the rule is edited: D8
and slice 6 are head rows and are rewritten to this, §[5](#5-signing) stands as
the argument that asked the question, and the answer is here.

**The decision.** Signing is out of the platform's way. Real builds are signed by
**fastlane**, with the keys supplied as ordinary **secrets**; local work uses
ad-hoc debug keys. Nothing needs the login keychain of an interactive session.

### The evidence, and it is secondhand

beacon is a separate repository and is **not** checked out in this workspace, so
the reading below is the operator's 2026-08-10 inspection and is marked as such,
the same way [#313](313-workload-identity-image-builds.md) marks its own beacon
facts. Every path is qualified with its repository because a bare one would be a
claim about *this* tree.

- `kasofsk/beacon:.github/workflows/ios-fastlane-deploy.yml` passes
  `match_password`, `match_service_account_key`, `app_store_connect_key_id`,
  `app_store_connect_issuer_id` and `app_store_connect_key_content` — all from
  repository secrets.
- `kasofsk/beacon:mobile/app/ios/fastlane/Matchfile` declares
  `storage_mode("google_cloud")` against the bucket `daekon-match-certs`.
- `kasofsk/beacon:.github/workflows/android-fastlane-deploy.yml` passes
  `android_upload_keystore_base64`, its three passwords, and
  `play_store_service_account_key` — again all secrets.

**Why that closes D8.** `match` fetches certificates from its storage backend and
installs them into a keychain **it creates and unlocks itself** from
`MATCH_PASSWORD`; Android's upload keystore arrives inline as base64 and is
written wherever the build wants it. Neither path reads a login keychain. So
section 1's `security list-keychains` result — only
`/Library/Keychains/System.keychain` for a uid that has never logged in — costs
the signing story nothing, and a task user with no console session is not
disadvantaged relative to the login user this design moves work off.

### What it changes in this document

- **D8 and slice 6 close.** The per-project **file** keychain §[5](#5-signing)
  records as "the direction" is retired rather than deferred: `match` creates and
  unlocks its own keychain per run, so provisioning one at the node would be a
  second mechanism doing the first one's job. Nothing smaller survives, which is
  why slice 6 is closed rather than reduced.
- **§[6](#6-provisioning-and-the-two-gates-smell)'s five root-requiring acts
  become four.** Item 4 — *the per-project file keychain, once D8's operator
  input exists* — is gone. Provisioning a project on a node is the user, the
  group, the `sudoers` line and the `WORKER_HOST_ROOT` directory, and slice 4's
  runbook writes exactly those.
- **§[5](#5-signing)'s cost claim survives and gets one line stronger.** "What
  the per-project user costs signing today: nothing" was argued from there being
  no working setup to break; it now holds for a better reason — the mechanism
  beacon actually uses is indifferent to which uid runs it. What the uid still
  buys signing is confinement: the keychain fastlane creates per run lands under
  `chug-beacon`'s home rather than in the home the daemon and the operator share.
  That is a bonus, not a requirement, and it is not a reason to take this design.
- **"[What this does not decide](#what-this-does-not-decide)"'s signing bullet is
  answered**, and only that bullet — the other five stand.
- **M5 keeps its own reason and loses its dependent.** It asks whether a fresh
  uid builds with Xcode without a per-user first launch, which decides whether
  provisioning is one command or a procedure; it was never a signing measurement.
  Slice 6's `0 (M5)` dependency goes with slice 6, and M5 stays in slice 0.
- **Correction 3's remaining half stops being load-bearing, without becoming
  false.** [#440](440-native-worker-daemon.md) D2 rests the GUI domain on
  CoreSimulator *and* the keychain. The first is falsified by section 1. The
  second is not falsified — a session-less user really has no login keychain —
  but no signing path this platform serves needs one, so it is no longer a reason
  to keep the daemon in the login user's GUI domain. Both premises of D2 are now
  spent, which strengthens **C2**'s case exactly as
  §[3](#c2--a-root-daemon-launchdaemon-spawning-with-setuid) says and changes
  nothing about C2's cost. Recorded, still not scheduled: the three triggers there
  are unchanged.

### What it does not settle

- **The secrets themselves.** Signing keys delivered as secrets are ordinary
  forwarded values — untimed, injected verbatim, and on a host task readable out
  of the running process by the same uid (§[4](#4-what-the-uid-restores-and-what-stays-shared),
  bound 3). This design narrows *who* that uid is and closes nothing about
  lifetime. Two of the names above are stored cloud service-account keys and are
  carried to [#529](529-secret-handling.md) as a candidate for
  [#313](313-workload-identity-image-builds.md)'s mechanism —
  [recorded there](529-secret-handling.md#candidate--2026-08-10-job-558-a-named-consumer-for-313s-mechanism-arriving-from-beacons-real-workflows),
  as a candidate and not as a decision.
- **Whether a host task can run fastlane at all.** Nothing here was run on a
  node. #526's rung table still measures **zero** valid signing identities on
  `gumbo-air-0` in any session, and no job type in this repo declares any of the
  names above. "Signing does not block the uid change" is what closed; "iOS
  release builds work on this fleet" is not, and no row here claims it.
- **Anything about which repository the keys live in.** They are beacon's, in
  beacon's secrets, and they reach this platform only if beacon's deploy becomes
  a chuggernaut job.

## Correction — 2026-08-10, job #561 (slice 0 is measured: M1 passes, and the staff primary group is load-bearing in two directions)

Appended by the job **recording** slice 0. The measurements are job #557's, taken
on `gumbo-air-0` on 2026-08-10 from a `mode: host` task the **daemon's own launch
path** spawned — which is the whole point of M1, and the gap
§[1](#1-the-measurement-and-the-line-under-it) drew its line at. Nothing above
the rule is edited except the head, which is where slice 0's row, D7's amendment,
the new D12 and the answered measurement table now live.

**Provenance, and it is one hop.** Job #557's raw archive — `report.md`,
`captures-M1-M8.txt`, `udids-501/502.txt` — is a task artifact, and a work
container reaches neither the platform API nor another job's outputs. So the
numbers below were transcribed from #557's work summary as it reached this job,
not read out of the capture. **Where the capture and this section disagree, the
capture wins** and the row is rewritten; the same rule
[the #558 correction](#correction--2026-08-10-job-558-signing-is-answered-fastlane-from-secrets-so-d8-closes-and-slice-6-with-it)
applied to its secondhand beacon facts.

### M1 — it passes, and the session it passed in is the caveat

`chug-probe` (uid 502, **never** console-logged-in) drove CoreSimulator under
`sudo -n -u chug-probe -H` from the daemon-spawned task: its own device set (uid
501 and uid 502 each list 11 devices, **0 shared UDIDs**), a boot reaching
`Finished`, then a delete verified gone. `sudo` succeeded non-interactively with
no tty, which is M2's other half.

The design is therefore **not** sent to
§[10](#10-the-rejected-alternative-one-shared-worksalot-uid)'s shared uid, and
**C1 — sudo from the existing agent — is viable rather than assumed**. That
section stays where it is: it is the record of a live alternative and the
fallback if the open question below goes the wrong way.

**One difference from §1's ssh probe, and it is the caveat rather than a detail.**
`launchctl managername` reported **Aqua** from the daemon path, where the ssh
probe saw `Background` — the daemon is a LaunchAgent in `gui/501`
(`deploy/prod/install-worker-launchd.sh` bootstraps into `gui/$(id -u)`) and the
login user is console-logged-in, so the task inherits the **daemon's** session,
not the target uid's. `launchctl print gui/502` still returns rc 125: uid 502 has
no GUI domain of its own, which is the property §1 measured and this run did not
change.

### M2 — the composed environment does not survive, so option (c) is a requirement

`sudo`'s `env_reset` **strips** the composed environment — `CHUG_*` and
`DEVELOPER_DIR` among it. `-H` sets `HOME`; `env_keep` preserves `PATH`. So what
survives the escalation is the pair `floor_env` already carries from the daemon
(`INHERITED`, `crates/container/src/host.rs`) — by coincidence rather than by
design — and everything the launch composes **on top** of that floor is what is
lost. That is the whole of the task's declared environment.

`-E` / `--preserve-env` would recover it, at the cost of a `SETENV` grant in the
`sudoers` rule — which widens the binding D2 keeps narrow, and would put the
environment back in the daemon's hands rather than in a file. So the task
environment crosses **out of band**, and a `0600` file the wrapper sources is the
argv-safe default.

**What this changes in the body:**
§[3](#the-environment-must-not-cross-on-the-command-line)'s
"the environment must not cross on the command line" was argued; it is now
measured, and [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)'s
option (c) is a **requirement of the escalation**, not a later question. The mode
caveat that section already states is unchanged and still the price of C1: the
non-root daemon cannot `chown`, so under C1 the file is `0640` `chgrp`'d to the
project group, and only C2 makes it the `0600` this row would prefer.

### M3 and M8 — one decision, in two directions

Taken apart, these read as two findings. They are not, and the shared `staff`
primary group is why.

- **M3.** `/Users/worksalot` is `0750`, group `staff`, and `chug-probe`'s
  **primary group is `staff`** — so the home is traversable and `worker.env`
  (`0644`) is **readable** by it. `keys/worker.creds` and `keys/worker_git`
  (`0600`) are denied, and the login keychain is blocked by `Library` at `0700`.
  So the daemon's state is protected today by **per-file modes**, not by the home
  mode, and the boundary D7 promises does not arrive with D7: a project user
  needs a **non-`staff` primary group**.
- **M8.** `chug-probe` execs `/Users/worksalot/.local/bin/claude` (2.1.198, rc 0)
  — **only** through that same `0750`+`staff` traversal. It works because the
  task execs the bare name off the `PATH` M2 found preserved, and
  `AgentCli::discover_on` (`crates/worker/src/agent_cli.rs`) resolves the CLI only
  to advertise the capability, never to hand a launch a path.

Tighten the home to `0700` **or** drop the project user from `staff` — both of
which M3 recommends — and the agent CLI stops being executable. That is **D12**,
recorded as one decision: the CLI moves to a node-wide path the way
`CHANNEL_PATH_HOST` (`crates/container/src/lib.rs`) already did for the channel
binary ([#490](490-agent-work-on-a-mac.md) **D2** — its node-provisioned path;
D3 is the other half, the daemon *discovering* a CLI on its own `PATH`, and it is
what M8 above measures), in the same act that gives the project user a group of
its own.

**Two places in the body this corrects:**

- §[6](#6-provisioning-and-the-two-gates-smell)'s provisioning creates the group
  `chug-{project}` without saying it must be the user's **primary** group. On
  macOS the default primary group is `staff`, so provisioning as written leaves
  every project user reading `worker.env`. The list of root-requiring acts — four,
  since the #558 correction removed the keychain — keeps its count; item 2 gains
  the word *primary*, and slice 4's runbook is what writes it.
- §[7](#7-task-directories-ownership-and-every-call-site-that-changes) argues D7
  from a conditional — *if `/Users/worksalot` is `0700` (M3), no project user can
  traverse to a task directory at all*. The premise is **false as measured**: the
  home is `0750` and the project user is in `staff`, so today it traverses. D7's
  conclusion survives, for a different reason than the one written there: the
  root must move because D12 is about to remove that traversal, not because it is
  already absent.

**And a sequencing consequence, which is why D12 is a slice and not a note.** A
design that fixes M3 without moving the CLI breaks agent host work silently — the
node keeps advertising the capability its boot-time discovery found on the
*daemon's* `PATH`, and the failure surfaces inside a task as a missing binary.
Slice 8 lands the move; slice 4's provisioning depends on it. **Its work list is
two files and a suite, not one file**: `AGENT_PATH` is rendered independently by
`deploy/prod/install-worker-launchd.sh` and by `deploy/prod/build-worker.sh`'s
own macOS plist — the second is the one the deploy actually reaches `gumbo-air-0`
with — and `deploy/prod/install-worker-launchd.test.sh` asserts the pair carries
one `PATH` that resolves the CLI, so it moves with them. Editing only the
hand-run installer would fail that suite and change nothing on the node.

### M4, M5, M6, M7 — confirmations, and one caveat worth carrying

- **M4 — runtimes are shared.** `/System/Library/AssetsV2`, ~7.9 G, `root:wheel`;
  only the device set is per-home. §[4](#4-what-the-uid-restores-and-what-stays-shared)
  guessed `/Library` and the answer is its system sibling, which is the same
  conclusion: provisioning a project is **not** tens of GB, and the runtime slice
  M4 could have created does not exist.
- **M5 — a fresh uid builds.** `xcodebuild -version` reports Xcode 26.5 and a
  compile for `arm64-apple-ios18.0-simulator` succeeds, with no per-user first
  launch and no licence prompt. **The caveat travels with the answer**: a full
  `xcodebuild` **project** build was not run, and `-version` does not hit the
  licence gate. So "provisioning is one command" is measured for the toolchain's
  reach and not for a real build.
- **M6 — argv is readable across users.** `chug-probe` read `worksalot`'s full
  argv, injected marker and all. The design's assumption was right, and this is
  what makes M2's file answer *correct* rather than merely convenient: the
  obvious repair for a stripped environment is `sudo -u X env VAR=… cmd`, and it
  would publish every injected secret to the process table.
- **M7 — the docker socket is not reachable.** Colima's socket is `0600` owned by
  `worksalot`; `docker version` from uid 502 is denied. **D11 holds as measured**:
  host-mode docker inverts from granted-for-free to unreachable-unless-granted,
  and beacon needs an explicit node-side grant on day one, in
  `docs/reference/runbooks/worker-docker-grant.md`'s shape. #517 D2's rule is
  untouched — node-side, never a job-type field.

### What it changes in this document

- **The head, which is where slice 0's result belongs.** The `Status:` line, slice
  0's row (`**Landed**`, carrying job #561's number because job #557 merged
  nothing), D2's rationale, D7 — *necessary and measured not sufficient* — the new
  **D12**, the new **slice 8**, two rows of the current-state table, and the
  measurement table, which now carries an answer per row.
- **"[What this does not decide](#what-this-does-not-decide)"'s M8 bullet is
  answered**, and only that one. Agent host work survives the uid change
  *conditionally*, and the fix it anticipated — the CLI moving to a node-wide path
  — is D12 and slice 8 rather than a slice of its own to be decided later. The
  first bullet, *whether the daemon should become root*, is untouched in substance
  and closer in practice: the open question below can fire C2's first trigger.
- **Nothing in §§2, 5, 8, 9 or 10 moves.** M7 confirms §8 rather than changing it,
  M4 confirms §4's guess, and §10 stays recorded as the fallback it always was —
  though the open question below is **not** what would revive it.

### The open question, and it is the one that decides the design

**M1 passed in an Aqua session inherited from the login user's console session.**
The daemon path with **no console session** — a headless reboot, before anyone
logs in — is **untested**, and M1 is the row every other slice rests on. Neither
measurement has yet asked the question in its hard form: §1's ssh probe had no
GUI domain *of the target uid's* but ran on a console-logged-in machine, and
job #557's run inherited that console session outright.

**How it would be taken.** Reboot `gumbo-air-0` with no auto-login, leave it at
the login window, release the same `mode: host` job, and re-run the probe:
`launchctl managername`, `simctl list devices`, `create`, `boot`,
`bootstatus -b`, delete. Same script, one machine state changed. It needs an
operator at the node, which is why it is not a row in slice 0.

**Check the premise in the same visit, because it may make the question moot
under D2's shape.** `deploy/prod/install-worker-launchd.sh` writes the plist to
`$HOME/Library/LaunchAgents` and bootstraps it into `gui/$(id -u)` — a GUI-domain
agent, which is loaded by a graphical login. If that domain does not exist before
someone logs in, the daemon is not running either, and there is no launch path to
ask the question of: the node comes back **dead rather than half-working**, which
is the loud failure and not the silent one. That is reasoning from where the
plist lives, **not** a measurement, and it is worth one `launchctl print gui/501`
at the login window before the probe — the answer decides which of the two
outcomes below is the real one.

**What a failure would mean, plainly.** If the daemon *does* run headless and the
simulators do not, a build node that reboots unattended comes back serving host
tasks and unable to drive CoreSimulator until a human logs in at the console — an
outage invisible from the platform, because every layer above the launch looks
healthy. The repairs are the two already recorded and both need root: C3
(`launchctl asuser <uid>`), which places the command in the target uid's own
per-user domain, or C2, the root daemon. So a failure here does not revive
§[10](#10-the-rejected-alternative-one-shared-worksalot-uid) — a shared uid
inherits the same absent session — it moves **C2 from recorded to scheduled**,
which is the first of the three triggers §3 already names. If instead the daemon
does not run at all without a login, this design is not what is exposed: that is
a property of D2's LaunchAgent shape, it predates every slice here, and it is
[#440](440-native-worker-daemon.md) D2's to own. Until one of the two is
measured, treat unattended reboot as unproven on this node.

### Still unmeasured, and named so it is not read as settled

- **A full `xcodebuild` project build** as a project user (M5's caveat, above).
- **Directory-services deletion** of a project user, which
  §[1](#1-the-measurement-and-the-line-under-it) and D9 already carry as refused
  (`eDSPermissionError -14120`); nothing here re-tested it.
- **Anything about a second project actually running.** `chug-probe` is a probe
  user, not `chug-beacon`: no project's work has been run under a project user,
  and slice 1 is what makes that possible.

## Correction — 2026-08-12, job #571 (slice 8: the agent CLI is node-wide, and the move is an ordering, not just a path)

D12's other half is landed, ahead of every platform slice and ahead of slice 4's
provisioning, which is the whole point of it being a slice.

**What landed, and it is two lines of `PATH` plus the suites that pin them.**
Both macOS renderings of `AGENT_PATH` — `deploy/prod/install-worker-launchd.sh`
(hand-run) and `deploy/prod/build-worker.sh`'s own plist (what the deploy reaches
`gumbo-air-0` with) — now tail at **`/usr/local/lib/chuggernaut/bin`** instead of
the login user's `~/.local/bin`. Nothing else about
[#490](490-agent-work-on-a-mac.md) D3 changes: the daemon still discovers a bare
`claude` on its own `PATH` at boot, still advertises `agent_cli`, and still
refuses an agent-shaped host launch **by name** when it found none.

**Why that path.** It is the directory shape `CHANNEL_PATH_HOST`
(`/usr/local/lib/chuggernaut/chuggernaut-channel-host`) already has — the
platform's own node-local tree, outside every home, traversable by any uid — and
it is a *directory of its own* rather than `/usr/local/lib/chuggernaut` itself,
because that `PATH` is every host task's too and putting the lib directory on it
would make `chuggernaut-channel-host` and `worker-refresh.sh` bare-name
resolvable in every task. It keeps the **tail** position slice 4 of #490 chose,
for the reason that slice gave: a directory ahead of `/usr/bin` silently
reselects `git` or `ssh` for work that never asked.

**The ordering, which is the substance of this slice.** The move buys nothing on
its own — it is what makes D12's *other* direction safe to apply. So, plainly:
**the operator must place the CLI at `/usr/local/lib/chuggernaut/bin/claude`
before any project user is taken out of `staff`** (or `/Users/<login>` is
tightened to `0700`). Until then a converted mac keeps working exactly as M8
measured, through the traversal; after the group change, a CLI reachable only
under the login user's home is one the task's uid cannot exec. The runbook step
is in `docs/reference/runbooks/chug-node-adoption.md`, and it wants a real file
rather than a symlink into a home — a symlink resolves to the same denied path.

**What this deliberately does not do.**

- **It does not install the CLI, and it does not create the directory.**
  Installation is D3's operator step and stays one; the deploy writes only what
  it builds. A node whose directory is absent or empty is exactly a node with no
  CLI, which is the case #490 D5 already handles.
- **It does not make an already-installed agent follow.** A plist keeps the
  `PATH` it was rendered with, so a converted mac needs the installer re-run (or
  a deploy that re-renders the plist) after the CLI is placed — the same caveat
  #490 slice 4 carried, restated because the path it applies to has moved.
- **It leaves exactly one CLI directory on that `PATH`**, on purpose. Keeping
  `~/.local/bin` beside the node-wide entry as a fallback would let the boot
  probe advertise a CLI a project user cannot exec, converting D5's refusal by
  name into a failure inside the task — the silent shape this design's §7 is
  written against. Both suites assert the absence, not just the presence.

**What the suites pin now.** `deploy/prod/install-worker-launchd.test.sh` case 1b
asserts the node-wide directory is on the rendered `PATH`, that the login user's
is not, and — new — compares the two renderings' `AGENT_PATH` defaults **whole**
rather than grepping the deploy for a substring, which is possible only because
neither default carries a home path any more and the two are one string.
`deploy/prod/build-worker.test.sh` case 2s asserts the same pair on the plist the
deploy actually renders. Both were red against the unfixed tree.

## Correction — 2026-08-12, job #563 (slice 1 lands, and the file modes C1 was argued to be stuck with are not the ones it got)

Appended by the job that **implemented** slice 1. It changes nothing about the
decisions: C1 is the mechanism, the environment crosses in a file, the binding is
told to the backend. What it corrects is two places where the body predicted a
cost the code did not have to pay, and one place where it under-specified how a
node says yes.

### The declaration, which the body left implicit

D5 collapsed §8's provisioning gate into `WORKER_HOST_PROJECTS` and said nothing
about how a node turns the binding *on*. Read literally that makes every listed
project's launch refuse the moment this code ships — the whole fleet, before any
user exists anywhere — which is the outcome D5's own boot-refusal argument is
against.

So the slice added an on/off declaration beside the roster rather than a second
roster: **`WORKER_HOST_USERS`**, parsed in `WORKER_KVM`'s exact shape (`1`/`0`,
anything else a hard config error). Off — every node today — resolves nothing,
binds nobody, and composes byte-identical launches. On, the daemon derives
`chug-{project}` for each project the roster already names (D6, unchanged),
resolves each through `getpwnam_r` in its own view, and hands
`HostBackend::new` the result. **The fail-closed half is unchanged and is what
the roster now means**: a project the node declared and could not resolve refuses
its launches by name, and the derivation collision D6 asks for is a hard config
error at parse — but only when the declaration is on, because with it off two
same-named projects already share the daemon's uid and nothing here changes that.

### The modes: `0640` was the price of C1, and it turned out not to be

§[3](#the-environment-must-not-cross-on-the-command-line) and
[the M2 correction](#m2--the-composed-environment-does-not-survive-so-option-c-is-a-requirement)
both concluded that under C1 the environment file is `0640` `chgrp`'d to the
project group, because *"a `0600` file is owned by whoever wrote it, and under C1
the writer is the non-root daemon, which cannot `chown` it to the task user"*.
The premise is true and the conclusion does not follow: the daemon cannot
`chown`, but it **can write as the task user through the same escalation the
launch takes** — `sudo -n -u {user} -H sh -c 'umask 077; cat > "$1" && chmod
"$2" "$1"'`, contents on **stdin** so nothing sensitive rides argv (M6), the path
and the mode on argv because neither is a secret.

So the environment file is `0600` **owned by the task user**, which the body
records as available only under C2. The same move fixes something neither
section noticed: an injected `0600` ssh key written by the daemon would be
unreadable to the task that has to use it, and one widened to `0640` is a key
OpenSSH refuses outright. Every injected file is materialized through the
escalation instead, so a bound launch's credentials keep their declared modes and
their declared owner.

**What did not change is the directory.** §7's `0770` group-shared task
directory stands exactly as argued: the daemon has to write `meta.json` and read
the task's results, so the directory cannot be `0700`-owned-by-the-task without
C2. `HOME` is the bound user's own, out of its passwd entry, and the wrapper
takes `umask 007` so what the task creates inside stays readable to the daemon
that harvests it and to no other project.

The mode applies to **every** level the launch creates under that directory, not
only the one an injected file's parent names. Rust's `DirBuilder::recursive`
applies its mode to each component it creates, so binding the leaf alone would
leave `chuggernaut/cloud/` `0700` and daemon-owned while
`chuggernaut/cloud/{identity}/` was `0770` — a level the task user cannot
traverse, which makes a nested credential (`auth::workload`'s token and ADC
documents) unwritable at launch and unreadable if it were written. The
one-level paths that motivated §7 hid it: `chuggernaut/` is pre-created bound
before any file is materialized.

### What the slice does not do, so the row is not read as more than it is

It **takes effect at the next deploy and does nothing until a node declares a
binding.** No node sets `WORKER_HOST_USERS`, `deploy/prod/build-worker.sh` does
not forward it (slice 3), no node has the users or the `sudoers` line (slice 4),
and the `sudo` path has never run from this code — M1 measured the mechanism from
a `mode: host` job's own script, not from `spawn_task`. Teardown is untouched:
`kill`, `remove` and the daemon's own post-exit deletes still run as the daemon,
which is slice 2, and the environment file is recorded in the launch's
`meta.json` `files` so slice 2 reclaims it rather than re-deriving its path.

## Correction — 2026-08-12, job #567 (slice 5: the siblings carry the supersession, and one composition claim does not survive the landed code)

Appended by the job that **wrote** slice 5. It decides nothing new: the work was
to make two sibling documents say what this design replaces, and the one thing
worth appending here is what writing them turned up.

**What was written where.**
[#322's 2026-08-12 amendment](322-macos-native-runtime.md#amendment--2026-08-12-job-567-per-project-users-supersede-this-decision-in-design-which-of-the-three-bounds-survives-and-what-is-not-yet-achieved)
takes its job #526 correction bound by bound; the
[#309 amendment](309-host-native-execution.md#amendment--2026-08-12-job-567-8s-pool-is-the-wrong-granularity-for-macos-its-option-c-is-a-requirement-there-and-10s-list-becomes-the-roster)
takes §8 clause by clause and then says what §10's list is for. Neither section
is reworded — both documents are append-only below their heads — and both heads
gained a pointer instead.

**The answer to "which of #526's three bounds survives", stated once here so it
is not only in the sibling.** *Single-tenancy* is **replaced** as a security
control and **kept** as the roster of users that must exist, keeping §10's
hostile-flake job that no uid touches. *Exit-time deletion* **survives** with its
task-side half unchanged and its daemon-side half moved into slice 2, which has
not landed. *Short credential TTLs* are **untouched**, and the forwarded-secret
open item is narrowed in readership and unchanged in lifetime. **None of the
three becomes unnecessary** — bounds 2 and 3 are exactly the ones that hold
*within* a project user, which is the residue §[2](#2-per-project-not-per-task)
already priced.

### One claim in this document does not survive its own implementation

§[3](#the-environment-must-not-cross-on-the-command-line) closes with *"It
composes exactly as [#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host)
predicted: the secrets stop riding `environ` into the shell that spawns the
task's own children, while still reaching the task itself."* **As landed, they do
not stop.** `env_file_body` emits `export NAME='…'` and `take_over_env` has the
wrapper source that file before `exec` (`crates/container/src/host.rs`), so the
composed environment is in the task shell's own `environ` and every child
inherits it exactly as before.

What the file **does** deliver is the property the escalation actually needs: the
environment survives `sudo`'s `env_reset` (M2) without crossing on argv (M6), and
it does so at `0600` owned by the task user. §8's option (c) is therefore a
**requirement of the escalation** — which is what slice 5 was asked to record —
and *not* the child-process hardening §8 filed it under. Getting that second half
would need the consumer-side change §8 priced as "every consumer must change";
**no slice here proposes it**, and this correction is not a proposal either.
Nothing about C1, D2 or the file's mode changes.

### What the amendments deliberately do not claim

- **That the boundary is achieved.** Both say, in the present tense, that slice 1
  is inert on every node and that what bounds a host task today is what #526
  recorded. Slice 4 additionally waits on slice 8's landed path being **used**:
  the operator must place the agent CLI at `/usr/local/lib/chuggernaut/bin`
  before any project user leaves `staff` (D12), or agent host work breaks at the
  moment the group changes.
- **That M1 is finished.** The headless case is carried into #322 as an operator
  **deferral** dated 2026-08-10, with the method (reboot with no auto-login,
  `launchctl print gui/501` at the login window, then the same `mode: host`
  probe) and the cost of a failure (a node that reboots unattended serves host
  tasks it cannot drive CoreSimulator from, healthy at every layer above the
  launch) — not as an oversight, and not as a reason
  §[10](#10-the-rejected-alternative-one-shared-worksalot-uid) revives.
- **Anything about [#517](517-docker-access-for-jobs.md) D1, the docker
  escalation, or [#529](529-secret-handling.md)'s decisions.** D11's inversion is
  restated in both siblings as a consequence of the uid and never as a new rule.

### What is left of "[What this makes wrong elsewhere](#what-this-makes-wrong-elsewhere)"

Of that list's six bullets, this slice discharges three and a half: the job #526
correction in [#322](322-macos-native-runtime.md), that document's §5
recommendation of a *dedicated task user with a login session* (superseded in
both halves — per project, and no session),
[#309 §8](309-host-native-execution.md#8-secrets-on-a-shared-host), and §10's
half of the fourth bullet. **Still owed:**
`docs/reference/runbooks/worker-host-projects.md` §2, which is slice 4's;
[#490](490-agent-work-on-a-mac.md)'s M5 fork, whose two reasons the list already
separates and which no slice here has been asked to amend; and
[#517](517-docker-access-for-jobs.md)'s host-mode table, whose *default: granted,
for free* row stops being true only on a node that binds users — so it is
correct until slice 4 lands, and amending it now would be the error this design
keeps warning about.

## Correction — 2026-08-12, job #565 (slice 2 lands, and the listing is a delete too)

Appended by the job that **implemented** slice 2. D4 is unchanged and every
surface §[7](#7-task-directories-ownership-and-every-call-site-that-changes)
named is escalated, including the two the daemon performs with no task left to
ask. What it corrects is three places where the body under-specified *how* a
delete crosses the boundary, and one surface it did not name at all.

### The listing is a delete too, and §7 only said the delete was

§[7](#the-read-family-and-one-concrete-break) says the MCP-log sweep "must follow
the **task** user's home, and — since it is a delete — through the escalation."
Following that home is what makes the *listing* impossible for the daemon:
`sweep_agent_cache` reads the cache root to match on the task directory's own
name rather than computing the CLI's key, and that root now sits inside a home
the daemon has no business traversing. A sweep that escalated only its deletes
would list nothing, match nothing, and report a clean pass — which is the exact
silence [#490](490-agent-work-on-a-mac.md) D6 exists to prevent, arriving through
a different door. So the listing escalates too (`find … -mindepth 1 -maxdepth 1
-print0`, NUL-separated because a subtree name is the CLI's slug of a path and
nothing else can be ruled out of it).

This is the only *read* this design escalates, and it is not a hole in D4's read
family: `copy_file`, `find_file`, `logs` and `inspect` all read **inside the task
directory**, which the `0770` group mode is exactly what makes reachable. This
one reads inside the task user's **home**, where no group carries the daemon.

### The escalation empties; the daemon's own delete is the verdict

An escalated `rm -rf` of a task tree **cannot succeed**, and the body does not
say so. Unlinking `host-{stamp}-{n}` from the host root needs write permission on
that root, which is the daemon's and not the task user's — so the escalation
deletes everything *inside* the tree and then fails on the last step. Ordering
the two the other way round is what makes this readable rather than a permanent
false failure: the escalation empties what the daemon cannot descend into, the
daemon then unlinks what only it can, and **the daemon's own result is the
verdict**. A refusal is carried into that verdict rather than discarded, so a
real leak names both the path and the escalation that could not reach it, and an
expected final-unlink refusal names nothing because there was no leak.

The same ordering answers the agent cache, where the parent *is* the task user's
own directory: there the escalation unlinks the entry itself and succeeds, and
the daemon — which may not even be able to `stat` the path afterwards — is never
asked.

### A delete falls back to the daemon; a launch still does not

[`HostUsers::refusal`](#the-methods-that-escalate) refuses a launch whose binding
will not resolve rather than running it as the daemon, because that fall back
would silently restore the shared uid this design removes. **A teardown falls
back deliberately**, and the asymmetry is not an inconsistency: a delete the
daemon performs can reach nothing the daemon could not already reach, so falling
back widens no boundary — it only recovers the reclaim a node with a missing
`sudoers` line would otherwise leak silently. `kill` falls back the same way, and
says so at `error!` when it does.

### Which teardowns are told the binding, and which resolve it

`spawn_reaper`'s pair is handed the launch's own resolved user, because the
launch that spawned it knew. `kill`, `remove` and the boot sweep resolve it out
of the tree's **own `meta.json`** — `project`, through the node's binding — since
the daemon that launched the task may have been replaced (spec §3.1's drain
guarantee) or may be booting into someone else's leftovers. That is why the boot
sweep needed no new state: `meta.json` already recorded the project, and slice 1
already recorded the environment file in the same record's `files`, so
[#529](529-secret-handling.md)'s rule that cleanup covers what the platform
placed is satisfied by reclaiming what is recorded rather than by re-deriving a
path.

The daemon's own **exit-code write** still does not escalate, exactly as
§[7](#the-daemons-own-deletes-which-no-task-is-left-to-perform) argues: it rides
the task directory's group like the read family.

### What is asserted, and what still cannot be

The escalation itself has still never run — no node has the `sudoers` line
(slice 4) and no gate host has a second uid to become — so this slice is tested
exactly where slice 1's `write_as` is: the composition, and the pieces around it.
`every_teardown_escalates_through_the_launchs_own_shape` asserts each script's
argv is the launch's own `escalated` shape;
`the_escalated_teardown_scripts_do_what_they_claim_on_this_shell` runs all three
scripts under `/bin/sh` **without** the escalation, which is what catches the one
form that would go silently wrong — `kill -s TERM -- -{pgid}`, measured working
on `dash` (the gate) and `bash` (`/bin/sh` on the Mac);
`the_mcp_log_sweep_follows_the_task_users_home_and_not_the_daemons` asserts the
home resolution and sweeps a bound cache end to end through the fallback; and
`a_teardown_that_cannot_delete_names_what_it_leaked` asserts the leak names the
path and the escalation. What no test on this fleet can show is a delete
*succeeding only because* it escalated — that needs the node slice 4 provisions,
and it is the first thing to check there.

### What the slice-5 siblings say about this slice, which was true when they said it

Slice 5 (job #567) landed hours before this one and its two sibling amendments
record slice 2 as **not landed**, in present tense, inside append-only bodies
neither this job nor any later one rewrites:
[#322](322-macos-native-runtime.md)'s "the daemon-side half … is #537's slice 2,
still Proposed", and [#309](309-host-native-execution.md)'s §8 residual-risk row.
Both are dated records of 2026-08-12 and both are correct as of the correction
that carries them; **the slice table at the head of this document is the
authority on what has landed**, and it is what a reader chasing either sentence
arrives at. Nothing else in those amendments turns on the ordering — what they
each argue about the uid boundary is unaffected, because slice 2 landing is what
they said the boundary needed.

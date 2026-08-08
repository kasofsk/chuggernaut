# Design — agent work on a Mac

Status: PROPOSED — D1 through D7 decided, seven slices, nothing implemented.

Written against the tree at `d556a6c` (job #489's `code` merge, the last commit
touching source before this branch): every claim below was read out of the
source named beside it, not inferred from the docs. **No host task of any kind
has ever run on real hardware**, so with one exception the whole document is
reasoning over a tree, not observation of a fleet. The exception is D1's
slugifier measurement, which was run against an unauthenticated CLI and is
labelled where it appears. [What must be measured on the air
first](#what-must-be-measured-on-the-air-first) gathers every remaining
unverified fact, the decision it holds up, and what that decision becomes if it
turns out false.

This document is [#322](./322-macos-native-runtime.md) P2's **agent half**,
which that design files as *"Later, deliberately"*. It is being taken up early
because the requirement arrived: **agentic debugging against the iOS
simulator**. That is agent work, on a Mac, holding global simulator state — the
three things #322 phase 1 excluded on purpose.

Everything #322 says still holds; this amends it rather than restating it. The
per-launch mode routing, the `/workspace` rebase and the `xcode:` scheme are
landed elsewhere ([#309](./309-host-native-execution.md) P0–P2, #322 W2, #322
W4) and are preconditions, not subjects.

## What phase 1 refuses, and why

`work.type: command` only, on a Mac, is enforced twice — the field rule
`validate_host_serves_commands_only` (`crates/types/src/job_type.rs`) and the
node's own `HostBackend::admit` refusal of a launch whose env sets
`CLAUDE_CONFIG_DIR` (`crates/container/src/host.rs`). #322 §2 states the reason:

> `agent::transcript_path` returns
> `{CLAUDE_CONFIG_DIR}/projects/-workspace/{session_id}.jsonl`, where
> `-workspace` is the agent CLI's **slugification of the cwd** — a measured
> property of an external CLI… Change the cwd to `{task_dir}/workspace` and the
> transcript lands elsewhere; the harvest silently finds nothing.

and names the failure class it will not accept:

> an agent host task is not self-correcting… the CLI is present, the run looks
> healthy, and the harvest finds nothing.

That design's proposed remedy was *"a **computed** slugifier replacing a
measured constant"*. D1 rejects the remedy while keeping the requirement.

## Decisions

### D1 — the transcript is resolved by session id, not by a computed path

**Decision: the backend gains an operation that answers "which files under this
directory are named `{session_id}.jsonl`", and the harvest copies the file that
resolves. The directory name is never computed and never assumed unique, and a
resolution that is not exactly one file is a refusal rather than a guess
([D1b](#d1b--how-absence-and-ambiguity-fail)).**

Four candidates were considered. The first three are #322's; the fourth is the
one measurement produced.

1. **Compute the slug.** Replicate the CLI's algorithm. Works — see the
   measurement below — but pins the platform to an external tool's undocumented
   behaviour, at a specific version, and fails **silently** when that behaviour
   moves. `crates/agent/src/lib.rs`'s own doc comment already records that the
   CLI's *published documentation disagrees with its behaviour*, which is the
   strongest available evidence that this is not a stable contract.
2. **Stabilise the cwd** so the slug stays constant, via a per-task symlink.
   Unnecessary once D1 is settled, and per-task mount namespaces — the version
   of this that would actually work — are impossible on macOS (#322 §2 rejects
   option (ii) on exactly that ground).
3. **Scan for the only directory under `projects/`.** `CLAUDE_CONFIG_DIR` is
   per task, so the directory should be unique. **Falsified by measurement**: a
   run from a second cwd inside the *same* config dir creates a *second*
   directory. An agent that changes directory and re-invokes the CLI breaks it,
   and it breaks by finding the wrong transcript rather than none.
4. **Resolve the file by session id.** Adopted.

**Why 4 is the right shape and not merely the surviving one.** The session id is
**ours** — the platform passes `--session-id` (`crates/agent/src/claude.rs`) and
the CLI names the transcript after it. So the question "what did the CLI call the
directory" never has to be answered. The only external contract relied on is one
the platform itself supplies, which is the difference between depending on a
tool's output and depending on its input.

**Measured, not reasoned** — CLI **2.1.224**, macOS 26.5.1, 2026-08-07. The
tree's existing constant was measured against 2.1.211.

| what was run | result |
| --- | --- |
| fresh `CLAUDE_CONFIG_DIR`, cwd `…/host-tasks/t-0001/workspace` | one directory in `projects/`, named `-Users-david--claude-jobs-…-host-tasks-t-0001-workspace` |
| second run, same config dir, cwd `…/t-0001/elsewhere` | **two** directories — candidate 3 falsified |
| `find projects -name "{session_id}.jsonl"` for each id | **exactly one match each** |

The slug rule `crates/agent/src/lib.rs` documents — every non-alphanumeric
character becomes `-`, including the leading `/` — **held** on a deep path:
`/.claude` became `--claude`, two adjacent non-alphanumerics becoming two
dashes. So candidate 1 is *feasible*; it is rejected for fragility, not for
being wrong today.

Two limits on this measurement, recorded because they bound what it proves. The
runs were **not authenticated** (`Not logged in`): path construction happens
before authentication, so the directory and file names are real, but no
transcript **content** was produced — M1 below is the re-confirmation. And it was
measured on the operator's Mac, not on `gumbo-air-0`; the paths differ, the
algorithm is what was under test.

#### D1a — where the resolution happens, and what it costs

The previous draft of this document said "glob
`{CLAUDE_CONFIG_DIR}/projects/*/{session_id}.jsonl`" and stopped. **That is not
implementable by the interface that fetches the transcript**, and the correction
is the substance of this decision rather than a detail of it.

`transcript_path` (`crates/agent/src/lib.rs`) returns a `String`, and its only
consumer is `Harvester::collect_agent` (`crates/platform-ops/src/harvest.rs`),
which calls `self.backend.copy_file(id, &path)`. That is an **exact-path**
contract on every implementation of it:

- Docker (`crates/container/src/docker.rs`) hands the path to the archive
  endpoint, which does not glob — a `*` component is a 404, mapped to
  `Ok(None)`.
- Host (`crates/container/src/host.rs`) rebases the path and calls
  `std::fs::read` — a `*` is an ordinary path component, `NotFound` is
  `Ok(None)`.
- The worker-RPC proxy carries the path verbatim on the wire
  (`crates/store/src/worker.rs`, `crates/worker/src/backend.rs`).

And `Ok(None)` in `collect_agent` is a `tracing::warn!` and a green run. A glob
written into that string would therefore have produced **exactly** the failure
this design exists to prevent: the CLI present, the run healthy, the harvest
empty. The naive reading was worse than the status quo, because it would have
broken the container path too.

**Decision: resolve first, then copy by exact path — two operations, not one
smarter one.**

- A new backend operation, `find_file(id, dir, name)`, returns the wire paths of
  every file under `dir` whose file name is `name`. It returns a list, because
  "several" must be distinguishable from "one" (candidate 3's falsification is
  what makes several reachable, and [D1b](#d1b--how-absence-and-ambiguity-fail)
  decides what happens to it). It is **bounded**: past a small cap it refuses
  rather than returning a longer list ([`docs/reference/style.md`](../reference/style.md)
  Tier 2 rule 3).
- The harvest then copies the single resolved path with the **existing**
  `copy_file_chunked`.

Three properties earn this shape over one `copy_file_matching` returning bytes:

1. **The reply-size problem is already solved, once.** `copy_file`'s worker-RPC
   reply is bounded by `MAX_COPY_FILE_BYTES` (`crates/store/src/worker.rs`) —
   a fraction of a NATS payload, well under a megabyte — and
   [`#362`](./362-binary-artifacts.md) S1 built `copy_file_chunked` precisely so
   a larger artifact still travels. `collect_agent` uses the **unchunked** call
   today, so on a worker-proxied node a transcript past that bound already comes
   back as a refusal rather than an artifact. D4 and D6 both say agent sessions
   run for hours, which makes that latent limit a certainty here. A resolve
   returning a short path crosses the wire trivially and the bytes ride the
   chunked path that already exists; one fat operation would have had to grow
   its own slicing.
2. **The exact-path contract survives untouched**, so `collect_output`'s archive
   read and `rebase_path`'s totality rule (#322 §2, landed job #485) are not
   renegotiated to make a transcript findable.
3. **The scan is node-local in both deployments.** On a Docker-endpoint node the
   tar streams in-process; on a worker node it streams inside `chug-worker`
   against its own docker. Only the resolved path crosses NATS.

The cost, stated: on Docker the archive endpoint is the only post-exit read —
the container has **exited** by harvest time, so `exec find` is not available —
so resolution streams a tar of `projects/` and reads its headers, and the
subsequent copy streams the member again. Two node-local passes over a per-task
directory holding one session's transcripts. That is the price of not computing
a name.

**Surfaces the slice touches**, so the contract is not mistaken for a
one-function change:

| surface | change |
| --- | --- |
| `crates/container/src/lib.rs` | `ContainerBackend::find_file`, and the `StubBackend` fake beside it |
| `crates/container/src/docker.rs` | stream the `dir` tar, collect matching member names, reassemble each into a wire path |
| `crates/container/src/host.rs` | rebase `dir`, walk it depth-bounded, map results **back** to wire paths — the inverse of `rebase_path`, which is one-way today |
| `crates/worker/src/backend.rs` | route: Docker in-process, worker over RPC |
| `crates/store/src/worker.rs`, `crates/worker/src/daemon.rs` | the RPC request/reply pair and its arm in the op match |
| `crates/test-utils/src/lib.rs` | the harness fake |
| `crates/agent/src/lib.rs` | `transcript_path` splits into the directory and the file name; the whole computed path survives only as D1a's fallback |
| `crates/platform-ops/src/harvest.rs` | resolve, then `copy_file_chunked` |

**No `WORKER_RPC_VERSION` bump, and the reason is a caller obligation.**
`crates/types/src/version.rs` states the rule — an additive op does not bump the
version, because a daemon that does not know an op answers rather than crashes.
`handle` in `crates/worker/src/daemon.rs` confirms it: an unrecognised op
returns an `unknown op` error reply. So the **caller** must degrade, and this is
the decision: on that error, fall back to today's computed exact path, and log
that it did. A rolling refresh is normal operation and every node in the fleet is
a container node whose computed path is correct; refusing them until they update
would buy nothing. The rejected alternative — bump the version and let an
unrefreshed node fail placement — is the right move for a *breaking* change and
this is not one.

**The claim the previous draft made about container behaviour is withdrawn.**
Generalising the lookup **does** change the container path: a refreshed node
resolves where it used to compute. That is why slice 1 lands and is proven on
ordinary container agent jobs before any host agent task exists, and why the
fallback above is part of the slice rather than a nicety.

#### D1b — how absence and ambiguity fail

The standing rule in #322 is that a silent empty harvest is unacceptable. Today it is
exactly that: `Ok(None)` is a `warn` and the job stays green.

Three cases have to be separated before anything can be made loud, and the first
two are distinguishable in the code that already exists:

- **No session id** (`out.session_id` is `None`) — the agent never started, or
  the provider is a fake. Not a defect. Unchanged.
- **A session id, and resolution returns nothing.** The CLI ran far enough to
  name a session and the file it must have written is not there. That is a
  platform break, not an empty run.
- **A session id, and resolution returns several.** The case D1a's list return
  makes representable.

**Decision: the second case escalates, reason `transcript_unresolved`, naming
the config dir and the session id — but it is armed only after M6 below.**

**Decision: the third case is treated as the second.** Several matches for one
session id is refused with the same reason code and the same count named — not
resolved by picking one. Two reasons, and the first is decisive: the artifact
store keys **one** transcript per task (`ArtifactKind::SessionTranscript` is a
single blob name, `crates/store/src/artifacts.rs`), so "several" has no
representable answer to fall back on. And any rule for choosing among them —
newest `mtime`, shortest path — would be a guess about a tool's behaviour, which
is exactly what D1 removed; a wrong-but-present transcript is the failure the
brief names as possibly worse than none. The cost, stated: a case that might be
benign becomes a loud miss, and the operator resolves it by reading the config
dir. That cost is small because the case is expected unreachable — the
measurement above found exactly one match per session id, and the id is unique
per launch — but "expected unreachable" is a reason to make it cheap to refuse,
not a reason to leave it undecided. `find_file`'s bound (D1a) is the same
refusal at a larger count; this makes the small counts agree with it.

The Harvester does not raise it and cannot: it holds a backend and an artifact
store, no `&mut Core` (`crates/platform-ops/src/harvest.rs`), and its own
charter says a job must never fail because its *reporting* failed. That charter
stays. So `collect_agent` **returns** the outcome and the dispatcher decides,
which is where every escalation is decided already (`Core::escalate`,
`crates/dispatcher/src/core.rs`).

The honest tension: escalating is a job-state change for a reporting miss, which
is the thing the charter exists to forbid. It is justified only because the
narrowed conjunction cannot occur on a healthy platform — and whether that is
true is not known, because nobody has checked whether a legitimate run can emit a
session id and no transcript. So the arming is staged. Until M6 says otherwise,
both refusing cases log at **error** with the reason code and store a
`transcript-missing` marker artifact: loud in the job's artifact list, visible
without reading a node's logs, and no state change. If M6 comes back "it
happens", the marker is the permanent answer and the escalation is never armed.

**The marker is a fourth `ArtifactKind`, and that is not a one-function change.**
`ArtifactKind` (`crates/store/src/artifacts.rs`) is a closed enum whose
`as_str`/`parse` round-trip **is** the stored object name, so a new variant
reaches the API's content-type match (`crates/api/src/routes.rs`), the union in
`web/src/api/envelopes.ts` — which is **hand-written**, absent from
`web/src/api/types.gen.ts`, so the Rust change does not propagate into it the way
[`docs/README.md`](../README.md) §2's generated client would — and the label map
in `web/src/components/TaskArtifacts.tsx`. The ripple is at least
compiler-checked at both ends: a Rust `match` and a TypeScript
`Record<ArtifactKind, string>` both fail on an unhandled variant, so slice 2's
surface list is a floor rather than a guess.

The cheaper alternative — append the marker to the harvested `Stdout` blob
instead of adding a kind — is **rejected on a property the tree already
guarantees**: the task-output endpoint serves the live tail and, after exit, the
harvested `stdout.log` *at the same byte offsets*, so a poller handing its offset
back never drops a line across the exit transition (`web/src/api/envelopes.ts`).
Appending bytes that were never on stdout breaks that identity to save an enum
variant.

### D2 — the host channel binary is node-provisioned, not injected

**Decision: a host-capable node keeps its own native `chuggernaut-channel` at a
node-local path and uses it for host launches. It is never injected, and the
launch carries no channel file at all.**

**Injection is not merely wrong here, it is impossible.** `rebase_path`
(`crates/container/src/host.rs`) maps `/workspace/*` and `/chuggernaut/*` and
refuses everything else with no fall-through — #322 §2's totality rule, landed
job #485. The channel binary is injected at `/usr/local/bin/chuggernaut-channel`
(`Core::channel_mcp`, `crates/dispatcher/src/exec.rs`), which does not map, so a
host launch carrying it would be refused outright.

That leaves injecting it under `/chuggernaut/`, or provisioning it node-side.
**Node-side, for a reason beyond it being #322 §6's stated direction:** injection
would make the **dispatcher** choose which of two binaries to send, and the
dispatcher deliberately does not reason about node capability. #309's whole
selector design keeps mode resolution at the node — the dispatcher says what the
task is, the node says how it runs. A node substituting its own binary keeps the
knowledge where the fact is.

**The binary already exists on a dual-mode Mac and is thrown away.**
`deploy/prod/worker-refresh.sh` compiles `--bin chuggernaut chuggernaut-channel`
natively and then installs the *image's* copy over it, because #480 established
that the **injected** binary must be a Linux ELF — a Mach-O is what no Linux
container can exec. So on the air today the Mach-O is built every refresh and
discarded. D2 installs it to a second path instead of deleting it.

**This partially reverses job #487, and the comment it reverses is worth
quoting** because it is correct for the mode it was written against:

> A host-only node compiles the daemon ALONE. The native `chuggernaut-channel`
> was always thrown away — the installed copy rides out of the image, because a
> Mach-O is what no Linux container can exec (#480) — and on this node there is
> **no container to inject one into either**, so building it spends the node's
> own cargo on a file nothing will read.

True while host mode is command-only. False once a host **agent** task exists,
because that task is the reader. So `NATIVE_BINS` must keep the channel binary on
a node that serves host mode, whether or not it also serves containers.

**The principle, stated so the next correction does not swing back.** #480's
rule generalises: *ask each binary the question its own executor asks.* The
injected binary's executor is a container, so it must be a Linux ELF for the
container platform, and #480's `e_machine` guard keeps asserting exactly that.
The host binary's executor is the node, so it must run on the node — and gets
the same `--version` style check every other node-installed binary gets. Two
binaries, two executors, two guards, neither one weakening the other.

One consequence is left to the implementing slice: the second install path and
its config variable, since `WORKER_CHANNEL_BINARY` names the container copy
today. **Whether a node lacking a runnable host channel refuses at boot or at
launch is not open** — [D5](#d5--the-nodes-refusal-changes-what-it-asserts-not-whether-it-exists)
decides it at launch, because the node sees launches and a boot-time refusal
would take a dual-mode node's container capacity down with it.

That a Mach-O `chuggernaut-channel` actually works as a stdio MCP server outside
a container is M2 below, and it is unverified.

### D3 — the CLI is discovered on the daemon's own `PATH`, and the operator installs it there

**Decision: the daemon probes for the agent CLI on its own `PATH` at startup,
advertises what it found in `NodeCapabilities`, and refuses an agent host launch
by name when it is absent. Installation is an operator step onto a directory that
`PATH` already carries — not a new responsibility for any module in this repo.**

The CLI is invoked as **bare `claude` on `$PATH`** (`crates/agent/src/claude.rs`),
not by absolute path. In a container that resolves because
`deploy/prod/Dockerfile.agent-rust` runs `npm install -g
@anthropic-ai/claude-code`. A host task has no image, so the resolution is the
daemon's own `PATH` — and on a Mac that list is explicit and short:
`AGENT_PATH` in `deploy/prod/install-worker-launchd.sh`, rendered into the
launchd plist, defaulting to the homebrew and system `bin` directories.

**The decision turns on that list**, which is why the previous draft was wrong to
say installation belongs in `nix/chug-node/darwin.nix`. That module installs no
software — it is options, assertions and a docker probe, and its own prose says
the macOS daemon is a launchd agent installed by
`deploy/prod/install-worker-launchd.sh` from a template. Worse, a nix profile
directory is **not** on `AGENT_PATH`, so a CLI installed the way that draft
proposed would have been invisible to the probe this decision designs, and D5
would then have refused every agent host launch by name. Loud, and the wrong
outcome.

What `AGENT_PATH` *does* carry is where a Mac's `npm install -g` and homebrew put
binaries. So the install needs no new machinery: the operator installs the CLI
the ordinary way and the probe finds it. If a future node prefers nix, that slice
extends `AGENT_PATH` in the same commit — the rule is that installation and
`PATH` move together, never one without the other.

Discovery rather than operator-typed config reuses
[#322](./322-macos-native-runtime.md) W4's shape deliberately. W4 discovers
installed Xcodes because [#309](./309-host-native-execution.md) rejects config
that *"relocates a physical fact into config that goes silently wrong after a
rebuild."* Whether a node has a working agent CLI is the same kind of fact.

**What D1 bought here.** Pinning the CLI version used to be correctness-critical:
`transcript_path` was measured against a specific release, so a bump could
silently move the transcript and empty the harvest. D1 removes that dependency —
the transcript is resolved by a session id the platform supplies — so version
pinning drops from *correctness* to *reproducibility*. That is the practical
dividend of not depending on someone else's undocumented output.

**The refresh collision is already bounded, and the bound has a cost.**
[#440](./440-native-worker-daemon.md) slice 3 (job #460) declines a daemon swap
while host tasks run, asked at accept and again at the swap boundary *"because a
host task can"* start between them (`crates/worker/src/daemon.rs`). So a long
agent session does not get its daemon swapped underneath it. The consequence to
state rather than discover: **a multi-hour agent session blocks node updates for
its duration**, and with D4's one host task per node there is no way to drain
around it.

### D4 — one host task per node stays

**Decision: keep [#309](./309-host-native-execution.md) §2 option (iii).
`enforce_host_capacity` continues to require `WORKER_SLOTS = WORKER_SLOTS_MAX =
1` on a host-capable node. Agent work on a Mac does not unpin it.**

CoreSimulator holds **global device state**, which is what option (iii) exists
for; #322 §5 records it as the reason serialisation was chosen on the Mac
independently of the `/workspace` collision. Agentic debugging sessions are
long, so the cost of serialising is higher than for command work — and it is
still the right trade for the first phase, because the alternative is
[#309](./309-host-native-execution.md) §5b device leases plus #322 §5's
per-task device set, which is a larger design than this one and can be taken
later without rework.

What this buys: no lease acquisition, no per-task device sets, no teardown of a
device set on crash, and no new failure mode where two agents disagree about a
simulator. What it costs, stated plainly: **a long agent session occupies the
node's single host slot for its duration**, so a second host task queues behind
it. Note what serialisation does *not* buy — isolation. Simulator state one task
leaves behind is still there for the next; that is M7 below.

**What does *not* follow is that the job is blocked, and the reason is worth
being precise about.** Placement is per **task**, not per job: `place()` is
called once per launch with that launch's own required mode
(`crates/worker/src/backend.rs`), and [#361](./361-per-run-placement.md) settled
that this needs no job-record field. A capability requirement belongs to a task;
a job is satisfiable when the **fleet** can place each of its tasks, not when
some single node can serve all of them. That is what makes #322's worked case
work at all — a host job type's work task goes to the Mac while the `ci`
evaluator `.chug/jobs/_defaults.yaml` appends carries an explicit image and is
placed on a container node. Host work, container CI, one job, two nodes.

So the mitigation here is task-level placement, **not** the host node also
serving containers. A host-only Mac would be no worse for ordinary work: it
would simply never be chosen for a container launch.

Revisit when serialisation actually bites, not before. The trigger is a real
queue of host work, not the anticipation of one.

### D5 — the node's refusal changes what it asserts, not whether it exists

**Decision: replace `HostBackend::admit`'s `CLAUDE_CONFIG_DIR` test with a
capability test. The node refuses an agent-shaped host launch when it cannot
serve one — no usable CLI (D3) or no host channel binary (D2) — naming which.
The refusal is at launch, not at boot.**

That design is explicit that the node-side refusal **stays permanently**, covering a job
type that reaches a host node by a `placement.node` pin the schema never saw. So
this was never "delete both guards" once the field rule lifts.

It is also the only test a node **can** apply, and that follows from placement
being per task ([D4](#d4--one-host-task-per-node-stays)): the node sees a launch,
never a job type. Asserting "this job type was allowed to do this" is not
available to it; asserting "I can or cannot serve this launch" is.

At launch rather than at boot, because a dual-mode Mac that cannot serve agent
host work can still serve every container launch and every command host launch —
and the air is dual-mode by constraint. A boot-time refusal would convert a
missing CLI into a lost container slot.

The field rule `validate_host_serves_commands_only`
(`crates/types/src/job_type.rs`) lifts in the same slice, since it exists only to
forbid what this design enables.

### D6 — credential lifetime is unchanged in mechanism, longer in duration

**Decision: keep #322's teardown as it stands. Record the longer window; add no
rotation.**

That design bounds credential lifetime by the wrapper deleting `chuggernaut/` as
its first act after the command returns. An agent host task is the same process
shape — a command the wrapper wraps — so the guarantee holds structurally and
needs no new mechanism.

What changes is duration: an agent session runs for hours rather than minutes, so
the credentials sit on disk for hours. They remain inside the 0700 task directory
and outside `workspace/`, which is the property that matters — a stray `git add`
in the task's own tree cannot reach them. Rotation mid-run would add a failure
mode, a credential replaced under a running task, to shorten a window that is
already contained.

**The guarantee is only as good as its premise, and the premise is unmeasured.**
It assumes the agent CLI confines itself to `CLAUDE_CONFIG_DIR`. In a container
that assumption is free — anything it wrote elsewhere died with the container. On
a host there is no such boundary: a file the CLI writes to the daemon user's
`$HOME` outlives the task and is shared with the next one. M4 and M5 below are
that premise, split into the part that must hold for the task to work at all and
the part that decides whether this decision survives.

Revisit if host tasks ever run unattended on a shared node, which D4 currently
prevents.

### D7 — a host task's resources are unbounded, and the node flag does not say so

**Decision: `resources.cpu` / `resources.memory` remain unenforced for host
tasks. `NodeCapabilities.resources_enforced` stays node-scoped, which means it
reads `true` on the dual-mode Mac this design is for; that is accepted rather
than fixed, and the per-launch warning is the signal that is actually true.**

**The fact, stated correctly**, because the previous draft of this section had it
backwards. `resources_enforced` is not a property of a host *task*: the daemon
computes it as `serves_container(modes)` (`crates/worker/src/daemon.rs`), and
`serves_container` is true for any node naming `container`. So it is `false`
only on a **host-only** node. `gumbo-air-0` is `container,host` by this
document's binding constraint — the same fact [D5](#d5--the-nodes-refusal-changes-what-it-asserts-not-whether-it-exists)
leans on to put the refusal at launch — so the air advertises
`resources_enforced: true` while every host task it runs is not cgroup-bounded.
The field's contract in `crates/types/src/worker.rs` is "whether the node
enforces a task's `resources.cpu`/`memory`", absent ⇒ `true`; on a dual-mode node
that is one bit answering two questions, and it answers the container one. The
daemon's own unit test says as much — a both-modes node keeps the flag "it still
has a docker daemon" — so this is the flag working as designed and describing
the wrong thing here, not a bug introduced by this design.

**Accepted, for now, on two grounds.** Nothing consumes it: outside its own
construction and its tests, `resources_enforced` has no reader in the workspace —
no placement filter, and nothing in `web/src` — so today the misdescription
misleads a human reading `/api/v1/platform/fleet`, not a scheduler making a
choice. And the honest fix is
larger than this design — the field is one bool per node, so making it truthful
means making it per **mode**, a shape change to `NodeCapabilities` that every
consumer of the advertisement would have to be re-read against. The trigger for
doing it is concrete: **the first reader that decides something from this flag.**
Until then a slice here would change a wire type to correct a diagnostic.

**What is true per task already exists and is worth naming**, since it is the
granularity the flag lacks: `HostBackend::admit` logs a `tracing::warn!` naming
the launch's `cpu`/`memory` and citing [#309](./309-host-native-execution.md) §7
whenever a host launch declares either (`crates/container/src/host.rs`). A job
type that asks for a bound on a Mac is told, per launch, that it will not get
one.

An agent task driving simulators is heavier than the command work #309 §7's
acceptance was written against, so the exposure is larger even though the
mechanism is unchanged.

D4 is what makes this tolerable rather than reckless: with one host task per
node, an unbounded task can starve **itself and the node's container slots**,
but it cannot be starved by a sibling host task, and there is no contention to
arbitrate. A node that needs a bound needs per-task launchd jobs, which #322 P2
lists and which D4's deferral defers with it.

## What must be measured on the air first

Everything above is reasoning over source, plus D1's unauthenticated slugifier
run. Seven facts are load-bearing and unverified. They are cheap — a shell
session on `gumbo-air-0` and one agent job — and they come **before** slice 1,
because three of them would change a decision rather than confirm it. Slice 6
remains the end-to-end proof; this is the set that precedes the first line of
code.

| # | The fact | Holds up | If it is false |
| --- | --- | --- | --- |
| **M1** | `--session-id` still names the transcript on an **authenticated** run, and content is written | D1 | The premise dies. Candidate 1 returns and the platform is pinned to a CLI version, with D3's version pin back to correctness-critical |
| **M2** | A Mach-O `chuggernaut-channel` runs as a stdio MCP server launched by the CLI outside a container | D2 | Host mode needs a different channel transport; "a second artifact by mode" is the wrong shape and D2 is re-opened |
| **M3** | The daemon's launchd `PATH` (`AGENT_PATH`, `deploy/prod/install-worker-launchd.sh`) resolves `claude` on `gumbo-air-0` | D3 | The slice extends `AGENT_PATH`, or the operator moves the install onto it — decided before D5's refusal is written, or every launch is refused by name |
| **M4** | The CLI writes into a `CLAUDE_CONFIG_DIR` inside a 0700 task directory owned by the daemon's user | D6 | The directory's mode or owner changes, or the config dir moves out of the task directory and teardown changes with it |
| **M5** | The CLI writes **nothing** it must not leak outside `CLAUDE_CONFIG_DIR` — `$HOME`, caches, keychain | D6 | D6 is false as stated: a host has no container boundary, so those files outlive the task and are shared with the next one. Teardown grows, or the daemon user does |
| **M6** | A legitimate run never yields a session id that resolves to no transcript — nor, on an authenticated multi-cwd run, to several | D1b | The escalation is never armed and the marker artifact is the permanent answer for whichever branch it happens on |
| **M7** | Simulator state left by one task does not disturb the next | D4 | Serialisation is insufficient on its own and #322 §5's per-task device set is pulled forward, ahead of leases |

M1, M2 and M3 are the three that gate work rather than confirm it: each has a
different decision on the other side of a "no".

## Slices

| # | Type | Scope | Contract | Depends |
| --- | --- | --- | --- | --- |
| **0** | `design` or operator | M1–M7 measured on `gumbo-air-0`, recorded as a correction to this document | Each row answered yes/no with the command that answered it | none |
| **1** | `code` | D1/D1a: `ContainerBackend::find_file` across both backends, the worker RPC pair, the fakes; the harvest resolves then `copy_file_chunked`s; unknown-op falls back to the computed path | The surface table in D1a; **no** `WORKER_RPC_VERSION` bump; proven on container agent jobs, which this changes | 0 (M1), #322 W4 (job #489) |
| **2** | `code` | D1b: zero **and** several become an error-level miss carrying a `transcript-missing` marker artifact; the escalation is armed only if M6 said it is safe | `Harvester::collect_agent`'s return, its best-effort charter unchanged; a fourth `ArtifactKind` (`crates/store/src/artifacts.rs`) and its ripple — `crates/api/src/routes.rs`'s content type, `web/src/api/envelopes.ts`'s hand-written union, `web/src/components/TaskArtifacts.tsx`'s label map | 1 |
| **3** | `code` | D2: a host-capable node keeps `--bin chuggernaut-channel` in `NATIVE_BINS`, installs it at its own path, and proves it runs on the node. #480's `e_machine` guard on the injected copy is untouched | both deploy scripts; the two-executor rule | 0 (M2) |
| **4** | `code` | D3: probe the agent CLI on the daemon's `PATH` at startup, advertise it, refuse by name when absent | `NodeCapabilities`, additive — no `WORKER_RPC_VERSION` bump | 0 (M3), 3 |
| **5** | `code` | D5: `HostBackend::admit`'s `CLAUDE_CONFIG_DIR` test becomes a launch-time capability test; `validate_host_serves_commands_only` lifts | `HostBackend::admit`; spec §1.1's host row | 4 |
| **6** | `code` | The first agent host task actually run on `gumbo-air-0`, with the transcript resolved and harvested end to end | none — this is the confirmation | 5 |

Slice 6 is not ceremony, and neither is slice 0. Every decision above rests on
reading the tree; nothing here has been observed end to end, and this design
should not be called IMPLEMENTED until it has.

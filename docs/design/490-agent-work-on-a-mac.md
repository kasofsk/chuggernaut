# Design — agent work on a Mac

Status: IMPLEMENTED — slices 0–7 landed in jobs #491–#514; D6 amended in job #512 and its sweep landed in job #514; M7 at two samples.

Written against the tree at `d556a6c` (job #489's `code` merge, the last commit
touching source before this branch): every claim below was read out of the
source named beside it, not inferred from the docs. **No host task of any kind
has ever run on real hardware**, so with one exception the whole document is
reasoning over a tree, not observation of a fleet. The exception is D1's
slugifier measurement, which was run against an unauthenticated CLI and is
labelled where it appears. [What must be measured on the air
first](#what-must-be-measured-on-the-air-first) gathers every remaining
unverified fact, the decision it holds up, and what that decision becomes if it
turns out false. **That table has since been answered** — read it together with
[the slice 0 correction](#correction--2026-08-08-job-492-slice-0-measured-d1s-recorded-limit-and-a-live-transcript-loss-defect),
which supersedes the preceding paragraph's "one exception" and D1's recorded
limits. **And the sentence in bold above is history**: slice 6 ran two agent
host tasks on `gumbo-air-0`, and what they measured is [the job #510
correction](#correction--2026-08-09-job-510-slice-6-ran-what-two-host-tasks-on-the-air-measured).

## Current state

*The **mutable head** ([#415](415-knowledge-architecture.md) [D2](415-knowledge-architecture.md#d2-every-design-doc-opens-with-a-mutable-current-state-head)):
rewritten to current truth whenever anything below it changes. Everything after
this section is append-only — the original argument, never edited.*

**Every slice is landed, and slice 6 ran on the air**: two agent host tasks on
`gumbo-air-0`, the first host work of any kind this platform has performed
([the job #510
correction](#correction--2026-08-09-job-510-slice-6-ran-what-two-host-tasks-on-the-air-measured)).
M1, M2, M3 and M4 are answered — M3 came back **no** and slice 4 is the answer to
it, since the CLI's install directory is on the `PATH` both macOS installers
render, so an agent already installed keeps the old one until its plist is
re-rendered. **M6 is answerable but not answered**: slice 2 built the instrument
and recorded the procedure ([the job #494
correction](#correction--2026-08-08-job-494-slice-2-landed-and-how-m6-gets-answered)),
whose precondition — a deploy carrying slice 1 — is now met, so what it wants is a
population to count.

**D6 is amended and closed. M7 is not.**

- **[D6](#d6--credential-lifetime-is-unchanged-in-mechanism-longer-in-duration) is
  amended a second time and closed** ([the job #512
  correction](#correction--2026-08-09-job-512-d6-amended-the-premise-is-false-the-guarantee-holds-and-the-teardown-grows-by-one-path)).
  Its first amendment was slice 5's, because the teardown it said to keep would
  have deleted every host transcript before the harvest could read it, so it
  spares the CLI's own config directory with the secrets half unchanged ([the job
  #497 correction](#correction--2026-08-08-job-497-d6-amended-the-teardown-spared-the-clis-own-tree)).
  Its remaining premise — that the CLI confines itself to `CLAUDE_CONFIG_DIR` —
  is what slice 6 measured under an authenticated CLI running as the daemon's
  user, and it is **false**: one directory, the CLI's own MCP log tree under the
  daemon user's cache, outlives the task and accretes across tasks. **The
  guarantee that premise was holding up survives anyway** — nothing in that
  residue is a credential, and the injected tree is still deleted the moment the
  command returns — so D6 keeps its decision and grows its teardown by exactly
  that one path. The M5 row's own "Teardown grows, or the daemon user does" is
  resolved in favour of the first, with the second rejected on the tree rather
  than left hanging; [slice 7](#slices) landed that sweep in job #514, so the
  teardown a host task's own wrapper begins now ends with the CLI's MCP-log
  subtree for that task — listed out of the cache root by the task directory's
  own name, never computed, and best-effort on the nix reaper's charter
  (`sweep_agent_cache` and `reclaim_agent_cache`, `crates/container/src/host.rs`).
- **[D7](#d7--a-host-tasks-resources-are-unbounded-and-the-node-flag-does-not-say-so)'s
  revisit trigger has fired, and the fix was not the one it priced.** D7
  accepted the misdescription on the ground that *"nothing consumes it"*, and
  named the trigger: **the first reader that decides something from this flag**.
  Job #524 is that reader ([#309](./309-host-native-execution.md) §7's placement
  predicate), and it needed no shape change to `NodeCapabilities` — the reader
  narrows the node's one bool to the launch's resolved mode, so a dual-mode air
  still advertises `true` and still never has that `true` applied to a host
  launch. The per-launch warning D7 called "the signal that is actually true" is
  now a refusal: a host launch declaring `cpu`/`memory` is `BackendError::Launch`
  naming the field and the node. See [the 2026-08-09
  amendment](#amendment--2026-08-09-job-524-d7s-trigger-fired-and-the-warning-became-a-refusal).
- **[M7](#what-must-be-measured-on-the-air-first) has two samples and no
  verdict.** Simulator state the first task left made the second **cheaper**, not
  disturbed; two observations of "did not disturb" are not "cannot disturb", and
  the second run archived a baseline for a third to diff.
  [D4](#d4--one-host-task-per-node-stays) is unchanged.
- **The `simctl spawn` failure recorded beside those two runs was
  misattributed**, and the attribution is withdrawn ([the job #527
  correction](#correction--2026-08-09-job-527-the-simctl-spawn-finding-was-misattributed-the-argument-not-the-session)).
  It is not a property of the daemon's session — both error codes reproduce over
  an ordinary SSH session, and they separate by **argument**: `spawn` runs the
  named program inside the simulator's own filesystem, and iOS ships `launchctl`
  but no `uname`. `.chug/prompts/work/mac-proof.md` §2 had asked for a command
  that cannot pass anywhere, and now asks for one that does.

Everything else about the machinery holds. The platform gap job #502 found is
closed: a container level of a host job type is handed no `runtime.env`, so the
appended `ci` evaluator runs as the ordinary container task its own image makes
it ([the job #507
correction](#correction--2026-08-08-job-507-the-launch-path-now-scopes-runtimeenv-to-the-level-it-launches)),
and [#309](./309-host-native-execution.md) §1's "host work, container CI, one
job" ran end to end for the first time.
No other decision was overturned. Three things changed on the way here:

- **M3 made slice 4's work required rather than confirmatory.** `claude` is
  installed on `gumbo-air-0` and was not on the daemon's `PATH`, so D3's probe
  would have found nothing and D5 would have refused every agent host launch by
  name; slice 4 landed the `PATH` alongside the probe (below).
- **Candidate 1 is now rejected on correctness, not only on fragility.** The CLI
  slugifies the *resolved realpath*, so a computed slug is wrong today on any
  task root reached through a symlink — which is the ordinary shape on macOS.
- **Slice 1 repairs a live production defect and is no longer only an enabler
  for host mode.** Work-agent transcripts are being silently dropped on the
  container fleet right now, by `copy_file`'s worker-RPC size bound, at a rate
  the artifact store makes measurable. That also makes M6's premise already
  false, which is slice 2's problem.

The correction below carries the evidence for each.

**What slice 1 landed**, exactly as D1a's surface table specifies:
`ContainerBackend::find_file(id, dir, name)` on both backends — Docker streams
the directory's tar and reads its headers (the container has exited, so
`exec find` is unavailable), the host backend walks the rebased directory and
maps results back through `unrebase_path`, `rebase_path`'s new inverse — plus
the `find_file` RPC pair, its arm in the daemon's op match, the routing, and both
fakes. `Harvester::collect_agent` resolves and then reads with
**`copy_file_chunked`** at the artifact store's own `MAX_BLOB_BYTES`, so the
production defect below is repaired: a transcript over `MAX_COPY_FILE_BYTES` is
harvested whole, and one over the new ceiling is refused at **error** level
naming the loss rather than dropped. `find_file` is bounded at
`container::FIND_FILE_MATCHES_MAX` matches and, on a host node, by scan depth and
entries visited as well. **No `WORKER_RPC_VERSION` bump**: a daemon that does not
know the op answers `unknown op`, and the caller falls back to the computed path
— for a **container** launch only, per the realpath finding below.

**What slice 2 landed** (job #494): zero matches and several matches are logged
at **error**, and every miss — including a resolution that never answered —
stores the fourth `ArtifactKind`, `transcript-missing.json`, naming the branch
that refused, the session id, the directory searched and, for several, the paths
found. `Harvester::collect_agent` returns that outcome on an `AgentHarvest`
rather than raising it, and `TranscriptMiss::escalation` is **written and
unarmed** — `ESCALATION_ARMED` is `false`, so no job's state can change on this
outcome until M6 is answered.

**What slice 3 landed**, in `deploy/prod/build-worker.sh` and
`deploy/prod/worker-refresh.sh` only — no Rust changed. A host-capable node is
handed a **second** channel binary at `/usr/local/lib/chuggernaut/chuggernaut-channel-host`,
beside the injected `/usr/local/lib/chuggernaut/chuggernaut-channel`, whose
`e_machine` guard is untouched. Both scripts derive `serves_host` beside
`serves_container`, in the daemon's own spelling; `--bin chuggernaut-channel` is
back in `NATIVE_BINS` unconditionally, since between them the two rules cover
every legal `WORKER_MODES` (this is job #487's condition reversed, and its
premise — that host mode is command-only, so nothing reads the file — is what
[slice 5](#slices) removed). On **Darwin** the host copy comes out of the node's
own `cargo build` and the injected one out of the image; on **Linux** both are
the same bytes out of the same image, staged and guarded separately because the
node's userland is not the container's. Each is asked its **own** executor's
question before anything is installed: the injected copy against the container's
architecture, the host copy by being **run** on the node, with an ELF in the host
slot refused by name as the other half of the pair. Nothing **read** the new
file until slice 5, which is where the daemon-side config variable D2 left open
belongs: a host agent launch's MCP config names that path, and
`HostBackend::admit` stats it per launch and refuses when nothing runnable is
there. A
container-only node's deploy is unchanged, and a host-capable one differs by
exactly one `docker cp` (asserted as a delta in both suites).

**What slice 4 landed** (job #496): `worker::agent_cli` probes the daemon's own
`PATH` for an executable `claude` at boot, on a host-capable node only, and
`NodeCapabilities` carries the answer as a new `agent_cli` flag — additive,
defaulting to **false** when absent, so a daemon predating the probe reads as
unable to serve agent work. No `WORKER_RPC_VERSION` bump. An agent-shaped host
launch on a node that found none was refused **by name** in the daemon, naming the
`PATH` searched, ahead of `HostBackend::admit`'s blanket `CLAUDE_CONFIG_DIR`
refusal — slice 5 replaced both with the single capability test in `admit`, which
still carries the daemon-composed text naming that `PATH`. And M3's remedy: both macOS
renderings (`deploy/prod/install-worker-launchd.sh` and
`deploy/prod/build-worker.sh`) now carry the login user's `~/.local/bin` at the
**tail** of the default `AGENT_PATH` — the tail because that `PATH` is every host
task's too, and a user-writable directory ahead of `/usr/bin` would silently
reselect `git` or `ssh`. Installing the CLI stays the operator's step (D3); what
moved is the directory the daemon looks in.

**What slice 5 landed** (job #497), which is the slice that actually permits
agent work on a Mac. `HostBackend::admit`'s `CLAUDE_CONFIG_DIR` refusal is now a
test of the node's `AgentCapability` — the CLI the daemon discovered (D3) and a
runnable channel binary of the node's own (D2) — refusing **by name** whichever
is absent and admitting the launch when neither is. The daemon still discovers
and now hands both facts to the backend at construction, the way it already
hands it `Supervision`, so slice 4's `admit_agent_cli` is gone rather than
duplicated: one place judges an agent-shaped launch, which is what D5 names.
`validate_host_serves_commands_only` is deleted, so a `mode: host` job type may
declare `work.type: agent` (and `human`); the `image` and `runtime.env` rules
under `mode: host` are untouched, and an evaluator declaring its own image still
resolves to container mode — host work, container CI, one job, asserted as its
own test.

Three things had to follow for such a launch to be able to run at all, none of
them named in the slices, all of them found by writing the test that admits one:
`Core::channel_mcp` routes on the launch's `image` — the selector every backend
routes on — and for a host task **injects nothing** and names
`/usr/local/lib/chuggernaut/chuggernaut-channel-host`, the path slice 3
installs, since an MCP config's `command` is file *contents* that no backend
rebases (which is also why the path is a constant rather than the deploy's
`WORKER_HOST_CHANNEL_BINARY` knob: overriding that relocates the install away
from where a launch execs it, and the capability refusal is what says so).
`CLAUDE_CONFIG_DIR` joins the two variables whose **values** the host backend
rebases (#322 §2's fourth surface) — without it every agent host launch was
refused by `rebase_env`, and with it the CLI's transcript tree lands inside the
task directory where slice 1's `find_file` looks, which took **amending D6**:
the wrapper's teardown deleted that tree whole at process exit, so it now
reclaims the injected tree's entries and spares the CLI's own config directory
([the job #497 correction](#correction--2026-08-08-job-497-d6-amended-the-teardown-spared-the-clis-own-tree)).
And the agent's **command**
resolves its three `/chuggernaut` paths through `$CHUG_HOST_CREDS`, because a
launch's `cmd` is the one surface the rebase does not reach — the same
indirection `bootstrap_cmd` uses for the clone destination. Container launches
are byte-identical through all three.

**What slice 6 produced** (jobs #504–#509), which is a record rather than a
change: an authenticated agent CLI running as a native macOS process on
`gumbo-air-0`, its `session.jsonl` harvested at **462,085 bytes**, and
`update_status`/`submit_result` carried first-call through the Mach-O
`chuggernaut-channel` the node installs — M1's last conjunction and M2's residual,
both of which needed an authenticated CLI as the launcher and so could not be
measured before. Two launch-blocking defects were found by attempting it and
fixed between the runs: a wire prefix that matched mid-segment, refusing every
launch carrying this repository's `REPO_URL` (job #505), and a container level
inheriting the job type's `runtime.env` (job #507). Both had survived five slices
of full CI gates and stage-0 review, because none of those slices had ever
launched a host task.

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

| # | Type | Scope | Contract | Depends | Status |
| --- | --- | --- | --- | --- | --- |
| **0** | `design` or operator | M1–M7 measured on `gumbo-air-0`, recorded as a correction to this document | Each row answered yes/no with the command that answered it | none | **Landed** (job #492) |
| **1** | `code` | D1/D1a: `ContainerBackend::find_file` across both backends, the worker RPC pair, the fakes; the harvest resolves then `copy_file_chunked`s; unknown-op falls back to the computed path | The surface table in D1a; **no** `WORKER_RPC_VERSION` bump; proven on container agent jobs, which this changes — and it **repairs a live defect**, so a work-agent transcript over `MAX_COPY_FILE_BYTES` harvested whole is an acceptance criterion available today | 0 (M1), #322 W4 (job #489) | **Landed** (job #491) |
| **2** | `code` | D1b: zero **and** several become an error-level miss carrying a `transcript-missing` marker artifact; the escalation is armed only if M6 said it is safe — and M6's premise is already false, so the escalation is not armed until slice 1 has removed the known cause | `Harvester::collect_agent`'s return, its best-effort charter unchanged; a fourth `ArtifactKind` (`crates/store/src/artifacts.rs`) and its ripple — `crates/api/src/routes.rs`'s content type, `web/src/api/envelopes.ts`'s hand-written union, `web/src/components/TaskArtifacts.tsx`'s label map | 1 | **Landed** (job #494) |
| **3** | `code` | D2: a host-capable node keeps `--bin chuggernaut-channel` in `NATIVE_BINS`, installs it at its own path, and proves it runs on the node. #480's `e_machine` guard on the injected copy is untouched | both deploy scripts; the two-executor rule | 0 (M2) | **Landed** (job #495) |
| **4** | `code` | D3: probe the agent CLI on the daemon's `PATH` at startup, advertise it, refuse by name when absent — **and put the CLI on that `PATH`**, which M3 says it is not | `NodeCapabilities`, additive — no `WORKER_RPC_VERSION` bump; `AGENT_PATH`/`WORKER_PATH` in `deploy/prod/install-worker-launchd.sh` | 0 (M3), 3 | **Landed** (job #496) |
| **5** | `code` | D5: `HostBackend::admit`'s `CLAUDE_CONFIG_DIR` test becomes a launch-time capability test; `validate_host_serves_commands_only` lifts | `HostBackend::admit`; spec §1.1's host row | 4 | **Landed** (job #497) |
| **6** | `code` | The first agent host task actually run on `gumbo-air-0`, with the transcript resolved and harvested end to end; **M5's authenticated residual and M7 measured under the daemon** | The row's "none — this is the confirmation" was wrong twice over, and so was its promise that M5 and M7 would be *settled*: job #502 built the config half it said did not exist, and settling M7 needs two host tasks rather than one. What ran is two `mac-proof` jobs on the air, green end to end on the second, with two launch-blocking defects found and fixed between them; **M5's residual is measured and D6 is re-opened rather than settled, and M7 has two samples and no verdict** | 5 | **Landed** (job #510) — the runs are jobs #506 and #509, recorded in [the correction below](#correction--2026-08-09-job-510-slice-6-ran-what-two-host-tasks-on-the-air-measured); this row and the head are the deliverable a `mac-proof` job cannot carry, since its `wrap_up` is `type: none` and it merges nothing |
| **7** | `code` | [D6's second amendment](#correction--2026-08-09-job-512-d6-amended-the-premise-is-false-the-guarantee-holds-and-the-teardown-grows-by-one-path): a host task's teardown reclaims the agent CLI's own MCP-log subtree for **that task**, located by **listing** the cache root and matching the task directory's own name — never by computing the CLI's slug | Held to `crates/worker/src/nix.rs`'s reaper charter — it leaks disk rather than ever failing a job — so an absent, unreadable or undeletable subtree is logged and skipped, and it never contributes to `remove`'s failure list. Only a subtree keyed under **this** task's directory; the node's unrelated ones (an actions-runner's, dated June) are untouched. Bounded in entries examined per pass | 6 | **Landed** (job #514) — `sweep_agent_cache` lists the cache root and removes each immediate child whose name contains the task directory's own, `reclaim_agent_cache` runs it from both `spawn_reaper` and `remove` and returns nothing a caller could fail on, and `AGENT_CACHE_ENTRIES_MAX` bounds the pass (`crates/container/src/host.rs`); a pass that matched nothing logs what it examined, because a silent miss is the failure mode the slice exists to avoid |

Slice 6 was not ceremony, and neither was slice 0. Every decision above rested on
reading the tree until slice 6 ran it, and the first attempt was refused before
any process started: two launch-blocking defects had survived five slices of full
CI gates and stage-0 review because nothing had ever launched a host task. That is
why this design was not called IMPLEMENTED before the run — see [the job #510
correction](#correction--2026-08-09-job-510-slice-6-ran-what-two-host-tasks-on-the-air-measured).

## Correction — 2026-08-08, job #492 (slice 0 measured, D1's recorded limit, and a live transcript-loss defect)

Appended by the job that ran slice 0. Nothing above is edited except the head.
The shell measurements were taken by the operator on `gumbo-air-0` and on the
operator's Mac (macOS 26.5.1, arm64); the production survey was read out of this
platform's own artifact store across jobs #478–#489; the code citations were
re-read at `6c35c5d` while writing this. No decision above is overturned. One
becomes required rather than confirmatory, one gains a stronger argument, and one
turns out to be repairing a defect that is already happening.

### The M table, answered

**Where** is part of every answer. *On the air* (`gumbo-air-0`), *on a Mac* (the
operator's) and *in production* (the container fleet, worker-proxied) are three
different claims, and a verdict that blurs them proves less than it looks like it
does. The first table is the one that matters; the qualifications are below it.

| # | Verdict | Where | What answered it |
| --- | --- | --- | --- |
| **M1** | **yes** | production **and** a Mac | Job #490's own evaluator task — an authenticated agent run, 1,168,237 cache-read tokens — harvested a **317,723-byte** `session.jsonl` at the path `Harvester::collect_agent` computed from the platform's own `--session-id`. Two macOS runs confirm the file is named for the supplied id, and `find -name '{session_id}.jsonl'` returns exactly one match |
| **M2** | **yes**, with the launcher untested | a Mac | `cargo test -p chuggernaut-channel --test stdio` passes natively in 0.98s. That test *is* the measurement (`crates/chuggernaut-channel/tests/stdio.rs`): it spawns the real built binary, drives newline-delimited JSON-RPC over stdio and asserts the NATS submission lands. Nothing in the binary is container-shaped — `JobContext::from_env` (`crates/chuggernaut-channel/src/server.rs`) reads env and stdio and nothing else |
| **M3** | **NO** | the air | `AGENT_PATH` defaults to `/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin` (`deploy/prod/install-worker-launchd.sh`), and the installed plist prepends `/run/current-system/sw/bin`. `claude` on the air is at `/Users/worksalot/.local/bin/claude` (2.1.198). `PATH=$AGENT_PATH command -v claude` finds nothing; it resolves only on the login `PATH` |
| **M4** | **yes** | the air **and** a Mac | The CLI created `CLAUDE_CONFIG_DIR` and wrote the transcript inside a `0700` task directory owned by the daemon's user; 11,387 bytes on the air |
| **M5** | **yes**, qualified | the air **and** a Mac | Run under `env -i` with an isolated `HOME`: **zero** files created outside `CLAUDE_CONFIG_DIR` |
| **M6** | **deferred to slice 2** | — | Not measurable before a host agent task exists, and it has been overtaken: the production finding below shows the premise is **already false** on the container path |
| **M7** | **deferred to slice 6** | — | Simulator cross-task interference needs two host tasks and a simulator; slice 6 is the first point at which either exists |

**M2's residual, stated because the row's wording asks for more than the test
gives.** M2 asks whether a Mach-O `chuggernaut-channel` runs as a stdio MCP
server *launched by the CLI*. The test proves the binary speaks MCP over stdio
as a native Mach-O and reaches NATS; the CLI as the process that launches it is
not exercised, because the air has no channel binary installed at all
(`/usr/local/bin` there holds only the `chuggernaut` daemon) — which is exactly
what slice 3 fixes, and slice 6 is where the CLI-as-launcher half is observed.
D2 is not re-opened by this: the residual is about who spawns the process, not
about whether a Mach-O can serve the protocol, and it was the second that D2
called unverified.

**M5's qualification, which is the one that matters for D6.** The runs were
unauthenticated. The platform does not authenticate the CLI by an interactive
login: `Core::inject_platform_agent_secrets`
(`crates/dispatcher/src/exec.rs`) injects every secret under the reserved
`global/agents` scope into every agent container, env-named by the secret, and
prod names that secret `CLAUDE_CODE_OAUTH_TOKEN` (`deploy/prod/README.md`). So
**no keychain write is expected** — an env-carried credential has nothing to
store — but none has been observed either, and "expected" is the word this whole
section exists to distrust. D6 stands; its premise is measured for the
unauthenticated case and assumed for the authenticated one, and slice 6 is where
the assumption is discharged by re-running the `env -i` sweep with the token set.

**M3 is the one that changes work rather than confirming it.** D3 decided this on
`AGENT_PATH`, which is the correct side of the branch — but it reads as a
precaution, and the measurement makes it a prerequisite. Without it, D5's refusal
fires by name on **every** agent host launch while a working CLI sits installed
on the node: loud, correct, and useless. The gap is named precisely: the operator's
`~/.local/bin` is not on the daemon's `PATH` and nothing in this repo puts it there.

The knob already exists, which shrinks slice 4 rather than growing it.
`AGENT_PATH` is `${WORKER_PATH:-…}` (`deploy/prod/install-worker-launchd.sh`),
so a node's env file can extend the daemon's `PATH` today without a code change;
the prepend of `/run/current-system/sw/bin` is that same script deriving
`dirname` from the env file's `WORKER_CARGO`. Slice 4 therefore chooses between
three shapes and should say which and why: extend the shipped default, set
`WORKER_PATH` per node, or move the CLI install onto a directory the default
already carries. D3's rule — installation and `PATH` move together, never one
without the other — is what makes any of the three acceptable and doing neither
half not.

### D1's recorded limits: one was false, and the one that remains is narrower

D1's measurement notes say the runs "were **not authenticated** … so the
directory and file names are real, but no transcript **content** was produced".
**The second half is false.** An unauthenticated run writes a full transcript: 8
lines / **10,071 bytes** on macOS, **11,387 bytes** on the air. What an
unauthenticated run lacks is **model output**, not a transcript — the CLI frames
the session, records its own refusal and closes the file.

The limit that actually remains, in its place: those runs exercise **path
construction and a session's opening frame**, not the working record of a long
session, so they say nothing about a transcript's size, growth or content under
load. M1 is what covers that half, and it covers it from production rather than
from a shell: 317,723 bytes of authenticated transcript at the computed path.

D1's other recorded limit — "measured on the operator's Mac, not on
`gumbo-air-0`" — is now **closed** by M4: the same behaviour was reproduced on
the air, including the byte count.

### A fourth measurement row: the slug is of the **resolved realpath**

| what was run | result |
| --- | --- |
| on the air, cwd `/tmp/m45-chug/task/workspace` | one directory in `projects/`, named `-private-tmp-m45-chug-task-workspace` |

macOS `/tmp` is a symlink to `/private/tmp`. So the CLI slugifies the cwd it
**resolves**, not the cwd string it was handed.

**This converts D1's rejection of candidate 1 from a fragility argument into a
correctness one**, and the new argument is the stronger of the two because it
does not wait on anybody changing anything. D1 rejects "compute the slug" on the
ground that it pins the platform to an external tool's undocumented behaviour and
fails silently *when that behaviour moves*. It is wrong **now**: a slugifier
implementing the documented character rule would have to replicate symlink
resolution as well, and on any task root reached through a symlink it would
compute a directory that does not exist and find nothing — silently, which is
the precise failure class this design exists to prevent. D1's measurement notes
say the character rule "held" on a deep path and that candidate 1 is therefore
*feasible*; that sentence is correct about the character rule and incomplete as a
description of the algorithm.

Two follow-ons, neither of which disturbs D1 itself — resolving by session id
never asks what the directory is called, so D1 is immune to the whole question:

- **The host root's own realpath is worth checking per node.** `HOST_ROOT_DEFAULT`
  is `/var/lib/chuggernaut/host-tasks` (`crates/container/src/host.rs`), overridden
  by `WORKER_HOST_ROOT` (`crates/worker/src/config.rs`). On macOS `/var` is
  itself a symlink into `/private/var`, by the same mechanism that produced the
  row above — so the shipped default is a symlinked path on the one platform this
  design is for. Nothing breaks under D1; it is named here so that nobody
  re-derives a computed path later and is surprised by it.
- **D1a's unknown-op fallback is candidate 1 in miniature, and must not be taken
  on a host launch.** D1a decides that a daemon answering `unknown op` falls back
  to today's computed exact path. That is sound where it applies — an unrefreshed
  node is a container node, whose cwd is `/workspace` with no symlink in it — and
  it is *unsound* on a host node, where the cwd is under the host root. Slice 1
  should scope the fallback to container launches rather than to "any unknown-op
  reply". This is a constraint on how the decision is implemented, not a change
  to it.

### The production finding: the transcript loss is not latent, it is happening

D1a argues for `find_file` plus the **existing** `copy_file_chunked` partly on
the ground that `collect_agent` uses the unchunked `copy_file`, so "on a
worker-proxied node a transcript past that bound already comes back as a refusal
rather than an artifact", and that D4/D6's hours-long sessions "make that latent
limit a certainty here". **It is not latent. It is happening now, on the
container path, and it has been for weeks.**

`MAX_COPY_FILE_BYTES` is `(MAX_REQUEST_BYTES - COPY_FILE_ENVELOPE_BYTES) / 4 * 3`
= **690,432** bytes (`crates/store/src/worker.rs`). Surveying every agent task's
artifacts across jobs #478–#489:

- Roughly **fifty** transcripts were harvested. The largest is **673,149 bytes**.
  **Not one exceeds 690,432.** Across a sample that size, a distribution that
  genuinely stops 17KB short of a bound is not a distribution, it is a ceiling.
- The agent tasks with **no** `session.jsonl` at all — only `stdout.log` — are
  overwhelmingly **task 1**, the *work* agent: missing on #478, #479, #480, #481,
  #483, #484, #485, #487 and #489. Task 1 is the longest-running session in a job
  and produces the most valuable record. Evaluator tasks, which are shorter,
  almost always survive.

The mechanism is unambiguous in the code, and every step of it was re-read:

1. `Harvester::collect_agent` (`crates/platform-ops/src/harvest.rs`) calls
   `self.backend.copy_file(id, &path)`.
2. On a worker-proxied node that routes to the RPC — `FleetBackend`'s
   `NodeHandle::Worker` arm (`crates/worker/src/backend.rs`). The whole prod
   fleet is worker-proxied, so the in-process `NodeHandle::Docker` arm beside it,
   which applies no such bound, is not the path production takes.
3. The daemon measures the file **before** encoding its reply and refuses one
   over the bound with `COPY_FILE_TOO_LARGE` (`copy_file` in
   `crates/worker/src/daemon.rs`, via `copy_file_over_bound`).
4. `collect_agent`'s `Err` arm is a `tracing::warn!`. The job stays green, no
   artifact is stored, and nothing in the operator UI says a record was lost.

So the exact failure class #490 exists to prevent — the run looks healthy and the
record is missing — is a **live, recurring, silent data-loss defect on the
fleet's normal path**, not a hazard specific to host mode.

**One detail of the brief that led to this correction is worth fixing here**,
since D1b turns on which branch reports what. The over-size refusal surfaces on
the **`Err`** arm ("transcript copy failed: {e}"), not on the `Ok(None)` arm
whose text reads "agent may not have started". Both are `warn`, both leave the
job green and the artifact absent, so the *operator-visible* outcome is
identical and the loss is real either way — but the two are distinguishable in
the logs, and a survey looking only at `Ok(None)` would have measured the wrong
population. Slice 2's marker artifact should therefore name **which** of the two
happened; "no transcript" is not one condition.

**What this changes for slice 1.** Its standing, first: it is no longer only an
enabler for host mode, it repairs a production defect, and that gives it an
acceptance criterion that needs no host task and no Mac — *a work-agent
transcript larger than 690,432 bytes is harvested whole on an ordinary container
job*. D1a already said slice 1 "changes the container path" and had to be proven
there; this says what proving it means.

Second, the bound it moves to. `copy_file_chunked` takes a caller-chosen
ceiling, and `collect_output`'s is `store::MAX_BLOB_BYTES` — 16 MiB
(`crates/store/src/artifacts.rs`). Slice 1 should choose that ceiling
deliberately rather than by copying the neighbour, because D4 and D6 both say
agent sessions run for hours and 16 MiB is a bound a long session can plausibly
reach. Whatever it picks, a transcript over it must not be dropped the way one
over 690,432 is being dropped today; that is the same defect one order of
magnitude up, and re-creating it silently would be the worse outcome.

An in-situ datum on how fast that number arrives, measured inside this very
document's own run: this `design` job's work transcript passed **298,180 bytes**
within its first ten tool calls — 43% of the cap, before a line of the document
was written — and stood at **516,468 bytes**, 75% of it, by the time the document
was committed. That is a `design` job that read a dozen files and wrote one, and
it comes within 174KB of losing its own record. D4's and D6's agentic debugging
sessions are the long ones.

**What this changes for slice 2.** M6's premise — "a legitimate run never yields
a session id that resolves to no transcript" — is **already false today**, at a
measurable rate, for a reason that is not the agent failing to start. Two
consequences:

- Slice 2 designs against real data rather than a hypothesis. The rate is
  readable straight out of the artifact store, and it will change under slice 1,
  which is itself the measurement of whether slice 1 worked.
- The escalation D1b stages behind M6 stays **unarmed** until slice 1 has removed
  this cause. Arming it first would escalate a known platform bug once per long
  job, which is noise rather than signal. The `transcript-missing` marker is the
  right instrument for the interval, and the ordering — slice 1 before slice 2 —
  was already the stated dependency; this makes it a correctness ordering rather
  than a convenience.

The honest limit on the inference: the survey establishes a ceiling and a
distribution, not a per-job causal chain, because a green job's `warn` lines are
not retained per task in the artifact store. A task-1 miss whose logs show
`Ok(None)` rather than the `Err` arm would mean a second cause exists alongside
this one. Slice 1's acceptance criterion settles it either way — if transcripts
over the bound start arriving and task-1 misses stop, the cause was the bound; if
they keep happening, slice 2 has a second thing to find, and it will have the
marker artifact to find it with.

## Correction — 2026-08-08, job #494 (slice 2 landed, and how M6 gets answered)

Appended by the job that ran slice 2. Nothing above is edited except the head and
the slice table's own row. No decision is overturned: D1b is implemented as
written, including its staging — the escalation is written and **unarmed**.

### What is loud now, and what is merely recorded

| branch | level | marker | why |
| --- | --- | --- | --- |
| resolution returned **zero** | `error` | yes, `"branch": "zero"` | D1b: the agent named a session and the file it must have written is not resolvable |
| resolution returned **several** | `error` | yes, `"branch": "several"`, naming every path | D1b: one session id names one transcript, and the store keys one per task |
| resolution **never answered** | `warn` | yes, `"branch": "unresolvable"`, carrying the error | an unreachable node, or a host task on an N-1 daemon with no computed-path degrade — a reporting miss, not a platform break |
| resolution named one path and the **copy** produced nothing | `error` when the path was over the blob ceiling, `warn` otherwise | yes, `"branch": "uncopied"`, naming the path, plus `"lost": true` on the ceiling refusal | the record the platform had already resolved is gone: over the ceiling it is lost outright, and a transport miss or an N-1 degrade onto a computed path that holds nothing loses it for this run |

The last two rows are not in D1b's text and are a deliberate reading of it: D1b
separates absence from ambiguity and says nothing about the two ways a run ends
with no stored transcript *without* the resolution having refused anything. Both
are marked because the operator-visible outcome is identical — no transcript —
and their `"reason"` field is absent because `transcript_unresolved` is D1b's
code for the two refusals only, which is also why neither can escalate.

Marking the fourth row is what makes the marker's absence mean something. The
`uncopied` branch is where #492's measured cause lived — a transcript past the
copy bound — and it is also the shape an N-1 container node produces today, since
slice 1's computed-path degrade succeeds at *resolving* and the computed path may
hold nothing. Left unmarked, those two would have been silent absences
indistinguishable from a run that named no session, which is the class this
design exists to make loud. Only the level splits, and it splits the way slice 1
already had it: over the ceiling the platform refused and the record is lost, so
`error`; a transport miss is an ordinary reporting failure, so `warn`.

The surfaces the fourth `ArtifactKind` reached, which is the ripple D1b
predicted: `crates/store/src/artifacts.rs` (the variant, `as_str`/`parse`, and
`bucket_for` — the marker is an audit record, so it lives in `artifacts` and a
revoke never deletes it), `crates/api/src/routes.rs` (`application/json`),
`web/src/api/envelopes.ts` (the hand-written union), and
`web/src/components/TaskArtifacts.tsx` (the label map, and *not* the `BINARY`
list — the marker is meant to be read in place). Plus the return-shape change
D1b asks for: `Harvester::collect_agent` now returns an `AgentHarvest`
(`crates/platform-ops/src/harvest.rs`), which absorbed the `collect` wrapper
whose only job was to hide the result text, and its three callers —
`crates/dispatcher/src/exec.rs`, `crates/dispatcher/src/eval.rs`,
`crates/dispatcher/src/forge_ingest/triage.rs` — read fields off it.

### M6's measurement procedure, and why it could not be run here

M6 asks whether a legitimate run can yield a session id and no transcript.
Job #492 measured absence at **15.5%** (284 agent tasks across jobs 450–489,
44 without a transcript), and that number **cannot answer M6**: its cause was
the `copy_file` size bound, and the largest surviving transcript was 686,799 bytes
against a 690,432-byte cap, none above it. Slice 1 removed that cause — and slice
1 is **merged, not deployed**. Prod ran `9016dc3` (job #487) when this was
written; slice 1 is `71361dc`. Every absence measured before the next deploy is
still measuring the old defect.

So the procedure is:

1. **Precondition — deploy.** Prod must be running a build that carries slice 1
   (`71361dc`) *and* job #493's `RUST_LOG` default, without which the harvest's
   log lines are invisible on the fleet whatever level they are emitted at. The
   marker artifact is what makes the answer readable regardless, which is why it
   is the instrument rather than the logs.
2. **Wait for a population**, counted in agent tasks rather than days: #492's
   sample was 284 tasks over 40 jobs. Fewer than ~100 tasks says nothing about a
   rate that may be low single digits.
3. **Count the markers.** Every way of ending an agent task with a session id and
   no stored transcript now leaves an artifact, so the answer is a listing rather
   than a log grep: for each agent task in the window, is `transcript-missing.json`
   present, and what is its `branch`? The residual rate is markers ÷ agent tasks.
4. **Split it by branch, because only two branches are M6's question.**
   `"zero"` and `"several"` are D1b's refusals and are what M6 asks about.
   `"uncopied"` with `"lost": true` is #492's cause — a transcript past the blob
   ceiling — and its count is the check on whether slice 1's raise to 16 MiB was
   enough. `"uncopied"` without it and `"unresolvable"` are transport and
   N-1-degrade misses: they say a node needs refreshing, not that a run failed to
   write a transcript.
5. **Read the verdict off those two counts.** Zero markers with `"zero"` or
   `"several"` over a real population is M6 = **yes**, and D1b's escalation
   becomes armable by flipping `ESCALATION_ARMED` in
   `crates/platform-ops/src/harvest.rs` and having the dispatcher act on
   `TranscriptMiss::escalation` from the spawned harvest task. Any such marker is
   M6 = **no**, and D1b already decided what that means: the escalation is never
   armed and the marker is the permanent answer.

The absence to compare against is still the artifact store's, not the marker's —
and the marker is what makes that comparison read cleanly: a task with neither a
transcript nor a marker means the run named no session at all, which D1b's first
case says is not a defect. That sentence is only true because step 4's last two
branches are marked; before them, a size refusal and a degrade onto an empty
computed path both landed in the same silent bucket as a run that never started
an agent.

## Correction — 2026-08-08, job #497 (D6 amended: the teardown spared the CLI's own tree)

**D6 said "keep #322's teardown as it stands", and as it stood it deleted the
transcript before anything could harvest it.** Found in review of slice 5, in
the tree rather than on hardware: `CLAUDE_CONFIG_DIR` is `/chuggernaut/claude`
(`crates/agent/src/lib.rs`), which the host backend rebases to
`{task_dir}/chuggernaut/claude` — and `supervised_cmd`'s wrapper deleted
`{task_dir}/chuggernaut` whole, as its first act after the command returned,
*before* writing `exit_code`. `ClaudeProvider::run` awaits `backend.wait`, which
polls for that file, and only then does the harvest resolve the transcript. So
every agent host task would have landed on `MissBranch::Zero` — an error log and
a `transcript-missing.json` marker — on every run, with slice 1's `find_file`
looking into a directory that no longer existed. `spawn_reaper`'s repeat of the
teardown had the same effect one path over.

**The amendment: the teardown reclaims the injected tree's *entries*, sparing
`container::host::AGENT_STATE_DIR` (`claude`).** D6's guarantee is that the
**secrets** do not outlive the task, and it is intact: the ssh identity, the ADC
document and the MCP config carrying the NATS credential are all deleted at
process exit exactly as before. What changed is one leaf that holds no injected
credential at all — the CLI's own config directory, which is a *harvested
artifact* and therefore takes the lifetime `logs` and `copy_file` already have:
inside the 0700 task directory until `ContainerBackend::remove` reclaims the
whole of it. #322 §2's teardown note already states that ordering as the
intended one for anything the harvest must read after the process is gone.

Two consequences worth recording. **M5 gets sharper, not weaker**: the premise
"the CLI writes nothing it must not leak outside `CLAUDE_CONFIG_DIR`" now also
bounds what sits in the task directory between exit and removal, so a CLI that
writes a credential *into* its config dir extends that item's window from the
process to the task — still contained, still 0700, and still the thing M5
measures. And the leaf name is now a cross-crate contract: `container` cannot
depend on `agent`, so `ClaudeProvider`'s own test asserts
`CLAUDE_CONFIG_DIR == "{WIRE_CHUGGERNAUT}/{AGENT_STATE_DIR}"` — a rename on
either side would delete the transcript again, silently.

The regression is asserted at both tiers that can express it: the wrapper's own
teardown in `crates/container/src/host.rs` (the injected credential gone, the
transcript readable, `reclaim_credentials` sparing the same leaf), and end to
end in `crates/container/tests/host_backend.rs`, where an admitted agent-shaped
launch writes a transcript through `$CLAUDE_CONFIG_DIR` and the test resolves it
with `find_file` and reads it with `copy_file` **after** the task has exited.

## Correction — 2026-08-08, job #502 (slice 6 needed machinery, and the appended `ci` refuses an `xcode:` job type)

Appended by the job that built slice 6's **machinery**. Nothing above is edited,
including the head and slice 6's own row: the machinery is not the confirmation,
so the row stays `Proposed` and the head stays as it is until the proof has
actually run on the air. A later `docs` job flips both and links this section
from the head.

### Slice 6's stated contract — "none — this is the confirmation" — was wrong

Every other slice's row names a contract; slice 6's names none, on the reasoning
that a confirmation builds nothing. That reasoning skipped a step. A job type is
repo-versioned config that must **merge** before a job of that type can be
created (`docs/spec.md` §1.1), and `.chug/jobs/` declared **zero** job types with
`mode: host`. So slice 5 left the tree in a state where the node would admit an
agent host launch and nothing in the repo could ask for one. Slice 6 had a config
half, and it was invisible because the row said there was nothing to build.

What this job landed, and it is only the machinery:

- `.chug/jobs/mac-proof.yaml` — `runtime: {mode: host, env: "xcode:26.5"}`, no
  top-level `image:` (disallowed under host mode, where `runtime.env` is
  required — `crates/types/src/job_type.rs`), `work: {type: agent}` because the
  agent host path and its transcript are slice 6's subject and a command would
  prove neither, `wrap_up: {type: none}` because the branch carries no commits,
  and `placement: {node: air}` for the reason `.chug/jobs/android-proof.yaml`
  pins `nuc` — the air is the only node whose Xcode and simulators are the thing
  under test, and an unpinned release would satisfy `host` on some future second
  Mac and prove nothing about this one. `min_dispatcher` is the runtime block's
  epoch, which is a field rule and **not** a statement that host mode is served:
  the real prerequisite is a deploy carrying slice 5.
- `.chug/prompts/work/mac-proof.md` — the agent's instructions.

It is deliberately **not** in `.chug/jobs/_defaults.yaml` and no other job type
gains `mode: host`. Releasing it stays an operator act, because it is an
authenticated agent running on a Mac's login user with no container boundary.

### The appended `ci` evaluator cannot pass under an `xcode:` job type

Found while checking the worked case rather than assuming it, and it is a
platform gap no config can close.

[D4](#d4--one-host-task-per-node-stays) states the case as settled: a host job
type's work task goes to the Mac while the `ci` evaluator
`.chug/jobs/_defaults.yaml` appends carries an explicit image and resolves to
container mode — host work, container CI, one job. Mode resolution does work that
way, and `crates/types/src/job_type.rs` asserts it. **What no test asserts is
which `runtime.env` each level's launch is handed**, and the answer is: the job
type's, unconditionally.

1. A command evaluator's launch reads `job_type.runtime_env()` beside the
   evaluator's own image (`crates/dispatcher/src/launch_queue.rs`); an agent
   evaluator's does the same (`crates/dispatcher/src/eval.rs`). Neither consults
   the mode that level resolved to.
2. The worker daemon's launch handler resolves the declared environment for every
   launch through one path, container and host alike
   (`crates/worker/src/daemon.rs`). Its `xcode:` arm refuses only when the
   **node** serves no host mode — the air does — so it resolves the version
   against the node's installed Xcodes and injects a macOS `DEVELOPER_DIR` and
   `CHUG_ENV_PATH` into the container's environment.
3. The §4.1 bootstrap prelude then refuses, because `CHUG_ENV_PATH` names a
   directory no Linux container can see (`crates/container/src/lib.rs`): *"this
   node realised it somewhere this container cannot see"*, exit 1, before
   `.chug/tasks/ci.sh` runs a line.
4. And the evaluator is not overridable. A job type declaring its own `ci` does
   not replace the project default, it **collides** with it and fails
   `JobType::with_defaults` (`crates/types/src/job_type.rs`), so there is no
   config-level way to drop it, rename it or give it a different environment.

**This is right for `nix:` and wrong for `xcode:`, which is why it survived.**
Both existing tests of the worked case declare `nix:.#chug-mobile`, and a nix
environment genuinely is realised node-side and mounted into a container
([#373](373-project-toolchains.md) P2) — a container CI evaluator under a host
job type gets a usable toolchain. An Xcode is the one environment that cannot
cross that boundary, which is [#322](322-macos-native-runtime.md)'s whole
premise. So the gap is exactly as wide as the case this design is for, and it
reaches every `mode: host` + `xcode:` job type in this repo, not just this one.

**What it costs slice 6, precisely.** Less than it looks. A work agent task's
transcript and output archive are harvested in the same spawned task that ran it,
before the job reaches evaluation at all (`crates/dispatcher/src/exec.rs`,
`crates/platform-ops/src/harvest.rs`) — so slice 6's actual subject, an agent host
task run end to end with its transcript resolved and harvested, is observable
from a released `mac-proof` job today. What the job does **not** do is finish: the
red `ci` evaluator escalates it instead of taking it to Done, and with
`rework_budget` at its default of 0 it escalates once rather than looping. A proof
that merges nothing loses little by escalating; a proof whose report is
indistinguishable from a platform failure loses a lot, which is why this is
recorded here rather than absorbed.

**The fix is a `code` slice and it is not this one.** Scope a launch's
`runtime_env` to the mode that launch resolves to: a level carrying its own
`image` has opted back into container mode — `validate_top_level_image`'s own doc
comment says exactly that — and should not inherit a host-only environment
reference. The narrower alternative, having the daemon's `xcode:` arm refuse a
**container** launch as well as a host-modeless node, is worth doing on its own
merits (`crates/worker/src/xcode.rs`'s contract is that no launch falls through to
an unusable toolchain, and this one falls through to an unreachable one), but it
is not sufficient: it converts a confusing bootstrap refusal into a named launch
refusal and leaves the evaluator just as red. Only the first makes a host job type
green.

### What the prompt measures, and what it refuses to settle

M5 and M7 are the two rows the M table left open, and slice 6's row promises both
are "settled here". One of them cannot be, and the prompt says so rather than
producing a number that reads like an answer.

**M5** is measurable in one run and this is the run that can do it: job #492's
zero-residual result was taken under `env -i` with an isolated home and an
**unauthenticated** CLI, not under the daemon. The prompt asks for a list rather
than a verdict — a `find -newer` sweep of the daemon user's home against a
reference file the platform wrote at launch (so it predates the CLI process, which
the agent's own "before" snapshot cannot), named probes of the directories an
agent harness actually writes to, and the login keychain's mtime either side. It
also requires the attribution limit be stated: the daemon and an operator's login
session run beside the task, so a `-newer` hit proves a file changed during the
window and not that this task changed it. `security find-generic-password` is
restricted to attribute-only form, because `-w`/`-g` return the secret and can
raise a GUI prompt on a headless node.

**M7 cannot be answered by one host task**, and the row that says it is settled
here is the second thing this correction corrects. The question is whether state
one task leaves disturbs the *next*, which needs two. What one run can produce is
the baseline the second gets diffed against, so the prompt captures
`simctl list devices`/`list runtimes` as JSON before and after, forbids `erase`,
`delete` and `shutdown all`, and writes the raw captures to the task's output
archive rather than only into prose. A task that tidied up after itself would
have measured nothing.

## Correction — 2026-08-08, job #507 (the launch path now scopes `runtime.env` to the level it launches)

Appended by the `code` slice the [job #502
correction](#correction--2026-08-08-job-502-slice-6-needed-machinery-and-the-appended-ci-refuses-an-xcode-job-type)
named and deliberately did not take. That section's present-tense description of
the gap is history as of this one: **a container level of a host job type is now
handed no `runtime.env`**, so `mac-proof`'s appended `ci` evaluator is an
ordinary Linux container launch and the "host work, container CI, one job" case
of [#309](309-host-native-execution.md) §1 runs end to end. Slice 6's row is
unchanged and still `Proposed` — the proof is a job that runs on the air, and
this is only the platform half that lets it reach Done.

**What the fix is, and where it lives.** `types::JobType` gained the per-level
accessors the resolution rule always implied: `level_image`, `level_mode` and
`level_runtime_env`, each taking a `Level` (`Work`, `Eval(&Evaluator)`,
`WrapUp`). A level's **own** `image` resolves it to container mode, and a level
whose mode differs from the job type's inherits no `runtime`. `runtime_env()` is
now private, so a launch site cannot ask the job-type-level question by accident
— which is exactly how the defect survived: validation knew the rule
(`crates/types/src/job_type.rs`) and three launch sites
(`crates/dispatcher/src/eval.rs`, `crates/dispatcher/src/exec.rs`,
`crates/dispatcher/src/launch_queue.rs`) each read the job type's declaration
unconditionally. `Core::command_launch_config` now takes the `Level` rather than
a pre-resolved image, so the image and the runtime come from one rule; the
dispatcher's own `eval_image` helper is gone into `JobType::level_image`.

**Container mode is deliberately untouched**, and that is the narrow reading the
[#309 precedence rule](309-host-native-execution.md#coexistence-on-a-mixed-fleet)
supports rather than the broad one — and #309's own head and body now say so,
corrected in this job. Under `mode: container` an `image` and an `env` **layer** — that is
[#373](373-project-toolchains.md) Decision 2's whole cell — so a level naming its
own image there still gets the declared environment; dropping it would have
silently taken the toolchain away from the `ci` evaluator of every nix-layered
container job type. The rule that fires is the one `docs/spec.md` §1.1's
`mode: host` row states: a level carrying its own image "is a container task
regardless and does not inherit `runtime`".

**What was untestable before and is asserted now.** The unit half is in
`crates/types/src/job_type.rs` — a host job type's work level yields its
`xcode:` reference and its explicit-image evaluator yields `None`, the mirror
case where no level declares an image, and container mode's layering. The launch
half is `crates/dispatcher/tests/runtime_levels.rs`, tier 2: a `mac-proof`-shaped
job driven from release to Done, asserting the work run carries `xcode:26.5` and
no image while the `ci` evaluator's launch config carries an image and no
`runtime_env`. It fails against the previous tree with `Some("xcode:26.5")`,
which is the assertion the four slices before it never made.

## Correction — 2026-08-09, job #510 (slice 6 ran: what two host tasks on the air measured)

Appended by the `docs` job that recorded slice 6. Nothing above is edited except
the head, slice 6's own row and the note under the slice table. Everything below
is read out of the two `mac-proof` runs' own reports and output archives, taken
on `gumbo-air-0`; nothing here was re-derived from the tree, and where a run
could not attribute what it saw, that limit is carried with the number.

### What ran, in order

| job | outcome |
| --- | --- |
| #502 | the machinery — `.chug/jobs/mac-proof.yaml` and `.chug/prompts/work/mac-proof.md` ([that correction](#correction--2026-08-08-job-502-slice-6-needed-machinery-and-the-appended-ci-refuses-an-xcode-job-type)) |
| #504 | the first host launch ever attempted, **refused before any process ran** |
| #505 | the fix: a wire prefix matched mid-segment, so every launch carrying this repository's `REPO_URL` was refused |
| #506 | **the first agent host task this platform has run**; work PASS, escalated on its container `ci` evaluator |
| #507 | the fix: a container level of a host job type inherited the job type's `runtime.env` ([that correction](#correction--2026-08-08-job-507-the-launch-path-now-scopes-runtimeenv-to-the-level-it-launches)) |
| #508 | the deploy carrying both fixes |
| #509 | **green end to end** — host work and container CI in one job |

### What the runs proved

- **M1's last conjunction closed.** An *authenticated* agent CLI ran as a native
  macOS process on the air and its `session.jsonl` was harvested at **462,085
  bytes** (#506). Slice 0 could establish this only as two separately verified
  halves — an authenticated run in production, which is a container, and an
  unauthenticated one on a Mac.
- **M2's residual closed.** The Mach-O `chuggernaut-channel` the node installs
  carried `update_status` and `submit_result` on the **first call with no
  errors**, on both runs. That is the half job #492 could not measure: it could
  test the binary, but not the CLI as the process that launches it, and only an
  authenticated CLI is doing the launching.
- **[#309](309-host-native-execution.md) §1's worked case runs.** "Host work,
  container CI, one job" is the case [D4](#d4--one-host-task-per-node-stays)
  states as settled and job #502 found the platform could not serve. #509 is the
  first job to perform it.

### M5 — the residual is one directory, and D6 is open

Exactly **one** attributable write outside `CLAUDE_CONFIG_DIR`, identical on both
runs: the agent CLI's own MCP log tree under the daemon user's
`Library/Caches`, at `claude-cli-nodejs/<workspace-key>/mcp-logs-chuggernaut-channel/` <!-- runtime -->,
holding one `.jsonl` per session.

- It **accretes across tasks.** #509 found #506's subtree still sitting beside
  the one its own run created, and nothing on the node reclaims either — not the
  task teardown, which reaches only the task directory, and not the CLI.
- The **keychain was not written**: the login keychain's mtime was unchanged
  across the run and nine plausible service names returned nothing, probed
  attribute-only. That is what an env-injected `CLAUDE_CODE_OAUTH_TOKEN`
  predicts, and #492 said "expected" was the word this measurement exists to
  distrust — it is now observed instead, twice.
- **The attribution limit stands.** The daemon and an operator's own login
  session run beside the task, so a `find -newer` hit proves a file changed
  during the window and not that this task changed it. The one entry above is
  attributable because it is named for this platform's own MCP server; nothing
  else in either sweep was.

**So D6 is false as stated.** Its premise is that the CLI confines itself to
`CLAUDE_CONFIG_DIR`, and on a host there is no boundary to make that free. D6's
guarantee about **secrets** is untouched — the injected credential tree is still
deleted the moment the command returns, and nothing in the residual is a
credential — but the decision as written no longer describes what a host agent
task leaves behind. The M5 row already wrote the two ways out, "Teardown grows,
or the daemon user does", and **this job takes neither**: it gives the decision
its subject, which is a single named directory whose growth is unbounded across
tasks. D6 is open, and the head says exactly that and no more.

### M7 — two samples, and deliberately no verdict

| run | action | device-set delta |
| --- | --- | --- |
| #506 | first boot of an iPhone 17 that already existed | **~1.91 GiB** — the device populating its data |
| #509 | re-boot of that same device | **+20.4 MiB** |

Neither run created a device and neither changed the installed runtimes, which is
what the prompt's ban on `erase`, `delete` and `shutdown all` was for. The second
run inherited the first's state and was **cheaper** for it, not disturbed.

That is two samples, not an answer, and the difference matters because slice 6's
row promised M7 would be settled here. "Did not disturb" observed twice is not
"cannot disturb": both runs did the same small thing to the same device, so
nothing has yet provoked the interference M7 asks about — a task finding a device
in a state it did not expect. #509 wrote its own before/after `simctl` captures
into its output archive, so a third run diffs bytes rather than prose.
[D4](#d4--one-host-task-per-node-stays) is unchanged and
[#322](322-macos-native-runtime.md) §5's per-task device set stays where it is.

### A finding neither M-row asked for: `simctl spawn` fails under the daemon's session

`xcrun simctl spawn <udid> …` fails on the air under the worker daemon —
`LaunchdSimError` 111, `NSPOSIXErrorDomain` 2 — while `xcrun simctl launch`
against the same booted device works, and Preferences and Safari were both
launched inside it. **Reproduced on both runs**, so it is a property of the
session the daemon's launchd agent gives a host task rather than a flake.

Recorded, not fixed here. Two things it touches and this job does not decide:
`.chug/prompts/work/mac-proof.md` §2 proves a running simulator with `spawn`, so
a third run should prove it with `launch`; and any job type meaning to run a
process *inside* a simulator from a host task has a constraint nobody had
measured.

### The part worth stating plainly

Both #505 and #507 were **launch-blocking**, and both were invisible to a full CI
gate and to every stage-0 reviewer across five slices — because none of those
five slices had ever launched a host task. Each had been in the tree for slices;
each surfaced in the first second of an attempted launch. That is the argument
[the slice table](#slices) makes for slice 6 existing, and it is now evidence
rather than a claim.

## Correction — 2026-08-09, job #512 (D6 amended: the premise is false, the guarantee holds, and the teardown grows by one path)

Appended by the `design` job that closes
[D6](#d6--credential-lifetime-is-unchanged-in-mechanism-longer-in-duration).
Nothing above is edited except the head and one new row in [the slice
table](#slices). The measurement is jobs #506's and #509's, recorded in
[the job #510 correction](#correction--2026-08-09-job-510-slice-6-ran-what-two-host-tasks-on-the-air-measured)
and re-inspected on `gumbo-air-0` on 2026-08-09 as counts and file names only,
with no values read; every code citation below was read out of the tree at
`fbe42e1` while writing this. No decision other than D6 is touched: D1–D5 and D7
stand as written, and [M7](#m7--two-samples-and-deliberately-no-verdict) keeps
its two samples and its absence of a verdict.

### Both halves, and they have to be said together

D6 names its own premise and marks it unmeasured:

> It assumes the agent CLI confines itself to `CLAUDE_CONFIG_DIR`. In a container
> that assumption is free — anything it wrote elsewhere died with the container.
> On a host there is no such boundary.

**The premise is false.** Two authenticated agent host tasks under the daemon's
own user each made exactly one attributable write outside `CLAUDE_CONFIG_DIR`,
identical on both runs: the CLI's own MCP log tree under the daemon user's
`Library/Caches` <!-- runtime -->, one `.jsonl` per session under a directory
named for this platform's channel server. It **accretes** — #509 found #506's
subtree still sitting beside its own — and nothing on the node reclaims either.

**The guarantee that premise was holding up is intact.** D6 bounds the lifetime
of *credentials*, and no credential-shaped content is in the residue. The
injected tree is still emptied by the task's own wrapper the moment the command
returns and re-emptied by the daemon's reaper (`supervised_cmd` and
`reclaim_credentials`, `crates/container/src/host.rs`); the login keychain was
not written, across an mtime check and nine attribute-only service probes, which
is what `Core::inject_platform_agent_secrets`
(`crates/dispatcher/src/exec.rs`) predicts — a token delivered as an environment
variable gives a keychain nothing to store.

**Saying only the second half is how a design rots.** A decision whose stated
premise turned out false but whose guarantee survives should say exactly that,
out loud, rather than being quietly re-derived on the strength of the conclusion
it happened to reach. The premise is what a later reader would have generalised
from — "the CLI stays inside its config directory" is a sentence someone would
lean on when adding the next host-mode surface — and it is the part that must be
withdrawn by name.

### What is in the residue, and why "it is only logs" is not the argument

Inspected on the node, counts and names only:

| | |
| --- | --- |
| whole cache | **68K**, four subtrees, two of them left by host tasks |
| the subtree key | the task's own workspace path, so two tasks never share one |
| `sk-ant`, `CLAUDE_CODE_OAUTH_TOKEN`, `Bearer `, `creds`, `PRIVATE KEY`, `nats://` | **zero** files each |

The tempting close is "MCP logs carry no credentials, so nothing that matters
outlives the task". **Refuse it**, on two grounds.

- It is a claim about *content at one moment*, and the thing being decided is a
  *property of the system*. An MCP log is a record of traffic, and the traffic on
  this transport is the platform's own: `update_status` and `submit_result`
  carried a work summary through it on both runs. Nothing stops a future tool
  from carrying a diff, a file or a project's own text over the same channel, and
  when one does, the sweep either already exists or is written under time
  pressure after somebody notices.
- "Harmless" is not a bound. The subtree grows once per task and nothing reclaims
  it, which is the unbounded-and-quiet shape [`docs/reference/style.md`](../reference/style.md)
  Tier 2 rule 3 refuses on its own terms, independently of what the bytes say.

So the absence of credentials is what makes this an **amendment** rather than an
incident. It is not what makes the sweep optional. The property D6 owns is *what
outlives a task*, and one directory does.

### The M5 fork, resolved: teardown grows, the daemon user does not

M5's row wrote the choice as "Teardown grows, or the daemon user does". The
second is rejected, and on the tree rather than on taste:

1. **The daemon inhabits a domain a per-task user cannot.** The macOS worker
   daemon is a launchd agent in the **login user's GUI domain**
   ([#440](./440-native-worker-daemon.md) D2); `deploy/prod/install-worker-launchd.sh`
   bootstraps the plist into `gui/$(id -u)` literally, and that domain is how the
   native conversion works at all. A per-task user has no such session to be
   bootstrapped into — and [#322](./322-macos-native-runtime.md)'s whole premise
   is a task reaching the login user's Xcode, simulators and provisioning.
   Isolating the user isolates the task from the machine it exists to drive.
2. **There is no concurrency for a second user to isolate.** `enforce_host_capacity`
   (`crates/worker/src/daemon.rs`) refuses to boot a host-capable node whose
   slots and slot ceiling are not both 1 — [#309](./309-host-native-execution.md)
   §2 option (iii), kept by [D4](#d4--one-host-task-per-node-stays). One host task
   runs at a time, so the only task a second user could separate this one from is
   its own **predecessor**, which is what a teardown does at a fraction of the
   cost.

A weaker version of the same move — leave the user alone and give the task its
own `HOME` — is rejected for a third reason on top of both of those. `floor_env`
(`crates/container/src/host.rs`) carries exactly `PATH` and `HOME` from the
daemon into a host task, so this is nearly a one-line change, and that is
precisely what makes it dangerous: it relocates **every** tool's per-user state
rather than the one directory that was measured. CoreSimulator keeps its device
set under the user's home, so a per-task `HOME` delivers
[#322](./322-macos-native-runtime.md) §5's per-task device set as a side effect —
the thing [D4](#d4--one-host-task-per-node-stays) defers deliberately — and takes
the operator-provisioned simulators with it. It is also unmeasured: on macOS a
process's home directory is not always the environment's `HOME`, so the change
could relocate the toolchain and leave the cache exactly where it is.

### Where the subtree is located, and why it is listed rather than computed

The constraint that shapes the slice: it must remove **only** the subtree keyed
by this task's own workspace path. The node's cache also holds subtrees belonging
to other tools — one from a `beacon` actions-runner, dated June — and a sweep
that took those would be deleting another tool's state.

The key is the CLI's slugification of the **resolved realpath** of its cwd, which
[the slice 0 correction](#a-fourth-measurement-row-the-slug-is-of-the-resolved-realpath)
measured directly: a cwd under `/tmp` produced a key naming `/private/tmp`. So
"compute the name" is [D1](#d1--the-transcript-is-resolved-by-session-id-not-by-a-computed-path)'s
candidate 1 arriving in a second place, and it inherits every objection D1 made
plus the realpath finding that turned that rejection from fragility into
correctness. Three shapes were weighed.

1. **The worker computes the slug.** Canonicalize `{task_dir}/workspace`, apply
   the character rule, remove that one name. Feasible — `std::fs::canonicalize`
   supplies the realpath half a shell slugifier lacks. Rejected twice over. It
   re-adopts in a quieter place the dependency D1 spent a decision removing: a
   wrong slug removes nothing, and "removed nothing" is indistinguishable from
   "the CLI wrote nothing", so the failure is silent in exactly the way this
   document exists to prevent. And it is **incomplete today**, not only
   tomorrow — D1's candidate 3 was falsified by a second cwd inside one config
   dir producing a second directory, and the cache is keyed the same way, so a
   computed single name leaves behind every subtree but one.
2. **The wrapper does it in shell**, beside the `find` that already empties the
   credential tree. It is the tightest possible moment and it is where the
   existing teardown lives. Rejected: the wrapper is one `sh -c` string, and the
   construct this needs there is a glob-driven `rm -rf` — the
   highest-consequence line in `crates/container/src/host.rs`, written in the one
   language where the containment check is hardest to make and where
   [`docs/reference/style.md`](../reference/style.md)'s own shell-quoting rule
   exists because a line can mean two things in two shells. It also does nothing
   when the wrapper is killed, which is the exact gap `reclaim_credentials` was
   written to cover.
3. **The worker lists the cache root and matches on the task directory's own
   name.** Adopted.

**Why 3 is right and not merely the survivor**, which is D1's argument reaching
the same place by the same route. A host task directory is named from
`TASK_PREFIX` and two hex fields (`HostBackend::launch`,
`crates/container/src/host.rs`) — every character alphanumeric or `-`, so it
survives *any* character-mapping slugifier unchanged and appears verbatim inside
the key. The predicate is therefore "an immediate child of the cache root whose
name contains this task's directory name", and it relies on nothing about the
prefix, the realpath, the leading slash or how many dashes fall where. It matches
on a token **the platform itself supplies**, which is the same distinction D1
drew between depending on a tool's output and depending on its input, and it
covers the multi-cwd case candidate 1 misses, since every cwd inside the task
directory yields a key containing that name.

Two costs, stated rather than discovered later:

- It still assumes alphanumerics survive slugification. If the CLI moved to
  hashing its key, the predicate matches nothing and the residue accretes as it
  does today — a leak, logged, not a wrong deletion. That asymmetry is the reason
  for the shape: a miss leaks disk, a false match deletes another tool's state,
  and the predicate is chosen so failure lands on the first.
- A cwd **outside** the task directory produces a key this predicate deliberately
  does not match, because such a subtree is not keyed to the task and may be
  shared with something else. That is residue this sweep does not bound, and it
  is named here because M5's whole finding was a residue nobody had looked for.

### Where it runs, and the charter it is held to

Beside `reclaim_credentials` in `spawn_reaper` (`crates/container/src/host.rs`),
which is the daemon's own repeat of the wrapper's teardown and fires when the
wrapper exits — the same moment the credential tree is emptied, and a moment that
arrives whether or not the dispatcher ever calls `remove`. And again from
`remove`, so a daemon restarted between the two still reclaims the subtree when
the task directory goes.

Its charter is the nix stale-root reaper's, quoted because the sentence is
already in the tree: it "leaks disk rather than ever failing a job"
(`crates/worker/src/nix.rs`). A cache root that does not exist is a skip; an
unreadable directory is a warning and zero removed; a removal that fails is a
warning and the next task's is still attempted. **It never contributes to
`remove`'s failure list** — that strictness exists for the 5–10 GB `target/`
nothing else on the node reclaims ([#309](./309-host-native-execution.md) §2(c)),
and borrowing it for a 68K log tree would trade a bounded leak for a failed task.
The pass is bounded in entries examined, the way `crates/worker/src/nix.rs`'s is.

### What this does not change, and the trigger it sharpens

D6 is otherwise as written and as amended in
[job #497](#correction--2026-08-08-job-497-d6-amended-the-teardown-spared-the-clis-own-tree):
no rotation, the injected tree deleted at return, `AGENT_STATE_DIR` spared for
the harvest that runs after the process is gone.

D6's revisit trigger — *"Revisit if host tasks ever run unattended on a shared
node, which D4 currently prevents"* — **stays**, and the payload point above is
what sharpens it. The sweep bounds how long the residue sits on the node; the
trigger is about who else can read it while it is there. An MCP log carrying
request payloads on a node one operator uses is a different exposure from the
same log on a node several projects share, and that is a decision about
isolation rather than about lifetime. The sweep does not retire it and must not
be read as having done so.

One cost of the sweep, recorded because it is the instrument that produced this
correction: it deletes the trail an operator would read when debugging a host
task's MCP transport, and after it lands that trail has to be caught on the node
while the task is running. Accepted — the log is not harvested, so it was never
readable from the UI in the first place, and a debugging convenience is not a
reason to leave an unbounded accretion on a machine.

## Correction — 2026-08-09, job #527 (the `simctl spawn` finding was misattributed: the argument, not the session)

Appended by the `code` job that fixes the prompt which produced it. Nothing above
is edited except one clause in the head; [the finding
itself](#a-finding-neither-m-row-asked-for-simctl-spawn-fails-under-the-daemons-session)
stands as jobs #506 and #509 honestly recorded it, because what was wrong is its
attribution and not its observation. The measurements below were taken by an
operator on `gumbo-air-0` on 2026-08-09 and are quoted here; a work container has
no Mac, so nothing in this section was re-run while writing it. The two tree
facts are checkable and were checked: the prompt's §2 text, and the finding above.

### What was measured, and where

Over an **SSH session** — `launchctl managername` reporting **Background**, so
not the daemon's GUI session at all — against a freshly created and booted
iPhone 17, deleted afterwards:

| command | result |
| --- | --- |
| `xcrun simctl spawn <udid> launchctl list` | **works** — prints the simulator's own launchd jobs (`com.apple.progressd`, `com.apple.CoreAuthentication.daemon`, …) |
| `xcrun simctl spawn <udid> uname -a` | `NSPOSIXErrorDomain` code 2, *No such file or directory* |
| `xcrun simctl spawn <udid> /bin/ls /` | `LaunchdSimError` 111, underlying `SimXPCErrorDomain` 111, *Invalid or missing Program/ProgramArguments* |
| `xcrun simctl spawn <udid> /usr/bin/uname -a` | the same `LaunchdSimError` 111 |

### Why the session is ruled out, which is the load-bearing part

Both of the finding's error codes reproduce **outside** the daemon's session.
That alone falsifies "a property of the session the daemon's launchd agent gives
a host task": a property of that session cannot be exhibited by a session that is
not it. What replaces it is visible in the same table — the four commands
separate cleanly by **argument**, not by session. A program that exists inside
the iOS runtime spawns; one that does not, fails, with the error distinguishing
*name not found* from *program not runnable*. Reproducing on both #506 and #509
was read as ruling out a flake and it does; it does not rule in the session,
because both runs passed the same argument.

### The real constraint, and it is ordinary

`simctl spawn` executes the named program **inside the simulator's own
filesystem**, so the program has to be there. iOS ships `launchctl`; it does not
ship `uname`, and a host absolute path such as `/bin/ls` does not resolve inside
the simulator at all. So the finding's second claim — that a job type meaning to
run a process inside a simulator from a host task "has a constraint nobody had
measured" — is withdrawn too. The constraint is the one every iOS engineer
already works under, it is not specific to host tasks or to this platform, and a
ported workflow shelling out to `simctl spawn` hits it exactly when it names a
binary the runtime does not carry.

### The cause was in the prompt, and the prompt is fixed here

`.chug/prompts/work/mac-proof.md` §2 asked for `xcrun simctl spawn <udid> uname
-a` as its proof of a running simulator — a command that cannot succeed on any
simulator in any session. Two proof runs ran it, failed, and reported the failure
as a finding about the daemon. §2 now asks for `spawn <udid> launchctl list`,
which is measured working, and states why the obvious command does not, so the
next run does not re-derive the same false attribution.

The finding above suggested "a third run should prove it with `launch`". That
suggestion was made on the wrong diagnosis and is **not** what this job applied:
`launch` needs an installed bundle and hands back a pid rather than output, where
the rung wants something quotable from inside the running simulator. `spawn` with
an argument the runtime carries is both the shorter proof and the one that
demonstrates the corrected fact. `launch` stays named in the prompt as the answer
for a rung that needs what the runtime does not ship.

### What this does not touch

[D4](#d4--one-host-task-per-node-stays) and
[M7](#m7--two-samples-and-deliberately-no-verdict) are unchanged, and so is
[#322](322-macos-native-runtime.md) §5's deferred per-task device set. The
tenancy question is a different one, recorded by its own job, and the confound in
that measurement does not reach here: this correction rests on the errors
separating by argument, which it would do in any session. Two documents citing
the finding are corrected in the same commit —
[#308](308-gha-port.md)'s open-findings list and
[#322](322-macos-native-runtime.md)'s pointer at slice 6.

## Amendment — 2026-08-09, job #524 (D7's trigger fired, and the warning became a refusal)

[D7](#d7--a-host-tasks-resources-are-unbounded-and-the-node-flag-does-not-say-so)
is accepted on two grounds and names its own trigger. Both have moved.

**"Nothing consumes it" is no longer true.** [#309](./309-host-native-execution.md)
§7's placement predicate landed in job #524, so `resources_enforced` decides
something: a launch declaring `resources.cpu`/`memory` is placed only on a node
that enforces them. D7 priced that trigger as *"a shape change to
`NodeCapabilities` that every consumer of the advertisement would have to be
re-read against"* — and the actual cost was smaller, because the truthfulness
D7 wanted lives in the **reader** rather than in the wire. `PlacementCandidate::bounds`
narrows the node's one bool to the launch's resolved mode, so the air's `true`
is applied to the container launches its Docker `HostConfig` bounds and to
nothing else. The field's contract is unchanged, the wire is unchanged, and
`WORKER_RPC_VERSION` did not move. D7's own framing — *"one bit answering two
questions, and it answers the container one"* — turns out to be the whole fix:
ask it only the question it answers.

**The per-launch warning is now a refusal.** D7 names `HostBackend::admit`'s
`tracing::warn!` as "the granularity the flag lacks". §7 is explicit that an
unenforceable bound is *"never a silent ignore"*, and a warned-then-run task is
exactly the option 1 §7 rejected, so job #524 made it `BackendError::Launch`
naming the field and the node. Read D7's paragraph accordingly: the signal is
still per launch, but it stops the launch rather than annotating it.

**What D7 decided is otherwise unchanged.** A host task's cpu and memory are
still bounded by nothing on this node or any other; `resources.task_timeout`
still bounds it in time; and [D4](#d4--one-host-task-per-node-stays)'s one task
per node is still what makes that tolerable. The only difference is that a job
type asking for a bound on a Mac is now *refused* rather than told.

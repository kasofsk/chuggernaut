# Design #577 — The fleet on Nomad: an execution fabric for chuggy

Status: PROPOSED — argued, not built, and its tenant does not exist yet.

Written against the four fleet machines as surveyed over ssh on 2026-08-13;
every hardware figure below was measured on the box rather than recalled, and
the command that produced each class of figure is named beside the table. This
design is deliberately **greenfield**: it does not build on, extend or preserve
anything in this tree. The platform this repository implements is being
archived and torn down, and chuggy — designed elsewhere — replaces it. Where a
question here has been answered before in these docs, it is re-answered from
first principles rather than inherited, because the machines are being rebuilt
from scratch and none of the current arrangement survives.

Everything after the horizontal rule is the append-only argument.

## Current state

Four machines, all on one LAN segment, all reachable by ssh as `worksalot`.

| Host | Arch / CPU | RAM | Free disk | LAN | Notes |
| --- | --- | --- | --- | --- | --- |
| `gumbo-nuc-0` | x86_64, i7-10710U 6c/12t | 31 GiB | 313 G of 450 G | 192.168.129.133 | NixOS 26.11, kernel 6.18.31. The only Linux box |
| `gumbo-mini-0` | arm64, M2, 8 cores | 8 GiB | 63 G of 228 G | 192.168.129.202 | macOS 15.7.2. Never sleeps, never moves |
| `gumbo-air-0` | arm64, M1, 8 cores | 8 GiB | 155 G of 461 G | 192.168.129.167 | macOS 26.5.2. Most free disk in the fleet |
| `gumbo-air-2` | arm64, M1, 8 cores | 16 GiB | 82 G of 229 G | 192.168.129.152 | macOS 26.5.1. Most RAM in the fleet |

Figures from `uname -a`, `sw_vers`, `sysctl hw.model hw.ncpu hw.memsize`,
`df -h` and `ipconfig getifaddr`, run on each host.

Three measured facts that shape everything below:

| Fact | How it was established | Consequence |
| --- | --- | --- |
| All four hosts are on `192.168.129.0/24` behind gateway `.1` | `ipconfig getifaddr` / `ip -4 addr`, and `netstat -rn` for the default route | Cluster traffic needs no overlay, no tunnel and no MTU adjustment |
| The tailnet's tagged-to-tagged restriction covers **Tailscale ssh only**, not traffic | From `gumbo-mini-0`: ssh to the NUC is refused by policy, while `nc -z` to a port with a listener connects and a port without one returns an instant RST rather than a timeout | Tailscale is an access convenience here, not infrastructure |
| Both Airs answer `hostname` with `gumbo-air-0` | `scutil --get LocalHostName` distinguishes them; the tailnet calls `gumbo-air-2` by the name `dev-air` | Host identity must be reconciled during the rebuild, before any node registers |

## Decisions

| # | Decision | One-line rationale |
| --- | --- | --- |
| **D1** | **Nomad is the execution fabric.** One cluster over all four machines: a server plus Linux client on the NUC, `raw_exec` clients on the three Macs. | It is the only fabric that runs Linux containers and **native macOS processes** under one control plane, at an operational weight suited to four machines. |
| **D2** | **Kubernetes is rejected.** | A macOS node requires a virtual-kubelet provider that wraps every task in a VM and, in doing so, gives up pod logs, secrets, config maps and volumes — the four things an agent task most needs. See §2. |
| **D3** | **A VM per task is rejected**, despite being the textbook answer for iOS CI. | On 8 GiB hosts the VM's slice must still hold Xcode, a simulator and an agent, and a macOS base image is tens of gigabytes against the Mini's 63 G free. The cost is not proportionate to the isolation. See §3. |
| **D4** | **Isolation is a non-admin unix user per project**, with a per-task workspace wiped at task start rather than at task end. | It is the only isolation primitive macOS enforces without a hypervisor, it makes the package prefixes read-only to the task, and wiping at start means a crashed task leaves its evidence behind. |
| **D5** | **Every macOS task creates and destroys its own simulator.** | A simulator carried between tasks carries app state — logged in, past onboarding — and the evaluator is supposed to be independent of what the work task did. |
| **D6** | **The evaluation task installs the work task's artifact; it never rebuilds it.** | An iOS rebuild costs more than the whole evaluation, and rebuilding would also re-derive the artifact under test rather than testing the one produced. |
| **D7** | **The artifact directory is keyed by chuggy's decision sequence number**, and a complete directory is the record that the effect already happened. | Nomad generates its own dispatch identifiers, so exactly-once by decision seq has to be constructed; the filesystem is the cheapest place that is also durable. |
| **D8** | **Linux capacity is the NUC alone, and that is sufficient.** No second Linux node, no multi-architecture images, and no registry until a second Linux node exists. | Images built on the one node can be referenced by local tag; every macOS task is a native process with no image at all. |

## Slices

| # | Slice | Status |
| --- | --- | --- |
| S1 | Establish whether a simulator-driving task runs under a session-less user, and whether a Nomad client must be a GUI-domain agent | Proposed |
| S2 | Archive the current platform's operational data, then stop it | Proposed |
| S3 | Nomad server plus Linux client on the NUC, declared in the host's flake | Proposed |
| S4 | First macOS client, one project user, one macOS task run end to end by hand | Proposed |
| S5 | chuggy's actor, journal volume and dispatch of work and evaluation jobs | Proposed |
| S6 | Remaining two macOS clients | Proposed |
| S7 | Port the mobile CI workloads off GitHub Actions | Proposed |

---

## The record

### 1. The choice turns on macOS, and only on macOS

The fleet runs two kinds of work. Agent tasks in Linux containers are easy: every
candidate fabric does them well, and the NUC has more capacity than the stated
scale — dozens of jobs and hundreds of tasks per day — requires. Native macOS
work is the hard half, and it is the reason three of the four machines are Macs
at all: iOS and Android builds, and agents that drive a simulator.

So the fabric should be chosen on how well it runs macOS, and the Linux half
treated as a tiebreak that no candidate loses.

### 2. Why not Kubernetes

macOS cannot host a kubelet. A Mac joins a Kubernetes cluster only through a
virtual-kubelet provider, where a pod's first container is a macOS VM booted
through Virtualization.framework and later containers are side-cars on the host.
That is a real and working route, and it costs four things at once:

- **No logs from the VM.** For a platform whose product is the agent transcript,
  the transcript would have to be shipped out of band by the workload itself.
- **No secrets and no config maps.** This is a consequence of the virtual-kubelet
  architecture rather than of virtualization: the provider must re-implement
  whatever the real kubelet does, and macOS has no first-boot injection channel
  of the cloud-init kind, so it was scoped out.
- **No persistent volumes.** Which removes both the warm build cache and the
  natural way to hand an artifact from one task to the next.
- **A two-VM ceiling per host**, enforced by the framework.

Any one of these is survivable. All four, on the workload that most needs them,
is not. The ceiling turns out not even to be the binding constraint — see §6 —
which is a sign of how far the fit is from the shape of the work.

### 3. Why not a VM per task

Running each macOS task in a fresh VM is the textbook answer for iOS CI, and the
reason the answer exists is sound: a clean room per build. It was rejected here
on the hardware rather than on the principle.

An 8 GiB Mac that hands a VM 6 GiB, and then runs Xcode, a booted simulator and
an agent *inside* that slice, is strictly worse off than the same machine running
the same three things natively, where the operating system can use all of it. And
a macOS base image with Xcode is tens of gigabytes per host, against 63 G free on
the Mini. Boot time is not the objection — image clones are cheap and thirty
seconds is nothing against a task measured in tens of minutes — memory and disk
are.

What the VM was buying is bought more cheaply in §5.

### 4. The cluster

The NUC is the Nomad server and the only Linux client: control plane, chuggy's
actor, the journal on a host volume, and container tasks. The three Macs are
clients running native processes.

Nothing is held in reserve. A Nomad task and a CI runner are both just processes,
so a Mac can carry the existing mobile CI runner and join the cluster at the same
time — it is advertised with reduced resources so the scheduler under-fills it.
There is no VM ceiling to ration, no second container runtime to keep away from
the first, and no node that has to be exclusively one thing.

Because all four hosts share one subnet, node addresses are LAN addresses and
there is no overlay to tune. This deletes a class of failure — a mismatched MTU
under a tunnel, where pods hang on large responses while a ping succeeds — rather
than configuring around it.

Single-writer fencing is stronger here than the Kubernetes alternative offered.
Pinning the actor to the NUC and taking a local file lock beside the journal is
genuine at-most-one execution; a volume-attach fence only approximates it.

### 5. The execution model

Multi-tenancy on macOS is a unix user, not a directory convention. Each project
gets a non-admin account, and the task runs as it. Separate homes, separate
caches, and enforcement by filesystem permission rather than by convention —
and because the account is not an administrator, the package prefixes under
`/opt` and `/usr/local` are read-only to it, which blocks most of what an
autonomous agent could otherwise install system-wide.

The workspace under the project user's root: <!-- runtime -->

- `repo/` — a persistent clone, fetched per task <!-- runtime -->
- `cache/` — build caches, deliberately warm across tasks <!-- runtime -->
- `work/{seq}/` — wiped at task **start**, not at task end <!-- runtime -->
- `out/{seq}/` — artifacts; the evaluation task reads this <!-- runtime -->

Wiping at the start rather than the end means a crashed task leaves its evidence
in place instead of taking it to the grave. Keying both per-task directories by
chuggy's decision sequence number makes the filesystem the idempotency record
(D7): a complete `out/{seq}` means the effect already happened, which is the
property the fabric does not supply on its own. <!-- runtime -->

On top of the workspace, per task: a freshly created simulator, destroyed
afterwards (D5), and an environment with no inherited shell profile.

**What this does not give you**, stated plainly: an agent can still pollute its
own project user's home between tasks, and cross-project isolation is exactly as
strong as the user boundary and no stronger. The guarantee is *reset to a known
state*, not *provably identical starting state*. For a fleet running the
operator's own agents against the operator's own repositories that is the right
trade, but it is a trade and not an equivalence.

A mobile build tool that manages its own signing material is not a substitute for
any of this. Such tools are task runners with opt-in hygiene actions, and their
implicit assumptions hold when the only thing running is the tool. Here the thing
running is an agent with a shell.

### 6. The worked example: a feature, implemented and QA'd

The workload that stresses this hardest is not a build. An agent implements a
mobile feature, builds the app, boots the simulator and drives it to check its
own work; then a **second** agent independently boots the app and does QA. Two
macOS tasks per ticket, sequential, each ten to sixty minutes, each holding a
simulator and carrying credentials.

The work task fetches the branch into `work/{seq}`, lets the agent implement and
build with the cache warm, creates a simulator, drives it, and copies the built
application into `out/{seq}`. The evaluation task creates its *own* simulator,
installs that artifact without rebuilding it (D6), and reports a verdict. <!-- runtime -->

The second stage is what makes D4 and D5 load-bearing rather than hygienic. An
evaluator that inherits the work task's machine state — a dependency installed by
hand, a simulator already past onboarding, a stale build product — can be fooled
by the residue of the thing it is judging. Independence is a correctness
property, not tidiness.

**Concurrency is bounded by memory, not by policy.** Xcode plus a booted
simulator plus an agent is approximately one whole 8 GiB Mac. Realistic capacity
is about one macOS task per Air and per Mini, perhaps two on the 16 GiB machine,
and a single ticket consumes two macOS slots in sequence. This coincides with a
second macOS limit — auto-login supports one graphical session at a time, so at
most one project user per Mac can hold a simulator-capable session. Two different
ceilings landing in the same place is convenient rather than accidental: both
follow from a Mac being a single-user workstation.

### 7. What chuggy asks of the fabric

chuggy is a single-writer actor over a durable journal, which appends its
decision **before** emitting any effect. What it needs from a fabric is small:

- Dispatch of a parameterized batch job carrying the decision sequence, the
  ticket and the branch — the work and evaluation spawns.
- Placement expressed as a constraint over node attributes: the kernel, the
  architecture, and node metadata such as an installed toolchain version.
- A secret store that renders into a task without a separate secrets service.
- Log collection, so the transcript is retrievable without the workload
  arranging it.
- A durable volume for the journal, and at-most-one execution of the actor.

Against the four fabric properties chuggy's model trusts rather than models:
restart and reschedule policy covers container relaunch; the scheduler covers
quota and placement; blocking queries cover watch delivery. A maximum task
runtime has no direct equivalent and must be synthesized — a wrapped timeout is
sufficient, but it should be a deliberate line of the adapter rather than an
assumption inherited from a fabric that supplied it.

Nomad is the only production backend. The second implementation is an in-process
fake for tests and trace replay, and it should be **adversarial** rather than
inert — replaying a decision twice, regressing the applied cursor, crashing
between the journal append and the effect. chuggy's model accepts at-least-once
delivery and drops the assumption that a task runs at most once, so surviving
duplicates is load-bearing, and no real fabric produces those hazards on demand.

### 8. What is retired

The platform this repository implements is archived, not migrated. Its source is
already public through the mirror; what needs deliberate archiving is the
operational data that lives nowhere else — the job records, the bare repositories
behind the ssh front, and a final encrypted snapshot through
`deploy/prod/backup-r2.sh`.

That data is a different disclosure class from source code: job records carry
prompts, diffs, evaluator output and, in logs, potentially credentials. It goes
to private storage. The public mirror is not an archive.

One consequence needs planning for rather than discovering. **This repository's
CI is its own evaluation gates** — there is no workflow file, and
`.chug/tasks/ci.sh` runs because a job runs it. Tearing the platform down removes
the gate from the thing that replaces it, during exactly the window when chuggy
is being bootstrapped by hand. The mobile CI runner already in the fleet is the
answer: chuggy's repository points at it, and the shell suites, comment lint, doc
checks and compiler gate run as ordinary CI until chuggy can gate itself.

### 9. Open questions

1. **Does a simulator-driving task run under a session-less user?** A Nomad
   client installed as a system daemon has no graphical session, and a task
   escalated to a project user has none either. This is S1 and it is first,
   because it decides how every Mac client is installed and whether D4's user
   boundary survives contact with the simulator.
2. **User-switching under `raw_exec`** requires the client to run as root, which
   interacts directly with question 1.
3. **Exactly-once dispatch.** D7 proposes the filesystem as the record; it needs
   to be checked against a crash between the dispatch and the directory's first
   write.
4. **Licence.** Nomad is not OSI open source and has no established fork. Fine
   for internal use, but it deserves an explicit decision rather than a default.
5. **A single server** on the one Linux node, which is also the journal's home.
   Correct at this scale, and worth accepting out loud.

### 10. The rejected alternatives, and what would revive them

**Kubernetes** (§2) revives if macOS stops mattering — if mobile work leaves the
fleet, the Linux half is all that remains and the ecosystem argument becomes the
only argument. It also revives if a virtual-kubelet provider grows secret
projection and log collection, since three of the four objections are
implementation gaps rather than consequences of virtualization.

**A VM per task** (§3) revives on better hardware. The objection is 8 GiB and
63 G, not the principle; a fleet of 32 GiB Macs with room for the images would
make the clean room affordable, and it is a strictly stronger guarantee than D4.

**A macOS-only VM orchestrator** was considered and set aside because it answers
only the macOS half, leaving the Linux half to a second system. It becomes
attractive if the two halves are ever operated separately anyway.

**Keeping mobile work on the existing CI runner indefinitely** is the
lowest-risk option and is not foreclosed by anything above. S7 is last for that
reason: the runner keeps working throughout, and porting it is a convenience,
not a prerequisite.

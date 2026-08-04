# Design #372 — `chug-node`: NixOS and nix-darwin modules for worker-host preparation

Status: PROPOSED — supersedes design job #265.

Written against the tree at `5aeb439`, this branch's base. The first draft was written at `fef87f9` and has
been revised twice after review; the one commit between those two trees,
`5aeb439` (`job/371: design`), amended exactly one file —
[`docs/design/367-android-emulator-execution.md`](./367-android-emulator-execution.md),
by 598 lines — and that amendment inverted the first draft's C1, which is what
C1 is now about. Every claim about current behavior below
was read out of `spec.md` and the source in this repo; where the brief and the
tree disagree, the tree wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree). Claims about *Nix* — what
`virtualisation.docker` generates, whether nix-darwin evaluates `assertions`,
what `nix.gc` looks like on each platform — were read out of upstream sources
fetched while writing (nixpkgs `nixos-25.05`, nix-darwin `master`) and are cited
with what was read, not with what is remembered. This design carries forward
job #265's decision and its "must not declare the container" conclusion; it does
not reopen either.

## What could not be verified, and what that means

Three facts this design leans on are unreachable from a work container, and the
document is written to be sound without them.

- **Job #265's own record.** A work container's NATS credential grants a KV read
  of exactly its own job key — `work_container_permissions` in
  [`crates/auth/src/nats.rs`](../../crates/auth/src/nats.rs) calls `kv_read` on
  the `JOBS` bucket with `job_key(owner, project, seq)` for *this* seq only. The
  platform API is not routable from here either. So #265 is known to this
  document only through the brief, which quotes its decision, its triggers and
  its four reasons. Everything quoted was **re-derived from the tree** below
  rather than taken on trust.
- **The host repo.** The ssh front refuses it: `git ls-remote` for another
  project under the same owner returns `access denied: job:kasofsk/chuggernaut:372
  may not read <other>`. So the ~90-line comment block the brief calls "the
  specification of what the module must encode" was not read. Its *content* is
  reconstructed instead from this repo's own artifacts — the two label keys, the
  moby filter bug and the label-inheritance rule are all recoverable from
  [`deploy/managed-label.test.sh`](../../deploy/managed-label.test.sh),
  [`crates/container/src/docker.rs`](../../crates/container/src/docker.rs) and
  the upstream sources — which is a better basis for a module anyway: a module
  built from a comment is a module that inherits the comment's errors.
- **Whether the GitHub mirror repo is publicly readable.** §2.2 recommends
  `github:` as the transport, which is only free of credentials if the mirror is
  public. `deploy/prod/chug-mirror-install.sh` takes an arbitrary `--mirror-url`,
  says it "Stores NO secrets", and its guidance installs a **write** deploy key
  out of band; nothing in it or in `deploy/prod/README.md` §3 records the repo's
  visibility, and GitHub is not reachable for a probe that would settle it. That
  the mirror *exists for this project* is recorded — README §3 quotes the live
  `com.chuggernaut.mirror` push for `kasofsk/chuggernaut.git` — but visibility is
  an assumption, and §2.2 now carries it at the point of decision along with the
  fallback if it turns out false.

Anything below that depends on a fact from any of the three is marked as such.
Two things the first draft got wrong are *not* in this list, because both were
recoverable all along: the macOS docker boundary, recoverable from this tree and
now stated in §1.1, and C1's reading of #367, recoverable from the tree this
branch is now based on. Neither was an unreachable fact; both were reachable ones
read at the wrong moment.

## Corrections (verified against the tree)

**C1 — the brief was right about `/nix/store`; this document's first draft was
not.** This correction is to the draft, not to the brief. The brief says "#367
wants `/nix/store` mounted into job containers, [which] makes nix GC a worker
concern." Read at `fef87f9`,
[#367](./367-android-emulator-execution.md) §3.3 recommended a *curated*
node-local bind — `WORKER_ANDROID_SDK_DIR` at `/opt/android-sdk` — and the draft
recorded the brief as wrong on that basis. `5aeb439` then amended #367, and the
amendment says the opposite:

- §3.2 marks the original T2 **dead**: the SDK on the target node is
  nix-provisioned, "so it is not a directory — it is a *view into a store*, and
  binding the view without the store yields dangling symlinks and a wrapper
  whose interpreter is missing."
- The surviving recommendation, taken by §3.3, is "**`/nix/store` bind-mounted
  read-only at `/nix/store`, plus a resolved `ANDROID_SDK_ROOT`**".
- §3.4 is titled *What the `/nix/store` mount exposes*; §8 calls it "the design's
  widest grant"; and §7's implementation row has `build_host_config` in
  [`crates/container/src/docker.rs`](../../crates/container/src/docker.rs)
  populating "a read-only `/nix/store` mount whose missing source is **refused,
  not created empty**".
- §8 then names the hazard in container mode, in as many words: "a
  `nix-collect-garbage` that removes the SDK closure mid-run breaks a live
  emulator."

So the exposure is unconditional for every launch #367's allow-list admits,
rather than contingent on how one operator happens to fill one directory.
Nothing is shipped: `build_host_config` today emits exactly one bind, the cache
(`binds: cache_dir.map(…)`, and its doc comment says `None` "yields `binds:
None`"). But the premise §5's A5 has to argue against is now the store mount, and
§7's boundary case is the store mount plus #367 §3.5's stable path — not
`WORKER_ANDROID_SDK_DIR`. Both are rewritten to it below.

The draft's failure mode is worth a sentence, because it is one a design job will
repeat. The citation was verified, and it was correct when read; a rebase moved
the cited document underneath it. A claim about a sibling *design* is not the
same kind of fact as a claim about code — code in this tree changes through this
job's own merge, where a conflict is visible, whereas a design doc changes in
another job's merge that touches nothing this one can see. §11 carries that as a
standing risk rather than a one-off.

**C2 — nix-darwin has `assertions`, and enforces them the same way.** The brief
asks to check rather than assume, so: nix-darwin's modules/system/default.nix
defines `assertions` and `warnings` as options and binds
`system.build.toplevel = throwAssertions (showWarnings (stdenvNoCC.mkDerivation …))`,
with `throwAssertions` a `throw` over the failed messages — the same shape NixOS
uses. Its own modules/services/nix-gc module uses it in anger (`nix.gc.automatic
requires nix.enable`). So **`darwin-rebuild build` fails on a failed assertion
exactly as `nixos-rebuild build` does**, and #265's "an assertion beats a
setting … it makes the boundary compile-time" argument transfers to macOS intact.

What does *not* transfer is narrower than the brief expects, and the difference
matters for §3:

| Mechanism | NixOS | nix-darwin |
| --- | --- | --- |
| `assertions` / `warnings` | yes | **yes** (C2) |
| `nix.gc.automatic` / `.options` | yes | **yes**, `modules/services/nix-gc` (`interval`, not `dates`) |
| A docker module | `virtualisation.docker` | **none** — no `virtualisation`, no docker; docker comes from colima or Docker Desktop, outside the closure |
| Declarative directory creation | `systemd.tmpfiles` | **none** — `system.activationScripts.postActivation.text` is the analogue |
| Unit supervision | `systemd.services` | `launchd.daemons` |
| cgroup-shaped limits | yes | none (#308 H.3 cost 3) |

The honest summary is not "macOS can enforce less because it has no assertions".
It is: **the assertion mechanism is identical; the surface it can assert *about*
is much smaller, because the hazard the module exists to prevent lives in a
NixOS module that has no macOS counterpart.**

**C3 — the cache directory is not owned by anything today, and `spec.md` §3.1
overstates it.** §3.1 says the host cache dir "is created and owned by the worker
daemon at startup". In the shipped deployment it is not. `WORKER_CACHE_DIR` is
passed to the daemon **as env only** — [`deploy/prod/build-worker.sh`](../../deploy/prod/build-worker.sh)
(`CACHE_ENV="-e WORKER_CACHE_DIR=$WORKER_CACHE_DIR"`) and
[`deploy/prod/worker-refresh.sh`](../../deploy/prod/worker-refresh.sh)
(`CACHE_ARGS`) both bind nothing into the daemon container, and both say so.
So the `std::fs::create_dir_all(dir)` in
[`crates/worker/src/daemon.rs`](../../crates/worker/src/daemon.rs) creates the
path **inside the daemon container's writable layer**, which is discarded at the
next swap. The host path used to be created by dockerd, as root, mode 0755, the
first time a job container bound `{dir}:/cache/sccache`; #379 made that a typed
mount in `build_host_config`
([`crates/container/src/docker.rs`](../../crates/container/src/docker.rs)), so
the engine now refuses such a launch and **nothing creates the path at all**.

This makes the brief's "cache dir via `systemd.tmpfiles`" *more* justified than
it states, and it is the cleanest example of the module's whole thesis: a
condition the worker depends on, that nothing on the host declares, that came
into being as a side effect of the first container launch and now does not come
into being at all. Note also that the cache's *contents* are genuinely safe to
lose (§3.1: "carries no job state … an empty or cold cache is always safe") but
its *directory* is not: a node whose `WORKER_CACHE_DIR` is missing fails every
launch. §5 declines to assert about it as a hygiene item, a judgement made
before #379, and a follow-up should revisit it on those terms.

**C4 — #265's reason 4 has drifted and is weaker than stated; reasons 1–3 hold
exactly.** Re-verified below in [§8](#8-why-the-module-must-not-declare-the-container).
Reason 4 said `virtualisation.oci-containers` "expects a registry-pullable image
or `imageFile`". Current nixpkgs also has `imageStream` and a `pull` option
(`enum ["always" "missing" "never" "newer"]`, default `"missing"`), so `pull =
"never"` would in principle accept a node-locally-built tag. The conclusion is
unchanged — reasons 1–3 are each independently fatal, and `pull = "never"` makes
the unit fail hard whenever the tag is absent, which is precisely the state a
careless prune produces — but a design that repeats a stale reason is a design a
future reader over-applies.

**C5 — the drain material is #308 §H.6, not §H.2.** The brief cites "#308 H.2"
twice, for the three clocks and for the drain hook. H.2 is "What it buys" and
contains neither; both live in **H.6, "NixOS layering: where tooling lives"** —
the clock table, and "Drain already has a hook. `schedulable` in
`crates/worker/src/backend.rs` …". This document cites H.6 throughout (§6, §7).
Recorded so the next reader chasing the brief's pointer does not conclude the
citation was invented.

**C6 — there is no `e2e!` guard in `crates/test-utils`; §2's N1 cites a macro
that does not exist.** Added by job #405, which re-verified the reference and
recorded it here rather than editing §2's body. N1 proposes a third `ci.sh`
stage "mirroring the `e2e!` guard's shape (`crates/test-utils`)". That crate
exports two skip macros — `require_nats!` and `require_nats_config!` in
[`crates/test-utils/src/nats.rs`](../../crates/test-utils/src/nats.rs) — plus the
`backend_suite::docker_available()` predicate, and nothing named `e2e`;
`git log -S 'macro_rules! e2e' --all -- crates/` returns no commit, so the macro
was never defined in this tree. `testing.md` and `CLAUDE.md` both now say so
outright ("there is no `e2e!` macro"), and that is why this is a Correction
rather than a dated record: dating protects a claim that *was* true when written,
and this one never was. The *name* does still survive in one place —
`crates/dispatcher/tests/fleet_e2e.rs`'s module header cites an "`e2e!`/
`require_nats!`" guard pair — left untouched here because `crates/dispatcher` is
job #403's scope, and recorded as a follow-up for whoever next opens that file.
N1's argument survives the substitution intact — those two macros are
exactly the "skip loudly when the dependency is absent" shape N1 wants — so read
N1 with `require_nats!` in place of `e2e!`; its cost and its
inert-until-a-node-provisions-nix conclusion are unchanged.

---

## 1. The problem, restated from the tree

A worker node is a machine that must satisfy a set of conditions before the
platform can use it. Today those conditions are satisfied *implicitly*, by
whatever the operator did when the node was joined, and **nothing on the node
records them**:

1. **A docker daemon reachable at `/var/run/docker.sock`**, running at boot,
   because both the daemon container and every job container are started
   `--restart=always` (`build-worker.sh`; `RUN_NEW` in `worker-refresh.sh`).
2. **A login user who can drive docker over ssh.** `build-worker.sh` runs
   `ssh "$WORKER_SSH" "… docker build …"` — that user needs socket access — and
   binds `$HOME/chuggernaut-worker/keys` into the daemon container.
3. **A cache directory at `WORKER_CACHE_DIR`**, per C3 owned by nobody.
4. **Enough free disk for a whole image generation.** `worker-refresh.sh`
   refuses below `WORKER_REFRESH_DISK_FREE_GB_MIN` (default 30 GB), with a
   derivation history spanning four incidents.
5. **Images that survive between jobs.** The agent images back no running
   container by design, so any unfiltered image sweep deletes them. This is the
   2026-07-25 failure the brief names, and the repo already carries half its
   fix: every image carries `LABEL chug.managed="true"`, locked by
   [`deploy/managed-label.test.sh`](../../deploy/managed-label.test.sh), whose
   header says outright that "the host-side fix filters on this label; **it is
   inert unless every image carries it**". The other half of that fix lives in a
   repo this project does not own — which is the gap.

Condition 5 is the load-bearing one, and it has a second edge the label test
also locks: the *container*-ownership marker is a **different key**,
`MANAGED_LABEL = "chuggernaut.managed"` in
[`crates/container/src/docker.rs`](../../crates/container/src/docker.rs), and no
image may carry it, because containers inherit their image's labels and the
dispatcher's §3.6 startup sweep reaps that marker (#266 → #268 killed the whole
worker fleet exactly this way). Any host-side rule that names a label must name
`chug.managed` and must never name `chuggernaut.managed`. A host repo cannot be
expected to hold that distinction in a comment; a module can hold it in code.

### 1.1 The macOS boundary: dockerd does not run on the mac

This is the platform fact the darwin half turns on, and the first draft of this
document was written without it. It is recoverable from this repo, so it is not
in the preamble's unverifiable list — it is a correction to the design, made
here rather than in the Corrections section because everything downstream
depends on it.

macOS has no Linux kernel, so **dockerd runs inside a VM**. On this fleet that
VM is colima: [`deploy/prod/boot.sh`](../../deploy/prod/boot.sh) starts it
(`colima status … || colima start`),
[`deploy/prod/chug-install.sh`](../../deploy/prod/chug-install.sh) names it as
the macOS source of the `docker` dependency, and
[`deploy/prod/README.md`](../../deploy/prod/README.md) §8 records the socket
path a mac must be pointed at
(`unix:///Users/<you>/.colima/default/docker.sock`).

The consequence that matters: **a bind mount's source path is resolved by the
daemon, which means inside the VM, not on the mac.** A host path the VM does not
share does not produce an error — dockerd creates an empty directory at that path
*inside the VM* and binds that. Silent, empty, wrong. That is precisely the C3
defect this module exists to remove, reproduced by a module that dutifully
creates the directory on the wrong side of a boundary it did not know about.

What crosses, read from colima's own shipped defaults
(`embedded/defaults/colima.yaml`, upstream `main`): `mounts` defaults to `[]`,
and the file states what that default means — *"Colima mounts user's home
directory by default to provide a familiar user experience"*, *"Colima default
behaviour: `$HOME` is mounted as writable."* Anything outside `$HOME` crosses
only if the operator adds it to `mounts` (a `location` + `writable` pair, applied
at `colima start`).

The repo already depends on this and is the proof: `build-worker.sh` binds
`$HOME/chuggernaut-worker/keys:/data/keys:ro` and the air's worker has its NATS
credentials, so `$HOME` demonstrably crosses. Nothing in the tree binds any path
outside `$HOME` on a mac.

Three decisions follow, each drawn where it belongs rather than here: `cacheDir`
must be asserted to be VM-visible (§5 A7), the GC-root question is inert on
darwin until the store is shared into the VM or host mode lands (§5 A5), and boot
persistence — condition 1 above — has no NixOS-style mechanism to lean on (§5
A8). Docker Desktop and OrbStack have the same VM shape with *different* sharing
defaults, which is why A7 checks against a declared set of shared paths rather
than hard-coding `$HOME`.

---

## 2. Where the module lives, and how a host gets it

Job #265's decision — a flake output this repo exports, imported by host repos, no
shared repo and no shared ownership — is carried. What #265 left implicit is
that **this repo has no flake at all**: `find . -name "*.nix"` returns nothing.
So "export a flake output" is really "add a flake", and the shape of that flake
is a decision in its own right.

### 2.1 Options for the flake

**F1 — a flake whose only outputs are the two modules, with no inputs
(recommended).** A NixOS/nix-darwin module is a function of the arguments the
evaluating host passes it (`{ config, lib, pkgs, ... }`); it needs no `nixpkgs`
input of its own. A flake with zero inputs has an empty lock file that never
churns, cannot pin a second nixpkgs into a host's evaluation, and cannot go
stale.

```nix
{
  description = "Chuggernaut worker-node host preparation";
  outputs = { self }: {
    nixosModules.chug-node  = import ./nix/chug-node/nixos.nix;
    darwinModules.chug-node = import ./nix/chug-node/darwin.nix;
  };
}
```

**F2 — a flake with a `nixpkgs` input**, so the module can reference `pkgs`
from a known version and export checks. Rejected: it pins a second nixpkgs into
every consumer's closure for no gain, and the module deliberately builds nothing
(§4 — it sets options and creates directories; the only package it could want,
`docker`, must be the *host's*).

**F3 — no flake; host repos `fetchGit` the module path directly.** Works, and is
one fewer file here, but gives up the thing that makes this tractable: a flake
input is a pinned, lockfile-recorded, `nix flake update`-able reference, and
without it every host repo hand-rolls its own fetch and its own pinning.

### 2.2 How a host reaches this repo

Both candidate transports must answer the same question — *what credential does
root's `nix` daemon need, and where does it live?* — and the first draft asked it
of only one of them.

**The tailnet ssh front.** `nix` evaluates as root with its own ssh
configuration, so a flake input pointed at `ssh://git@100.116.243.42:2222/…`
(the sole remote in this checkout) needs a key readable by root **in the host
repo's system closure**, and the front is reachable only on the tailnet, so no
machine off it can evaluate the host's flake at all. The credential is smaller
than the first draft implied — `issue_node_credential` in
[`crates/auth/src/ssh.rs`](../../crates/auth/src/ssh.rs) already mints exactly
this class: a **read-only, repo-scoped, long-lived** node certificate
("Validity is long (renew by re-running) — a node credential is
operator-installed, not per-job ephemeral"), and it is how `WORKER_GIT_KEY`
reaches a node today. What is *not* small is where it has to sit: root's ssh
config in a repo this project does not own. It is a workable fallback, not the
default.

**The GitHub mirror.** The mirror is live for this project — README §3 quotes the
running `com.chuggernaut.mirror` push for `kasofsk/chuggernaut.git`, and
[`deploy/prod/chug-mirror-install.sh`](../../deploy/prod/chug-mirror-install.sh)
generalizes it to a per-project launchd agent running `git push mirror main:main
--force-with-lease` every `INTERVAL` seconds (default **300**). It needs no
credential **if the mirror repo is publicly readable**, and that is the
recommendation's load-bearing assumption:

> **§2.2 assumes the mirror repo exists at a public URL for this project.** The
> existence is recorded; the *visibility* is not, anywhere in the tree, and is
> unverifiable from a work container (preamble). The install script's only
> credential is a **write** deploy key installed out of band, which tells us
> nothing about read access.

If it turns out private, this section does not need re-deciding from scratch —
the answer is the ssh front above with an `issue_node_credential` key installed
for root, accepting the tailnet-only constraint. A GitHub read deploy key plus
`nix.settings.access-tokens` is the third option and the worst of the three: it
puts a GitHub credential in the closure *and* keeps the five-minute lag. Whoever
hits `error: unable to download` should find that answer here rather than
re-litigate it.

So, recommended:

```nix
inputs.chuggernaut.url = "github:kasofsk/chuggernaut";   # host repo's flake
```

with two consequences to state plainly rather than discover:

- **The mirror lags `main` by up to five minutes.** A host cannot pin a rev the
  platform merged seconds ago. For a module that changes roughly never, this is
  a non-issue; it is recorded so nobody debugs it twice.
- **Only `main` is mirrored** (`main:main`), so a module change is consumable by
  a host only after it merges here. There is no way to test a host against a job
  branch except by pointing the input at a local path — which is the normal
  `nix` workflow (`--override-input`), and is worth one line in the runbook.

### 2.3 The CI problem this creates, and it is real

`.chug/tasks/ci.sh` is diff-aware with two stages: a `web/` diff runs the npm
build, a Rust-path diff runs the cargo gate, and everything else gates in
seconds on the four pure-shell checks. A `nix/`-only diff would run **none** of
them — the modules would land ungated. Worse, `grep -in nix deploy/*/Dockerfile.*`
finds nothing: **no agent image has `nix`**, so no evaluator could run
`nix flake check` even if the gate asked for it, and per CLAUDE.md agent
reviewers launch read-only and cannot run anything at all.

Three honest options, none free:

- **N1 — a third `ci.sh` stage that runs `nix flake check` when `nix` is on
  `PATH` and skips loudly otherwise**, mirroring the `e2e!` guard's shape
  (`crates/test-utils`). Correct in the limit, inert today: no node has nix in
  its agent images, so it would always skip. It is only real once a node
  provisions nix — which is #308/#309 territory, not this job's.
- **N2 — add `nix` to the agent image.** A multi-hundred-MB addition to an image
  rebuilt on every node on every deploy, for a directory that changes annually.
  #367 §3.1's second ground applies directly. Rejected.
- **N3 — no automated gate; the host repo's own `nixos-rebuild build` is the
  gate.** This is honest about where the module is actually exercised: a broken
  module fails the *consumer's* build, loudly, at the moment they adopt it.

**Recommendation: N3 now, N1 written into `ci.sh` as a skipping stage in the
same job that adds the modules** — so the day a node has nix, the gate is
already there and turns on by itself rather than being remembered. Flag it in
the module's own doc header that its only enforcement today is downstream.

---

## 3. One module or two?

The two platforms need the same three options and radically different
implementations of them. The choice is where the seam goes.

**M1 — two sibling modules, each self-contained.** ~30 lines of duplicated
option declarations. Simplest to read; no shared file to reason about.

**M2 — a shared `options.nix`, imported by `nixos.nix` and `darwin.nix`, each
supplying only `config` (recommended).**

The deciding argument is not DRY — at three options DRY barely pays, and
STYLE.md's simplicity principle would otherwise favour M1. It is that **the
host-facing surface must be identical on both boxes even where the enforcement
is not.** A host repo should write the same three lines on `gumbo-nuc-0` and
`gumbo-air-0` and get the same *meaning*; the fact that only one of them can
compile-time-refuse a bad prune setting is the platform's problem, not the
operator's. Under M1 the darwin author's cheapest move is to omit the options
they cannot fully implement, and then the two host repos diverge — which is the
drift this whole design exists to stop, reappearing one level down.

Consequence worth stating: with M2 a *shared* option whose darwin
implementation is weaker must say so **in the option description**, and the
darwin module should emit a `warnings` entry when a host sets an option whose
guarantee it cannot give. An option that silently means less on one platform is
the "silent lying" failure mode #308 H.3 cost 3 rejects.

**Does M2 still pay once §1.1 is admitted?** The fair objection is that if the
darwin module implements almost nothing, an identical surface with an empty
implementation *is* the silent lie the paragraph above rejects, and M1 plus a
short darwin module would be more honest. That objection was true of this
document's first draft, which had darwin contributing one directory and one
warning — because it was designed against a host model where dockerd runs on the
mac. With the VM boundary stated, the accounting reverses: the darwin module
carries **two assertions the NixOS module does not need at all** (A7's
VM-visibility check and A8's boot acknowledgement), because the hazards they
guard are properties of the mac that NixOS does not have. The platforms are not
"one real module and one stub"; they are two different hazard sets behind one
surface. M2 pays.

The rule that keeps it honest: **shared options live in `options.nix` and mean
the same thing on both platforms; a fact that exists on only one platform gets a
platform-namespaced option (`chug.node.darwin.*`) declared by that platform's
module.** A host reading `chug.node.darwin.` in its own config knows exactly what
it is looking at. That is why §4 has three shared options and two darwin ones,
rather than five shared options two of which are inert on Linux.

A5's rework tightens this, and the tightening is worth recording precisely.
`gcRoots` was a shared option whose darwin implementation meant less than its
NixOS one; removing it leaves the "say so in the description" escape used
**exactly once** — for `user`, whose description records that only NixOS adds it
to the `docker` group (§4), because only NixOS has a docker group to add it to
(A3).

That single use is the rule working, not the rule bent. `enable` and `cacheDir`
carry the same guarantee on both platforms by construction; `user` carries a
platform asymmetry that is real, irreducible, and therefore stated. M2's honesty
condition is met by construction for two shared options and by prose for the one
where construction cannot reach — which is what M2 asked for. An implementer
reading this section should expect exactly one shared option to carry a platform
caveat, and should not add a second without the same justification.

Both modules are **host preparation, not services** — they declare conditions and
own no lifecycle (§7). That is also why the option namespace below is `chug.node.*`
rather than the brief's suggested `services.chug-node.*`: on both platforms
`services.*` is the namespace for things that run, and a `services.` prefix is a
standing invitation for the next contributor to add the unit §7 forbids. The
deviation is small, deliberate, and the only one this document makes from the
brief's suggested shape.

---

## 4. The option surface

Kept deliberately tiny — #265's whole argument is that the shared surface is
small and a large one is overhead that re-creates the central control plane
CLAUDE.md rejects. **Three shared options**, and two more that exist only on
darwin because the facts they name exist only there (§3's namespacing rule).

Five, and it went *down* in rework: `gcRoots` was a fourth shared option until
C1 forced A5 to be re-argued, and A5 now declines it. The growth over a bare
`enable`/`user` pair is the two darwin options, and both earn their place by
being *unassertable otherwise*; §4.1 and §5's rejected-candidates list are the
record of what else was kept out.

```nix
chug.node = {
  enable   = mkEnableOption "Chuggernaut worker-node host preparation";

  user     = mkOption {
    type = types.str;
    description = ''
      The login user deploys ssh in as and the worker daemon's keys live under.
      On NixOS this user is added to the `docker` group; on both platforms
      $HOME/chuggernaut-worker/keys is the bind source for /data/keys.
    '';
  };

  cacheDir = mkOption {
    type = types.nullOr types.path;
    default = null;
    description = ''
      The node-local build cache (`WORKER_CACHE_DIR`, spec 3.1). Created and
      owned by ${user} rather than by dockerd-at-first-bind. Null leaves
      caching off, which is always safe. On darwin this path must be visible
      inside the docker VM (1.1) — asserted, see 5 A7.
    '';
  };
};
```

Plus, declared by the darwin module only, the two facts §1.1 makes real:

```nix
chug.node.darwin = {
  vmSharedPaths   = mkOption {
    type = types.listOf types.path;
    default = [ config.users.users.${cfg.user}.home ];
    description = ''
      Host prefixes the docker VM shares, matching this mac's `colima start
      --mount` (or Docker Desktop file sharing). The default is the home
      directory, which colima shares writable out of the box. Bind sources
      outside these prefixes resolve inside the VM, not on the mac.
    '';
  };

  dockerBootAgent = mkOption {
    type = types.nullOr types.str;
    default = null;
    description = ''
      Name of the `launchd` entry in this configuration that starts the
      container runtime at boot, or the literal "external" to record that
      boot persistence is handled outside this closure. Null warns (5 A8).
    '';
  };
};
```

That is the whole surface. What a host repo writes — and note that the two
`cacheDir` values are **not** the same path, which is the point of §1.1:

```nix
# gumbo-nuc-0 — NixOS
imports = [ chuggernaut.nixosModules.chug-node ];
chug.node = {
  enable   = true;
  user     = "worksalot";
  cacheDir = "/var/cache/chuggernaut/sccache";
};
```

```nix
# gumbo-air-0 — nix-darwin
imports = [ chuggernaut.darwinModules.chug-node ];
chug.node = {
  enable   = true;
  user     = "worksalot";
  cacheDir = "/Users/worksalot/chuggernaut-worker/sccache";  # under $HOME: crosses
};
chug.node.darwin.dockerBootAgent = "colima";   # the launchd.user.agents entry below
```

A `cacheDir` of `/var/cache/chuggernaut/sccache` on the air would evaluate,
activate, create a directory on the mac, and cache nothing — dockerd would bind
an empty `/var/cache/chuggernaut/sccache` from inside the VM, discarded with the
VM. A7 refuses it at `darwin-rebuild build`.

### 4.1 Two options the brief suggests, and why they are rejected

- **`nodeName`.** Would set `WORKER_NODE` — but the module does not declare the
  container, so there is nowhere to put it. An option that sets nothing is a
  lie, and worse: it would read as the node's identity while the *actual*
  identity keeps arriving from `build-worker.sh`'s `-e WORKER_NODE=$NODE`,
  giving two sources of truth for the one thing the whole fleet is keyed on.
- **`slots`.** Capacity is **runtime state with an owner**, and that owner is
  [#293](./293-worker-capacity.md): `platform_fleet_capacity_set` in
  [`crates/api/src/routes.rs`](../../crates/api/src/routes.rs) forwards a
  `set_slots` command, the dispatcher persists intent and re-pushes it after a
  daemon restart, and `WORKER_SLOTS` survives only as the first-boot value
  (`crates/worker/src/config.rs`, whose doc comment says so explicitly). Putting
  a number in the system closure would make `nixos-rebuild switch` a **fifth**
  capacity mechanism — the exact proliferation #293 exists to retire — and it
  would silently fight the dispatcher's reconciliation on every rebuild.

Both rejections share one rule, and it is the rule that keeps the surface small:
**the module may declare machine facts; it may not declare platform state.**

---

## 5. The assertion set

An assertion is right where a host owner would plausibly set something
*reasonable for their own workload* that silently breaks a worker. Two
properties make it strictly better than contributing a setting, and both were
checked rather than assumed:

- An assertion reads the **final merged** value of `config`, so `mkForce` in the
  host's config cannot defeat it — it can only turn a silent breakage into a
  build failure.
- It fails at `nixos-rebuild build` / `darwin-rebuild build`, before activation,
  so the node is never in the broken state at all.

The pattern used throughout is therefore **contribute *and* assert**: the module
supplies the correct value with `mkDefault` (so a stock host needs to know
nothing), and asserts the merged result (so a host that overrides gets a
compile-time refusal naming the reason).

### A1 — the prune constraint (NixOS only)

`virtualisation.docker.autoPrune` generates a `docker-prune` oneshot whose
`ExecStart` is literally `docker system prune -f` ++ `autoPrune.flags`, on a
`startAt = autoPrune.dates` timer (nixpkgs `nixos-25.05`,
nixos/modules/virtualisation/docker.nix).
With `flags = [ "--all" ]` — a correct setting for a GHA runner, whose images
*are* backed by running containers — that sweep removes every image not backing
a running container, which is every agent image, every night.

The module contributes:

```nix
virtualisation.docker.autoPrune.flags = [ "--filter=label!=chug.managed" ];
```

(list options merge, so this composes with a host's own flags) and asserts that
the merged flag list contains it whenever `autoPrune.enable` is true.

**A1b — at most one `label!=` filter, and this is the assertion a settings-only
approach cannot express at all.** [moby#40286](https://github.com/moby/moby/issues/40286)
— "Multiple `label!` filters for prune are ORed unlike all other filter combos
which are ANDed" — is **open**, filed 2019-12-05, unresolved at the time of
writing. So a host that adds its own exclusion (`--filter=label!=gha-runner`,
entirely reasonable) turns the pair into "prune anything lacking *either*
label", which spares nothing and reads, in the host's config, like belt and
braces. The module must assert that **exactly one** `label!=` filter is present
and that it is `chug.managed`, with the message naming the issue. A host wanting
a second exclusion must express it another way (a `label` allow-filter, or its
own timer with its own filter), and the assertion says so.

**Honest limits of A1**, none of which change the recommendation:

- It protects images and containers carrying `chug.managed`. Because containers
  inherit their image's labels and all three images carry it, both `chug-worker`
  and every job container are covered.
- It does **not** protect the retained `chug-worker-swap` container or the
  `docker:cli` image behind it. `worker-refresh.sh` deliberately keeps that
  swapper (`SWAP_NAME`, not `--rm`) because it is "the only record of the moment
  the node is most likely to break", and spec §3.1 records the same intent — but
  it carries no label, so a filtered `--all` prune still deletes the transcript
  and the image. The fix is one flag in `worker-refresh.sh`'s
  `docker run -d --name "$SWAP_NAME"` — `--label chug.managed=true`, **never**
  `chuggernaut.managed`, which would make the dispatcher's §3.6 sweep reap it
  (#268). That is a `code` job against this repo, not a module option, and it is
  the clearest evidence that the label contract needs an owner on *both* sides.
- Prune filters do not apply to BuildKit cache. Harmless: `worker-refresh.sh`
  caps it itself (`BUILDER_KEEP_STORAGE`).
- Nothing here binds an operator typing `docker system prune -a` by hand.
  Assertions bound configuration, not people.

### A2 — docker enabled, and started at boot (NixOS)

Contribute `virtualisation.docker.enable = mkDefault true` and `enableOnBoot =
mkDefault true`; assert both.

`enableOnBoot` is the interesting half and it is exactly the class the brief
asked to hunt for. The generated unit is `wantedBy = optional cfg.enableOnBoot
"multi-user.target"`, and nixpkgs' own option description says: "This is
required for containers which are created with the `--restart=always` flag to
work." Every container this platform starts uses that flag. A host owner turning
`enableOnBoot` off to save idle resources on a mostly-idle box would produce a
node that looks fine until its next reboot and then never comes back — with no
error anywhere, because socket activation makes docker *available*, just not
*running*, and `--restart=always` needs the latter.

### A3 — the worker user can reach docker (NixOS)

Contribute `users.users.${cfg.user}.extraGroups = [ "docker" ]`; assert
membership in the merged config. `build-worker.sh` ssh's in as that user and
runs `docker build` and `docker inspect` directly; without the group every leg
of that path fails with a permission error that reads like a broken socket.

On darwin there is no docker group and no docker module: the module can only
check at activation whether `docker info` answers, and must do so as a **loud
warning, not a failure** — a colima VM that happens to be down during a
`darwin-rebuild switch` is an operational state, not a configuration error.
Under M2's rule this asymmetry is written into the option description. A7 extends
the same probe to the cache bind, and A8 covers what an activation-time probe
structurally cannot: a reboot.

### A4 — `live-restore`, so a rebuild does not kill in-flight tasks

This one was not in the brief and is the highest-value find. nixpkgs sets
`daemon.settings.live-restore` to `versionOlder config.system.stateVersion
"24.11"` — i.e. **false on any host installed at 24.11 or later**. Without it, a
dockerd restart kills every running container. The docker unit's `ExecStart`
references `--config-file=${daemonSettingsFile}`, a store path derived from
`daemon.settings`, so **any change to the docker daemon's settings or package
restarts the unit during `nixos-rebuild switch`** — and takes every in-flight job
container with it.

That is a worse failure than the prune outage in one respect: it is
*intermittent*. Most rebuilds touch nothing docker-shaped and are harmless, so
the hazard trains the operator that rebuilds are safe, and then one isn't.

Contribute `virtualisation.docker.daemon.settings.live-restore = mkDefault true`
and assert it. The cost is stated in nixpkgs' own description and is worth
naming in the assertion message: **live-restore is incompatible with docker
swarm**, so a host that wants swarm cannot be a chug node. That trade is right —
this fleet runs no swarm — and an assertion is how a host that wants swarm finds
out at build time instead of at 3 a.m.

One irreducible wrinkle: toggling `live-restore` itself changes
`daemon.settings`, so the *first* rebuild that adopts this module restarts
dockerd and kills whatever is running. That first rebuild must be drained (§6).

### A5 — GC roots: no assertion, and — after C1 — no option either

The brief proposes asserting against an aggressive `nix.gc`, which exists on both
platforms (NixOS `nix.gc.{automatic,dates,options}`, nix-darwin
`nix.gc.{automatic,interval,options}`). The first draft rejected the assertion
and offered a *construction* in its place: `chug.node.gcRoots`, a list of store
paths the module would pin under `/nix/var/nix/gcroots/chug-node/`. **C1 takes
the ground out from under both halves**, and re-arguing them against the
amended #367 kills the option and strengthens the rejection.

**The assertion stays rejected, and now for a structural reason rather than a
contingent one.**

1. `nix-collect-garbage` never deletes a path reachable from a GC root. The
   hazard is not the GC policy; it is an **unrooted dependency**. Asserting
   against `--delete-older-than` would forbid a correct, wanted setting and
   *still* leave an unrooted path exposed to a plain `nix-collect-garbage`.
2. **The module cannot see what the worker will bind.** Per §8 it does not
   declare the container, so it sets no `WORKER_*` and has no view of the run
   spec; the store paths a running task depends on are composed at launch, on the
   far side of a boundary this module deliberately does not cross. An assertion
   needs a subject and this one has none — and that is a property of the module's
   *shape*, so it does not expire. The draft's reason 2 was the contingent
   version of this ("nothing to assert about yet, because nothing mounts a store
   path"), and C1 is what happens to contingent reasons.
3. An assertion here would be a rule about the *host's* housekeeping; a root is
   a statement about *this platform's* dependency. The second is the module's
   business, the first is not.

**And the construction does not survive the re-check.** Ask what
`chug.node.gcRoots` would still protect once #367's own mitigations are in
place, case by case:

- **The current SDK closure — already rooted.** #367 §3.5 **S3** requires the
  host's `configuration.nix` to maintain a stable path (`/etc/chug/android-sdk`)
  pointing at the current `androidsdk` output, resolved at use. An
  `environment.etc` entry is part of `system.build.toplevel`, so that closure is
  rooted by the **system profile** — which is precisely the mitigation #367 §8
  names: "a closure referenced by the NixOS **system profile** is GC-rooted by
  that profile, whereas an ad-hoc `nix build` is not." A `gcRoots` entry naming
  the same path is a second root on an already-rooted path. **Redundant.**
- **A closure provisioned ad-hoc — the only case `gcRoots` uniquely covers, and
  it is one #367 forbids.** The same passage continues: "A0's provisioning must
  stay in `configuration.nix`, and 'provision it by hand for a quick test' is a
  footgun worth naming." An option whose sole distinct purpose is to make a
  practice the design rules out survivable is subsidizing the footgun, and it
  would make the unsupported path the *easier* one to take. **Rejected.**
- **The generation flip under a running task — the real residual, and `gcRoots`
  misses it too.** `nixos-rebuild switch` to a bumped SDK moves the system
  profile to the *new* closure, leaving the old one rooted only by the old
  generation — which the next `nix.gc` with `--delete-older-than` collects. A
  task that started before the switch is holding the old path: #367 §3.5(a) has
  the engine resolve the stable symlink host-side **at container create**, so the
  bind's source is whatever the SDK was when the container started. `gcRoots`
  cannot help, because it pins what the *current* configuration names, which is
  the new path. This is **the drain problem (§6) wearing a GC costume** — under
  §6's drain-before-rebuild discipline no task is running across the flip and the
  case cannot arise, and without that discipline the rebuild was already unsafe
  for A4's reasons before GC entered the picture.

So the module's contribution here is **nothing**, and that is the result rather
than a gap. The hazard is fully discharged by two rules that already have
owners — #367 §8's provision-from-the-system-closure rule, and §6's
drain-before-rebuild step, which the module's doc header carries (§6, item 2) —
and the module's job is to not mint a third mechanism for a hazard two rules
already cover. The option surface drops from six to five (§4).

What is given up, stated so a later reader can reverse it cheaply: `gcRoots` was
four lines of nix and harmless, and a consumer with a runtime store dependency
that genuinely *cannot* be expressed as an `environment.etc` entry would want it.
If that consumer appears this is a small addition, not a re-design. It is
declined now because #265's whole thesis is that the shared surface is tiny, and
an option carried for no live use case is how a tiny surface stops being tiny.

**On darwin the question is inert twice over.** Per §1.1 `/nix/store` is not in
colima's default mount set, so a mac store path cannot be the source of a
container bind at all; and #367's mechanism is KVM-gated and nuc-only, so it does
not reach the air on any path. Two futures reopen it — an operator adding
`/nix/store` to `colima --mount` read-only, or macOS **host** mode
([#322](./322-macos-native-runtime.md)) running tasks natively on the mac, where
store paths are directly visible and unrooted ones directly collectable. The
second also changes §6's drain answer (its closing paragraph), so the two should
be re-decided together rather than pre-empted by an option now. Note that host
mode is where [#309](./309-host-native-execution.md) §9's *per-task* GC root
belongs — task-scoped, not node-scoped — which is a further reason not to spend
the node-scoped option first.

### A6 — the cache directory: create it, assert nothing

Per C3 the directory is currently conjured by dockerd as root. The module
creates it owned by `chug.node.user` (NixOS `systemd.tmpfiles.rules`; darwin an
activation `install -d -o`). No assertion **about its lifetime**: spec §3.1
makes a missing or cold cache always safe, so a host that deletes it nightly is
*wasteful, not wrong*, and asserting against waste is how an assertion set stops
being read.

There is an assertion about its *location*, and only on darwin — A7.

### A7 — the cache directory must cross the VM boundary (darwin only)

Assert `chug.node.cacheDir` is under one of `chug.node.darwin.vmSharedPaths`.

This is the darwin module's counterpart to A1, and it is the same class of
defect: a setting a reasonable host owner would write, that produces no error
and no working result. `/var/cache/chuggernaut/sccache` is the obviously correct
answer on Linux and is what an operator copying the nuc's config would write on
the air. Per §1.1 it activates cleanly, creates a directory on the mac, and then
dockerd binds an *empty* directory of the same name from inside the VM — so
`sccache` reports 0% hits forever, on a path that visibly exists when the
operator goes looking. The failure is invisible from both sides at once, which is
the worst shape a failure can have.

Why an assertion rather than defaulting `cacheDir` to something under `$HOME`:
a default cannot catch the copied-from-the-nuc case, which is the case that
happens. Why `vmSharedPaths` rather than hard-coding `$HOME`: a mac that shares
`/opt/chug` via `colima start --mount /opt/chug:w` is a legal, working
configuration, there is no `mkForce` out of a failed assertion, and Docker
Desktop's file-sharing set differs again. The option is the host's declaration of
its own VM, and its default (the user's home) is what colima gives out of the
box.

Honest limit: the assertion checks the host's *declaration*, not the VM. An
operator who changes `colima start --mount` without updating `vmSharedPaths`
gets the old silence back. That is unavoidable — the VM's mount table is runtime
state on a machine nix does not model — and it is why A3's activation-time
`docker info` probe should also `docker run --rm -v ${cacheDir}:/probe` and warn
if the bind is not the directory it just created. A warning, not a failure:
activation with the VM down is an operational state (A3).

### A8 — docker at boot on darwin: acknowledged, not declared

Condition 1 of §1 — docker running *at boot* — is asserted on NixOS by A2 and has
no equivalent here, and the exposure is worse on the air than on the nuc:
nothing in the air's configuration declares that the colima VM starts at all, so
a reboot leaves `chug-worker` down, `--restart=always` irrelevant (there is no
daemon to honour it), and the node reading UNHEALTHY until someone looks. A3's
activation-time probe does not cover it: it fires when someone happens to
rebuild, which is exactly not when a machine reboots.

**Rejected: the module declares the launchd agent itself.** nix-darwin has the
mechanism — `launchd.agents`, `launchd.daemons` and `launchd.user.agents` are all
`attrsOf` submodules in its `modules/launchd/default.nix`, and this project's own
`com.chuggernaut.boot` agent (`deploy/prod/README.md` §2, running
`deploy/prod/boot.sh`: colima, then compose, then wait-for-NATS) is the same idea
already working. But that agent is on the **Mini**, a box this project owns end
to end and installs imperatively with `install-launchd.sh`. The air is a host
repo's box, and there the objections bind:

- The module does not install docker on darwin and cannot (C2: no
  `virtualisation`, no docker in the closure — it comes from homebrew). Declaring
  the agent that *starts* software the module did not put there is declaring a
  lifecycle for something it does not own, which is §8's line.
- There is more than one right implementation — colima with this mac's profile
  and `--mount` flags, Docker Desktop's login item, OrbStack — and the module
  cannot pick without being wrong on two of the three.
- It would collide with an agent the operator already has, and duplicate launchd
  agents racing `colima start` fail in ways nobody enjoys reading.

**Recommended: `chug.node.darwin.dockerBootAgent`, an acknowledgement with a
compile-time half.** Set to the name of a `launchd` entry in this configuration
and the module asserts the entry exists (`config.launchd.user.agents ? ${name}`,
or `agents`, or `daemons`) — which catches the real regression, someone deleting
or renaming the agent later and nothing noticing until the next power cut. Set to
`"external"` and the module is silent: boot persistence is handled outside the
closure, and the *acknowledgement is now a line in the host repo's git history*
with an author and a date, which is more than exists today. Left null — the
default, and what an unmodified adopter has — the module emits a `warnings` entry
naming the hazard.

The honest weakness: `"external"` enforces nothing. It converts an invisible
hazard into a recorded decision, which is the most a host-preparation module can
do about a runtime that lives outside the closure, and it is strictly more than
the silence there is now. The alternative — an unsilenceable warning on every
rebuild — was rejected on A6's own reasoning: a warning that cannot be resolved
is a warning that gets filtered.

### Candidates weighed and rejected

- **`virtualisation.docker.storageDriver`.** nixpkgs' own description: "Changing
  the storage driver will cause any existing containers and images to become
  inaccessible." That is precisely the class — but it is unassertable: every
  value is legal, only a *change* is destructive, and a module cannot see the
  previous generation's value. Best available is a `warnings` entry when it is
  set non-null on a chug node, plus a runbook line. Recommend the warning.
- **`nix.optimise.automatic`.** Hard-links identical store files; running
  processes are unaffected. Not a hazard.
- **Tmpfiles age-rules over the cache dir.** Covered by A6's reasoning: the
  cache is disposable by design.
- **Anything resource-shaped on macOS.** No cgroups, so nothing to assert
  (#308 H.3 cost 3, #309 §7). Not a gap in this module — a property of the
  platform, and the reason `resources:` is a different design's problem.
- **Disk headroom for `WORKER_REFRESH_DISK_FREE_GB_MIN`.** Tempting, since the
  30 GB floor has a four-incident derivation. But free disk is runtime state,
  not configuration; a module can assert nothing useful about it, and the
  refresh already refuses loudly with the numbers. Left alone.

---

## 6. Drain

**Recommendation: no drain hook in the module. It is a runbook step, and the
runbook already has the mechanism.**

The brief is right that a rebuild on a busy node needs a drain, and #308 H.6 is
right that `schedulable` (`crates/worker/src/backend.rs`) is the mechanism-shaped
thing. But the question H.6 left open has since been answered twice over:

- [#293](./293-worker-capacity.md) shipped operator capacity control.
  `platform_fleet_capacity_set` (`crates/api/src/routes.rs`) forwards
  `{node, slots, by}`, and [`docs/runbooks/worker-capacity.md`](../runbooks/worker-capacity.md)
  states it plainly: "Set it to `0`. That is a full drain and it is a first-class
  state, not a hack." Lowering below live occupancy never kills — `free = slots −
  running` goes non-positive and placement skips the node.
- [#309](./309-host-native-execution.md) §6 then examined exactly this question
  and concluded: "**do not invent a drain op** … adding a second one here would
  be the fourth capacity mechanism #293 exists to retire."

A module-side hook would be strictly worse than the button that exists. To drain
before a rebuild the module would have to hold an API token and the dispatcher's
address **in the host's system closure** — a platform credential in a repo this
project does not own, which is a far larger coupling than the module itself, and
it would mint the fifth capacity mechanism §4.1 just rejected. Every argument
runs the same direction.

So the deliverable here is documentation, and it is small:

1. `docs/runbooks/worker-capacity.md` gains a "before `nixos-rebuild switch` /
   `darwin-rebuild switch`" section: set the node to `0`, watch `occupied` in the
   fleet status reach zero, rebuild, restore the slot count.
2. The module's own doc header says the same thing, so the operator meets it
   where they are editing.
3. **A4 is the real mitigation.** With `live-restore = true`, the common rebuild
   — one that restarts dockerd — stops being destructive at all, and the drain
   is reduced to what it should be: **required for a reboot, for the one rebuild
   that adopts A4, and for any rebuild that bumps a node toolchain** (the
   paragraph below). Every other rebuild becomes safe to run hot.

That third case is A5's, and it is worth naming separately because it is not
visible from the docker side at all. A rebuild that bumps a node toolchain moves
the system profile off the old closure, so the *next* `nix.gc` can collect a
store path a task started before the rebuild is still using (A5's third case).
Draining first removes that window entirely — the same step for a different
failure, which is an argument for the step rather than against it. Note the
dependency runs both ways: A5's claim that "under §6's drain-before-rebuild
discipline no task is running across the flip and the case cannot arise" holds
only while toolchain-bump rebuilds stay inside the drained set enumerated above.
Removing them from item 3 would silently invalidate A5.

Worth stating for the reader who comes back to this: the drain answer changes if
**host mode** ships. #309 §6's exception — host tasks must not live in the worker
daemon's cgroup, or `systemctl restart` kills them — is a real, unsolved
constraint that a future `chug-node` module *would* own, because on a host-mode
node the daemon is a unit the module declares. That is a reason to keep this
module's shape open, not a reason to build the hook now.

---

## 7. The boundary: what the module must not reach into

Design #308 H.6's three clocks, restated as the module's charter:

| Clock | Owner | Changes via | The module |
| --- | --- | --- | --- |
| System closure | operator, root | drain + rebuild | **is this** |
| Worker daemon | platform | `deploy/prod/worker-refresh.sh` | must not touch |
| Task environment | project repo | `git push`, no deploy | **must not touch** |

Per-project tooling — Flutter versions, Android SDK composition, Xcode, a Rust
toolchain — is **clock 3**. It does not enter this module, in any form, ever.
CLAUDE.md's "factories and job-type config are project-owned and repo-versioned —
a per-consumer forge" is the same rule from the platform side, and #308 H.6 spells
out the consequence of breaking it: the node becomes a central control plane and a
tool bump stops shipping in the commit that needs it.

The operational test, which is short enough to survive being quoted:

> **If two projects on the same node could reasonably want different values for
> it, it does not belong in `chug.node`** — and that applies to an option's
> *value*, not only to its name.

The second clause is not decoration. A nix option can pass the test on what it is
called and fail it on what has to be written into it, and that is exactly the
shape the live case takes.

**The live case, per C1, is #367's `/nix/store:ro` mount plus §3.5's stable
path** — not the `WORKER_ANDROID_SDK_DIR` the first draft tested, which #367 §3.2
marks dead. It has three parts and the line runs between them:

- **The store mount itself — machine fact, and the module's role is *nothing*.**
  `/nix/store:/nix/store:ro` cannot fail the test: there is one store per node,
  so two projects *cannot* want different values for it. It also needs no module
  support whatsoever — the store exists because the node is a nix machine, and
  the bind is composed worker-side, which per §8 this module does not own. Note
  what passing the ownership test does **not** buy: #367 §8 calls this "the
  design's widest grant", and §3.4 attaches four conditions to it — one of which
  is a store-wide secret scan #367 could not run. Cheap to own is not cheap.
- **The stable path — machine fact, but the module still declines it.** #367 §3.5
  **S3** has the host's config maintain `/etc/chug/android-sdk` pointing at the
  current SDK output, so that no content hash ever enters chug-side config. The
  *convention* — a fixed location, activation-maintained, resolved at use —
  generalizes past Android and reads like a machine fact this module could own,
  say as `chug.node.toolchainPaths = { android-sdk = …; }`. Apply the second
  clause and it collapses: the attribute *name* is a machine fact, but the
  **value** is a derivation two Android projects will want composed differently.
  An option like that imports clock 3 through its value while looking like clock
  1 at its key. Worse, it would launder #367 §3.6's *recorded, time-limited*
  exception — "a borrowed clock, on one node, for one project", with a named
  trigger to end it — into a supported platform interface, which is how a
  deliberate exception becomes permanent. The host repo writes the one
  `environment.etc` line in stock NixOS. #367 §3.5 already assigns the runbook
  sentence to itself ("any nix-provisioned node toolchain this platform consumes
  is referenced through an activation-maintained stable path, never through a
  store path"), so this module does not duplicate it either.
- **The SDK composition — clock 3, decisively.** Which API levels, which NDK,
  which system images. There must be no `chug.node.androidSdk`, no
  `chug.node.flutterVersion`, no `pkgs.androidenv` call anywhere in this module.

The result is that the module's answer to the widest, most tempting case in the
tree is to add nothing at all — which is the strongest available evidence that
the boundary is drawn in a place that holds.

The next design job in this series owns the other side of that line — project-
supplied toolchains, flake devshells and `runtime.env`. This document stops here
deliberately: the two designs meet at exactly one point, the *location* of a
node-side path, and neither may define the other's contents.

---

## 8. Why the module must not declare the container

Job #265's conclusion, carried and re-verified against the tree rather than
restated.

**R1 — a systemd unit would race the swapper.** Verified in
`worker-refresh.sh`'s `swap` phase: the daemon runs *inside* `chug-worker`, so it
cannot remove itself; it launches a detached `docker:cli` sibling whose command
is `sleep 2; docker rm -f chug-worker …; docker run -d --restart=always --name
chug-worker …`. To a supervising systemd unit that `rm -f` is indistinguishable
from a crash, so the unit would restart the container and collide with the
swapper's own `docker run` on the same `--name`. Holds exactly.

**R2 — two supervisors.** `virtualisation.oci-containers` generates a unit with
`Restart = "always"`, and both `build-worker.sh` and the swapper's `RUN_NEW`
pass `docker run --restart=always`. Holds exactly.

**R3 — two sources of truth for the run spec.** Verified and stronger than
stated: `RUN_NEW` composes the docker socket and keys bind **sources recovered by
inspecting the live container** (not reconstructed — the script explains that
re-deriving `$HOME` inside the root-run swapper would bind an empty directory and
strand the daemon without NATS creds), plus `WORKER_NODE`, `NATS_URL`,
`NATS_CREDS`, `RUST_LOG`, `WORKER_REFRESH_GIT_URL`, `WORKER_GIT_KEY`,
`$CACHE_ARGS`, `$DISK_ARGS` and `$SLOTS_ARGS` — each carried forward with its own
recorded reason (#55/#82's silent-revert class). A nix-declared unit would hold a
second, static copy of all of it, and a reboot would resurrect a node with
whatever the closure last said. Holds, emphatically.

**R4 — image delivery.** Per C4, weakened but not load-bearing: `pull = "never"`
exists, so the mechanical objection is softer than #265 stated. It does not
rescue the proposal — R1–R3 are each independently fatal, and a `pull = "never"`
unit fails hard the moment the tag is missing, which is the exact state the prune
incident produced.

**When nix *should* own the container:** after image delivery moves to a
registry, so that the run spec has one place to live and the image is fetchable
rather than node-built. That is a change to how deploys work — it touches
`build-worker.sh`, `worker-refresh.sh`, §3.1's self-refresh design and #313's
workload-identity image builds — and it must be argued on its own. **It is not a
side effect of adding a module, and this design does not propose it.**

---

## 9. Scope

**In:** two flake outputs and the modules behind them; the five options in §4;
the assertions and constructions in §5; the runbook additions in §6.

**Out:** implementing the modules (this is a design job); project toolchain
supply, flake devshells and `runtime.env` (the next design job); host-native
execution (#309); declaring or supervising `chug-worker` (§8); moving image
delivery to a registry; any change to `spec.md` §3.1's wire contract — the module
adds no field, no subject and no schema, which is why it needs no version-skew
gate (§14).

One documentation change to `spec.md` is implied and should ride with the
implementation: §3.1's "created and owned by the worker daemon at startup" is
inaccurate per C3 and should say what actually happens.

---

## 10. Implementation slices

1. **`code`** — add `flake.nix` (F1), `nix/chug-node/{options,nixos,darwin}.nix`,
   and the skipping `nix flake check` stage in `.chug/tasks/ci.sh` (N1). No
   behavior change to anything shipped. **No `MODULES.md` rows**: the registry is
   Rust-only — its header scopes it to `crates/dispatcher/src/**/*.rs`,
   `crates/domain/src/**/*.rs` and `crates/platform-ops/src/**/*.rs`, and
   `.chug/tasks/check-modules.sh` walks exactly those three directories for
   `*.rs`, so a `nix/` file gets no row and the gate would neither require nor
   check one. What the slice does owe the docs tree is a pointer: one line in
   `crates.md`'s layout section saying `nix/` holds the host-preparation modules
   and is not a crate.
2. **`code`** — label the swapper container `chug.managed=true` in
   `worker-refresh.sh` (A1's third bullet), and extend
   `deploy/managed-label.test.sh` to lock it, including the negative case that it
   must not be `chuggernaut.managed`.
3. **`docs`** — the `worker-capacity.md` drain-before-rebuild section (§6), the
   §3.1 cache-ownership correction (§9), and the host-repo adoption runbook
   including `--override-input` for testing against a job branch. **Shipped.**
   The §3.1 correction landed separately in jobs #379/#380 — the sentence now
   reads "provisioned with the node and owned by neither the daemon nor any
   container", which is C3's finding. The other two shipped in job #404, written
   from the 2026-08-03 adoption on both prod nodes rather than from this
   document: [`docs/runbooks/worker-capacity.md`](../runbooks/worker-capacity.md)
   §4.1 (drain, and why the switch that *enables* A4's `live-restore` is the one
   that costs), and
   [`docs/runbooks/chug-node-adoption.md`](../runbooks/chug-node-adoption.md)
   (the flake input, the `chug.node` block, A1b's coupled removal of a host's own
   `label!=` filter, `--override-input`, and verification). Two facts the
   adoption produced that this document did not predict are recorded there: a
   host repo carrying a hand-rolled filter fails A1b at `nixos-rebuild build`
   until its copy is deleted, and **a failed `darwin-rebuild switch` can leave
   `/nix/var/nix/profiles/system` and `/run/current-system` disagreeing** — which
   is how `gumbo-air-0` ran a month on an unapplied configuration, silently.
4. **Adoption, in the host repo** — outside this platform's job graph. It is the
   first real gate on the modules (N3), and it is where the ~90-line comment
   block gets deleted.

Slices 1 and 2 are independent; 3 depends on nothing; 4 depends on 1.

---

## 11. Risks and open questions

- **`live-restore` is recommended from documentation, not from a test.** A4's
  claim — that a dockerd restart under `live-restore = true` leaves job
  containers running — is upstream-documented behavior this job could not
  exercise. It is the one recommendation here that should be *measured* on
  `gumbo-nuc-0` before it is trusted, and the measurement is cheap: start a
  sleeping container, `systemctl restart docker`, look.
- **This document cites a sibling design that is still moving, and that has
  already bitten it once.** #367 grew by 598 lines between this branch's first
  draft and its current base, and the amendment inverted C1 and with it A5 and
  §7. Nothing here depends on #367 *shipping* — the module is justified by C3 and
  the prune incident on their own — but every claim about what #367 *recommends*
  is a claim about a document that can change again in a merge this repo's code
  never sees. Mitigation, such as it is: those claims are cited by section number
  so re-checking them is a `grep`, and the two that carry weight (§3.3's mount,
  §3.5's stable path) are both named in A5 and §7 rather than assumed anywhere
  else.
- **The comment block was not read** (see the preamble). If it encodes a
  constraint not recoverable from this repo, §5's assertion set is incomplete —
  the review of this design is the right place to catch that.
- **macOS enforcement is different, not merely thin — and two exposures stay
  open there.** With no docker module on darwin, A1/A2/A4 have no analogue: the
  air keeps the prune, boot and live-restore hazards with no compile-time guard.
  A7 and A8 are the compensating pair and they are honest about their reach —
  A7 asserts the host's *declaration* of what its VM shares, not the VM, and A8
  accepts `"external"` as an acknowledgement rather than an enforcement. Named
  concretely, the two things that can still take the air down silently are **a
  hand-run `docker system prune -a`** (assertions bound configuration, not
  people) and **a reboot with nothing declaring the runtime's start** — A8
  reduces the second to a decision someone had to write down, which is the most
  a module can do about a VM outside the closure.
- **The mirror's visibility is assumed, not verified** (preamble; §2.2). If the
  GitHub mirror is private, `github:kasofsk/chuggernaut` fails at
  `nix flake update` with a download error and §2.2's fallback — the ssh front
  with an `issue_node_credential` key for root, tailnet-only — becomes the
  transport. The decision is pre-made there; it should not cost a re-design.
- **A1b assumes moby#40286 stays open.** If it is fixed, the single-filter
  assertion becomes unnecessarily strict. The assertion message should name the
  issue so whoever hits it can check.
- **The mirror is the only transport.** If the GitHub mirror is ever retired,
  §2.2 needs re-deciding, and host repos pinned to it break at `nix flake
  update` time rather than silently.
- **Nothing here is exercised by this platform's CI** until a node has nix
  (§2.3). That is a known, accepted gap, not an oversight.

## Correction — 2026-08-04, job #423 (the mirror's visibility is measured: it is public)

**The third unverifiable fact in the preamble is now verified, and §2.2's
conditional resolves in favour of what §2.2 already recommends.** Appended
rather than edited into the prose above, per
[#415](415-knowledge-architecture.md) D2; the preamble's third bullet and §11's
*"The mirror's visibility is assumed, not verified"* stay as written, and are
answered here.

**Measured 2026-08-04 from a work container, two ways.** `gh` is not installed
in the agent image, so the first is the REST call `gh repo view` makes:

```sh
curl -s https://api.github.com/repos/kasofsk/chuggernaut   # "private": false, "visibility": "public"
gh repo view kasofsk/chuggernaut --json visibility,isPrivate
# {"isPrivate":false,"visibility":"PUBLIC"}
```

The second is the property §2.2 actually depends on, rather than a field that
implies it — an **anonymous, credential-free** read succeeds:

```sh
git ls-remote https://github.com/kasofsk/chuggernaut | head -1
# 47d70dfadbf9c8995875d5bac1745d450d2defed	HEAD
```

That `HEAD` was this branch's base commit at the time of measurement, so the
mirror was current, not a stale public snapshot of an old tree.

What this changes, and nothing more:

- **§2.2's recommendation stands as written**, with its load-bearing assumption
  now a measurement. `inputs.chuggernaut.url = "github:kasofsk/chuggernaut"`
  needs no credential in a host repo's closure, which was the whole reason it
  beat the tailnet ssh front.
- **The fallback is not taken.** The ssh front with an `issue_node_credential`
  key installed for root stays what §2.2 says it is — the answer if the mirror
  *becomes* private, pre-decided so nobody re-litigates it — and the GitHub read
  deploy key stays third and worst.
- **Nothing else in §2.2 moves.** The five-minute lag and "the mirror is the only
  transport" are properties of the mirroring agent, not of its visibility, and
  both open questions in §11 that name them are untouched.

**Note that the answer cuts the other way too, and that half is not this
document's subject.** A publicly readable mirror force-pushed every five minutes
means every merge to `main` is a publication; the disclosure boundary that
follows is recorded in [`infra/README.md`](../../infra/README.md), which is
where a job adding a file — rather than a host consuming the flake — needs it.

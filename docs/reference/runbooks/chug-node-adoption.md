# Adopting `chug-node` in a worker node's host repo

**Audience:** whoever owns the NixOS or nix-darwin configuration of a machine
that is (or is about to be) a Chuggernaut worker node. You want the host's
config to *declare* the conditions the platform depends on, instead of
satisfying them by hand and hoping nobody edits them out. This page is the
whole procedure, the one edit that will fail your build if you skip it, and how
to tell the switch actually took.

It is *not* the design argument (that is
[design #372](../../design/372-chug-node-modules.md), including why the module
must not declare the `chug-worker` container). For draining a node before you
switch it, see [`worker-capacity.md`](worker-capacity.md) §4.1; for the
KVM/Android half of a node's configuration,
[`worker-kvm.md`](worker-kvm.md); for the platform's own deploy story,
[`deploy/prod/README.md`](../../../deploy/prod/README.md) §6.

Both prod nodes — `gumbo-nuc-0` (NixOS) and `gumbo-air-0` (nix-darwin) —
adopted the modules on 2026-08-03. This page is written from that, not from
first principles: the trap in §8 is one a from-first-principles runbook would
have missed.

---

## 1. What it declares, and what it deliberately leaves alone

`chug-node` is **host preparation, not a service** — hence `chug.node.*` rather
than `services.chug-node.*`. Since design
[#440](../../design/440-native-worker-daemon.md) D2 it owns **one** lifecycle and
no more — the native worker daemon's systemd unit, **off by default** (§4a) — so
the namespace argument stands unchanged and adopting the module still starts
nothing. What it contributes and asserts (NixOS,
[`nix/chug-node/nixos.nix`](../../../nix/chug-node/nixos.nix)):

| It sets | So that | And asserts the *merged* value, because |
| --- | --- | --- |
| `systemd.units."chug-worker.service"`, only when `chug.node.daemon.enable` | the native daemon comes back from a reboot, under one supervisor | its `ExecStart` and `EnvironmentFile` paths are asserted absolute — systemd rejects a relative one at load, so the node would come back with a unit that never ran. The unit names *where* the environment file is and never a `WORKER_*` value: lifecycle here, run spec in the deploy's file (#372 §8 R3, §4a) |
| `virtualisation.docker.enable` | the daemon and every job container have a socket | a host that turns it off is not a node |
| `virtualisation.docker.enableOnBoot` | `--restart=always` containers come back from a reboot | socket activation makes docker *available*, not *running* — the node comes back dead and silent |
| `daemon.settings.live-restore` | a dockerd restart does not kill in-flight tasks | the cost is real and worth a build failure: live-restore is incompatible with docker swarm |
| `autoPrune.flags += --filter=label!=chug.managed` | the nightly sweep spares the agent images | an unfiltered `docker system prune` deletes the node's whole image set — the 2026-07-25 outage |
| `users.users.<user>.extraGroups += docker` | `deploy/prod/build-worker.sh` can `docker build` over ssh | without it every leg of that path fails as if the socket were broken. It is *not* what gets the daemon to the socket any more: since design [#440](../../design/440-native-worker-daemon.md) slice 4 the Linux unit runs as root, so the membership is the deploy path's alone |
| a `systemd.tmpfiles` rule for `cacheDir` | the sccache dir exists, owned by your user | nothing else creates it any more — the mount is typed, so a missing source is refused |

Two different merge mechanisms, one outcome. The three scalar settings
(`enable`, `enableOnBoot`, `live-restore`) are `mkDefault`, so your own
definition simply wins; the three list contributions (`autoPrune.flags`,
`extraGroups`, the tmpfiles rule) **merge** with yours rather than either one
winning — which is what §5's coupled edit is about. Either way a stock host
needs to know none of it. The unit row is **neither** mechanism: it is a whole
unit declared by name, so a host that defines
`systemd.units."chug-worker.service"` itself gets an eval conflict rather than a
merge — the loud answer. A drop-in under
`/etc/systemd/system/chug-worker.service.d/` is the quiet one, and yours.

Every row but the cache directory is *also* asserted, and an assertion reads the
final merged `config` — so `mkForce` can only turn a silent breakage into a
`nixos-rebuild build` failure that names the reason. The unit's assertion reads
its own options instead, because a conflicting *definition* of it is an eval
error before any assertion gets to run. The cache directory is
created and not asserted on purpose: nothing about its *contents* is a
correctness condition — spec §3.1 makes an empty or cold cache always safe, so
there is nothing for an assertion to protect (design #372 §5 A6). Its
*presence* is a different matter, enforced at launch rather than at build: the
mount is typed, so a node whose cache dir has been deleted refuses every launch
until something recreates it (the tmpfiles rule runs at boot, or on an explicit
`systemd-tmpfiles --create` — not continuously).

On darwin ([`nix/chug-node/darwin.nix`](../../../nix/chug-node/darwin.nix)) there
is no `virtualisation.docker` to contribute to, so the four docker rows above
have no compile-time guard at all. Darwin asserts two other things instead: that
`cacheDir` crosses the VM boundary — dockerd runs inside a VM, so a bind source
the VM does not share resolves *inside the VM*, silently, as an empty directory —
and that `darwin.dockerBootAgent`, when set to a name, names a `launchd` entry
that exists (§4).

**What it does not touch, on purpose:** the daemon's environment file, its
credentials and the node's slot count. Adopting the module still changes no
daemon and no capacity — the one lifecycle it now declares is **off by
default** (§4a). `WORKER_*` still comes from
`deploy/prod/build-worker.sh`, which writes it into the node's environment file
the supervisor loads on every start (#440 D6/D7); a self-refresh copies nothing
forward. Capacity
still belongs to the Cluster page ([`worker-capacity.md`](worker-capacity.md)
§1). Per-project tooling — Flutter, an Android SDK composition, a Rust
toolchain — never enters this module in any form. The test, from
[`nix/chug-node/options.nix`](../../../nix/chug-node/options.nix): *if two projects
on the same node could reasonably want different values for it, it does not
belong here.*

**Nothing in Chuggernaut's own CI evaluates these files.** A `nix/`-only diff
runs no stage of `.chug/tasks/ci.sh` that would see them, and no agent image has
`nix`. **Your `nixos-rebuild build` is the gate** — which is why §6 has you run
it before you switch.

---

## 2. Before you start

- **Your host repo is flake-based.** Both prod host repos already were. A
  non-flake host repo can still consume the modules — `builtins.fetchGit` this
  repo and `import` `nix/chug-node/nixos.nix` directly — but then you hand-roll
  the fetch and the pinning that a flake input and its lock file give you for
  free (design #372 §2.1, option F3). That path works and is out of scope here.
- **You know which login user the platform ssh's in as** (`worksalot` on both
  prod nodes) and which cache path the node's `WORKER_CACHE_DIR` names
  (`/var/cache/chuggernaut/sccache` on the nuc,
  `/Users/worksalot/chuggernaut-worker/sccache` on the air —
  `deploy/prod/env.example`).
- **Grep your own config for `label!=` first.** That is §5's coupled edit, and
  finding it now is cheaper than finding it in an assertion message.

---

## 3. The flake input

```nix
# host repo's flake.nix
inputs.chuggernaut.url = "github:kasofsk/chuggernaut";
```

**Zero inputs, by design.** [`flake.nix`](../../../flake.nix) declares no `inputs`
at all: a NixOS/nix-darwin module is a function of the arguments the evaluating
host passes it, so it needs no `nixpkgs` of its own. Nothing here pins a second
nixpkgs into your closure, and the lock entry never churns.

Two consequences to know rather than debug:

- **`github:kasofsk/chuggernaut` is a mirror, and it lags.**
  `deploy/prod/chug-mirror-install.sh` pushes `main:main --force-with-lease`
  every 300s by default, so a module change is consumable up to five minutes
  after it merges. For a directory that changes roughly never this is a
  non-issue.
- **Only `main` is mirrored.** A job branch is not reachable through this input
  at all — see §7 for how to test against one anyway.

Re-pin later with `nix flake update chuggernaut` (on nix older than 2.19,
`nix flake lock --update-input chuggernaut`).

---

## 4. The import and the `chug.node` block

Small on purpose. NixOS:

```nix
# gumbo-nuc-0
{ chuggernaut, ... }:
{
  imports = [ chuggernaut.nixosModules.chug-node ];

  chug.node = {
    enable   = true;
    user     = "worksalot";
    cacheDir = "/var/cache/chuggernaut/sccache";
  };
}
```

nix-darwin:

```nix
# gumbo-air-0
{ chuggernaut, ... }:
{
  imports = [ chuggernaut.darwinModules.chug-node ];

  chug.node = {
    enable   = true;
    user     = "worksalot";
    cacheDir = "/Users/worksalot/chuggernaut-worker/sccache";  # under $HOME: crosses
    darwin.dockerBootAgent = "colima";                          # a launchd entry below
  };
}
```

The two `cacheDir` values are **not** the same path, and that is the point.
`/var/cache/chuggernaut/sccache` on a mac would evaluate, activate, create a
directory you can see, and cache nothing — dockerd would bind an empty
directory of that name from *inside* the VM. The darwin module refuses it at
`darwin-rebuild build` unless the path is under
`chug.node.darwin.vmSharedPaths`, which defaults to the user's home (what colima
shares out of the box). A mac sharing another prefix declares it:

```nix
chug.node.darwin.vmSharedPaths = [ "/Users/worksalot" "/opt/chug" ];
```

`darwin.dockerBootAgent` is an acknowledgement with a compile-time half. Name a
`launchd` entry in this configuration and the module asserts it exists — which
catches someone renaming or deleting it later. Use the literal `"external"` to
record that boot persistence lives outside this closure. Left unset it *warns*,
because nothing then declares that the container runtime starts at boot, and a
reboot leaves the node UNHEALTHY with `--restart=always` irrelevant: there is no
daemon left to honour it.

`cacheDir = null` is legal everywhere and leaves caching off, which is always
safe.

---

## 4a. The worker daemon's unit — opt-in, and NixOS only

Since design [#440](../../design/440-native-worker-daemon.md) D2 the daemon is a
native process rather than a container, and the module declares the systemd unit
that supervises it. **It is off by default and adopting the module starts
nothing** — turning it on is a separate, deliberate edit:

```nix
chug.node.daemon.enable = true;      # everything else has a default
```

The three knobs, all with the deploy's own defaults
([`nix/chug-node/options.nix`](../../../nix/chug-node/options.nix)):
`daemon.binary` (`/usr/local/bin/chuggernaut`), `daemon.environmentFile`
(`/etc/chuggernaut/worker.env`) and `daemon.path` (the unit's `PATH`, which
names `/run/current-system/sw/bin` first because the daemon shells out to `git`,
`ssh` and `docker`).

**What the module declares and what it must not.** The unit is the *machine
fact* — the binary path, `User=root`, `Restart=always`, which environment file
to read. The **run spec** is the platform's, in that environment file, and no
`WORKER_*` value is ever a nix option: that split is what answers
[design #372](../../design/372-chug-node-modules.md) §8's R3 rather than
contradicting it, and the module's own header argues all four of §8's reasons.
If the two halves name different files the unit **fails to start** saying which
file it could not load — a split has to fail loudly, and this one does.

**Order matters, and on NixOS there is one seam.** `/etc/systemd/system` is a
read-only symlink into the store, so `deploy/prod/build-worker.sh` — which
installs the binary, the environment file and its own copy of the unit — refuses
that node with the live daemon untouched. On a node whose configuration declares
the unit:

1. Declare `daemon.enable = true`, build, drain, switch (§6). Between this step
   and the next the unit exists with nothing to run and `Restart=always` loops
   it; that is loud and bounded, not damage.
2. Give the deploy somewhere to put its copy —
   `WORKER_UNIT_DIR_<node>=/run/systemd/system` in `deploy/prod/chuggernaut.env` <!-- runtime -->
   on the Mini — and run `build-worker.sh` against the node. systemd loads
   `/etc/systemd/system` **ahead of** `/run/systemd/system`, so the
   configuration's unit is the one that runs and the deploy's copy is outranked
   and discarded at the next boot. Do this only once the module declares the
   unit: without it, a `/run` unit is a daemon that disappears at the next power
   cycle, which is why the script refuses rather than falling back there itself.
3. After that the node is on the self-refresh path and **no unit is written
   again** — the swap installs a binary and asks the supervisor to restart
   (#440 D6).

Verify: `systemctl cat chug-worker.service` (the text should be the module's),
`systemctl show -p FragmentPath chug-worker.service` (under `/etc`), and
`systemctl is-active chug-worker.service`.

**darwin declares no agent, and asserts that it does not.** Setting
`chug.node.daemon.enable` on a mac fails `darwin-rebuild build` naming
[`deploy/prod/install-worker-launchd.sh`](../../../deploy/prod/install-worker-launchd.sh),
the opt-in installer that renders the GUI-domain agent from
`deploy/prod/launchd-worker/com.chuggernaut.worker.plist.template`. That
template is deliberately outside `deploy/prod/launchd/`, whose glob
`install-launchd.sh` installs wholesale on the Mini, and the installer refuses a
mac that runs the dispatcher or api agent. A mac's own configuration could
declare `launchd.user.agents` from the same template instead — **secondhand**:
no `macos-runner` configuration is checked out in this repo, so nothing here
verifies that shape.

**None of this is evaluated by Chuggernaut's CI** (§1). What CI does run is
`nix/chug-node/chug-worker-unit.test.sh`, which compares the unit template and
the module's defaults against `build-worker.sh`'s — text over text. A green
Chuggernaut job means the two renderings agree; **your `nixos-rebuild build` is
still the only thing that has ever evaluated the module.**

## 4b. Converting a mac — two node facts, and no way back

A nix-darwin node declares no agent (§4a), so its conversion is entirely
`deploy/prod/build-worker.sh`'s. Two things it needs that a NixOS node does not,
both measured on `gumbo-air-0` on 2026-08-06 and both refusals in the script
today (design [#440](../../design/440-native-worker-daemon.md)'s
[2026-08-07 correction](../../design/440-native-worker-daemon.md#correction-2026-08-07--d6-holds-on-linux-only-and-the-endpoint-was-never-rendered-job-476)):

1. **A Rust toolchain the deploy's ssh shell can see.** The worker image is a
   Linux container, so the binary extracted from it is an ELF file launchd loops
   on with `cannot execute binary file`. A mac compiles its own daemon instead,
   and the compiler has to be reachable **non-interactively** — a nix-darwin
   `cargo` in `/etc/profiles/per-user/<you>/bin` or `/run/current-system/sw/bin`
   often is not, because that shell sources neither the login nor the
   interactive profile:

   ```sh
   ssh <node> 'command -v cargo'      # the exact question the deploy asks
   ```

   Empty, but `cargo` works when you log in? Declare the absolute path on the
   Mini, in `deploy/prod/chuggernaut.env`: <!-- runtime -->

   ```sh
   WORKER_CARGO_air=/etc/profiles/per-user/<you>/bin/cargo
   ```

   **`rustc` has to be in that same directory** — the deploy asks that as a
   separate question, and refuses separately, because cargo resolves its
   compiler through `PATH` rather than from its own location. Declaring an
   absolute `WORKER_CARGO` makes `command -v` pass while the compile still
   fails, which is the failure mode this guard exists for:

   ```sh
   ssh <node> 'PATH=/etc/profiles/per-user/<you>/bin:$PATH; command -v rustc'
   ```

   A rustup or nix-darwin toolchain installs both together, so this is normally
   automatic. A `cargo` that does not exec at all — a rustup shim with no
   default toolchain — is a third named refusal.

   Making the toolchain visible in the host configuration instead is the more
   durable fix and is yours — `environment.systemPackages` reaches an ssh
   command shell through `/run/current-system/sw/bin`. Either way the value ends
   up in the node's run spec **and its directory leads the launchd agent's
   `PATH`**, because the node's own self-refresh compiles too and runs with the
   agent's environment. `WORKER_BUILD_DIR_<node>` moves the tree and target
   directory it builds in (default `~/chuggernaut-worker/build`); it is kept
   between deploys so a rebuild is incremental, and it wants a few GB.

2. **The docker endpoint the mac actually has.** The daemon defaults to
   `/var/run/docker.sock` — true while it was a container with that bind mount,
   false natively, where colima answers at `~/.colima/default/docker.sock`. The
   deploy derives it from the node's own `docker context inspect` and writes
   `WORKER_DOCKER_ENDPOINT` into the run spec, so an ordinary mac declares
   nothing; `WORKER_DOCKER_ENDPOINT_<node>` overrides it, and a socket that is
   not there refuses the deploy rather than producing a node that announces its
   slots and fails every launch with `backend unavailable: Socket not found`.
   **The value is a snapshot.** If you later `colima delete`, rename the
   profile, or switch to Docker Desktop, the daemon keeps dialling the old path
   until the node is re-converted — the same failure, with the node looking
   healthy.

**Converting is one-way.** #440 slice 6 deleted the `docker run` path, so there
is no scripted way back to a container daemon: a failed conversion strands the
node until it is fixed *forward*. Every check above refuses with the live daemon
untouched, and the staged binary must run on the node (`chuggernaut --version`)
before it is installed — but the window between `launchctl bootout` and a
healthy probe is real. **Drain first** ([`worker-capacity.md`](worker-capacity.md)
§4.1) and convert a node you can afford to lose for the length of a build.

**A mac converted before 2026-08-07 has a deadline, not a choice.** The daemon
executes the `worker-refresh.sh` **installed on the node**, and a copy written
by a pre-correction conversion still extracts the Linux binary out of the worker
image — so the next prod deploy that asks it to refresh renames an ELF file over
its working daemon and kickstarts launchd, which is the 2026-08-06 failure
again. `gumbo-air-0` is in exactly that state: re-convert it before the next
deploy, or drain it and `launchctl bootout gui/$(id -u)/com.chuggernaut.worker`
until you can. Re-converting is what replaces the installed script.

---

## 5. The one coupled edit: delete your own `label!=` filter

**A host repo that already hand-maintains a `--filter label!=chug.managed` in
`virtualisation.docker.autoPrune.flags` must remove its own copy.** This is not
optional and it is not cosmetic:

- The module contributes the filter itself, and `flags` is a list option, so
  the two **merge** rather than one winning.
- [moby#40286](https://github.com/moby/moby/issues/40286) — open since
  2019 — ORs multiple `label!` prune filters where every other filter
  combination is ANDed. Two exclusions therefore mean "prune anything lacking
  *either* label", which spares nothing while reading, in your config, like
  belt and braces.
- So assertion A1b permits **at most one** `label!=` filter whenever
  `autoPrune.enable` is true, and keeping both fails `nixos-rebuild build` with
  a message naming the issue.

The 2026-08-03 adoption on the beacon host repo is the shape to expect: a
hand-rolled filter plus roughly eighty lines of comment explaining why it was
there. Both went — the module holds the constraint in code, and the label
distinction it has to hold is one no comment reliably survives: the filter must
name **`chug.managed`** (the image marker) and must **never** name
`chuggernaut.managed` (the container-ownership marker the dispatcher's startup
sweep reaps — naming that one killed the whole worker fleet in #266/#268).

If you want a *second* exclusion for a non-chug workload on the same box,
express it as a `label` allow-filter or give it its own timer. The assertion
says so too.

---

## 6. Build, drain, switch

```sh
# 1. build first — this is the gate (§1). Nothing is activated.
sudo nixos-rebuild build --flake /etc/nixos#gumbo-nuc-0 --impure

# 2. drain: worker-capacity.md §4.1 — slots 0, wait for `occupied: 0`
# 3. switch
sudo nixos-rebuild switch --flake /etc/nixos#gumbo-nuc-0 --impure
# 4. restore the slot count
```

`--impure` here is whatever your host repo already needs; `chug-node` adds no
impurity of its own — both modules take only `{ config, lib, ... }` and read
nothing outside the evaluation.

**Drain this one.** The switch that adopts the module is precisely the switch
that *enables* `live-restore`, and live-restore only protects a dockerd restart
that happens while it is already active — so this restart kills running job
containers, and every later one does not.
[`worker-capacity.md`](worker-capacity.md) §4.1 has the full reasoning and the
commands.

---

## 7. Testing against a job branch

The mirror carries `main` only (§3), so a module change on a job branch is not
reachable through the flake input. Override it at the command line instead —
the input's name is whatever your `flake.nix` calls it:

```sh
# against a local checkout, uncommitted edits included
sudo nixos-rebuild build --flake /etc/nixos#gumbo-nuc-0 --impure \
  --override-input chuggernaut path:/home/worksalot/src/chuggernaut

# against a specific branch of a local clone
sudo nixos-rebuild build --flake /etc/nixos#gumbo-nuc-0 --impure \
  --override-input chuggernaut 'git+file:///home/worksalot/src/chuggernaut?ref=job/404'
```

`darwin-rebuild` takes the same flags. The override is not written to your lock
file, so nothing has to be undone afterwards — and prefer `build` to `switch`
while you are testing, since an assertion failure is the answer you are looking
for and `build` is where it arrives.

---

## 8. Verifying the switch took

### NixOS

```sh
# the merged prune line — exactly one label!= filter, naming chug.managed
systemctl cat docker-prune.service | grep ExecStart

# live-restore is actually in force in the running daemon
docker info --format '{{.LiveRestoreEnabled}}'        # → true

# the tmpfiles rule, and the directory it makes
grep -r chuggernaut /etc/tmpfiles.d/
ls -ld /var/cache/chuggernaut/sccache                 # owned by chug.node.user

# the group half
id -nG worksalot | tr ' ' '\n' | grep -x docker
```

Two notes on the cache rule. It exists only when `cacheDir` is non-null, and
its group field is the user's declared group — or a literal `-` (leave alone)
when the user declares none, which is correct rather than a bug.

`virtualisation.docker.storageDriver` set on a chug node produces a **warning**,
not a failure, and the warning is worth reading: only a *change* to the driver
is destructive, which nix cannot see, and on a worker node that change makes
every agent image inaccessible at once.

### darwin

**None of the NixOS checks carry over.** There is no `virtualisation.docker` for
the module to merge into, so no prune line and no `live-restore` to confirm; no
systemd and so no tmpfiles rule (the cache dir comes from the activation script);
and no docker group — `chug.node.user` on darwin is the ssh and keys user only
([`nix/chug-node/options.nix`](../../../nix/chug-node/options.nix)). Check these
instead:

```sh
# these two MUST agree. If they do not, your last switch did not finish.
readlink /nix/var/nix/profiles/system
readlink /run/current-system

# the cache dir, created by the activation script as chug.node.user
ls -ld /Users/worksalot/chuggernaut-worker/sccache
```

The third check is the switch's own output: the activation script probes `docker
info` **as `chug.node.user`** through a login shell, and prints `chug.node: docker
did not answer for <user>` when it cannot. Silence there is the darwin equivalent
of the daemon check — and a line there is an operational state, not a bad
adoption (see §9).

### The darwin trap: a failed switch that already moved the pointer

`gumbo-air-0` had been unable to complete a `darwin-rebuild switch` **since
2026-07-01**, and nothing surfaced it. A month of configuration changes sat
unapplied while the machine ran happily on the old closure.

The mechanism: macOS 26 SIP-protects `/etc/pam.d` **in both directions**. With
`security.pam.services.sudo_local.enable = false` and the symlink already
present on disk, activation tried to *remove* it and could not — and it failed
**after the system profile pointer had already advanced**. So
`/nix/var/nix/profiles/system` named the new generation, `/run/current-system`
named the old one, and the old one is what was running.

Two lessons, and they generalize past this one PAM case:

1. **A failed `darwin-rebuild switch` can leave the two pointers disagreeing.**
   The build succeeded; the activation did not; the profile moved anyway. There
   is no ambient alarm for this state.
2. **Comparing the two pointers is how you notice.** Make it the first thing
   you check after any darwin switch, and the first thing you check when a mac
   node behaves like a configuration you are sure you changed.

Recovery is to fix what activation choked on and switch again, then re-compare.
Until the two agree, treat every claim about that mac's configuration as
unverified — including the `chug.node` block you just added.

---

## 9. Troubleshooting

| What you see | What it means | What to do |
| --- | --- | --- |
| `chug.node: virtualisation.docker.autoPrune.flags carries 2 label!= filters` | your host repo still has its own copy | delete yours (§5); the module supplies it |
| `chug.node: … autoPrune is enabled without a label!=chug.managed exclusion` | something in the merged config replaced the flags list rather than adding to it | stop forcing `flags`; if you need a second exclusion, use a `label` allow-filter or its own timer |
| `chug.node: … live-restore is not true` | a host override turned it off | remove the override, or accept that this box cannot be a chug node (live-restore is incompatible with docker swarm) |
| `chug.node: … enableOnBoot is false` | an idle-resource optimisation that would make the node die at its next reboot, silently | leave it on |
| `chug.node: <user> is not in the docker group in the merged configuration` | `extraGroups` is being `mkForce`d elsewhere in your config | let the module's contribution merge |
| `chug.node: cacheDir … is not under any of chug.node.darwin.vmSharedPaths` | a Linux path copied onto a mac, or a prefix your VM shares that you have not declared | move the path under `$HOME`, or declare the shared prefix (§4) |
| `chug.node: chug.node.darwin.dockerBootAgent names "…", which is not a launchd entry` | the agent was renamed or removed | point it at the current entry, or use `"external"` |
| a warning that nothing declares the runtime starts at boot | `dockerBootAgent` is unset — the default | set it, or set `"external"` to record the decision with an author and a date |
| `chug.node: docker did not answer for <user>` on every darwin switch | the activation probe runs as `chug.node.user` through a login shell, because a mac's runtime is user-scoped | the VM is down, `docker` is not on that user's `PATH`, or activation could not `sudo -n`. All three are operational states, which is why this warns rather than fails |
| `chug.node: chug.node.daemon.enable is a NixOS option — it declares a systemd unit, and this is darwin` | the daemon knobs are Linux-only | install the macOS agent with `deploy/prod/install-worker-launchd.sh` (§4a) |
| `chug-worker.service` loops on `Restart=always`, journal says it cannot load the environment file | the unit is declared and the deploy has not run yet, or the two halves name different files | run `build-worker.sh` against the node, or point `daemon.environmentFile` and `WORKER_ENV_FILE_<node>` at one path (§4a) |
| `build-worker: … has no usable systemd unit directory at '/etc/systemd/system'` | NixOS: the deploy has nowhere to write its own copy of the unit | declare the unit (§4a) and set `WORKER_UNIT_DIR_<node>=/run/systemd/system` |
| the switch "worked" but nothing changed (darwin) | the two profile pointers disagree | §8 |
| running jobs died during a switch | that switch restarted dockerd without live-restore active — the adopting switch, or a reboot | drain next time ([`worker-capacity.md`](worker-capacity.md) §4.1) |

---

## Related

- [design #372](../../design/372-chug-node-modules.md) — §5 (the assertion set:
  A1/A1b the prune filter, A4 live-restore, A7 the VM boundary, A8 boot
  persistence), §6 (drain), §7 (what the module must not reach into), §8 (why it
  must not declare the container — still true of the container, amended for a
  unit over a binary by [#440](../../design/440-native-worker-daemon.md) D2).
- [`worker-capacity.md`](worker-capacity.md) §4.1 — drain before rebuild, and
  why the adopting switch is the expensive one.
- [`worker-kvm.md`](worker-kvm.md) — the KVM/Android half of a node's
  configuration: the stable toolchain path the host repo maintains (§1) and the
  GC roots that keep it alive under a rebuild (§7).
- [`nix/chug-node/options.nix`](../../../nix/chug-node/options.nix) — the charter,
  including the two-projects test for what may enter the module.
- [`docs/spec.md`](../../spec.md) §3.1 — normative: worker nodes and their node-local
  properties, including the cache directory's ownership.
- [`deploy/prod/README.md`](../../../deploy/prod/README.md) §6 — provisioning a
  worker node's daemon: the binary, the credentials and the environment file
  carrying its run spec. Since design
  [#440](../../design/440-native-worker-daemon.md) slice 7 the Linux *unit* may
  instead be this module's (§4a); the run spec never is.

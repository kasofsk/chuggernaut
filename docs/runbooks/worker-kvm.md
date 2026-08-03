# Turning KVM on for a worker node (Android emulator execution)

**Audience:** the prod operator. You want one node to run Android emulator work
— a Flutter build and a device-backed task against a real emulator — and no
other node to change at all. This page is the whole procedure and its failure
modes.

It is *not* the design argument (that is
[design #367](../design/367-android-emulator-execution.md), including why a
device passthrough beats a host runtime) and not the normative text
([`spec.md`](../../spec.md) §3.1). For capacity, see
[`worker-capacity.md`](./worker-capacity.md); for the standing deploy story,
[`deploy/prod/README.md`](../../deploy/prod/README.md) §6.

---

## 1. Three owners, and none of them can do the others' job

| Piece | Owned by | Where it lives |
| --- | --- | --- |
| the SDK, and a **stable path** to it | the node's NixOS config | `configuration.nix` — an `environment.etc` (or equivalent) entry pointing a fixed path such as `/etc/chug/android-sdk` at the current `androidsdk` output; §7 narrows this to a *direct* symlink once per-task GC roots are on |
| `WORKER_KVM`, `WORKER_KVM_PROJECTS`, `WORKER_ANDROID_SDK_DIR`, the optional `WORKER_FLUTTER_DIR`, **and `--device`** | `deploy/prod/build-worker.sh`, carried across every self-refresh by `deploy/prod/worker-refresh.sh` | the `chug-worker` container's `docker run` |
| **who** may use it | `WORKER_KVM_PROJECTS` — `owner/project` entries, comma-separated | the same `docker run` |

The stable path is not a nicety. `/nix/store/3zr1pgw…-androidsdk/…` is
content-hashed, so an SDK bump changes it: a pinned store path keeps testing the
**previous** SDK until `nix-collect-garbage` removes it, and then fails with an
`ENOENT` nobody typed recently. The daemon refuses a store hash outright
(design #367 §3.5) — name the activation-maintained path instead, and
`nixos-rebuild switch` moves the SDK and the path in one atomic step.

**Flutter is a second leaf, not a replacement.** A node whose jobs build a
Flutter app also sets `WORKER_FLUTTER_DIR` — its own stable path, mounted
read-only at `/opt/flutter`, with `FLUTTER_ROOT` pointed there. The two are
complementary: Flutter ships Dart, the gradle wrapper and the engine artifacts,
while `adb`, `emulator` and `platform-tools` come only from the Android SDK, so
an emulator proof needs both. Unset ⇒ no mount, no `FLUTTER_ROOT`, and the
launch is what it was; `WORKER_ANDROID_SDK_DIR` keeps its meaning either way, so
turning Flutter on is not a migration. It is **not** realised or GC-rooted (§7)
— only the Android SDK is.

**The allow-list is fail-closed.** Unset or empty grants *nobody*, so enabling
KVM on a node and granting it to a project are two separate acts. A node with
`WORKER_KVM=1` and no projects starts, says so in its log, and hands the device
to no launch.

---

## 2. The device is not optional, and forgetting it is a node-down

`chug-worker` is **itself a container**. The daemon's "does this node have the
device" check therefore reads the *daemon container's* view of it
(`crates/worker/src/daemon.rs`, `build_backend`), and a daemon that is given
`WORKER_KVM` without `--device /dev/kvm` **refuses to start**. It is then
restarted into the same refusal by `--restart=always`, and the node leaves the
fleet.

The two ways to get this wrong are not symmetric:

| Mistake | Result |
| --- | --- |
| the env var is dropped, the device survives | KVM turns off. Jobs still run, the node stays up. A quiet regression |
| the device is dropped, the env var survives | **the node goes down** and stays down until someone recreates the daemon by hand |

Both scripts are written around that asymmetry, so a normal deploy cannot
produce either:

- `build-worker.sh` adds `--device` whenever `WORKER_KVM` is on, mapping the
  value the way the daemon parses it (a boolean ⇒ `/dev/kvm`, an absolute path ⇒
  that device). A value that is neither is **refused before the live daemon is
  removed**.
- `worker-refresh.sh`'s swap re-composes `docker run` from scratch, so it reads
  the device off the **live container's** `.HostConfig.Devices` — the same
  inspect-what-is-actually-running rule the keys and socket mounts follow — and
  **refuses the swap** if `WORKER_KVM` is *on* and no device can be carried
  forward. A node stuck on an old SHA is a deploy warning; a node that will not
  start is an outage. Both scripts trim and read the value exactly as the daemon
  does, so an explicit `WORKER_KVM=0` — a node with the setting and legitimately
  no device — swaps normally rather than being frozen on its current SHA.

---

## 3. The procedure

**Before you start**, on the node: `ls -l /dev/kvm` (it is `0666`, so no `kvm`
group membership is needed), and confirm the stable SDK path resolves —
`readlink -f /etc/chug/android-sdk`. Also read design #367 §3.4: an allow-listed
launch mounts the node's `/nix/store` read-only, so a store holding operator
secrets is a store you do not hand to a project.

`build-worker.sh` needs ssh to the node, and the Mini cannot ssh a tagged worker
(Tailscale blocks tagged→tagged) — so this is a **laptop** step, exactly like
first provisioning the node:

```sh
WORKER_SSH=worksalot@gumbo-nuc-0 CHUG_WORKER_NODE=nuc \
  WORKER_NATS_URL=nats://100.116.243.42:4222 WORKER_SLOTS=2 \
  WORKER_KVM=1 \
  WORKER_KVM_PROJECTS=acme/beacon \
  WORKER_ANDROID_SDK_DIR=/etc/chug/android-sdk \
  WORKER_FLUTTER_DIR=/var/lib/chuggernaut/toolchain/flutter \
  deploy/prod/build-worker.sh
```

Pass **every** var the node should keep, not just the new ones: this recreates
the daemon container, so a var you omit is a var the node loses. Drop the
`WORKER_FLUTTER_DIR` line on a node that provisions no Flutter.

The same names are documented in `deploy/prod/env.example`, and a
deployment whose `WORKER_SSH` *is* set (i.e. one where `update.sh` runs
`build-worker.sh` for you) can put them in `chuggernaut.env` instead. On prod,
where deploys reach the node over the no-ssh self-refresh path, the values in
`chuggernaut.env` never reach the daemon — the swap inherits the **live
daemon's** env, which is the one the command above set.

---

## 4. Verifying

```sh
# the daemon came up and says what it enabled
ssh worksalot@gumbo-nuc-0 docker logs chug-worker 2>&1 | grep -i kvm
#   KVM passthrough enabled for the allow-listed projects device=/dev/kvm …

# the device is on the daemon container itself (this is the one people forget)
ssh worksalot@gumbo-nuc-0 \
  docker inspect chug-worker --format '{{json .HostConfig.Devices}}'
#   [{"PathOnHost":"/dev/kvm","PathInContainer":"/dev/kvm","CgroupPermissions":"rwm"}]

# during an allow-listed job: the job container has the device and the mounts
ssh worksalot@gumbo-nuc-0 docker inspect <job-container> \
  --format '{{json .HostConfig.Devices}} {{json .HostConfig.Mounts}}'
```

**The end-to-end check is a job, not a command.** Release an `android-proof`
job (`.chug/jobs/android-proof.yaml`, design #367 A2) and read its `stdout.log`
artifact: `.chug/tasks/android-proof.sh` climbs a five-rung ladder — the mounts
and env, `emulator -accel-check`, the toolchains, `flutter build apk --debug` of
`fixtures/mobile`, then an emulator boot and a device-backed task — and prints a
LADDER summary naming the rung that broke. Rung 1 failing is a
placement-or-allow-list problem and everything above it is a toolchain one,
which is the fork this table exists to tell apart.

`build-worker.sh` also proves the daemon is up before it claims success — it
waits for the daemon's own `worker up` line and fails loudly on a timeout, so a
KVM misconfiguration that stops the daemon booting fails the run rather than
leaving a dead node behind.

---

## 5. Turning it off

Recreate the daemon without `WORKER_KVM` (same command, that line dropped). The
device goes with it, and the next self-refresh has nothing to carry forward.
Narrowing the grant instead of removing the capability is the smaller change:
drop the project from `WORKER_KVM_PROJECTS` and no launch receives the device or
the mounts, while the node keeps working.

---

## 6. Troubleshooting

| Symptom | What it means | What to do |
| --- | --- | --- |
| the node vanishes from the fleet right after a KVM change, `docker ps -a` shows `chug-worker` restarting | the daemon is refusing to start. `docker logs chug-worker` names the reason — almost always `WORKER_KVM names /dev/kvm, which this node does not have`, i.e. the env without the device | recreate the daemon with `build-worker.sh` (which passes `--device`), or remove `WORKER_KVM` |
| `build-worker: WORKER_KVM='…' is neither 1/0 nor an absolute device path` | the value would be rejected by the daemon's own parse, so the script refused before touching the live daemon | use `1`/`0` or an absolute device path |
| `worker-refresh: WORKER_KVM='…' enables KVM but the live chug-worker has no device to carry forward` | the running daemon was created some other way — by hand, without `--device`. Only an *enabling* value gets here; `0`/`false`/`off` swaps normally | recreate it with `build-worker.sh`; the deploy leg carries this as a node warning, and the node stays up on its current SHA |
| the daemon logs `WORKER_KVM is on but WORKER_KVM_PROJECTS is empty` | the capability is on and granted to nobody — fail-closed, working as intended | add the `owner/project` entry and recreate the daemon |
| a job runs but the emulator reports no KVM | that project is not on the allow-list, so it gets neither the device nor the mounts | check `WORKER_KVM_PROJECTS` against the job's `JOB_PROJECT` (`owner/project`, exactly) |
| the SDK is missing inside the container | the stable path does not resolve on the node | `readlink -f` it; the mount refuses a missing source rather than creating an empty directory, so the launch fails loudly |
| `FLUTTER_ROOT` is unset in an allow-listed job, or `/opt/flutter` is empty | the node never set `WORKER_FLUTTER_DIR` — it is optional and off by default | set it (§3) and recreate the daemon; the Android SDK is unaffected either way |

---

## 7. Holding the toolchain against the node's garbage collector

The mounts above give a task nix store paths, and **nothing roots them**. Today
the node's SDK survives its weekly `nix-gc` only because it sits in
`environment.systemPackages` and is therefore reachable from
`/run/current-system` — a property of *whose* config supplies the toolchain, not
of the mount. Set `WORKER_NIX_GCROOTS_DIR` and the daemon stops depending on
that: before each allow-listed launch it realises the declared toolchain and
registers an **indirect GC root named by task id**, in one command through the
node's nix daemon, held for exactly that task's lifetime.

| Piece | Owned by | Where it lives |
| --- | --- | --- |
| the roots directory itself | `deploy/prod/build-worker.sh` (`mkdir -p`, then `sudo -n mkdir -p`) | a worker-writable host path, e.g. `/var/lib/chuggernaut/gcroots` |
| the mounts into `chug-worker` (the store and the profiles tree read-only, the socket dir and the roots dir read-write, plus the **directory holding** the toolchain path read-only when a device is attached) | `build-worker.sh`, carried across every self-refresh by `worker-refresh.sh` | the daemon's own `docker run` |
| `WORKER_NIX_GCROOTS_DIR`, `WORKER_NIX_CLIENT`, `WORKER_NIX_DAEMON_SOCKET`, `WORKER_NIX_STORE_DIR`, `WORKER_NIX_REALISE_TIMEOUT_SECS` | the same `docker run` | the daemon's environment |

Add the roots dir to the §3 command and the rest follows:

```sh
WORKER_NIX_GCROOTS_DIR=/var/lib/chuggernaut/gcroots \
  deploy/prod/build-worker.sh
```

A few things are worth knowing before you do:

- **It is a node-down if a mount is dropped**, exactly like the device: the
  daemon refuses to start without its roots dir, its client or the socket *in its
  own view*, and `--restart=always` loops the refusal. Both scripts are written
  around that — `build-worker.sh` refuses a deploy it cannot provision, and
  `worker-refresh.sh` refuses a swap whose live container has no nix mount to
  carry forward.
- **The client is a profiles path, deliberately.** `chug-worker` outlives many
  `nixos-rebuild`s and docker resolves a bind source host-side at create, so a
  client resolved into `/nix/store` is pinned to one generation and
  `--delete-older-than` can collect it out from under the running daemon. The
  profiles tree is itself a GC root; a store-hash value is refused outright.
- **A slow realise fails the launch, loudly.** It runs before the task exists, so
  `resources.task_timeout` cannot cover it; `WORKER_NIX_REALISE_TIMEOUT_SECS`
  (default 30, **1..45 only**) bounds it and breaking the bound refuses the launch
  rather than requeueing it as capacity. The ceiling is not a preference: the
  realise runs inside the `launch` RPC the dispatcher abandons after 60s, so a
  larger value is never reached — the task fails on *worker transport* instead,
  never naming the bound, while the node goes on launching a container nobody is
  waiting for. A toolchain too slow for 45s wants a warming job (design #373
  Decision 5), not a bigger number; both the daemon and `build-worker.sh` refuse
  one.
- **The stable path must be ONE hop into the store, and it is its PARENT that is
  mounted.** `nix-store --realise` resolves its argument *client-side* — inside
  `chug-worker`, before the nix daemon hears anything — so the daemon container
  has to be able to read the operator's symlink and follow it into the mounted
  store. A bind whose source *is* that symlink cannot deliver it: `mount(2)`
  resolves the source host-side, and the container gets the store path's content
  at a non-store path, which the client refuses ("not in the Nix store"). So
  `build-worker.sh` binds `dirname` of the toolchain path read-only, and requires
  the path itself to be a **direct, absolute symlink into the store**:

  ```nix
  systemd.tmpfiles.rules = [ "L+ /etc/chug/android-sdk - - - - ${pkgs.androidsdk}" ];
  ```

  A NixOS `environment.etc` entry does *not* qualify — it routes through
  `/etc/static`, a second hop no mount here reproduces. Check yours with
  `readlink /etc/chug/android-sdk` (one hop, not `readlink -f`): the answer must
  begin `/nix/store/`. `build-worker.sh` refuses the deploy when it does not, and
  the daemon re-derives the same property from inside the container at boot, so
  neither shape ever reaches a per-launch failure. Binding the parent also means
  the symlink is *shared*, not copied: a `nixos-rebuild` moves the toolchain
  under a running daemon rather than pinning the generation current at the swap.

- **This is the opposite of what the JOB container's mounts do, and both are
  right.** `/opt/android-sdk` and `/opt/flutter` bind the **leaf itself** — the
  stable path, not its parent — precisely so `mount(2)` resolves it host-side and
  the job container gets the toolchain's content at a fixed, hash-free path.
  Nothing runs `nix-store` against those mounts; the job just reads files, and an
  SDK at a non-store path works fully (design #367 measurement 2, down to
  `emulator -accel-check`). The parent-bind rule above is specific to the
  **realise target**, whose client *must* still see a store path. Do not
  "fix" either into the other.

Verify on the node while a task runs: `ls -l $WORKER_NIX_GCROOTS_DIR` shows one
`task-<id>` symlink per running task, `nix-store --query --roots <store-path>`
names it, and it is gone once the container is removed. A `task-<id>` left by a
worker that died is removed by the daemon's own bounded reaper on a later pass —
a leak of disk, never of a job.

| Symptom | What it means | What to do |
| --- | --- | --- |
| `WORKER_NIX_GCROOTS_DIR … is not a directory in the daemon's own view` in `docker logs chug-worker` | the dir is missing on the host, or is not mounted into the daemon container | recreate the daemon with `build-worker.sh`, which creates it and mounts every path |
| `the toolchain this node realises (…) does not resolve in the daemon's own view` | the SDK path's **parent** is not mounted into `chug-worker` (the realise resolves it client-side) | recreate the daemon with `build-worker.sh`; it adds that mount whenever a device is attached |
| `… resolves to … which is not under /nix/store` | the stable path reached the container as a plain directory — the leaf was bound instead of its parent, or the path is not a symlink into the store | make it one direct `L+` hop into the store (above) and recreate the daemon |
| `build-worker: … is not a direct symlink into '/nix/store' under a real parent directory` | `WORKER_ANDROID_SDK_DIR` is missing, is a plain directory, or is an `environment.etc` entry hopping through `/etc/static` | declare it with `systemd.tmpfiles` (above), or unset `WORKER_NIX_GCROOTS_DIR` |
| `WORKER_NIX_REALISE_TIMEOUT_SECS=… is over the ceiling` (daemon) or `is outside 1..45` (deploy) | the bound cannot fit inside the `launch` RPC | lower it to ≤45, and warm the toolchain with a scheduled job if that is not enough |
| `build-worker: cannot provision WORKER_NIX_GCROOTS_DIR` | neither the login user nor `sudo -n` could create it; the live daemon was left running | create it by hand on the node, or unset the var |
| `build-worker: … lacks the nix preconditions` | the node has no `/nix/store`, profiles tree or daemon socket | this node cannot hold roots — unset the var |
| `worker-refresh: … has no mount at '<path>' to carry forward` | the running daemon was created some other way, without the nix mounts | recreate it with `build-worker.sh`; the node stays up on its current SHA |
| a launch fails naming `WORKER_NIX_REALISE_TIMEOUT_SECS` | the realise did not finish inside the node's bound | warm the toolchain (a scheduled job declaring it), or raise the bound at node creation, up to the 45s ceiling |

---

## Related

- [design #367](../design/367-android-emulator-execution.md) — §2.3 (the three
  settings), §3.3 and §3.5 (the mounts and the stable path), §3.4 (what the
  `/nix/store` mount exposes), Phasing (A1 shipped the daemon, A2 is the gradle
  proof this unblocks).
- [design #373](../design/373-project-toolchains.md) — 3b (where the realise
  runs and its trust cost), 3c (the bound), Decision 4 and Correction C5 (the
  roots and the reaper) — the argument behind §7 above.
- [`spec.md`](../../spec.md) §3.1 — worker nodes, node-local properties.
- [`worker-capacity.md`](./worker-capacity.md) — the other node knob, and the
  same "recreate the daemon to change a boot value" shape.
- `deploy/prod/build-worker.sh`, `deploy/prod/worker-refresh.sh`,
  `deploy/prod/env.example` — where these settings are threaded through.

# Turning KVM on for a worker node (Android emulator execution)

**Audience:** the prod operator. You want one node to run Android emulator work
— `./gradlew connectedAndroidTest` against a real emulator — and no other node
to change at all. This page is the whole procedure and its failure modes.

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
| the SDK, and a **stable path** to it | the node's NixOS config | `configuration.nix` — an `environment.etc` (or equivalent) entry pointing a fixed path such as `/etc/chug/android-sdk` at the current `androidsdk` output |
| `WORKER_KVM`, `WORKER_KVM_PROJECTS`, `WORKER_ANDROID_SDK_DIR`, **and `--device`** | `deploy/prod/build-worker.sh`, carried across every self-refresh by `deploy/prod/worker-refresh.sh` | the `chug-worker` container's `docker run` |
| **who** may use it | `WORKER_KVM_PROJECTS` — `owner/project` entries, comma-separated | the same `docker run` |

The stable path is not a nicety. `/nix/store/3zr1pgw…-androidsdk/…` is
content-hashed, so an SDK bump changes it: a pinned store path keeps testing the
**previous** SDK until `nix-collect-garbage` removes it, and then fails with an
`ENOENT` nobody typed recently. The daemon refuses a store hash outright
(design #367 §3.5) — name the activation-maintained path instead, and
`nixos-rebuild switch` moves the SDK and the path in one atomic step.

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
  deploy/prod/build-worker.sh
```

Pass **every** var the node should keep, not just the new ones: this recreates
the daemon container, so a var you omit is a var the node loses.

The same three names are documented in `deploy/prod/env.example`, and a
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

---

## Related

- [design #367](../design/367-android-emulator-execution.md) — §2.3 (the three
  settings), §3.3 and §3.5 (the mounts and the stable path), §3.4 (what the
  `/nix/store` mount exposes), Phasing (A1 shipped the daemon, A2 is the gradle
  proof this unblocks).
- [`spec.md`](../../spec.md) §3.1 — worker nodes, node-local properties.
- [`worker-capacity.md`](./worker-capacity.md) — the other node knob, and the
  same "recreate the daemon to change a boot value" shape.
- `deploy/prod/build-worker.sh`, `deploy/prod/worker-refresh.sh`,
  `deploy/prod/env.example` — where these settings are threaded through.

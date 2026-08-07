# Design #367 — Android emulator execution: a container with `/dev/kvm`, not a host runtime

Status: IMPLEMENTED IN PART, amended 2026-08-02 — recommendation unchanged, phase A1's mechanism rewritten; A1 and A2 have since landed (jobs #374, #395), A3 and A4 are open.

Written against the tree at `1e567e3`, amended against `fef87f9`. Every claim
about this repository was read out of the source or out of
[`docs/spec.md`](../spec.md), not inferred from a sibling design; where the brief
and the tree disagree, the tree wins and the disagreement is recorded in
[Corrections](#corrections-verified-against-the-tree). The four external claims
the brief supplies were fetched and are quoted where used. One class of claim —
anything about the **beacon** repository — is **not verifiable from this tree**:
`~/beacon` is not present in this container, and every such claim below is
marked *(secondhand)*.

**The 2026-08-02 amendment.** An operator ran the experiment this document's
phase A0 asked for, on `gumbo-nuc-0` against the real `chuggernaut/agent:prod`
image. It **confirms the recommendation and refutes phase A1's mechanism**, so
the recommendation below is untouched and §[3](#3-part-two-the-toolchain-bulk)
and §[7](#7-sequencing-what-ships-first-and-what-it-unblocks) are rewritten
around what was measured. That measurement is *(secondhand)* on the same terms
as everything else about that node: `gumbo-nuc-0` is not reachable from this
container (`ssh` fails at host-key verification), and this container has no
`/dev/kvm`, no `/nix`, and no docker socket, so nothing about it could be
reproduced here. Corrections [8](#the-2026-08-02-measurement-corrections-812)
onward carry it; correction 12 is the one finding of the amendment that *is*
tree-verified, and it is a second thing A1 got wrong.

**What this document is for.** [#308](./308-gha-port.md) category F treats
"mobile" as one thing that needs host-native execution, and
[#309](./309-host-native-execution.md) §H and
[#322](./322-macos-native-runtime.md) build that route. #322 is explicitly
macOS-only. **No document in this tree designs the Android leg at all**, and
`/dev/kvm` appears in the whole corpus exactly three times — twice in #308
(lines 690 and 801) as a passing machine-fact, once in #309 (line 540) as an
example of a physical fact. This document fills that hole and argues that the
two legs of category F are not symmetric and should stop being sequenced as if
they were.

**Scope.** This does **not** redesign iOS — [#322](./322-macos-native-runtime.md)
owns the macOS instantiation, and where a primitive is shared this document says
so and defers to it. It does **not** re-litigate whether host-native execution
should exist; #309 and #322 decided that, and the recommendation below leaves
both intact. It changes nothing about `runtime:`, `CONFIG_SCHEMA_EPOCH`, or the
host backend.

Related: [`docs/spec.md`](../spec.md) §3.1 (backends, placement, worker RPC,
node-local build caching, "no host bind-mounts"), §3.5 (launch capacity queue),
§14.1 (config/version skew); [#308](./308-gha-port.md) §F, §G, H.2, H.4, H.5,
H.6 and the gap table; [#309](./309-host-native-execution.md) §4, §5a, §5b, §9,
§10, Phasing; [#322](./322-macos-native-runtime.md);
[#313](./313-workload-identity-image-builds.md) B-IV;
[#355](./355-project-task-images.md); [#361](./361-per-run-placement.md);
[#362](./362-binary-artifacts.md); [`docs/reference/style.md`](../reference/style.md);
[`docs/reference/crates.md`](../reference/crates.md); [`docs/reference/testing.md`](../reference/testing.md).

## Current state

*The **mutable head** ([#415](415-knowledge-architecture.md) [D2](415-knowledge-architecture.md#d2-every-design-doc-opens-with-a-mutable-current-state-head)):
rewritten to current truth whenever anything below it changes. Everything after
this section is append-only — the original argument and its 2026-08-02
amendment, never edited into the prose above them.*

**The recommendation shipped, and the Android leg did precede #322 W2 as
predicted.** A1 and A2 are in the tree: `WORKER_KVM` / `WORKER_KVM_PROJECTS`
gate a per-project `/dev/kvm` device and a read-only `/nix/store` mount, the
Android environment is injected beside the cache env, and
[`.chug/jobs/android-proof.yaml`](../../.chug/jobs/android-proof.yaml) plus
[`.chug/tasks/android-proof.sh`](../../.chug/tasks/android-proof.sh) climb the
ladder on a pinned node. What is still true from the original argument: the work
is **pinned** with `placement.node`, because no placement decision reads a node's
capabilities. `NodeCapabilities` itself now exists —
[#309](309-host-native-execution.md) §4 landed it on both worker transports in
job #483, which correction 2 below predates — but with `modes`, `platform`,
`resources_enforced` and `leases` only. A3 is still unbuilt: it is the `features`
field, plus the `choose_placement` predicate #309 P2 slice 6 and
[#322](322-macos-native-runtime.md) P1 also want, and it is only needed when a
second KVM node exists.

The rows below are the states of [7. Sequencing: what ships first, and what it
unblocks](#7-sequencing-what-ships-first-and-what-it-unblocks)'s table, which
keeps each phase's full argument.

| Phase | What | State |
| --- | --- | --- |
| **A0** | *Operator:* the store secret-scan gate and a stable SDK path in the node's `configuration.nix` | Pending — in the operator's repo, not this one; the rest was already done (correction 10) |
| **A1** | `WORKER_KVM` + `WORKER_KVM_PROJECTS`, the device and read-only store mount, the Android env vars, the pinned job type | **Landed** (job #374) |
| **A2** | The proof ladder: mounts and env → `emulator -accel-check` → the toolchains → a `flutter` build → an emulator boot | **Landed** (job #395) |
| **A3** | `NodeCapabilities` with `features` + the `choose_placement` predicate — one slice serving #309 P2 and #322 P1 | Proposed — needed only when a second KVM node exists |
| **A4** | `placement.features` as a job-type field + the epoch bump | Proposed — only when the pin stops expressing the requirement |
| **never** | Device leases for Android | Not planned — §[4](#4-exclusivity-and-why-android-does-not-need-a-lease); #309 §5b stays, motivated by iOS |

---

## Corrections (verified against the tree)

The brief is right about the thing that carries its argument — Android is a
device-passthrough problem, not a host-execution problem. Seven claims needed
adjusting, and five of them move work. The 2026-08-02 amendment adds five more
(8–12) in [its own subsection](#the-2026-08-02-measurement-corrections-812);
four of those move work too. Building phase A2 adds a thirteenth
([below](#building-a2-correction-13)), and it moves A2's own entrypoint; the
first real A2 run adds a fourteenth
([below](#the-first-real-a2-run-correction-14)), which bounds correction 11.

1. **#322 W1 has landed. The brief understates its own case.** It says the
   Android route needs neither `HostBackend` nor the `runtime:` selector nor the
   epoch bump, and implies #309 P0 / #322 W1 is still ahead. It is not:
   `crates/worker/src/daemon.rs` already holds
   `Arc<dyn ContainerBackend>` behind a single `build_backend` construction
   site that owns the `with_cache_dir` / `ping_all` wiring;
   `managed_running_total` is a **provided** trait method
   (`crates/container/src/lib.rs`); `WORKER_MODES` parses in
   `crates/worker/src/config.rs` (`parse_modes`, `WorkerMode::{Container,
   Host}`); and a node declaring a mode this build cannot serve refuses to start
   by name (`crates/worker/tests/nats_backend.rs`,
   `declared_mode_without_a_backend_refuses_to_start`). What remains on the host
   line is **W2 and beyond** — the host backend, the durable task registry, the
   `/workspace` rebase. Android needs none of it.

2. **`NodeCapabilities` does not exist anywhere in the code.** `grep -rn
   "NodeCapabilities" crates/` returns nothing; the only `capabilities` hit in
   the whole crate tree is an MCP protocol field in
   `crates/chuggernaut-channel/src/server.rs`. It is entirely a design-doc
   construct (#309 §4, cited forward by #322 P1, #355 §6 and #361). **This makes
   the `platform` contradiction cheap to settle**: it is a disagreement between
   two paragraphs of one unimplemented design, not between a design and a
   shipped wire field. Resolving it is a docs edit with no migration.

3. **The launch path already models devices; only the plumbing is missing.**
   `ContainerLaunchConfig` (`crates/container/src/lib.rs`) has exactly `image,
   cmd, env, files, cpu_limit, memory_limit, node` — the brief is right that
   there is no device field. But `bollard` 0.19's `HostConfig` already carries
   `devices`, `device_cgroup_rules` and `device_requests`, with `DeviceMapping {
   path_on_host, path_in_container, cgroup_permissions }` (read out of the
   prebuilt `libbollard-*.rmeta` under `/opt/chug-prebuilt-target`; the crate
   source is not vendored in this container, so the field *names* are verified
   and their exact optionality is not). So the Docker-side change is populating
   one existing struct field in `build_host_config`
   (`crates/container/src/docker.rs`), not a new API surface.

4. **`WorkerLaunchRequest` carries six fields, not seven** — `image, cmd, env,
   files, cpu_limit, memory_limit` (`crates/types/src/worker.rs`). There is no
   `node`: the node is the NATS subject (`req.worker.{node}.{op}`). Worth
   stating because it is the reason a node-side mechanism costs the wire
   *nothing*: the worker already knows which node it is.

5. **The job-type name never reaches the worker, and this constrains the
   allow-list shape.** The launch env is exactly `JOB_ID`, `JOB_PROJECT`,
   `JOB_BRANCH`, `BASE_BRANCH`, `REPO_URL`, `NATS_URL`, `CHANNEL_ROLE`,
   `JOB_TASK_ID`, `CHUG_TASK_ID`, `CHUG_PHASE`, `CHUG_EVALUATOR` plus secrets
   and git/NATS wiring (`Core::container_env`, `crates/dispatcher/src/exec.rs`).
   [#313](./313-workload-identity-image-builds.md) B-IV proposes a node-side
   allow-list of an **"(project, job type)"** pair; the job type half is not
   observable node-side today. A node-side allow-list can be keyed on
   `JOB_PROJECT` and on the requested `image`, and nothing finer, without a wire
   change. This is a finding for #313 as much as for this document.

6. **"nuc is `slots: 2` today" is no longer a redeploy-shaped fact.** #293 has
   landed in the wire types and the API: `set_node_slots` is the sixth provided
   trait method (`crates/container/src/lib.rs`), `SetSlotsRequest`/`SetSlotsOk`
   are on the wire (`crates/types/src/worker.rs`), and
   `platform_fleet_capacity_set` in `crates/api/src/routes.rs` gates it behind
   `platform_admin`. The live slot count of any particular node is runtime state
   this repo does not record, so *(secondhand)* stands for the number — but the
   consequence is verified and it matters: **"pin the device-bound type to a
   1-slot node" is now one authenticated API call**, not a `WORKER_SLOTS`
   redeploy. That materially strengthens the cheap interim #309 §5b endorses,
   and #309's own correction 4 ("the only way to change a node's slot count
   today is to restart its daemon") is stale.

7. **#308 H.4's AVD claim is wrong, and this document records the correction
   without being able to verify it** *(secondhand)*. #308 says the AVD
   "persists across runs and is created only if absent, so in a container that
   check fails every night, so it re-downloads the system image forever." The
   operator's inspection of beacon's flutter-integration-tests workflow
   (2026-08-01) reports the opposite in a comment — `ANDROID_USER_HOME` is
   per-workspace, so each run starts with no AVD configured and creates one on
   the fly with `avdmanager create avd --force`, which is fast and downloads
   nothing because the *system-image SDK package* is already installed on the
   runner. What persists is the **installed SDK**, not the AVD; the `adb
   devices` reuse check is an optimization for a shared host, not a requirement.
   This removes the principal "Android is host-state-dependent" argument, and
   §[3](#3-part-two-the-toolchain-bulk) is built on it. **If it is wrong, the
   toolchain half of this design is wrong and the device half is not** — they
   are independent, which is the point of separating them.

One more thing the brief flags that is worth pinning: `crates/worker/src/config.rs`
line 375 contains the string `"kvm"`. It is a *rejected `WORKER_MODES` value* in
a parse test, not a device reference. A future `grep -rn kvm` will hit it; it
means nothing.

A citation the brief and an earlier draft of this document both got wrong: the
**three clocks of [#308](./308-gha-port.md) are in H.6**, not H.2. `### H.2 What
it buys` runs lines 708–726 there and contains no clocks; *"Three clocks, plus a
fourth thing that has no clock"* is line 843, inside `### H.6 NixOS layering:
where tooling lives` — the same section as the layering rule
§[3.6](#36-the-clock-this-borrows-from-and-who-owns-it) quotes, so that section
cites one #308 section rather than two. The H.2 citations that remain in this
document are about what host mode buys, and are correct.

### The 2026-08-02 measurement (corrections 8–12)

Corrections 8–11 come from the operator's run on `gumbo-nuc-0` and are
*(secondhand)* — see the amendment note above for why they could not be
reproduced here. Correction 12 is read out of this tree and is not secondhand.

8. **The device half is measured, not argued** *(secondhand)*. On the live
   worker node, against the unmodified `chuggernaut/agent:prod` image:

   ```
   docker run --rm --device /dev/kvm -v /nix/store:/nix/store:ro \
     -e ANDROID_SDK_ROOT=/nix/store/3zr1pgwpc00zrj8qc8d631bdfw1z9c5y-androidsdk/libexec/android-sdk \
     chuggernaut/agent:prod  …
   ```

   `/dev/kvm` appeared inside the container as `crw-rw-rw-`, readable and
   writable, with **no `--privileged`**. `emulator -accel-check` exited 0 with
   *"KVM (version 12) is installed and usable."* A `pixel_8` AVD on
   `system-images;android-34;google_apis;x86_64` was created and booted
   headless: `adb devices` → `emulator-5554  device`, `INFO | Boot completed in
   30421 ms`, ~40s wall clock from launch to `sys.boot_completed=1`. Every
   load-bearing claim of §[1](#1-the-question-and-the-hypothesis-under-test)
   and §[2](#2-part-one-the-device) is now an observation rather than an
   inference from Google's docs, and §[8](#8-risks-and-open-questions)'s first
   risk — "the KVM precondition is the whole design, and it is unverified" — is
   **retired for this node**.

9. **A1's bind is wrong, and the reason generalizes** *(secondhand)*. A1
   specifies `WORKER_ANDROID_SDK_DIR` bound as a plain host path. That cannot
   work here, because the SDK is nix-provisioned and is not self-contained:

   - `ANDROID_SDK_ROOT` is
     `/nix/store/3zr1pgw…-androidsdk/libexec/android-sdk`, and the tree is a
     **symlink farm out of itself** — `emulator ->
     /nix/store/j92gsy…-android-sdk-emulator-36.6.11/…`, `ndk-bundle ->
     /nix/store/spgfki…`. Bind only the SDK path and every one of those
     dangles.
   - `emulator` is a **nix wrapper script** whose shebang is
     `#! /nix/store/zh1ijdh…-bash-5.3p9/bin/bash`, and which references
     `libglvnd`, `dbus`, `xkeyboard-config`, `libx11` and `systemd` by store
     path. In a Debian container that interpreter does not exist, so the
     wrapper does not even start.
   - The closure is **17.5 GiB across 197 store paths**; `/nix/store` on the
     box is 56G.

   The shape that works is **`-v /nix/store:/nix/store:ro` plus
   `ANDROID_SDK_ROOT`**. It is the same cost as A1 claimed — one bind,
   node-side, no wire change, no dispatcher change, no epoch bump — from a
   different source, and it exposes a great deal more than A1's curated
   directory did. §[3.4](#34-what-the-nixstore-mount-exposes) decides that
   exposure and §[3.5](#35-staleness-no-store-hash-in-any-chug-side-config)
   decides the hash-pinning problem it creates.

10. **A0 is already done, and one of its two requirements was never real**
    *(secondhand)*. `gumbo-nuc-0`'s NixOS configuration
    (`~/beacon/infra/gha-runner/configuration.nix`, a *different project's*
    repo) already provisions the SDK through
    `androidenv.composeAndroidPackages` with `includeNDK`, `includeEmulator`,
    `includeSystemImages`, `platformVersions ["34" "35" "36"]`, `abiVersions
    ["x86_64"]` and `ndkVersions ["28.2.13676358"]`, and it is present on the
    box. A0's other requirement — "the daemon's user in the `kvm` group" — is
    **moot**: `/dev/kvm` is mode `0666`, `worksalot` is not in the `kvm` group,
    and the container opened the device anyway. Stated here so a future reader
    does not go chasing group membership. The host's *GHA runner* systemd units
    do need `SupplementaryGroups = ["docker" "kvm"]` plus `PrivateDevices =
    false` and `DevicePolicy = "auto"`, because nixpkgs' systemd hardening
    defaults hide `/dev/kvm` — that is a systemd-unit concern for units the
    operator writes, and it does **not** reach the containerized `chug-worker`,
    which is started by `docker run` (`deploy/prod/build-worker.sh`) and has no
    unit of its own.

11. **The image needs nothing** *(secondhand)*. A1 calls for "a slim Android
    task image". The measurement says **no image change at all**: `java` is not
    on `PATH` in `chuggernaut/agent:prod`, and `avdmanager` ran regardless,
    because the nix wrappers resolve their own JDK out of the store. So T2's
    premise moved in the platform's favour — the toolchain-bulk problem of
    §[3](#3-part-two-the-toolchain-bulk) does not shrink, it **disappears**:
    zero bytes are added to any image the platform rebuilds per node per
    deploy. §[3.3](#33-recommendation-t2-now-and-t5-as-the-complement-rather-than-the-rival)
    is strengthened accordingly. **Bounded by
    [correction 14](#the-first-real-a2-run-correction-14)**: the wrapper trick
    covers the SDK tools and stops at gradle, which needs a real `JAVA_HOME` —
    supplied as a third mounted leaf, still not as image bytes. And narrowed by
    [correction 15](#the-second-real-a2-run-correction-15): it is the
    `cmdline-tools` launchers that are wrapped, not the deprecated `tools/bin`
    ones the measurement happened not to run.

12. **The worker daemon cannot see the host filesystem, so A1's "fail loudly if
    the mount is absent" cannot work as written** — and this one is
    tree-verified. `deploy/prod/build-worker.sh` line 133 starts the daemon as
    `docker run -d --restart=always --name chug-worker -v
    /var/run/docker.sock:/var/run/docker.sock -v
    $HOME/chuggernaut-worker/keys:/data/keys:ro …`. Its only host views are the
    docker socket and its own credentials. Node-local caching works anyway
    because the *docker daemon* resolves the bind's source path host-side —
    which the deploy script's own comment states ("the daemon itself never
    touches the cache files", lines 84–89) — though `crates/worker/src/daemon.rs`
    still calls `std::fs::create_dir_all` on `WORKER_CACHE_DIR`, which in the
    deployed shape creates a phantom directory *inside the worker container*
    and never touches the host path being bound. That is harmless for a cache
    that is safe to be empty. It is **not** harmless for a toolchain mount:
    a `-v`-style bind with a missing source is created by the docker engine as
    an empty root-owned directory rather than refused, so a node whose store —
    or whose stable SDK path (§3.5) — is not where the config says would launch
    a container with an empty directory in its place and fail deep inside the
    task instead of at the launch. The fix is in
    §[3.5](#35-staleness-no-store-hash-in-any-chug-side-config); the point here
    is that §[2.3](#23-recommendation)'s bullet — *"the daemon refuses to start
    if the setting is on and the device node is absent"* — is unimplementable
    from inside a container and is corrected there. (The docker `-v`-creates
    versus `--mount`-refuses distinction is engine-documented behavior; there is
    no docker daemon in this container, so it is stated, not demonstrated, and
    unlike correction 3 no `bollard` metadata was present here to check the
    field names against.)

### Building A2 (correction 13)

13. **The A2 entrypoint is `flutter`, not `./gradlew`.** The
    [Phasing](#7-sequencing-what-ships-first-and-what-it-unblocks) table asks A2
    to prove the design against "a boring `./gradlew connectedAndroidTest` on one
    module". That command cannot run against a fresh clone at all, and the
    reason is a property of the stock template rather than of this particular
    fixture: `flutter create` **gitignores** `gradlew`, `gradlew.bat`,
    `gradle-wrapper.jar` and `local.properties`, and
    `fixtures/mobile/android/settings.gradle.kts` hard-requires `flutter.sdk`
    out of `local.properties` (`require(flutterSdkPath != null)`). All four are
    written by the Flutter tool on its first build — which is why job #392's
    `flutter create` produced 88 files and tracked 67 of them. A job container
    starts from a clone, so `./gradlew` there is a missing file, and the wrapper
    it would need is the output of the build it was meant to replace.

    So `.chug/tasks/android-proof.sh` enters through `flutter build apk --debug`
    (rung 4) and the gradle half is proved *by* it: the build generates the
    wrapper and `local.properties` on the way. **This does not weaken the
    original intent** — the intent was to prove the device path against
    something boring rather than against a full integration suite, and a debug
    APK build is that.

    The same finding moves the rung *after* it. The stock skeleton ships no
    `androidTest` sources and no `integration_test` dependency, so
    `connectedAndroidTest` would demonstrate that gradle can run zero tests.
    Rung 5 instead installs rung 4's APK onto the booted emulator with `flutter
    install` and waits, bounded, for the app's own process to appear — adb, the
    device and the built artifact, with nothing bespoke added to a fixture whose
    value is being stock (`fixtures/mobile/README.md`). Adding a trivial
    `integration_test` to the fixture was the alternative and was rejected on
    that ground: it costs a `pubspec.yaml` dependency and a hand-written file in
    a tree that is regenerated rather than edited, to prove the same three
    things.

    What A2 does **not** measure, and the Phasing row asked for: two concurrent
    emulators in the node's memory, and the SIGSEGV of
    §[8](#8-risks-and-open-questions). The proof job is pinned and single, so
    concurrency is not exercised; both remain open questions for whoever unpins
    it (A3).

### The first real A2 run (correction 14)

14. **Correction 11 is right and it stops at gradle.** The measurement behind it
    — `java` is absent from `chuggernaut/agent:prod`'s `PATH` and `avdmanager`
    ran anyway, because the nix wrappers resolve their own JDK out of the
    mounted store — held on the node's first real ladder run (job #396): rungs
    1–3 passed, including the SDK tools running off the read-only mounts. Rung 4
    then failed with `ERROR: JAVA_HOME is not set and no 'java' command could be
    found`. Gradle, which `flutter build apk` invokes, is **not** a nix wrapper:
    it resolves its JDK from `JAVA_HOME` (or `PATH`) and has nothing to fall
    back on. So the wrapper trick covers the SDK tools and nothing beyond them,
    and "the image needs nothing" survives only because the JDK arrives the same
    way the other toolchains do — as a **third read-only leaf**
    (`WORKER_JDK_DIR` ⇒ `/opt/jdk`, `JAVA_HOME`; #397), not as image bytes.

    `JAVA_HOME` alone, deliberately: gradle's launcher prefers it over `PATH`,
    and a container `PATH` set from the daemon **replaces** the image's rather
    than prepending to it, which is a far wider blast radius than one variable
    for a tool that does not need it. The value is the derivation **root**, not
    the nested `lib/openjdk` — both carry `bin/java`, only the nested one carries
    `release`, and the root is what nixpkgs designates and what the node's own
    `configuration.nix` exports for its GitHub runners' Android builds. A rung-4
    failure that complains about the JDK's *layout* rather than its absence is
    the signal to revisit that choice.

    Three leaves is also the shape's own argument against itself: one symlink,
    one setting and one mount per tool scales linearly with the toolchain, which
    is precisely what [#373](373-project-toolchains.md) P2's project-declared
    toolchains exist to replace. Three is affordable; the trend is the finding.

### The second real A2 run (correction 15)

15. **`avdmanager` is two binaries and only one of them is a wrapper**
    *(secondhand)*. The wrapper property corrections 11 and 14 lean on belongs
    to `$ANDROID_SDK_ROOT/cmdline-tools/<version>/bin`, which nixpkgs wraps to
    prepend its own JDK to `PATH`. It does **not** belong to
    `$ANDROID_SDK_ROOT/tools/bin`, the deprecated launcher: that is a plain
    444-byte script which honours `JAVA_HOME` and then runs whatever JDK it
    names, dying with `NoClassDefFoundError:
    javax/xml/bind/annotation/XmlSchema` on a JDK with no JAXB. A 2×2 measured
    in a real container on `gumbo-nuc-0`, with the real mounts and
    `chuggernaut/agent:prod`:

    | | `tools/bin/avdmanager` | `cmdline-tools/13.0/bin/avdmanager` |
    | --- | --- | --- |
    | `JAVA_HOME` unset | OK | OK |
    | `JAVA_HOME=/opt/jdk` | **FAIL** | OK |

    So the rung-3 failure of the second ladder run (job #398) was **latent from
    the start rather than caused by the JDK mount**: run #396 cleared rung 3
    only because `JAVA_HOME` was unset then, and correction 14's third leaf
    merely exposed it. That is the difference between a revert and a fix, and
    the fix is the resolution order — `.chug/tasks/android-proof.sh` takes the
    launcher from `cmdline-tools/*/bin` first, **globbed** rather than assuming
    the `latest` symlink this SDK does not ship, falls back to `tools/bin` only
    with a shout, and fails rung 3 outright when the SDK holds neither. A silent
    fallback to a binary that dies three lines later is what made this cost two
    runs.

---

## 1. The question, and the hypothesis under test

> **Do Android emulator jobs need host-native execution, or are they an ordinary
> Linux container with `/dev/kvm` passed through?**

The hypothesis the brief asks to test first:

> Android is (a) `/dev/kvm` device passthrough into an ordinary Linux container
> and (b) a node-local Android SDK, mounted the way sccache already is. Neither
> needs the host backend, the `runtime:` selector, or the epoch bump.

**Verdict: confirmed, and more strongly than stated.** Containerization does not
merely avoid the host backend — it *dissolves* the exclusivity primitive #308
H.5 introduced on Android's behalf (§[4](#4-exclusivity-and-why-android-does-not-need-a-lease)).
The one thing Android does share with iOS is capability-aware placement, and
only once the fleet has a second KVM node (§[5](#5-the-platform-contradiction-and-how-a-node-advertises-kvm)).

The claim is narrow and worth stating in its negative form too. This does not
say host-native execution is unnecessary — iOS genuinely needs it, for the
reason #308 §F gives and no container cleverness touches: `xcrun simctl` needs a
macOS host. It says **the two legs of category F have different shapes and the
Android one is cheaper by roughly an order of magnitude**, and the corpus
currently prices them as one.

### What #308 got wrong, precisely

Design #308 H.2 lists **"`/dev/kvm` for the Android emulator"** as one of the
things *host mode buys*. That is the error, and it is a single-bullet error with a
large consequence: `/dev/kvm` is available to an ordinary container. Google's own
`android-emulator-container-scripts` documents the launch as

```
docker run -e ADBKEY="$(cat ~/.android/adbkey)" --device /dev/kvm \
  --publish 8554:8554/tcp --publish 5555:5555/tcp <image>
```

— `--device /dev/kvm`, **not** `--privileged` (fetched 2026-08-01; the docs
state "KVM must be enabled on your host … bare-metal Linux or inside cloud
Virtual Machines with nested virtualization enabled" and that "Docker Desktop on
macOS and Windows is not supported for KVM acceleration"). That distinction is
load-bearing for a fleet that is deliberately bind-mount-free: a single named
device node is a far narrower grant than `--privileged`, and it is
representable in the launch path the platform already has.

The industry moved the same way. GitHub's 2024-04-02 changelog announces
hardware-accelerated Android virtualization on **Linux** runners (extending it
to 2-vCPU runners; Linux is the only OS named), and
`ReactiveCircus/android-emulator-runner` now reads "It is now recommended to use
the **Ubuntu** (`ubuntu-latest`) runners which are 2-3 times faster than the
**macOS** ones which are also a lot more expensive." Both fetched 2026-08-01.
*(secondhand)* beacon's own Android leg reportedly already runs
`runs-on: ["self-hosted","linux","x64","gumbo"]` with `ubuntu-latest` as the
cloud fallback; only the iOS leg is macOS.

So the honest reading of #308 §F is: **"impossible under containers — not hard,
impossible"** is true of Xcode and false of the Android emulator. #308's own
sentence names Xcode and `xcrun simctl` and then generalizes to "mobile". The
generalization is the bug.

---

## 2. Part one: the device

Design #308 conflates two problems that have nothing to do with each other, and
the whole design follows from separating them:

| | Problem | Nature | Settled by |
| --- | --- | --- | --- |
| **1** | **Device access** — the container needs `/dev/kvm` | Narrow, named, one struct field | Evidence above; §[2](#2-part-one-the-device) |
| **2** | **Toolchain bulk** — SDK + NDK + system images, many GB | Storage and provisioning; nothing to do with devices | §[3](#3-part-two-the-toolchain-bulk) |

### 2.1 Is `/dev/kvm` the same class as the docker socket?

[#309 §10](./309-host-native-execution.md#10-trust-and-tenancy) sets the rule the
brief asks about: a job type that needs a docker socket "is a node-side
allow-list entry, never a job-type field the platform honors on request", and
[#313](./313-workload-identity-image-builds.md) B-IV's recommendation is
explicitly praised for "resolv[ing] the contradiction by *complying* with the
rule rather than carving it out."

**Same mechanism, different rule.** The two are worth distinguishing carefully,
because the distinction decides the *end state*, not the first phase.

| | Docker socket | `/dev/kvm` |
| --- | --- | --- |
| What it grants | `POST /containers/create` with a bind ⇒ **root on the node**, and read access to every other project's containers (#313 correction 5) | `KVM_CREATE_VM` and friends ⇒ a guest VM the caller already could have run in software, slowly |
| Blast radius on success | the node's whole container fleet, i.e. the platform's own execution substrate | the container's own process tree, plus whatever a guest→host KVM escape reaches |
| Residual risk | structural — the capability *is* root-equivalence | a real kernel CVE class (guest escapes exist), but not a capability grant |
| Should it ever be a job-type field? | **No, permanently.** #309 §10 is right and it is not a phasing statement | **Yes, eventually** — it is a legitimate declarable requirement once more than one node has it |

So `/dev/kvm` is **not** in the docker socket's class, and this document should
not pretend it is in order to borrow the rule's authority. It is in the class of
`resources.cpu`: a physical capacity a job legitimately requires, that the node
legitimately asserts, and that the scheduler legitimately matches on.

The reason to nonetheless start node-side is different and purely economic: with
**one** KVM node in the fleet, `placement.node` already routes the work, a
job-type field buys nothing, and adding one to `Placement` — which carries
`#[serde(deny_unknown_fields)]` (`crates/types/src/job_type.rs`) — forces a
`CONFIG_SCHEMA_EPOCH` bump on its own, exactly as #309 §5b shows for
`placement.leases`. That is the whole cost argument and it is a phasing
argument, not a principle.

The honest cost of that "physical capacity" framing must be stated too:
`/dev/kvm` is not free of security consequence. Guest-to-host escapes through
KVM are a recurring CVE class, and a container holding `/dev/kvm` on a shared
node is a materially larger kernel attack surface than one without it. The
mitigation is the one the fleet already uses for the docker socket — **grant it
narrowly** (§2.3), and keep the KVM node's kernel current as a machine fact
under #308 H.6's "system closure" row.

Correction 8 settles the mechanics of the grant: `--device /dev/kvm` alone was
sufficient, `--privileged` was not needed, and because the device is mode
`0666` there is no group, no `device_cgroup_rules` entry and no uid mapping to
arrange. The narrow grant is available at its documented price.

### 2.2 Options for the device primitive

**D1 — node-side, unconditional: every container on a KVM-enabled node gets the
device.** Exactly the `WORKER_CACHE_DIR` shape: a `WORKER_KVM_DEVICE` (or a
bare boolean) parsed beside `parse_cache_dir`, threaded into `DockerBackend` by
a `with_devices`-style builder beside `with_cache_dir`, and populated in
`build_host_config`.

*For:* Zero wire change, zero schema change, zero dispatcher change, zero epoch
bump. It is the mechanism `docs/spec.md` §3.1 already blesses ("a node property added
worker-side, not a launch input"), and #313 B-IV's "costs the dispatcher
nothing" applies verbatim. One unit test on the produced `HostConfig`, beside
`host_config_with_cache_adds_one_bind`.

*Against:* It widens the kernel attack surface for **every** job on that node,
including ordinary `code` jobs that will never touch an emulator. That is a real
cost and it is the reason not to recommend this one unmodified.

**D2 — node-side, allow-listed (recommended).** As D1, but the device is added
only for launches whose `JOB_PROJECT` (and, optionally, requested `image`)
appears in a node-side allow-list — `WORKER_KVM_PROJECTS`, mirroring #309 §10's
`WORKER_HOST_PROJECTS` and #313 B-IV's per-`(project, …)` proxy binding. Both
keys are already observable node-side (correction 5), and neither costs the wire
anything.

*For:* Same zero-cost profile as D1. Narrows the grant to the project that asked
for it, so an unrelated `code` job on the same node runs exactly as it does
today. Fails closed: an unset allow-list means no container gets the device, so
enabling this on a node is an explicit act.

*Against:* An allow-list is operator-typed config about a project, which is the
shape #309 §4 rejects for *physical facts*. The distinction holds — "does this
node have KVM" is physical and belongs on the node; "may this project use it" is
policy and belongs in operator config — but the two live in the same env var and
a reader should not confuse them. Keep them as **two** settings for that reason:
one that says the device exists, one that says who may have it.

**D3 — a job-type field, `placement.devices: [kvm]` (or `resources.devices`),
carried on `ContainerLaunchConfig` and `WorkerLaunchRequest`.**

*For:* It is the honest end state. The requirement lives in the project repo
where #308 H.6 and `CLAUDE.md`'s per-consumer-forge principle want it; it is
reviewed through the merge gate like every other job-type change; and it
degrades correctly when a second KVM node appears, because it becomes a
placement predicate rather than a pin.

*Against, and this is decisive for phase one:* it costs a
`CONFIG_SCHEMA_EPOCH` bump (`deny_unknown_fields` on the nested block hard-rejects
an N−1 dispatcher, and the §14.2 park is the *correct* behavior for a
constraint whose silent loss is a wrong placement — the same argument #309 §5b
makes for `leases`). It costs two wire records. And with one KVM node it changes
no placement decision that `placement.node` does not already make. Every one of
those costs is real today; every one of the benefits arrives with the second
node.

**D4 — `--privileged`.** Named only to reject it. It is not what Google
documents, it is not what the GHA runners do, and it hands the container the
node. No.

### 2.3 Recommendation

**Take D2 now, with D3 named as the successor and a stated trigger.**

- The node declares the physical fact: `WORKER_KVM=1` (or a device path,
  defaulting to `/dev/kvm`), parsed in `crates/worker/src/config.rs` beside
  `parse_cache_dir`. **The failure must still be loud, but it cannot be a
  startup `stat`** (correction 12): the daemon runs in a container that cannot
  see the host's `/dev/kvm`. A `--device` whose host path is missing is refused
  by the engine at container create, so the loud failure lands on the launch as
  a `BackendError::Launch` naming the device — one launch per affected job
  rather than one refusal at boot, which is weaker than `build_backend`'s
  `WORKER_MODES` check and is the honest price of a containerized daemon. If a
  boot-time check is wanted later, the mechanism is a one-shot probe container,
  not a `stat`.
- The operator declares the policy: `WORKER_KVM_PROJECTS=owner/project,…`,
  empty ⇒ nobody, checked against the launch's `JOB_PROJECT`. Per
  §[3.4](#34-what-the-nixstore-mount-exposes) this one list gates the device
  **and** the store mount: they are granted together or not at all, decided at
  one site, so no future edit can hand out the wider of the two on its own.
- `DockerBackend` gains a device list exactly as it has `cache_dir` — a node
  property, `None` on the dispatcher's construction, never on the wire or in
  `ContainerLaunchConfig`. `build_host_config` populates `HostConfig.devices`
  with one `DeviceMapping { path_on_host: "/dev/kvm", path_in_container:
  "/dev/kvm", cgroup_permissions: "rwm" }`.
- **The switch to D3 fires when a second node has KVM.** At that point the pin
  stops expressing the requirement and starts constraining it, and the epoch
  bump buys something. Until then it buys an epoch.

Explicitly *not* recommended: adding a device field to `ContainerLaunchConfig`
without the schema field. A launch-config field with no way for a job type to
set it is a field the dispatcher would populate from something — and the only
somethings available are a node name (which is the pin, already there) or an
input (forbidden by #311 Decision 1, re-affirmed at length by
[#361](./361-per-run-placement.md)).

---

## 3. Part two: the toolchain bulk

The operator's constraint, and it is a requirement rather than a preference:

> **Do not bake the Android SDK / NDK / system images into a task image.**

### 3.1 One premise of that constraint does not survive contact with the tree

The stated reason is "many GB, **pulled per task**." That is not how this fleet
works, and saying so is the honest thing to do before designing around it:

- **There is no pull path at all.** [#355](./355-project-task-images.md) fact 3
  verified it and it still holds: `grep -rn "create_image\|docker pull"
  crates/` finds only a test fixture, and `DockerBackend::launch`
  (`crates/container/src/docker.rs`) calls `create_container` directly with
  `image: Some(config.image.clone())` and no preceding fetch. Images are built
  **on each node** by `deploy/prod/worker-refresh.sh` and consumed locally.
- A container start does not copy the image. It stacks a writable overlay over
  shared read-only layers. A 20 GB image costs 20 GB of node disk **once**, not
  per task.

So the "pulled per task" framing is wrong for this platform. **The constraint
still stands, on two grounds that are real:**

1. **Disk.** `deploy/prod/worker-refresh.sh` already refuses to build below a
   free-disk floor and says so loudly; #355 §7 calls that floor "re-derived four
   times across real incidents." An Android SDK image on the same partition as
   `/nix/store`-free but image-heavy node storage is a live risk, not a
   theoretical one.
2. **Rebuild time on every refresh.** The platform's three images rebuild **on
   every node on every deploy** (`build-worker.sh`, `worker-refresh.sh`). A
   multi-GB Android layer would be paid by every deploy on every node, and
   `agent-rust`'s leg already ran 673s on one node before #352.

Both arguments point the same way as the operator's, so the recommendation is
unchanged — but a design that repeats a false premise is a design a future
reader will over-apply. The correct statement is: *do not put the Android SDK
in an image the platform rebuilds on every deploy.*

**And per correction 11, the amended answer is stronger than the constraint
asked for.** The constraint says "do not bake it"; the measurement says nothing
needs to be added to any image *at all* — not the SDK, not a JDK, not the
emulator's shared libraries — because the nix wrappers carry their own
interpreter, their own JDK and their own libraries by store path, and the store
is the mount. `chuggernaut/agent:prod` booted an emulator unmodified. So the
size of this section's problem is not "how few GB can we get the Android image
down to"; it is **zero GB, and the whole question moves to what the mount
exposes** (§[3.4](#34-what-the-nixstore-mount-exposes)).

### 3.2 Options

**T1 — bake it into a platform image.** Rejected on §3.1's two real grounds.

**T2 — a second named node-local mount, read-only (recommended, and rewritten
by correction 9).** As originally written: `WORKER_ANDROID_SDK_DIR` on the node,
bind-mounted read-only at `/opt/android-sdk`, with
`ANDROID_SDK_ROOT`/`ANDROID_HOME` injected worker-side exactly as
`inject_cache_env` injects `SCCACHE_DIR` (`crates/worker/src/daemon.rs`).

**That version is dead.** The SDK on the target node is nix-provisioned, so it
is not a directory — it is a *view into a store*, and binding the view without
the store yields dangling symlinks and a wrapper whose interpreter is missing
(correction 9). The surviving form of T2, and the one the rest of this section
means, is:

> **`/nix/store` bind-mounted read-only at `/nix/store`, plus a resolved
> `ANDROID_SDK_ROOT`.** Node-side, read-only, injected beside
> `inject_cache_env`, provisioned by the operator's `nixos-rebuild` — still
> exactly #308 H.6's "system closure = machine facts" row, and already done on
> the target node (correction 10).

The mechanism's *cost profile* is unchanged — one mount, or two under
§[3.5](#35-staleness-no-store-hash-in-any-chug-side-config)'s preferred
resolution mechanism; no wire change, no dispatcher change, no epoch bump. Its
*exposure* is not, and §[3.4](#34-what-the-nixstore-mount-exposes) owes an
argument rather than a shrug.

**T3 — generalize `WORKER_CACHE_DIR` into a list of named node mounts.**
`WORKER_MOUNTS=sccache:/cache/sccache:rw,nix-store:/nix/store:ro`, one
mechanism, still node-side.

**T4 — #309 P5 declared caches.** The flake-attribute-derived per-project cache
set. It is the right general answer and it is host-mode machinery: #309 §9
scopes it to `WORKER_HOST_CACHE_ROOT` and derives the set from `runtime.env`,
which container mode does not have.

**T5 — a project task image under [#355](./355-project-task-images.md) with the
SDK baked in.** Under #355's recommended O2 the node *builds* the image locally
and never pulls it, and it is rebuilt on the **project's** clock, not on every
platform deploy — which neutralizes §3.1's second ground entirely. **Correction
11 changes what this option is for.** It is no longer needed to hold the
toolchain, because nothing needs to hold the toolchain; what it would still buy
is *ownership* — an image the project builds from its own flake, on its own
clock, instead of borrowing the node's system closure. That is the tension
§[3.6](#36-the-clock-this-borrows-from-and-who-owns-it) names and does not
resolve.

### 3.3 Recommendation: T2 now, and T5 as the complement rather than the rival

**Take T2**, and note T3 as the refactor to do when a *third* mount appears —
not before. Two mounts do not justify a list; docs/reference/style.md's simplicity-over-
generality principle applies, and #309 §9 is explicit that `WORKER_CACHE_DIR`
"should not be overloaded — it keeps its documented contract for container mode
unchanged."

**The amendment strengthens this, and the strengthening is worth stating
plainly.** With the store as the mount, the recommendation costs the platform
*nothing to build*: no Android layer in any platform image, no project image
required to carry a toolchain, no `sdkmanager` provisioning step to write and
maintain, and — per correction 10 — no A0 work outstanding on the target node.
The mount and the SDK are both already there; what is missing is the six lines
of worker code that use them. That is a materially better position than the
original T2 argued for, and it removes the only reason this section needed T5.

**Read-only is the load-bearing property, and it is what makes the whole design
work.** `docs/spec.md` §3.1 permits the one cache bind because it "carries **no job
state** — it is a build accelerator only, safe to be empty/cold", and
concurrency is safe because *sccache locks*. A nix store satisfies neither of
those the way sccache does — it is not safe to be empty, and its absence is a
hard failure rather than a slow build — so it needs a different justification,
and read-only is it:

- **Concurrency safety by construction.** Two emulator tasks on one node cannot
  corrupt a mount neither can write. sccache's justification (it locks) does not
  transfer; this one does not need it.
- **It forces the mutable state into the container's own writable layer**, which
  is where correction 7 says beacon already puts it: `ANDROID_USER_HOME` and
  `ANDROID_AVD_HOME` point into the container, the AVD is created per run with
  `avdmanager create avd --force`, and it dies with the overlay. No shared AVD,
  no shared adb server, no reuse check.
- **It is not a cache and should not be called one.** It is a read-only
  toolchain volume. Calling it a cache invites someone to make it writable.
- **And with a nix store it is not merely a policy — it is the store's own
  contract.** `/nix/store` is root-owned and immutable by design, which is the
  same property [#309 §10](./309-host-native-execution.md#10-trust-and-tenancy)
  already leans on when it lists the store among what a *host* task reaches and
  says the immutability "genuinely bounds this". A `:ro` bind adds a second
  lock to a door that was already locked.

**Correction 8 answers the measurement this section was most exposed on.** §4
and §8 both flagged "a read-only SDK mount may not survive contact with
`sdkmanager`" as the one result that could change the shape. It survived: the
AVD was created and the emulator booted with the SDK reachable only through a
read-only store, and nothing in the boot path needed to write into the SDK
tree. Two residuals, not one, remain — license-acceptance files, which
`androidenv` settles at build time rather than at run time, and any flow that
genuinely calls `sdkmanager` to *install* a package, which under this design is
a `nixos-rebuild`, not a task action. Both are narrower than the original risk
and neither reopens the lease question.

**Cold-start is the honest cost, and it is charged to the operator, not to
`task_timeout`.** #309 §9's "cold-realise cost" analysis applies here and
reaches the same answer: the first-run cost must be moved out of band. A
read-only mount cannot be filled by the first task, which is a feature — it
means the failure mode is "the node does not have the SDK" (loud, at launch)
rather than "the first task of the day takes forty minutes and looks slow"
(#309 §9's exact complaint about a cold `nix develop`). The node must therefore
**fail the launch loudly** when a KVM-and-SDK job lands and the mount is absent,
never fall through to an ambient SDK — and correction 12 says that loudness has
to be built rather than assumed, because a `-v`-style bind on a missing source
is created empty by the engine instead of refused.
§[3.5](#35-staleness-no-store-hash-in-any-chug-side-config) carries the
mechanism. On the target node the cold-start cost is already paid: the closure
is in the system profile, and `nixos-rebuild` paid for it out of band exactly
as this paragraph asks.

**T5 is complementary, not alternative** — and the split it completes is now
one layer emptier:

| Layer | Holds | Size | Clock | Mechanism |
| --- | --- | --- | --- | --- |
| Task image | `git`/`ssh`, the agent CLI — **nothing Android-specific** (correction 11) | unchanged | project repo (#355) or platform | `image:` |
| Node mount | the whole nix store, holding SDK + system images + NDK + their interpreters and libraries | 56G present, 17.5 GiB used by this closure | operator, `nixos-rebuild` | `/nix/store:ro` |
| Container overlay | the AVD, `ANDROID_USER_HOME`, gradle output | per task | the task | nothing — it is the overlay |

The original row-1 entry (JDK, emulator runtime deps) is struck: correction 11
measured `java` absent from `PATH` in `chuggernaut/agent:prod` and `avdmanager`
running regardless. A **stock** platform image plus a mounted store is the
split, and it is the one design #355 §9 already implies without saying so:
`image:` and the node-resolved environment reference are "the same slot in two
modes", and a node mount is neither — it is a third thing, the machine fact.

### 3.4 What the `/nix/store` mount exposes

The mount is one flag and it should not be allowed to arrive as one. `-v
/nix/store:/nix/store:ro` hands the job container **every package on the node**,
on a box that also hosts another project's GitHub Actions runners (job #265,
worker-node co-tenancy — a job, not a design doc, so there is nothing to link).
That is a far wider read surface than the curated
`WORKER_ANDROID_SDK_DIR` the original A1 imagined, and the difference deserves
an argument.

**What is actually reachable, stated precisely.** A nix store path is
world-readable *by design* — that is not an accident of permissions, it is how
the store works, and it is why "no secrets in the store" is a first-order nix
rule rather than a style preference. So the mount grants:

- every derivation output ever realised on the node and not yet
  garbage-collected — the GHA runner's toolchains, and any project source that
  was ever copied into the store by a `nix build .#` or `nix develop` of a
  local flake;
- nothing writable, and nothing outside `/nix/store` — not `/etc`, not the
  runner's work directories, not its credentials, not the docker socket;
- no *node* privilege: the store is data, and the container reading it is the
  same container that already runs arbitrary project code.

**The one thing that would make this unacceptable is a secret in the store**, so
it must be checked rather than assumed. NixOS's own idioms keep secrets out —
`*PasswordFile`/`*TokenFile` options, `sops-nix`/`agenix`, systemd credentials
— but a literal string in `configuration.nix` that reaches `pkgs.writeText` or
a `substituteInPlace` lands in the store in cleartext, and a GHA runner
registration token is exactly the kind of value that gets written that way. This
document cannot check it (the node is unreachable from here), so it becomes an
**A0 acceptance gate**: the operator greps the store for the runner's
registration token and any known secret material, records the result, and the
mount does not ship until that check is on the record.

Options weighed:

**E1 — mount the whole store, read-only (recommended, with conditions).**
One bind, zero moving parts, and the exposure is a read of world-readable data
on a node that is already policy-single-tenant for the project that gets the
device (D2's `WORKER_KVM_PROJECTS`, §2.3). Its strongest defence is that it is
**strictly less than the platform has already accepted elsewhere**:
[#309 §10](./309-host-native-execution.md#10-trust-and-tenancy)'s tenancy table
lists `/nix/store` among what a host task reaches, calls the store's
root-owned immutability a genuine bound, and accepts it — while host mode also
hands over the task user's home, every declared cache and the node's process
table. This mount is that row and none of the others, minus write access.
*Against:* it is still a widening, and it widens for a *container*, which is the
isolation model the platform otherwise sells as the strict one. A reader who
skips this section will read "one bind" and not know that.

**E2 — mount only the 197-path closure.** Compute the closure of the SDK and
bind each store path individually. *For:* the exposure becomes exactly the
toolchain and nothing else. *Against, and it is decisive:* it is **197 binds**
per container, recomputed per launch (the closure changes on every SDK bump), so
the worker would have to shell out to `nix path-info -r` at launch — a new
runtime dependency on nix *inside the containerized worker*, which per
correction 12 cannot even see the store. And it is fragile in the worst way:
any path the toolchain resolves lazily and the closure walk missed fails at
runtime as a bare `ENOENT` deep inside a test run. Trading a wide read for a
launch-time nix dependency and a new class of confusing failure is a bad trade.

**E3 — bake the closure into an image.** `nix copy --to docker://` or
`dockerTools`, producing an image that carries the 17.5 GiB closure. *For:* no
host mount at all; §3.1's rules apply unchanged. *Against:* it re-acquires
everything §3.1 rejects — the disk, and (unless it is a #355 project image) the
per-deploy rebuild — to buy an isolation property the read-only store already
has. **Named as the escape hatch** if the exposure argument is refused, and as
the thing #355 would make cheap.

**E4 — a chroot store holding only the Android closure.** Nix can realise a
closure into a store rooted elsewhere (`--store` with a `root=`), giving
`/var/lib/chug/android-store/nix/store` containing the 197 paths and nothing
else; bind *that* at `/nix/store` and the container sees store-path-correct
absolute paths for the toolchain and nothing about the other tenant. *For:* E2's
isolation with E1's single bind. *Against:* it duplicates 17.5 GiB on a disk the
`worker-refresh.sh` free-space floor already guards, and it needs an operator
step that must be re-run on every SDK bump — a second thing to keep in step with
`nixos-rebuild`, which is precisely the drift §3.5 is about to reject. The exact
invocation is also unverified from this container (no nix here).

**Recommendation: E1, read-only, with four conditions on the record.**

1. The mount is added **only for launches the D2 allow-list already admits** —
   `WORKER_KVM_PROJECTS` gates the device and the store together, one policy,
   one decision site. An unrelated `code` job on the same node sees neither.
2. It is **read-only, always**, and the code says so structurally rather than by
   convention (a mount type that cannot be constructed writable, not a `:ro`
   suffix a future edit can drop).
3. The **A0 secret check above is a gate**, not a note.
4. The node with the mount is **declared effectively single-tenant with respect
   to store contents**, in the same words #309 §10 uses for host nodes: the
   accepted risk is that a project's code can read what else was built on its
   node, and making that explicit and policy-checked is the honest version of
   accepting it. If that node ever hosts a second *chuggernaut* project, this
   decision is reopened, not inherited.

**E4 is the named escalation** for that last case, and E3 for the case where
the store may not be exposed at all.

### 3.5 Staleness: no store hash in any chug-side config

`3zr1pgwpc00zrj8qc8d631bdfw1z9c5y-androidsdk` is content-hashed: **any** SDK
bump changes it. A `WORKER_ANDROID_SDK_DIR` or `ANDROID_SDK_ROOT` holding that
string is operator-typed config that goes silently wrong on the next
`nixos-rebuild` — verbatim the failure
[#309 §4](./309-host-native-execution.md#4-capability-advertisement) cites when
it rejects dispatcher-side static config: *"it relocates a physical fact into
operator-typed config that goes silently wrong after a `nixos-rebuild`."* This
document does not get to reject that shape in §5 and adopt it in §3, so the
resolution here is the same one: **the physical fact is read from the node's
filesystem at the moment it is used, and no content hash appears in any
chug-side configuration, ever.**

The failure is worth spelling out because it is quiet in a specific way. After a
bump the old store path stays valid until garbage collection, so a pinned
`ANDROID_SDK_ROOT` keeps working — against the *previous* SDK — and the job goes
on passing while testing something the operator thinks was replaced. Then
`nix-collect-garbage` removes it and the job fails with `ENOENT` on a path
nobody typed recently. Silent wrongness followed by an unattributable failure is
the worst available shape.

Options:

- **S1 — pin the store path in `WORKER_ANDROID_SDK_DIR`.** Cheapest, and what
  A1 as written implies. **Rejected** on the paragraph above.
- **S2 — the worker reads the host's `ANDROID_SDK_ROOT` at launch.** Reads
  nicely and **cannot work**: the daemon is a container (correction 12) whose
  environment is whatever `deploy/prod/build-worker.sh` passed it, so "the
  host's environment" is not a thing it can see. It degenerates into S1 with a
  redeploy attached.
- **S3 — a stable path the operator's NixOS config maintains, resolved at use
  (recommended).** `configuration.nix` gains one `environment.etc` entry (or
  equivalent) pointing a fixed path — say `/etc/chug/android-sdk` — at the
  current `androidsdk` output. `nixos-rebuild switch` updates it atomically as
  part of the same activation that changes the store path, so the two can never
  disagree. Chug-side config then contains a **fixed string and no hash**.
- **S4 — the worker globs `/nix/store/*-androidsdk`.** **Rejected:** multiple
  generations coexist by design, the glob is unordered, and a wrong pick is
  invisible.

**Take S3.** Two mechanisms deliver it and the choice between them is a
measurement, not an argument:

- **(a) Let the docker engine resolve it — a second, tiny bind.** The container
  gets **two** mounts: `/nix/store:/nix/store:ro` (which the wrapper's
  store-path shebang and the closure both require, and which no amount of
  cleverness removes) and the stable path bound at a fixed container path,
  `/etc/chug/android-sdk` → `/opt/android-sdk:ro`. The engine resolves the
  symlink host-side at container create, so the *node's current* SDK is what
  lands there, freshly, on every launch — and `ANDROID_SDK_ROOT` becomes the
  literal constant `/opt/android-sdk/libexec/android-sdk`, with no hash and no
  resolution logic anywhere in this repo. Declaring that second mount as a
  `--mount`-style bind (missing source ⇒ refused) rather than a `-v`-style one
  (missing source ⇒ silently created empty) also discharges correction 12's
  loudness problem at the same time. Costs the worker nothing: it never needs to
  see the path, exactly as it never sees `WORKER_CACHE_DIR`'s. **One divergence
  from the measurement, stated because this is the section that leans hardest on
  it:** correction 8's run set `ANDROID_SDK_ROOT` to the store path itself,
  whereas (a) points it at a bind path *outside* `/nix/store`. That should be
  inert — the wrappers name their interpreter and libraries by absolute store
  path, and the store is mounted — but nothing measured it.
- **(b) Let the worker canonicalize it — one bind, one host view.** The daemon
  reads the symlink at each launch and injects the concrete store path as
  `ANDROID_SDK_ROOT`, so only `/nix/store` is mounted. Needs `/etc/chug`
  bind-mounted into the *worker* container, i.e. an edit to
  `deploy/prod/build-worker.sh` and a matching carry-forward in
  `deploy/prod/worker-refresh.sh`'s swap — the two places every worker knob
  already has to be threaded through.

**(a) is preferred** because it adds no host view to the daemon and puts the
resolution in the one process that is already resolving host paths; (b) is
preferred by anyone who would rather pay a deploy-script edit than a second
mount. A1 confirms **two** things before writing the config parse — that the
engine resolves the symlink host-side, and that `emulator` and `avdmanager`
behave identically with a non-store `ANDROID_SDK_ROOT` — and if either fails,
(b) is the fallback with no other consequence, because (b) injects the concrete
store path and so reproduces the measured value verbatim. The *contract* is
identical either way. That contract is the part that must not
be negotiated: **the node resolves; chug config names a stable path; a content
hash never enters `WORKER_*` or a job type.**

Note what this does *not* do: it does not put the resolution in the dispatcher,
in a job type, or in an input. The SDK's identity stays a physical fact of the
node, observed where it is used — which is the same answer
§[5.1](#51-resolution-the-defaults-table-is-right-the-doc-comment-is-wrong)
gives for capabilities and #309 §4 gives for modes. One class of problem, one
resolution mechanism, as the brief requires.

The rule generalizes past Android, and should be stated that way in the runbook:
any nix-provisioned node toolchain this platform consumes is referenced through
an activation-maintained stable path, never through a store path.

### 3.6 The clock this borrows from, and who owns it

Taking the SDK from the **node's system closure** means #308 H.6's **clock 1**
(operator, `nixos-rebuild`, needs a drain) is supplying what H.6 assigns to
**clock 3** (project repo, `git push`, no deploy at all). The same section is
explicit about this: *"Resist putting per-project tooling in the node's NixOS configuration …
makes the node a **central control plane** — which CLAUDE.md rejects outright."*
By that rule, provisioning `platformVersions ["34" "35" "36"]` on the node is
the wrong home for a fact that belongs in the project's `flake.nix`, and a
`compileSdk` bump becomes an operator ticket and a drain instead of a commit.

This document **names the tension and leaves it**, deliberately:

- The alternative — project-supplied toolchains, a flake per project, and the
  container-mode analogue of #309's `runtime.env` — is a design in its own
  right, and a separate job owns it. Reaching for it here would make an
  Android-leg document the venue for the platform's toolchain-ownership
  decision.
- Everything §[7](#7-sequencing-what-ships-first-and-what-it-unblocks) proposes
  survives that decision. `/nix/store:ro` plus a resolved stable path is the
  mechanism whether the store was filled by the node's closure (clock 1) or by
  a project flake realised on the node (clock 3); only *who ran the build*
  changes, and no chug-side config records the answer either way.
- The interim is honest as long as it is written down: **this is a borrowed
  clock, on one node, for one project.** A second Android project on a
  different node is the trigger to stop borrowing.

Cross-references for whoever picks it up: #308 H.6 (the three clocks and the
layering rule this bends), [#355](./355-project-task-images.md) (the
project-clock mechanism that already exists for images), and
[#309 §9](./309-host-native-execution.md#9-environment-and-state) (declared
environments and their GC).

---

## 4. Exclusivity, and why Android does not need a lease

[#308 H.5](./308-gha-port.md) introduces the device-lease primitive on Android's
behalf: "a 2-slot host node will happily run two tasks that collide on
`beacon-emu`", and the workflow's `concurrency` group with
`cancel-in-progress: false` exists "because two runs fight over the same
simulator and AVD." #309 §5b designs the lease; #361 verifies it and amends its
reasoning.

**In container mode the collision does not exist.** Walk the shared resources:

| Shared thing | Container mode | Needs a lease? |
| --- | --- | --- |
| The AVD (`beacon-emu`) | per-container `ANDROID_USER_HOME`, created per run, dies with the overlay (correction 7) | **No** |
| The adb server / ports 5554-5555 | container-local: `build_host_config` sets `nano_cpus`, `memory`, `binds` and nothing else, so Docker's default bridge network namespace applies and each container has its own | **No** |
| The SDK / system images | read-only `/nix/store` mount (§3.3), immutable by the store's own contract | **No** |
| `/dev/kvm` | KVM multiplexes VMs by design; a host runs many guests | **No** |
| CPU and RAM | `resources.cpu` / `resources.memory`, enforced today via `nano_cpus`/`memory` | **No — this is the existing mechanism** |

So the emulator's real constraint is **memory**, and memory is a declared
resource the platform already enforces. The 1-slot pin #308 H.5 offers as a
"cheap interim" is not needed as an exclusivity device at all; where it is still
wanted it is a *capacity* decision, and per correction 6 it is now one
`platform_admin` API call rather than a redeploy.

**This is a finding for #309 §5b and #308 H.5, and it should be recorded there.**
Not that the lease primitive is wrong — [#322 §5](./322-macos-native-runtime.md)
gives it a genuine motivating case (`xcrun simctl` mutates machine-global state,
`xcode-select` more so, and #322 recommends `slots: 1` on a Mac until per-task
device sets are proven) — but that **half its stated motivation evaporates.**
Design #309 §5b's own trigger condition ("when the host node must run a second,
non-device-bound task concurrently") is a host-mode condition, and Android in a
container never reaches it. The gap table's row 9, *"Node-level exclusive
resources — only bites once host nodes run device-bound work (H.5)"*, is right
in its qualifier and wrong in citing H.5's Android example.

**The one thing that could force a lease after all, stated so it is not a
surprise — and largely answered by correction 8.** The measurement created an
AVD and booted an emulator against a store that is immutable by construction, so
the boot path does not write into the SDK tree. What follows is retained for the
residual cases §3.3 names (a flow that installs an SDK package at task time),
and it is now unlikely rather than open. If the emulator turns out to need write
access into the SDK directory — license acceptance files, `sdkmanager` temp
state, an adb key the SDK tree expects — then the mount cannot be read-only,
the concurrency argument above loses its by-construction property, and an SDK
tree satisfies neither of `docs/spec.md` §3.1's justifications for the sccache bind.
The answer then is *not* a lease; it is a per-container copy-up of the small
mutable subset, with the bulk staying read-only. A lease would be the third
answer and the worst one. This is the first thing to measure in phase A1.

Two more empirical risks, from #308 H.4 and worth carrying forward rather than
re-discovering: `-gpu swangle` and `-no-snapshot` are fixes for a documented
emulator SIGSEGV about eleven minutes in, and #308 §F is right that these are
"adaptations to *GHA's* runner environment, not portable facts." Expect to
re-derive them against a container on the fleet's own kernel. If two concurrent
emulators prove unstable for reasons that are not memory, the answer is still
the capacity dial, not a new primitive.

**Two more, measured on 2026-08-02, recorded so A2 does not rediscover them**
*(secondhand)*:

- **The emulator wrote `/root/.android/emu-update-last-check.ini` despite
  `ANDROID_USER_HOME` being set.** So the tooling's mutable state is not
  entirely `ANDROID_USER_HOME`-rooted — some of it follows `HOME`. The bullet
  above ("it forces the mutable state into the container's own writable layer")
  still holds, because *both* destinations are the overlay and both die with it;
  what does **not** hold is any future assumption that setting
  `ANDROID_USER_HOME`/`ANDROID_AVD_HOME` is sufficient to relocate every write.
  Whatever A1 does about AVD home must also give the task a writable `HOME`, and
  anything that later tries to make `HOME` read-only or shared will break in a
  way that looks like an emulator bug.
- **`Unable to connect character device modem: address resolution failed for
  ::1`.** The container has no IPv6 loopback. Harmless for the boot — the AVD
  came up and `adb devices` saw it — but the emulator's **console/modem** path
  is what SMS injection, network-condition simulation and `telnet`-style console
  commands use, so any test that touches those should be expected to fail until
  the container gets a working `::1`. Cheap to fix at the container level if it
  is ever needed; not worth fixing pre-emptively.

---

## 5. The `platform` contradiction, and how a node advertises KVM

The brief is right that #309 §4 says two incompatible things about
`NodeCapabilities.platform`:

- the struct's doc comment: *"informational for placement diagnostics and
  **required for #308 category F targeting**"*;
- the absent-defaults table: *"Diagnostic only; **never a placement filter on its
  own**."*

They can be *narrowly* reconciled — "on its own" could mean "not as the sole
predicate" — but that reading makes the field useless for targeting, which is
what the doc comment claims it is required for. Two paragraphs of one document
disagreeing is not a contract; it must be settled.

### 5.1 Resolution: the defaults table is right, the doc comment is wrong

**Recommendation, as a finding for [#309 §4](./309-host-native-execution.md#4-capability-advertisement)
rather than a parallel mechanism:** keep `platform` as the diagnostic string the
defaults table describes, delete the "required for category F targeting" clause,
and target **both** legs of category F on specific, fail-closed capability
names.

The argument is the defaults table's own: `platform`'s absent-default is
`"unknown"`, and a filter whose default value matches nothing **fails open into
a wrong placement or fails closed into a fleet-wide stall**, depending on which
way the predicate is written — and neither is recoverable, because there is no
observation that distinguishes "N−1 daemon" from "genuinely unknown platform".
Design #309 §4 makes exactly this argument for `modes` and then does not apply
it to `platform`. Applying it is the fix.

More importantly, **neither leg of category F actually wants an OS string**:

- **iOS.** #322 §3 already made the *discovered Xcode set* the capability
  (`envs: ["xcode:16.4", "xcode:26.0"]`, read from the installed Xcode bundles
  at daemon startup) and says so in as many words: *"It is already the
  capability list."* A job needing Xcode 16.4 matches on `xcode:16.4`, not on
  `macos/aarch64`. `platform` adds nothing.
- **Android.** A job needs KVM and an SDK mount. `linux/x86_64` is neither
  necessary (a Linux node without KVM is useless to it) nor sufficient.

So the resolution is not a compromise — it is that `platform` was never the
right currency, and [#361](./361-per-run-placement.md) argues the general form of
this at length ("the currency of routine selection should be capability").

### 5.2 The field, and the shape smell worth naming

The proposal: **`features: Vec<String>`** on `NodeCapabilities`, absent ⇒ `[]`
(fails closed, exactly like `leases`), carried on both `PingOk` and
`WorkerAnnounce` as the same additive `Option<NodeCapabilities>` #309 §4
specifies — so no `WORKER_RPC_VERSION` bump. A KVM-enabled node with a
provisioned SDK advertises `["kvm", "android-sdk"]`; the daemon derives both
from its own config and its own filesystem, never from operator-typed strings
about what it *should* have. Correction 12 constrains *how* it derives them —
the daemon's container cannot stat the host, so a derivation that needs the host
filesystem is a probe container or nothing. Advertising from config alone
(`WORKER_KVM` is set ⇒ claim `kvm`) is the weaker thing this document settles
for at A3, and it is weaker in exactly the way #322 W1's
`declared_mode_without_a_backend_refuses_to_start` exists to prevent. Naming it
here so A3 does not discover it at design time.

The corresponding job-type side is `placement.features` and it is **phase A4**,
not now — §[2.3](#23-recommendation)'s trigger.

**The smell, stated honestly, because a reader will see it anyway.** Four
designs now each add a `Vec<String>` to `NodeCapabilities`: `modes` and `leases`
(#309 §4), `envs` (#322 P1), `images` (#355 §6), and now `features`. All five
are "names of things this node has, absent ⇒ empty, fails closed", and a single
namespaced set (`kvm`, `xcode:16.4`, `image:owner/project/name:sha`) would
express all of them with one predicate.

This document **does not propose that collapse**, for two reasons and it is
worth being explicit about both. First, it is out of scope — three of the four
fields belong to other designs and none of them is implemented, so collapsing
them is an edit to three unshipped documents by a fourth. Second, the fields
have genuinely different churn: `images` changes on every project image build
(#355 §5's reconciler), while `modes`, `envs` and `features` are boot-time facts
that design explicitly declines to give an ordering key *because* they are
boot-time facts. Merging a high-churn field into that set would drag the ordering question
back in. The finding to record is: **whoever implements #309 §4 first should
decide the whole set at once**, not accumulate it one design at a time.

### 5.3 The placement predicate

`choose_placement` (`crates/container/src/lib.rs`) needs no new shape — #355 §6
already says this and it is right. It is a pure function over
`&[PlacementCandidate]` with an optional pin, and it already skips ineligible
candidates. A required-features predicate is another skip, applied **after**
probing for #309 §4's bootstrap-deadlock reason, with its own distinct
`NoCapacity` message so the diagnosis survives:

- no candidate advertises a required feature → `NoCapacity("no node advertises
  kvm")`
- candidates advertise it but none is free → the existing `NoCapacity("no free
  slots on any node")`

Both transient by the §3.5 contract, both queued, neither consuming retry
budget. Tier 1 per [`docs/reference/testing.md`](../reference/testing.md), beside the existing
`choose_placement` tests.

**Interaction with #322, named as the brief asks.** The device grant and the
features predicate are shared primitives, and the *predicate* is the shared half:
`choose_placement` gains one required-capability argument once, and both
`xcode:16.4` and `kvm` are values in it. The *device* half is Android-only — a
Mac has no `/dev/kvm` and #322's simulator story is `simctl`-scoped teardown,
not device passthrough. So A3 below and #322 P1 are **the same slice**, and
whichever leg reaches it first should build it for both.

---

## 6. Alternatives

### 6.1 Redroid — containerized Android with no KVM at all

Runs Android as a normal Linux process tree on the host kernel; near-native
speed; no `/dev/kvm`. Verified against the cited article (fetched 2026-08-01):
it requires the host kernel modules `binder_linux` (`modprobe binder_linux
devices="binder,hwbinder,vndbinder"`) and `ashmem_linux`, unless the kernel
builds `CONFIG_ANDROID_BINDERFS` and ashmem in; it ships **no Google Play
Store** by default; and the article's own recommendation is Redroid "for
headless CI, device farms, ARM cloud" and the official emulator where
"Play-Store-dependent flows" matter.

*For:* No KVM, so it works on a node that is itself a VM without nested
virtualization — which is the one scenario that kills the recommendation
outright (§[8](#8-risks-and-open-questions)). Faster.

*Against, and it is decisive:* it trades a **narrower** node commitment for a
**wider** one. `--device /dev/kvm` needs a kernel that already has KVM, which
every bare-metal Linux node has. Redroid needs two out-of-tree kernel modules
loaded on the host — a `nixos-rebuild` with a custom kernel, i.e. exactly the
"machine facts" row of #308 H.6's table but a far heavier entry, and one that
must be re-derived on every kernel bump. And it changes **what is under test**:
Redroid is a container-Android, not the AVD system image the application ships
against, so a green Redroid run is weaker evidence than a green emulator run.
For a platform whose evaluation gates *are* the CI, weakening what a gate proves
is the wrong trade.

**Rejected, and named as the fallback** if the Linux node turns out to be a VM
without nested virtualization and cannot be replaced. **Correction 8 closes
that condition on the only node in question**: `emulator -accel-check` reported
KVM version 12 installed and usable from inside the container. The fallback
stays written down for a future node, but it has no live trigger.

### 6.2 Host-native Android on a pinned Linux node

The #309/#322 route applied to Android: `WORKER_MODES=container,host`, a
`HostBackend`, `runtime: { mode: host, env: "nix:.#android" }`.

*For:* One mechanism for both legs of category F. Reuses whatever #322 W2 builds.
Gives ambient SDK state for free — though correction 9's `/nix/store` mount
gives the container mode the same store access the host mode would have, so
this "for" is now nearly empty: the difference between the two routes on the
toolchain axis is write access to the store, which neither wants.

*Against:* It costs #322 W2 in full — the durable task registry, the liveness
ladder, restart recovery, the total `/workspace` + `/chuggernaut` wire-path
mapping, credential teardown at process exit, the agent-shaped-launch refusal —
plus N2's epoch bump and W4's env-ref resolution, for a leg that does not need
any of it. It re-acquires every problem containerization dissolves: ambient
mutable SDK state (#309 §9 calls per-project caches "a new mechanism, not a
widening of the old one" and mandates eviction), a shared adb server, and the
exclusivity primitive §[4](#4-exclusivity-and-why-android-does-not-need-a-lease)
shows is unnecessary. And it inherits #309's own stated schedule risk: "nobody
in this tree has built a non-container backend."

**Rejected.** It is the more general answer and it is strictly more expensive for
this leg.

### 6.3 Do nothing — keep Android on GitHub Actions

*For:* Zero platform work. It is also what happens by default if this ships late,
so it deserves to be priced rather than dismissed.

*Against:* Category F stays unported, which is the gap #308 ranks first. The
Android leg stays outside the platform's gating, so the property CLAUDE.md
describes — "every change merges through a Chuggernaut job, and the job's
evaluation criteria are the CI" — does not hold for the largest test suite the
consumer project has. And it leaves the corpus's claim that mobile requires
host-native execution unchallenged, which is the error that produced this
sequencing in the first place.

**Rejected as the end state, accepted as the honest status quo** until phase A1
lands. It is a legitimate answer to "what if the KVM precondition fails."

### 6.4 An ordinary container that SSHes into a Linux box

Design #322's option B, transposed. Named only to dismiss: the thing you would SSH to
is a Linux host that can run the container directly. #322's version of this
option exists because a Mac cannot run the container at all; that asymmetry is
the whole reason the option is interesting there and pointless here.

---

## 7. Sequencing: what ships first, and what it unblocks

**Yes — the Android leg can precede #322 W2, and by a wide margin.** Phases A0
and A1 touch no dispatcher code, no wire record, no schema, and no shared
`/workspace` handling. They are strictly node-side, which is the same property
that made #313 B-IV and `WORKER_CACHE_DIR` cheap.

| Phase | Kind | Work | Depends on |
| --- | --- | --- | --- |
| **A0** | operator | **Mostly done** (correction 10): KVM is confirmed usable from a container, the SDK is already provisioned by the node's NixOS config, and the `kvm`-group requirement was never real (`/dev/kvm` is `0666`). What remains is two things and neither is code: the **store secret-scan gate** (§[3.4](#34-what-the-nixstore-mount-exposes)) and the **stable SDK path** in `configuration.nix` (§[3.5](#35-staleness-no-store-hash-in-any-chug-side-config)). Both land in the operator's repo, not this one | — |
| **A1** | `code` | `WORKER_KVM` + `WORKER_KVM_PROJECTS` + a **stable** SDK path setting (never a store hash) in `crates/worker/src/config.rs` beside `parse_cache_dir`; `DockerBackend` device + read-only-mount properties beside `cache_dir`; `build_host_config` populates `HostConfig.devices` and a read-only `/nix/store` mount whose missing source is **refused, not created empty** (correction 12); `ANDROID_SDK_ROOT`/`ANDROID_HOME`/`ANDROID_USER_HOME` injected beside `inject_cache_env`, plus a writable `HOME`. **No image work** (correction 11). The job type pinned with `placement.node`. Confirms the two things §[3.5](#35-staleness-no-store-hash-in-any-chug-side-config) leaves open — engine-side symlink resolution, and a non-store `ANDROID_SDK_ROOT` — before the config parse is written, falling back to S3(b) if either fails. **No wire change, no dispatcher change, no epoch bump** | A0's two remaining items |
| **A2** | `code` | **Built** — `.chug/jobs/android-proof.yaml` + `.chug/tasks/android-proof.sh`, a five-rung ladder (mounts and env → `emulator -accel-check` → the toolchains → `flutter build apk --debug` of `fixtures/mobile` → an emulator boot and a device-backed task), pinned with `placement.node: nuc`, every wait bounded and every rung's verdict in stdout. **Correction 13 replaces this row's entrypoint**: `flutter`, never a bare `./gradlew`. As originally written: prove it against a **boring `./gradlew connectedAndroidTest` on one module**, not against the full flutter integration suite — #308 H.4 is right that it sits at the confluence of too many unbuilt things, and #309 and #322 both say the same about their own first targets. Measure what correction 8 did not: do two concurrent emulators fit in the node's memory; does the SIGSEGV reproduce; does anything in the suite need the emulator console (no `::1` in the container) or write outside `ANDROID_USER_HOME` (`/root/.android/emu-update-last-check.ini` already does) — §[4](#4-exclusivity-and-why-android-does-not-need-a-lease) | A1 |
| **A3** | `code` | `NodeCapabilities` on `PingOk`/`WorkerAnnounce` with `features`, and the `choose_placement` predicate — **this is [#309 §4](./309-host-native-execution.md#4-capability-advertisement)/§5a and [#322](./322-macos-native-runtime.md) P1, one slice serving both legs**. Unpins Android work. Needed only when a second KVM node exists | A2; §[5.2](#52-the-field-and-the-shape-smell-worth-naming)'s "decide the whole field set at once" |
| **A4** | `code` | `placement.features` as a job-type field + `CONFIG_SCHEMA_EPOCH` bump. Only when the pin stops expressing the requirement | A3 |
| **never** | — | Device leases for Android (§[4](#4-exclusivity-and-why-android-does-not-need-a-lease)). #309 §5b/P4 stays, motivated by iOS | — |

**A1 is the phase to start**, and its whole cost is one config parse, one
builder method, one `HostConfig` field, one or two read-only mounts and four env
vars — the same diff shape as the sccache work that already shipped. The
amendment makes it
cheaper, not dearer: the image work is gone (correction 11), A0 is nearly gone
(correction 10), and the two things it adds — a mount that refuses a missing
source, and a stable path instead of a hash — are both smaller than the "slim
Android task image" line they replace.

**What it unblocks:** the Android half of #308 category F, without waiting on any
of #322's W2, W3, N1, N2, W4 or W5. Restated as a schedule fact: the corpus currently
sequences Android behind six phases of macOS work it does not need.

Test placement per [`docs/reference/testing.md`](../reference/testing.md): the config parses
(`WORKER_KVM`, `WORKER_KVM_PROJECTS`, the stable SDK path), the produced
`HostConfig` (device present only for an allow-listed project; absent otherwise;
the store mount read-only and of a kind that refuses a missing source), the env
injection, and the `choose_placement` features predicate are all pure → **tier
1**, beside `host_config_with_cache_adds_one_bind` and `parse_cache_dir`'s
tests. Correction 12 moves one test that the original A1 put at tier 2: there is
no boot-time refusal to test, because the containerized daemon cannot stat the
host, so what tier 2 covers instead is that a launch against a node missing the
device or the store **fails as a named `BackendError::Launch`** rather than
starting a container that will fail later. **A regression test must assert the
negative space explicitly**: a launch for a project *not* on the allow-list
carries neither the device nor the mount. An emulator boot is **tier 3 / out of
tree** and belongs to the consumer project's own job.

---

## Contracts changed (per docs/reference/style.md's contract-first rule)

| Phase | Contract changed |
| --- | --- |
| A1 | `WorkerConfig` gains device/mount/allow-list fields; `DockerBackend` gains a node-property builder beside `with_cache_dir`; **`build_host_config`'s postcondition changes** — "whether a cache bind-mount is present" becomes "which node properties are present", and the dispatcher-side `binds: None` invariant must survive as `devices: None, binds: None, mounts: None`; a new node-side invariant — **a launch carries the device and the store mount together or carries neither**, decided once from the allow-list; and a mount-source contract — a missing store source is refused at create, never materialized as an empty directory (correction 12) |
| A3 | Two wire records additively (`PingOk`, `WorkerAnnounce` gain `Option<NodeCapabilities>`), no `WORKER_RPC_VERSION` bump; `choose_placement` postcondition gains the features predicate and a second distinct `NoCapacity` message |
| A4 | Job-type schema epoch (§14.1): `placement.features`, `CONFIG_SCHEMA_EPOCH` 2 → 3 (or later) with a frozen feature constant per `INPUTS_SCHEMA_EPOCH`'s precedent (`crates/types/src/version.rs`), plus the `min_dispatcher` rule |

---

## What this makes wrong elsewhere

- **[#308](./308-gha-port.md) H.2** lists "`/dev/kvm` for the Android emulator"
  among the things *host mode buys*. It is not a host-mode benefit; it is
  available in container mode with one flag.
- **#308 §F** — "Impossible under containers — not hard, impossible" is true of
  Xcode and false of the Android emulator. The sentence's own evidence (Xcode,
  `xcrun simctl`) supports the narrower claim only.
- **#308 H.4** — the AVD-persistence claim, per correction 7 *(secondhand)*.
- **#308's gap table, row 3** — "Host-native execution … carries mobile
  (category F, which nothing else unblocks)" carries **half** of category F.
- **#308 H.5 and [#309 §5b](./309-host-native-execution.md#5b-exclusive-resources-device-leases)**
  — the `beacon-emu` example is not a shared device in container mode. The
  primitive stands on #322 §5's simulator case; its Android motivation does not.
- **[#309 §4](./309-host-native-execution.md#4-capability-advertisement)** — the
  `NodeCapabilities.platform` doc comment contradicts the defaults table.
  Recommendation in §[5.1](#51-resolution-the-defaults-table-is-right-the-doc-comment-is-wrong).
- **#309 correction 4** — "the only way to change a node's slot count today is
  to restart its daemon" is now stale (correction 6).
- **[#313](./313-workload-identity-image-builds.md) B-IV** — its allow-list is
  described as per-`(project, job type)`; the job type is not observable
  node-side (correction 5).
- **`docs/spec.md` §3.1** — "no host bind-mounts … The one permitted exception is a
  **worker-provisioned node-local build cache**" becomes a small closed class,
  and the section gains a device-passthrough sentence. This is a `docs` job, not
  part of A1. **The amendment widens this edit.** The exception's justification
  is written around the cache's own properties — "carries **no job state** … safe
  to be empty/cold, and never affects correctness" — and a `/nix/store` mount
  satisfies the first and fails the second two: it is not safe to be empty and
  its absence is a correctness failure. So §3.1 does not gain a second member of
  the same class; it gains a **second class**, a read-only node toolchain volume
  whose absence must fail the launch. Saying so is the difference between a
  closed list of two and an open door.
- **#308 H.6's layering rule** — *"resist putting per-project tooling in the
  node's NixOS configuration"* — is what §[3.6](#36-the-clock-this-borrows-from-and-who-owns-it)
  knowingly bends by consuming the node's system closure. Not a correction to
  H.6, which is right; a recorded exception with a named trigger to end it.
- **`crates/container/src/docker.rs`** — `build_host_config`'s doc comment names
  the cache bind specifically.
- **`crates/worker/src/daemon.rs`'s `create_dir_all` on `WORKER_CACHE_DIR`** —
  harmless but misleading in the deployed shape: it creates a directory inside
  the worker's own container, not the host path being bound (correction 12).
  Worth a doc-comment sentence when A1 is next to it, not a change of behavior.
- **`docs/reference/crates.md`'s `container` row** — already noted as wrong by #309; a device
  property does not make it wronger, but the same edit should catch it.

---

## 8. Risks and open questions

- ~~**The KVM precondition is the whole design, and it is unverified.**~~
  **Retired 2026-08-02 by correction 8** — measured usable from an unmodified
  container on `gumbo-nuc-0`, no `--privileged`, emulator booted. The risk is
  retired *for that node*; a second KVM node re-asks the question and A0 is
  still the place to answer it.
- ~~**A read-only SDK mount may not survive contact with `sdkmanager`.**~~
  **Largely retired by correction 8** — AVD creation and boot needed no write
  into the SDK tree. Residual: a task flow that installs an SDK package at run
  time, which under this design is a `nixos-rebuild` and not a task action
  (§[3.3](#33-recommendation-t2-now-and-t5-as-the-complement-rather-than-the-rival)).
- **The `/nix/store` mount is the design's widest grant, and it is accepted
  rather than eliminated.** A read of every package on a node shared with
  another project's CI. §[3.4](#34-what-the-nixstore-mount-exposes) argues it
  and attaches four conditions; the one that can still fail is the secret scan,
  which this document could not run.
- **Garbage collection can delete a running task's toolchain.** #309 §9 and #308
  H.6 both flag this for host mode, and it arrives unchanged in container mode:
  a `nix-collect-garbage` that removes the SDK closure mid-run breaks a live
  emulator. The mitigation is a property of *how* the SDK is provisioned rather
  than new code — a closure referenced by the NixOS **system profile** is
  GC-rooted by that profile, whereas an ad-hoc `nix build` is not. So A0's
  provisioning must stay in `configuration.nix`, and "provision it by hand for a
  quick test" is a footgun worth naming.
- **A pinned store hash goes silently stale.**
  §[3.5](#35-staleness-no-store-hash-in-any-chug-side-config) rejects the pin
  and requires an activation-maintained stable path. The residual risk is
  procedural: nothing in this repo can enforce that the operator's
  `configuration.nix` keeps that path, and its disappearance shows up as a
  refused launch rather than a silent wrong result — which is the correct
  direction to fail, and is the reason correction 12's mount-source behavior is
  a contract rather than a detail.
- **The emulator's empirical tuning does not port.** `-gpu swangle`,
  `-no-snapshot` and the eleven-minute SIGSEGV are GHA-runner adaptations; #308
  §F says expect to re-derive them and it is right.
- **Kernel attack surface.** `/dev/kvm` in a container is a real CVE class
  (§2.1). The allow-list narrows *who*, not *what*; keeping the node's kernel
  current is a machine fact under #308 H.6's system-closure row and belongs in
  the node runbook.
- **Disk.** The store shares the partition with the node's images and the
  `worker-refresh.sh` free-disk floor: 17.5 GiB of Android closure inside a 56G
  store, growing by a closure on every SDK bump until something collects it.
  #355 §7's "the platform refresh must win" rule applies to a store the operator
  fills as much as to a built image, and nothing in this repo enforces it. The
  amendment improves this axis rather than worsening it — the alternative was a
  multi-GB image on the same disk.
- **Everything about beacon and about the 2026-08-02 measurement is
  secondhand.** The AVD correction, the `runs-on` labels, the emulator flags and
  corrections 8–11 all come from operator inspection of a node this document
  cannot reach. The measurement is detailed enough to be reproducible by anyone
  with node access, and reproducing it is the cheapest possible check on this
  design; correction 12 is the only amendment finding read out of this tree.
- **A3's field set should be decided once.** Five `Vec<String>` capability fields
  are proposed across four unimplemented designs
  (§[5.2](#52-the-field-and-the-shape-smell-worth-naming)). Whoever lands
  `NodeCapabilities` first inherits that decision whether or not they want it.

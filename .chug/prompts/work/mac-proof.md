# The Mac proof — the first agent host task

You are running **natively on macOS**, as the worker daemon's own login user, on
the fleet node `air`. There is no container around you: no image, no namespace,
no filesystem boundary between you and the machine. Everything you write outside
this task's own directory outlives the task and is inherited by whatever runs
next. That is the whole reason this job exists.

This is design [#490](../../../docs/design/490-agent-work-on-a-mac.md) slice 6.
Slices 1–5 built the machinery — the transcript resolved by session id, the
node's own Mach-O channel binary, the CLI discovered on the daemon's `PATH`, the
launch-time capability test. Nothing has ever exercised them end to end. You are
the first.

**You produce a report, not a change.** This job type's wrap-up is `type: none`:
the branch carries no commits and nothing merges. Do not commit. Do not push. Do
not edit the checkout. If you find a bug, name it in your report — fixing it is a
different job.

## What you must produce

Three things, in this order of importance:

1. **The measurements**, because they are what a decision gets made from.
2. **The evidence** that the host agent path works at all — which is mostly
   produced by you simply running.
3. **A summary printed LAST on stdout**, on the failing path as much as the
   passing one. A worker keeps only the final 700 KiB of a task's stdout, so a
   summary printed early is a summary nobody reads.

## Ground rules

- **Bound every wait.** macOS ships no `timeout(1)`, so poll with a fixed
  iteration cap and a fixed sleep, and report the bound you hit by name rather
  than blocking (`docs/reference/style.md` Tier 2 rule 3). Nothing here should
  need more than a couple of minutes.
- **Install nothing. `sudo` nothing.** You are on a real machine an operator
  owns. If something you need is absent, that is a finding, not an errand.
- **Do not clean up after yourself.** Sections 3 and 4 below depend on it — a
  task that tidies has measured nothing about what it leaves behind. The one
  exception is the simulator you boot, which you shut down and do not delete.
- **Do not read or copy anything out of `$HOME` that is not yours.** You are
  looking at file *names*, *counts* and *timestamps*, never at contents. The
  daemon user's home holds an operator's own credentials.
- **A failed rung is a result.** Do not work around it, do not retry it more
  than the bound you set, and do not omit it from the summary. Carry on to the
  rungs that do not depend on it.

## 1. Prove the path

Establish, and print, that you are where you think you are and that the
toolchain the platform selected is the one you got:

- The machine and the user: `uname -a`, `sw_vers`, `id -un`, `pwd`.
- The platform's own variables, which are what tell you this is a host task and
  not a container: `CHUG_WORKSPACE` (the clone destination — a real path under
  the node's host root, not `/workspace`), `CHUG_HOST_CREDS` (this task's
  credential tree), `CLAUDE_CONFIG_DIR`, `CHUG_TASK_ID`, `CHUG_PHASE`.
- The toolchain the node resolved for this job type's `runtime.env`:
  `DEVELOPER_DIR` and `CHUG_ENV_PATH`, and that `CHUG_ENV_PATH` is really on
  your `PATH` ahead of everything else.
- That the resolved Xcode is the one declared: `xcodebuild -version` and
  `xcrun --version`, plus `xcode-select -p` — and say whether the last one
  agrees with `DEVELOPER_DIR` or not, because the platform selects per process
  and deliberately never runs `xcode-select -s`.
- The agent CLI the daemon discovered: `command -v claude`.

**Report the MCP channel explicitly.** You reached `update_status` and you will
reach `submit_result` through a **Mach-O** `chuggernaut-channel` the node
installed and your own CLI spawned as a stdio MCP server. That is #490's M2
residual — the half job #492 could not measure, because it could test the binary
but not the CLI as its launcher. Say in your report whether those calls worked
first time, and quote any error if they did not: you are the only observer of it
there will ever be.

## 2. Do something only a Mac can do

`xcrun simctl`, against the iOS runtime this Xcode carries. Small, real and
verifiable — "Xcode cannot be containerized" is the entire premise of design
[#322](../../../docs/design/322-macos-native-runtime.md), so a simulator is the
honest demonstration and a `sw_vers` is not.

- `xcrun simctl list runtimes` — record which iOS runtime is available.
- Choose a device: prefer one that already exists for that runtime. If none
  does, create one named `chug-mac-proof` and **say that you created it**, since
  a second run finding it still there is itself the M7 datum.
- Boot it, bounded, and prove it actually booted rather than trusting the
  command's exit code — `xcrun simctl list devices` should show it `Booted`.
- Then do something *inside* it, so the proof is of a running simulator and not
  of a plist: `xcrun simctl spawn <udid> launchctl list` prints the simulator's
  own launchd jobs — `com.apple.progressd`,
  `com.apple.CoreAuthentication.daemon` and the rest. Quote the output.
- **`spawn` runs the program you name inside the simulator's own filesystem, so
  name one the iOS runtime actually ships.** This rung is deliberately not
  `uname -a`, which is the obvious command and cannot pass on any simulator in
  any session: iOS carries no `uname`, so `simctl` answers `NSPOSIXErrorDomain`
  code 2, and a host absolute path such as `/bin/ls` answers `LaunchdSimError`
  111 — measured on `gumbo-air-0` on 2026-08-09 over an ordinary SSH session.
  Two earlier runs of this prompt ran the old command, failed, and reported
  those codes as a property of the daemon's session; they are not, and
  [#490](../../../docs/design/490-agent-work-on-a-mac.md)'s job #527 correction
  is where that is withdrawn. If a rung of your own needs something the runtime
  does not ship, `xcrun simctl launch <udid> <bundle-id>` against an app the
  device already carries is the other honest proof.
- Shut down **only the device you booted**, by UDID. Never `shutdown all`,
  never `erase`, never `delete` — see the next section for why.

## 3. Measure M5: what the authenticated CLI leaves outside its config dir

[#490](../../../docs/design/490-agent-work-on-a-mac.md) D6 bounds credential
lifetime on the premise that the agent CLI confines itself to
`CLAUDE_CONFIG_DIR`. In a container that premise is free — anything written
elsewhere dies with the container. Here there is no such boundary. Job #492
measured the residual as **zero**, but under `env -i` with an isolated home and
an **unauthenticated** CLI, and not under the daemon. You are the authenticated
run under the daemon, and yours is the measurement that matters.

**Produce a list, not a verdict.** D6's fate is a decision for a later job; your
job is to make it decidable.

- **Pick a time reference that predates the CLI process.** Your own "before"
  snapshot cannot see what the CLI wrote at startup, because it started before
  you did. The platform wrote this task's credential tree at launch, so a file
  under `CHUG_HOST_CREDS` is older than the CLI: find one that nothing since has
  modified, name it and its mtime in the report, and use it as the reference for
  a `find -newer` sweep over the daemon user's home.
- **Sweep the home directory, bounded**, and say what the bound was: a depth cap
  and an output cap, both stated, are honest where an unbounded walk of a real
  person's home is neither safe nor finishable. Exclude the task's own tree
  (`CHUG_WORKSPACE` and `CHUG_HOST_CREDS`) and say you did.
- **Look where an agent harness actually writes**, by name rather than by luck:
  a dot-directory or dot-file whose name contains `claude`, and the same under
  `Library/Application Support`, `Library/Caches`, `Library/Preferences`,
  `Library/Logs` and `.config`. Record each as present or absent, with its
  mtime, and whether the sweep above says this task touched it.
- **The keychain, carefully.** The platform does not log the CLI in
  interactively — it injects an OAuth token as an environment variable
  (`crates/dispatcher/src/exec.rs`), so **no keychain write is expected**. The
  point is that "expected" is exactly what this measurement exists to distrust.
  Record the login keychain file's mtime before and after your run, and probe
  for a small, named set of plausible service names with
  `security find-generic-password -s <name>` — which prints attributes only.
  **Never pass `-w` or `-g`**: those return the secret and can raise a GUI
  prompt on a headless node. Report which names you searched, so a reader knows
  what the absence covers.
- **State the attribution limit.** The daemon and the operator's own login
  session are running beside you, so a `-newer` sweep proves a file changed
  during your window, not that you changed it. Say which entries you can
  attribute and which you cannot.

## 4. Measure M7: what the simulator state looks like, before and after

M7 asks whether simulator state one task leaves behind disturbs the next.
**One run cannot answer that** — it needs two host tasks, and yours is the
first. Say so plainly in your report and do not write a verdict. What you
produce is the baseline a second run gets diffed against:

- `xcrun simctl list devices --json` and `xcrun simctl list runtimes --json`,
  captured **before** you touch anything and again **after** you have shut your
  device down — the second is the state the next task actually inherits.
- The size and mtime of the CoreSimulator device set root, before and after, so
  a reader can see growth without reading a device's contents.
- A short diff of the two listings in the summary: devices added, devices whose
  state changed, runtimes unchanged.

**Write the raw captures to the output archive**, because your summary is prose
and a second run needs bytes. Collect the before/after JSON and the M5 listings
into a directory and `tar czf` it to `$CHUG_WORKSPACE/chug-output.tar.gz` — the
dispatcher harvests exactly that path into the task's `output.tar.gz` artifact
(`crates/platform-ops/src/harvest.rs`). Name the files so a second run can pair
them without guessing. This is the only part of your work that is
machine-readable later; treat it as the deliverable it is.

## Finishing

1. Narrate as you go with `update_status` — it streams to the operator's screen,
   and on this job it is also evidence (see §1). At least three calls: your
   one-line plan, after the simulator rung, and just before you submit.
2. **Print the summary LAST**, as a single block, on the failing path as much as
   the passing one. One line per rung with an explicit verdict, then the M5 list,
   then the M7 before/after diff, then what a second run should do.
3. Call `submit_result` with:
   - `summary`: open with **one plain sentence** stating the outcome
     (markdown-free — it is the readable first line of the commit body), then
     short `###` sections: `### The path` (what ran, and the channel), `### M5 —
     residual`, `### M7 — simulator state`, `### Notes` (what failed, what you
     could not attribute, what a second run needs). Bullets, not paragraphs.
   - `structured`: `{ "files_changed": [], "notes": "..." }` — the list is empty
     and that is correct; this job changes nothing.
4. Exit **non-zero if a rung in §1 or §2 failed**, and 0 otherwise — the same
   contract `.chug/tasks/android-proof.sh` holds itself to, so a broken host path
   escalates to a human instead of reading as a green proof. A *partial*
   measurement in §3 or §4 is not a failed rung: an absence you recorded and
   bounded is a result, and it exits 0. Print the summary before you exit,
   either way.

A reviewer will judge this against the brief: whether the measurements are
measurements rather than assertions, whether the limits are stated, and whether
a second run could actually be diffed against what you left behind.

# Runbook — proving host-task supervision on macOS

**Status: UNVERIFIED.** Nobody has run this yet, and nothing in the platform may
assume its answer until someone has. Design
[#440](../../design/440-native-worker-daemon.md) D3 says a host task on macOS is
kept out of the daemon's teardown set by the process group `spawn_task` already
creates — and marks `launchd`'s process-group teardown semantics **secondhand**,
because no file in this repo states them. This is the procedure that settles it.

It cannot run in CI. The evaluation gate is a Debian container with no `launchd`,
so this is operator-verified in the shape of `.chug/jobs/android-proof.yaml` and
`.chug/jobs/gcp-proof.yaml`: the operator runs one thing and reads one answer.
The Linux half of D3 is a transient systemd scope and is asserted in
`crates/container/tests/host_backend.rs`, which self-skips where no scope can be
created.

## Run it

On the Mac (the Mini, or any macOS node being considered for `host` mode), in a
checkout of this repo:

```sh
sh deploy/prod/macos-host-supervision-proof.sh
```

Takes about five seconds. It needs no platform credential, touches no deployed
agent, and installs nothing outside a temp directory — the agent it creates is
`com.chuggernaut.host-supervision-proof`, bootstrapped from `/tmp` and booted out
on exit. Its plist is deliberately **not** in `deploy/prod/launchd/`, because
`deploy/prod/install-launchd.sh` globs that directory and would install the proof
on the Mini.

## What it does

1. Installs a LaunchAgent standing in for the worker daemon. Its program
   backgrounds one task under `set -m`, so the task gets its own process group —
   the shell's equivalent of the `process_group(0)` that
   `crates/container/src/host.rs`'s `spawn_task` sets, which is the whole of
   `Supervision::ProcessGroup`.
2. Records both process groups and refuses to continue if they are the same,
   because then the run would have tested nothing.
3. Runs `launchctl kickstart -k gui/$(id -u)/com.chuggernaut.host-supervision-proof`
   — the restart a native `worker-refresh.sh` would perform.
4. Asserts the task is still alive, then waits for it to land its own exit code
   the way `supervised_cmd`'s wrapper does.

## Read the answer

| Last line | Meaning |
| --- | --- |
| `PASS: the task survived …` | D3's macOS mechanism holds. Record it in `docs/design/440-native-worker-daemon.md` and only then rely on it. |
| `FAIL: the task died with the agent that launched it` | D3's macOS mechanism does **not** hold. The fallback is [#322](../../design/322-macos-native-runtime.md) §6's second mitigation — one `launchd` job per task — which #440 deliberately does not pre-commit to. |
| any other `FAIL:` | The proof could not be set up; the line says which step. Nothing was proven either way. |

A `FAIL` of the second kind is a finding, not a broken script: it means a macOS
node must drain before its daemon is restarted, and it is what
[#440](../../design/440-native-worker-daemon.md) D4's refusal exists to make
survivable in the meantime.

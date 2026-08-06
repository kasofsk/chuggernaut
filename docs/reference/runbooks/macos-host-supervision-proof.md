# Runbook — proving host-task supervision on macOS

**Status: PASSED on macOS 26.5.1** — run on `gumbo-air-0` on 2026-08-06 against
the tree at `c8a8354`. The result and what it does and does not settle are
recorded in
[#440's proof section](../../design/440-native-worker-daemon.md#proofs-2026-08-06--d3-on-macos-and-on-linux);
read that before relying on the answer. The answer is a property of the host's
OS version, so **re-run this on any macOS node before it is considered for
`host` mode** rather than inheriting `gumbo-air-0`'s verdict.

Design [#440](../../design/440-native-worker-daemon.md) D3 says a host task on
macOS is kept out of the daemon's teardown set by the process group `spawn_task`
already creates. #440 marked `launchd`'s process-group teardown semantics
**secondhand** because no file in this repo stated them; this procedure is what
settled it, and the proof section is what retired the marking.

It cannot run in CI. The evaluation gate is a Debian container with no `launchd`,
so this is operator-verified in the shape of `.chug/jobs/android-proof.yaml` and
`.chug/jobs/gcp-proof.yaml`: the operator runs one thing and reads one answer.
The Linux half of D3 is a transient systemd scope and is asserted in
`crates/container/tests/host_backend.rs`, which self-skips where no scope can be
created — those assertions have still never executed, so D3's Linux half is
confirmed only by the hand run #440's proof section records. Until job #453 they
could not have executed under an **unprivileged** run — the shape every hand run
has taken since job #451 moved them to a `--user` scope: the probe cleared the
bus variables out of its own `systemd-run` call, so that skip was unconditional
and blamed the node
([the correction](../../design/440-native-worker-daemon.md#correction-2026-08-06--the-bus-the-client-needs-job-453)).
A run as root takes the system scope, which is a fixed socket path and needed no
bus variable to reach.

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
| `PASS: the task survived …` | D3's macOS mechanism holds on that host's OS version. Record the host and the version beside the 2026-08-06 run in `docs/design/440-native-worker-daemon.md`, and only then rely on it. |
| `FAIL: the task died with the agent that launched it` | D3's macOS mechanism does **not** hold. The fallback is [#322](../../design/322-macos-native-runtime.md) §6's second mitigation — one `launchd` job per task — which #440 deliberately does not pre-commit to. |
| any other `FAIL:` | The proof could not be set up; the line says which step. Nothing was proven either way. |

A `FAIL` of the second kind is a finding, not a broken script: it means a macOS
node must drain before its daemon is restarted, and it is what
[#440](../../design/440-native-worker-daemon.md) D4's refusal exists to make
survivable in the meantime.

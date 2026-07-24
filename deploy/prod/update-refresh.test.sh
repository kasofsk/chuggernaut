#!/bin/sh
# Shell test for update.sh's refresh_workers helper — no NATS, no worker node.
#
# update.sh has no full harness (it does a native cargo build, launchd restarts,
# etc.), so the correctness-critical piece — the worker self-refresh loop — is
# extracted into the refresh_workers function and tested in isolation here. We
# source update.sh with CHUG_UPDATE_LIB=1 so its guard returns after defining the
# helpers, WITHOUT running any deploy side effect, then drive refresh_workers
# against a stubbed `chuggernaut` binary.
#
# The contract under test (#186): the `admin worker-refresh` CLI ALWAYS exits 0
# — every outcome, INCLUDING "not confirmed within the wait window", returns Ok
# and only prints its story. So refresh_workers must NOT trust the exit code: it
# passes --wait-secs 900 (the default) and treats the absence of a "refresh OK:" line as a
# FAILED deploy step. No more `|| echo WARNING` masking a refresh that never
# landed.
#
# Run:  deploy/prod/update-refresh.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/update.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
mkdir -p "$BIN"
LOG="$WORK/calls.log"

# Pull in refresh_workers (and only that) — the guard returns before the deploy
# body when CHUG_UPDATE_LIB is set.
CHUG_UPDATE_LIB=1 . "$SUT"

# Env the function reads. DOCKER_NODES carries a real worker plus a NON-worker
# endpoint that must be skipped (only `worker` rows are refreshed).
export TARGET_SHA=abc123def456
export DOCKER_NODES="nuc|worker|4, air|dispatcher|0"
export NATS_URL=nats://localhost:4222
export KEYS_DIR="$WORK/keys"
export CHUG_IMAGE_TAG=prod
# refresh_workers drops each emitted leg from the pending list (chug_leg_drop),
# which reads CHUG_LEGS_PENDING. The deploy body seeds this before it ever calls
# refresh_workers; that init lives BELOW the CHUG_UPDATE_LIB gate, so the lib
# harness must seed it too or `set -u` aborts on the first drop.
export CHUG_LEGS_PENDING="worker-refresh:nuc"

# A stub `chuggernaut` that CONFIRMS the swap — prints the "refresh OK:" line the
# real CLI prints only on a confirmed refresh. Exits 0 (as the real CLI always
# does). Logs its argv so we can assert the wait flag was actually passed.
cat > "$BIN/chug-confirm" <<EOF
#!/bin/sh
echo "\$@" >> "$LOG"
echo "refresh requested: node=nuc from=old -> sha=$TARGET_SHA tag=prod"
echo "refresh OK: node=nuc version=chuggernaut+abc123def456"
exit 0
EOF

# A stub that does NOT confirm — mirrors the CLI's not-confirmed path: it prints
# a WARNING and STILL exits 0. refresh_workers must catch this via the missing
# "refresh OK:" line, not the exit code.
cat > "$BIN/chug-unconfirmed" <<EOF
#!/bin/sh
echo "\$@" >> "$LOG"
echo "refresh requested: node=nuc from=old -> sha=$TARGET_SHA tag=prod"
echo "WARNING: worker refresh node=nuc not confirmed within 90s (build may still be running)"
exit 0
EOF

# A stub mirroring the CLI's FAILED-at-build path (deploy #212): it prints the
# WARNING, the human-readable tail block, AND the machine-readable
# `worker-refresh-detail:` line update.sh harvests into the leg. Still exit 0.
cat > "$BIN/chug-failed" <<EOF
#!/bin/sh
echo "\$@" >> "$LOG"
echo "refresh requested: node=nuc from=old -> sha=$TARGET_SHA tag=prod"
echo "WARNING: worker refresh node=nuc FAILED at build (prod stays on the old images)"
echo "--- worker-refresh.sh tail (node=nuc, stage=build) ---"
echo "  docker: no space left on device"
echo "--- end worker-refresh.sh tail ---"
echo "worker-refresh-detail: build: docker: no space left on device (~11G free)"
exit 0
EOF

# A stub PUBLISHER of live progress (ticket #253): it prints the per-phase
# progress lines the real CLI relays off the node's `ping`, and — crucially —
# BLOCKS mid-refresh until it can see its own first progress line in
# refresh_workers' stdout. That is the streaming assertion: with the pre-#253
# command substitution the transcript could not appear until the CLI exited, so
# the line would never show up and the marker below would stay unwritten. The
# wait is bounded (10 × 1s) so a regression FAILS the case instead of hanging
# the suite.
export RELAY_OUT="$WORK/out5"
export STREAM_MARK="$WORK/streamed"
cat > "$BIN/chug-progress" <<EOF
#!/bin/sh
echo "\$@" >> "$LOG"
echo "refresh requested: node=nuc from=old -> sha=$TARGET_SHA tag=prod"
echo "refresh progress: node=nuc phase=build-image 1/3 worker, 3s elapsed"
i=0
while [ "\$i" -lt 10 ]; do
  if grep -q 'phase=build-image 1/3 worker' "$RELAY_OUT" 2>/dev/null; then
    echo streamed > "$STREAM_MARK"
    break
  fi
  i=\$((i + 1))
  sleep 1
done
echo "refresh progress: node=nuc still phase=build-image 3/3 agent-rust (240s in phase), 300s elapsed"
echo "refresh OK: node=nuc version=chuggernaut+$TARGET_SHA (312s)"
exit 0
EOF

# A stub mirroring the CLI's TIMEOUT path (#253): the node never confirmed, so
# the CLI prints the last progress it relayed plus a detail line naming the
# phase it is stuck in. Still exit 0 — the absence of "refresh OK:" is what
# fails the leg.
cat > "$BIN/chug-timeout" <<EOF
#!/bin/sh
echo "\$@" >> "$LOG"
echo "refresh requested: node=nuc from=old -> sha=$TARGET_SHA tag=prod"
echo "refresh progress: node=nuc phase=build-image 3/3 agent-rust, 60s elapsed"
echo "WARNING: worker refresh node=nuc not confirmed within 900s (build may still be running)"
echo "--- last progress (node=nuc, phase=build-image 3/3 agent-rust) ---"
echo "  worker-refresh: phase build-image 3/3 agent-rust"
echo "--- end last progress ---"
echo "worker-refresh-detail: not confirmed within 900s; stuck at phase=build-image 3/3 agent-rust (840s in phase); last: worker-refresh: phase build-image 3/3 agent-rust"
exit 0
EOF

# A MULTI-NODE stub for the parallel fan-out cases (ticket #254). One binary for
# the whole fleet: it reads `--node` off its argv and takes that node's
# behaviour from `$WORK/behave.<node>`, so a case scripts each node
# independently. It also serves the `--cancel` invocation, which is what lets a
# case assert that the deploy really cancelled the nodes still building.
export WORK LOG
cat > "$BIN/chug-fleet" <<'EOF'
#!/bin/sh
echo "$@" >> "$LOG"
node=""
cancel=0
while [ $# -gt 0 ]; do
  case "$1" in
  --node) node="$2"; shift 2 ;;
  --cancel) cancel=1; shift ;;
  *) shift ;;
  esac
done
behave="$(cat "$WORK/behave.$node" 2>/dev/null || echo "confirm:0")"
kind="${behave%%:*}"
arg="${behave#*:}"

if [ "$cancel" = 1 ]; then
  : > "$WORK/cancel.$node"
  # A node that already swapped cannot be un-swapped — the daemon declines and
  # the CLI says so (the version-skew record, spec §3.1).
  if [ "$kind" = swapped ]; then
    echo "refresh cancel declined: node=$node — refresh already past the swap — node stays on the new images"
  else
    echo "refresh cancelled: node=$node sha=$TARGET_SHA"
  fi
  exit 0
fi

: > "$WORK/started.$node"
echo "refresh requested: node=$node from=old -> sha=$TARGET_SHA tag=prod"
case "$kind" in
fail)
  sleep "$arg"
  echo "WARNING: worker refresh node=$node FAILED at build (prod stays on the old images)"
  echo "worker-refresh-detail: build: docker: no space left on device (~11G free)"
  ;;
rendezvous)
  # Confirm only once the PEER node's request has ALSO gone out. That is the
  # parallelism assertion: a serial fan-out never starts the peer while this
  # one is waiting, so the case FAILS instead of silently passing on a stopwatch
  # that happened to look fast. Bounded (15 x 1s) so a regression fails rather
  # than hanging the suite.
  i=0
  while [ "$i" -lt 15 ]; do
    [ -f "$WORK/started.$arg" ] && break
    i=$((i + 1))
    sleep 1
  done
  if [ ! -f "$WORK/started.$arg" ]; then
    echo "WARNING: worker refresh node=$node not confirmed (peer $arg never started — serial fan-out?)"
    exit 0
  fi
  sleep "$(cat "$WORK/delay.$node" 2>/dev/null || echo 0)"
  echo "refresh OK: node=$node version=chuggernaut+$TARGET_SHA"
  ;;
deaf)
  # A CLI the cancel does NOT stop: the cancel was never delivered (node or NATS
  # unreachable), or the daemon declined it, so this waiter keeps polling its own
  # node to its full --wait-secs. The deploy has to KILL it — it holds the
  # deploy's stdout, which on a real deploy is the ssh session's pipe. Ticks to a
  # file (not stdout) so the case can see whether it is still alive after
  # refresh_workers returns.
  i=0
  while [ "$i" -lt "$arg" ]; do
    echo tick >> "$WORK/alive.$node"
    i=$((i + 1))
    sleep 1
  done
  echo "WARNING: worker refresh node=$node not confirmed within ${arg}s"
  ;;
cancellable | swapped)
  # A long build that stops the moment the deploy cancels it. Bounded by $arg so
  # a regression (no cancel sent) fails the case instead of hanging.
  i=0
  while [ "$i" -lt "$arg" ]; do
    [ -f "$WORK/cancel.$node" ] && break
    i=$((i + 1))
    sleep 1
  done
  if [ -f "$WORK/cancel.$node" ]; then
    echo "WARNING: worker refresh node=$node FAILED at cancelled (prod stays on the old images)"
    echo "worker-refresh-detail: cancelled: cancelled by the deploy during build"
  else
    echo "refresh OK: node=$node version=chuggernaut+$TARGET_SHA"
  fi
  ;;
*)
  sleep "$arg"
  echo "refresh OK: node=$node version=chuggernaut+$TARGET_SHA"
  ;;
esac
exit 0
EOF

chmod +x "$BIN/chug-confirm" "$BIN/chug-unconfirmed" "$BIN/chug-failed" \
  "$BIN/chug-progress" "$BIN/chug-timeout" "$BIN/chug-fleet"

# The `secs` of one node's emitted leg, whatever its status.
leg_secs() {
  sed -n "s/.*\"name\":\"worker-refresh:$2\",\"status\":\"[a-z]*\",\"secs\":\([0-9]*\).*/\1/p" "$1" | head -1
}

# Reset the fleet stub's per-node scratch between cases.
fleet_reset() {
  rm -f "$WORK"/started.* "$WORK"/cancel.* "$WORK"/behave.* "$WORK"/delay.* \
    "$WORK"/alive.*
  : > "$LOG"
}

pass=0
fail=0

# ── Case 1: a confirmed refresh ⇒ refresh_workers succeeds, passing --wait-secs ─
: > "$LOG"
if CHUG_BIN="$BIN/chug-confirm" refresh_workers >"$WORK/out1" 2>&1; then
  # Anchored with a trailing space via grep -E word boundary: a bare -F
  # "--wait-secs 90" also matches "--wait-secs 900" as a substring (#207
  # review) and would pass no matter what the default became.
  if grep -qE -- "--wait-secs 900( |$)" "$LOG"; then
    echo "ok   - confirmed refresh returns 0 and passes --wait-secs 900"
    pass=$((pass + 1))
  else
    echo "FAIL - confirmed refresh must pass --wait-secs 900 (found no such flag in the call)"
    cat "$LOG"
    fail=$((fail + 1))
  fi
else
  echo "FAIL - a confirmed refresh must return 0 (got non-zero)"
  cat "$WORK/out1"
  fail=$((fail + 1))
fi

# Only the `worker` row is contacted — the dispatcher endpoint is skipped.
if [ "$(grep -c 'worker-refresh' "$LOG")" = 1 ] && grep -qF -- "--node nuc" "$LOG"; then
  echo "ok   - only the worker node is refreshed (non-worker rows skipped)"
  pass=$((pass + 1))
else
  echo "FAIL - refresh_workers should contact exactly the one worker node (nuc)"
  cat "$LOG"
  fail=$((fail + 1))
fi

# ── Case 2: an UNconfirmed refresh (CLI still exit 0) ⇒ refresh_workers FAILS ──
: > "$LOG"
if CHUG_BIN="$BIN/chug-unconfirmed" refresh_workers >"$WORK/out2" 2>&1; then
  echo "FAIL - an unconfirmed refresh must FAIL the step (refresh_workers returned 0)"
  cat "$WORK/out2"
  fail=$((fail + 1))
else
  if grep -qF "NOT confirmed" "$WORK/out2"; then
    echo "ok   - unconfirmed refresh fails the step loudly (non-zero, no WARNING-and-continue)"
    pass=$((pass + 1))
  else
    echo "FAIL - unconfirmed refresh should explain the failure"
    cat "$WORK/out2"
    fail=$((fail + 1))
  fi
fi

# ── Case 4: a FAILED refresh with a captured tail ⇒ leg carries the detail ────
: > "$LOG"
if CHUG_BIN="$BIN/chug-failed" refresh_workers >"$WORK/out4" 2>&1; then
  echo "FAIL - a failed refresh must FAIL the step (refresh_workers returned 0)"
  cat "$WORK/out4"
  fail=$((fail + 1))
else
  # The emitted worker-refresh leg must be `failed` AND carry the harvested
  # detail tail (deploy #212), not just the generic "refresh not confirmed".
  _leg="$(grep '@chug:leg' "$WORK/out4" | grep 'worker-refresh:nuc' | grep '"status":"failed"')"
  if printf '%s' "$_leg" | grep -q '"detail":"build: docker: no space left on device' \
    && printf '%s' "$_leg" | grep -q '"error":"refresh not confirmed"'; then
    echo "ok   - failed refresh leg carries the captured detail tail"
    pass=$((pass + 1))
  else
    echo "FAIL - failed refresh leg should carry a detail field with the tail"
    printf 'leg was: %s\n' "$_leg"
    cat "$WORK/out4"
    fail=$((fail + 1))
  fi
fi

# ── Case 3: no worker rows at all ⇒ clean no-op success ───────────────────────
: > "$LOG"
if DOCKER_NODES="" CHUG_BIN="$BIN/chug-confirm" refresh_workers >/dev/null 2>&1; then
  if [ ! -s "$LOG" ]; then
    echo "ok   - empty DOCKER_NODES is a clean no-op (no refresh calls)"
    pass=$((pass + 1))
  else
    echo "FAIL - empty DOCKER_NODES must make no refresh calls"
    cat "$LOG"
    fail=$((fail + 1))
  fi
else
  echo "FAIL - empty DOCKER_NODES must return 0 (clean no-op)"
  fail=$((fail + 1))
fi

# ── Case 5: live progress is RELAYED as it is produced, not buffered ─────────
: > "$LOG"
rm -f "$STREAM_MARK"
if CHUG_BIN="$BIN/chug-progress" refresh_workers >"$RELAY_OUT" 2>&1; then
  if [ "$(cat "$STREAM_MARK" 2>/dev/null)" = "streamed" ]; then
    echo "ok   - progress reaches the deploy log WHILE the refresh runs (not buffered to the end)"
    pass=$((pass + 1))
  else
    echo "FAIL - refresh_workers must stream the CLI's output live (the publisher never saw its own line relayed)"
    cat "$RELAY_OUT"
    fail=$((fail + 1))
  fi
  # Every relayed line lands in the deploy task's output, in order, and the
  # heartbeat carries its elapsed time.
  if grep -qF "phase=build-image 1/3 worker, 3s elapsed" "$RELAY_OUT" \
    && grep -qF "still phase=build-image 3/3 agent-rust (240s in phase), 300s elapsed" "$RELAY_OUT"; then
    echo "ok   - per-phase progress and elapsed-time heartbeats appear in the task output"
    pass=$((pass + 1))
  else
    echo "FAIL - relayed progress lines missing from the deploy task output"
    cat "$RELAY_OUT"
    fail=$((fail + 1))
  fi
else
  echo "FAIL - a confirmed refresh with progress must still return 0"
  cat "$RELAY_OUT"
  fail=$((fail + 1))
fi

# ── Case 6: a leg that never confirms keeps its last progress ────────────────
: > "$LOG"
if CHUG_BIN="$BIN/chug-timeout" refresh_workers >"$WORK/out6" 2>&1; then
  echo "FAIL - a refresh that never confirms must FAIL the step (unchanged semantics)"
  cat "$WORK/out6"
  fail=$((fail + 1))
else
  _leg="$(grep '@chug:leg' "$WORK/out6" | grep 'worker-refresh:nuc' | grep '"status":"failed"')"
  # Diagnosis starts from the job page: the failing leg names the phase it was
  # stuck in, and the last progress block is in the task output itself.
  if printf '%s' "$_leg" | grep -q '"detail":"not confirmed within 900s; stuck at phase=build-image 3/3 agent-rust' \
    && grep -qF -- "--- last progress (node=nuc, phase=build-image 3/3 agent-rust) ---" "$WORK/out6"; then
    echo "ok   - a never-confirmed leg carries its last progress (phase + lines) into the failure"
    pass=$((pass + 1))
  else
    echo "FAIL - timed-out leg should carry the stuck phase in its detail and the last progress in the log"
    printf 'leg was: %s\n' "$_leg"
    cat "$WORK/out6"
    fail=$((fail + 1))
  fi
fi

# ── The parallel fan-out (ticket #254) ───────────────────────────────────────
# From here on the fleet has TWO worker nodes (plus a non-worker row that must
# still be skipped), driven by the multi-node stub above.
export DOCKER_NODES="nuc|worker|4, air|worker|2, mini|dispatcher|0"
export CHUG_LEGS_PENDING="worker-refresh:nuc worker-refresh:air"

# ── Case 7: every node is requested UP FRONT, and each leg carries its own time ─
fleet_reset
echo "rendezvous:air" > "$WORK/behave.nuc"
echo "rendezvous:nuc" > "$WORK/behave.air"
echo 0 > "$WORK/delay.nuc"
echo 4 > "$WORK/delay.air"
if CHUG_BIN="$BIN/chug-fleet" refresh_workers >"$WORK/out7" 2>&1; then
  # Each node's stub blocks until it sees the OTHER node's request — so a
  # confirmed run PROVES both refreshes were in flight at the same time.
  if grep -q '"name":"worker-refresh:nuc","status":"ok"' "$WORK/out7" \
    && grep -q '"name":"worker-refresh:air","status":"ok"' "$WORK/out7"; then
    echo "ok   - both nodes are requested up front and confirm concurrently"
    pass=$((pass + 1))
  else
    echo "FAIL - a parallel fan-out should emit an ok leg per node"
    cat "$WORK/out7"
    fail=$((fail + 1))
  fi
else
  echo "FAIL - a fleet that all confirms must return 0 (serial fan-out? the peer rendezvous timed out)"
  cat "$WORK/out7"
  fail=$((fail + 1))
fi

# Per-node timing attribution: the fast node's leg must carry ITS OWN elapsed
# time, not the fan-out's — emitting every leg against one shared clock after
# `wait` would bill nuc for air's four seconds.
_nuc_secs="$(leg_secs "$WORK/out7" nuc)"
_air_secs="$(leg_secs "$WORK/out7" air)"
if [ -n "$_nuc_secs" ] && [ -n "$_air_secs" ] \
  && [ "$_nuc_secs" -le 2 ] && [ "$_air_secs" -ge 3 ]; then
  echo "ok   - per-node legs carry per-node secs (nuc ${_nuc_secs}s, air ${_air_secs}s)"
  pass=$((pass + 1))
else
  echo "FAIL - leg secs must be per node (got nuc='$_nuc_secs' air='$_air_secs'; expected nuc<=2, air>=3)"
  cat "$WORK/out7"
  fail=$((fail + 1))
fi

# ── Case 8: one node's failure CANCELS the refreshes still building ───────────
fleet_reset
export CHUG_LEGS_PENDING="worker-refresh:nuc worker-refresh:air"
echo "fail:1" > "$WORK/behave.nuc"
echo "cancellable:60" > "$WORK/behave.air"
_t0="$(date +%s)"
if CHUG_BIN="$BIN/chug-fleet" refresh_workers >"$WORK/out8" 2>&1; then
  echo "FAIL - a node that never confirms must still FAIL the deploy step"
  cat "$WORK/out8"
  fail=$((fail + 1))
else
  _elapsed=$(( $(date +%s) - _t0 ))
  # The whole point: air's 60s build is cut short instead of run to completion
  # against a deploy that is already failing.
  if grep -qE -- "--cancel.*--node air|--node air.*--cancel" "$LOG" && [ "$_elapsed" -lt 30 ]; then
    echo "ok   - the first failure cancels the nodes still in flight (${_elapsed}s, not 60)"
    pass=$((pass + 1))
  else
    echo "FAIL - a failing node must cancel the rest (no --cancel for air, or it ran to completion in ${_elapsed}s)"
    cat "$LOG"
    fail=$((fail + 1))
  fi
  # The node that FAILED is already finished — cancelling it would be noise.
  if grep -- '--cancel' "$LOG" | grep -q -- '--node nuc'; then
    echo "FAIL - the failed node must not be cancelled (it already reported)"
    cat "$LOG"
    fail=$((fail + 1))
  else
    echo "ok   - only the still-in-flight nodes are cancelled"
    pass=$((pass + 1))
  fi
  # Deploy-level semantics unchanged: both nodes fail the deploy, and the
  # cancelled one names the node that caused the abort.
  _leg="$(grep '@chug:leg' "$WORK/out8" | grep 'worker-refresh:air' | grep '"status":"failed"')"
  if printf '%s' "$_leg" | grep -q '"error":"refresh cancelled"' \
    && printf '%s' "$_leg" | grep -q "worker 'nuc' did not confirm"; then
    echo "ok   - a cancelled node's leg is failed and names the node that aborted the deploy"
    pass=$((pass + 1))
  else
    echo "FAIL - the cancelled leg should be failed and name the culprit node"
    printf 'leg was: %s\n' "$_leg"
    cat "$WORK/out8"
    fail=$((fail + 1))
  fi
  # And no leg is left behind for the exit trap to mark skipped.
  if [ -z "$(printf '%s' "$CHUG_LEGS_PENDING" | tr -d '[:space:]')" ]; then
    echo "ok   - every node's leg is emitted and dropped in the MAIN shell (no subshell loss)"
    pass=$((pass + 1))
  else
    echo "FAIL - worker-refresh legs left pending after the fan-out: '$CHUG_LEGS_PENDING'"
    fail=$((fail + 1))
  fi
fi

# ── Case 9: a node that already swapped stays swapped, and the leg says so ────
# The version-skew invariant (spec §3.1): the swap is deliberately NOT two-phase,
# so a failed deploy can leave a node ahead of the dispatcher. When that happens
# the cancel is DECLINED and the reason has to survive into the deploy record —
# otherwise the operator cannot tell which nodes moved.
fleet_reset
export CHUG_LEGS_PENDING="worker-refresh:nuc worker-refresh:air"
echo "fail:1" > "$WORK/behave.nuc"
echo "swapped:60" > "$WORK/behave.air"
if CHUG_BIN="$BIN/chug-fleet" refresh_workers >"$WORK/out9" 2>&1; then
  echo "FAIL - an already-swapped node does not rescue a failing deploy"
  cat "$WORK/out9"
  fail=$((fail + 1))
else
  _leg="$(grep '@chug:leg' "$WORK/out9" | grep 'worker-refresh:air' | grep '"status":"failed"')"
  if printf '%s' "$_leg" | grep -q 'already past the swap'; then
    echo "ok   - a node that swapped before its cancel arrived records the skew in its leg detail"
    pass=$((pass + 1))
  else
    echo "FAIL - the leg detail should carry the declined cancel (node stays on the new images)"
    printf 'leg was: %s\n' "$_leg"
    cat "$WORK/out9"
    fail=$((fail + 1))
  fi
fi

# ── Case 10: a waiter the cancel does not stop is KILLED, not waited out ──────
# The cancel is best-effort: it can be undelivered, or declined because the node
# is converging on another deploy's sha. Either way the waiter never sees a
# verdict for OUR sha and would run its full --wait-secs — with the deploy's
# stdout (the ssh session's pipe) held open behind it. The drain is bounded and
# the leftovers are killed, so refresh_workers returns on the drain bound.
fleet_reset
export CHUG_LEGS_PENDING="worker-refresh:nuc worker-refresh:air"
echo "fail:1" > "$WORK/behave.nuc"
echo "deaf:600" > "$WORK/behave.air"
_t0="$(date +%s)"
if WORKER_REFRESH_CANCEL_WAIT_SECS=3 CHUG_BIN="$BIN/chug-fleet" \
  refresh_workers >"$WORK/out10" 2>&1; then
  echo "FAIL - a deploy with an uncancellable node must still fail"
  cat "$WORK/out10"
  fail=$((fail + 1))
else
  _elapsed=$(( $(date +%s) - _t0 ))
  if [ "$_elapsed" -lt 30 ]; then
    echo "ok   - an undeliverable cancel still returns on the drain bound (${_elapsed}s, not 600)"
    pass=$((pass + 1))
  else
    echo "FAIL - refresh_workers waited out an uncancellable node (${_elapsed}s)"
    fail=$((fail + 1))
  fi
  # And the waiter is really gone: killing the background JOB alone leaves the
  # CLI and its `tee` reparented and still running (and still holding stdout),
  # which is what the tick file catches.
  _ticks="$(wc -l < "$WORK/alive.air" 2>/dev/null || echo 0)"
  sleep 3
  _ticks_after="$(wc -l < "$WORK/alive.air" 2>/dev/null || echo 0)"
  if [ "$_ticks_after" -eq "$_ticks" ]; then
    echo "ok   - the uncancellable waiter is killed, not orphaned (no ticks after the deploy gave up)"
    pass=$((pass + 1))
  else
    echo "FAIL - the CLI survived the fan-out (ticks $_ticks -> $_ticks_after); killing \$! only kills the subshell"
    fail=$((fail + 1))
  fi
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

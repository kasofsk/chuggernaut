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

chmod +x "$BIN/chug-confirm" "$BIN/chug-unconfirmed" "$BIN/chug-failed" \
  "$BIN/chug-progress" "$BIN/chug-timeout"

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

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

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

chmod +x "$BIN/chug-confirm" "$BIN/chug-unconfirmed"

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

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

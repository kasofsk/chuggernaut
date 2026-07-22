#!/bin/sh
# Shell test for restart-verify.sh — no NATS, no Docker, no launchd required.
#
# It drives restart-verify.sh with a fake `launchctl` on PATH (a no-op) and an
# injected HEALTHCHECK_CMD that reports health by inspecting the installed
# "binary": a file whose contents are GOOD (a dispatcher that comes up) or BAD
# (one that crash-loops). Rollback works by copying chuggernaut.prev over
# chuggernaut, so a GOOD .prev heals a BAD build exactly as in prod.
#
# Run:  deploy/prod/restart-verify.test.sh   (exits 0 if all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/restart-verify.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Fake launchctl (kickstart is a no-op in the test — nothing to supervise).
mkdir -p "$WORK/bin"
cat > "$WORK/bin/launchctl" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$WORK/bin/launchctl"

# A repo whose "binary" is just a text file we can mark GOOD or BAD.
REPO="$WORK/repo"
BIN="$REPO/target/release/chuggernaut"
mkdir -p "$REPO/target/release"

pass=0
fail=0
check() { # <name> <expected-rc> <actual-rc> <output-file> <must-contain>
  name="$1"; want="$2"; got="$3"; out="$4"; needle="$5"
  if [ "$got" = "$want" ] && grep -qF "$needle" "$out"; then
    echo "ok   - $name (rc=$got)"
    pass=$((pass + 1))
  else
    echo "FAIL - $name: rc want=$want got=$got; expected output to contain: $needle"
    echo "----- output -----"; cat "$out"; echo "------------------"
    fail=$((fail + 1))
  fi
}

# The injected probe: healthy iff the installed binary reads GOOD. Re-reads the
# file each call, so a rollback that overwrites it flips the result — just like
# a real dispatcher coming up on the restored binary.
run_sut() { # <target-sha> <prev-sha> -> writes rc to $RC, output to $OUT
  OUT="$WORK/out"
  set +e
  PATH="$WORK/bin:$PATH" \
  CHUG_HEALTH_NO_ENV=1 \
  CHUG_REPO="$REPO" \
  HEALTH_TIMEOUT_SECS=0 \
  HEALTH_INTERVAL_SECS=0 \
  HEALTHCHECK_CMD="grep -q GOOD '$BIN'" \
    "$SUT" "$1" "$2" >"$OUT" 2>&1
  RC=$?
  set -e
}

# 1. Healthy new build -> exit 0.
printf GOOD > "$BIN"
printf GOOD > "$BIN.prev"
run_sut sha-new sha-old
check "healthy build passes" 0 "$RC" "$OUT" "is healthy"

# 2. Bad build, good previous binary -> rollback heals, exit 1 with the story.
printf BAD  > "$BIN"
printf GOOD > "$BIN.prev"
run_sut sha-new sha-old
check "bad build rolls back and passes" 1 "$RC" "$OUT" \
  "new build failed health check, rolled back to sha-old, now healthy"
if grep -q GOOD "$BIN"; then
  echo "ok   - rollback restored the previous binary"
  pass=$((pass + 1))
else
  echo "FAIL - rollback did not restore the previous binary"
  fail=$((fail + 1))
fi

# 3. Bad build AND bad previous binary -> rollback also fails, loudest exit 2.
printf BAD > "$BIN"
printf BAD > "$BIN.prev"
run_sut sha-new sha-old
check "bad build + bad rollback shouts (exit 2)" 2 "$RC" "$OUT" "CATASTROPHE"

# 4. Bad build, no previous binary at all -> rollback impossible, exit 3.
printf BAD > "$BIN"
rm -f "$BIN.prev"
run_sut sha-new sha-old
check "bad build + no prior binary shouts (exit 3)" 3 "$RC" "$OUT" "ROLLBACK IMPOSSIBLE"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

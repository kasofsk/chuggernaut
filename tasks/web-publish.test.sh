#!/bin/sh
# Shell test for web-publish.sh — no npm, no ssh, no Mini.
#
# It drives web-publish.sh with stub `git`, `npm`, and `ssh` on PATH and a fake
# web/dist, then asserts the #186 wipe-hazard fix: the served UI can NEVER be
# replaced by an empty/index-less tree. A staged tarball that is empty or missing
# index.html must make the publish REFUSE before the remote `rsync --delete` ever
# runs (that --delete is what would wipe the live UI). The happy path still ships.
#
# Run:  tasks/web-publish.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/web-publish.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
mkdir -p "$BIN"
LOG="$WORK/ssh.log"

# Stub git: rev-parse HEAD -> a fixed SHA. Stub npm: no-op (dist is pre-staged).
cat > "$BIN/git" <<'EOF'
#!/bin/sh
[ "$1" = rev-parse ] && echo "cafef00d"
exit 0
EOF
cat > "$BIN/npm" <<'EOF'
#!/bin/sh
exit 0
EOF
# Stub ssh: consume stdin (the streamed tarball) and log that it ran — its
# presence in the log means the remote --delete swap was reached.
cat > "$BIN/ssh" <<EOF
#!/bin/sh
cat >/dev/null 2>&1 || true
echo "ssh ran" >> "$LOG"
exit 0
EOF
chmod +x "$BIN/git" "$BIN/npm" "$BIN/ssh"

# A dummy key file so the script skips key materialization and takes the SSH path.
KEYF="$WORK/key"
: > "$KEYF"

PROJ="$WORK/proj"
mkdir -p "$PROJ/web/dist"

fail=0
pass=0

run_publish() { # runs the SUT from $PROJ with the stubs; writes rc/out
  OUT="$WORK/out"
  : > "$LOG"
  set +e
  ( cd "$PROJ" && PATH="$BIN:$PATH" MINI_DEPLOY_KEY_FILE="$KEYF" sh "$SUT" ) \
    >"$OUT" 2>&1
  RC=$?
  set -e
}

# ── Case 1: dist has no index.html ⇒ REFUSE, do NOT reach the remote --delete ──
rm -rf "$PROJ/web/dist"; mkdir -p "$PROJ/web/dist"
printf 'x' > "$PROJ/web/dist/orphan-asset.js"   # content, but no index.html
run_publish
if [ "$RC" -ne 0 ] && grep -qF "no index.html" "$OUT"; then
  echo "ok   - index-less dist is refused (rc=$RC)"
  pass=$((pass + 1))
else
  echo "FAIL - index-less dist must be refused"
  cat "$OUT"
  fail=$((fail + 1))
fi
if [ -s "$LOG" ]; then
  echo "FAIL - refused publish must NOT reach ssh/rsync --delete (would wipe the UI)"
  fail=$((fail + 1))
else
  echo "ok   - refused publish never reached the remote --delete"
  pass=$((pass + 1))
fi

# ── Case 2: empty dist ⇒ REFUSE (an empty --delete sync would wipe the UI) ─────
rm -rf "$PROJ/web/dist"; mkdir -p "$PROJ/web/dist"
run_publish
if [ "$RC" -ne 0 ]; then
  echo "ok   - empty dist is refused (rc=$RC)"
  pass=$((pass + 1))
else
  echo "FAIL - empty dist must be refused"
  cat "$OUT"
  fail=$((fail + 1))
fi
if [ -s "$LOG" ]; then
  echo "FAIL - refused (empty) publish must NOT reach ssh/rsync --delete"
  fail=$((fail + 1))
else
  echo "ok   - refused (empty) publish never reached the remote --delete"
  pass=$((pass + 1))
fi

# ── Case 3: a real dist with index.html ⇒ ships (ssh/rsync reached) ───────────
rm -rf "$PROJ/web/dist"; mkdir -p "$PROJ/web/dist/assets"
printf '<!doctype html>' > "$PROJ/web/dist/index.html"
printf 'body{}' > "$PROJ/web/dist/assets/app.css"
run_publish
if [ "$RC" -eq 0 ] && grep -qF "is live" "$OUT" && [ -s "$LOG" ]; then
  echo "ok   - a valid dist ships to the remote swap (rc=0)"
  pass=$((pass + 1))
else
  echo "FAIL - a valid dist must publish"
  cat "$OUT"
  fail=$((fail + 1))
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

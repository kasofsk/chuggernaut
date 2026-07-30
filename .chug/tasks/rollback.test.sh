#!/bin/sh
# Shell test for rollback.sh — no Mini, no ssh, no network.
#
# It builds a throwaway two-repo git fixture (an "upstream" with a main history
# plus a side branch, and a clone of it standing in for the job branch's
# workspace), then drives rollback.sh with a stub `ssh` on PATH. The property
# under test is the one that makes an irreversible external effect safe to
# automate: a rollback either ships a commit that is genuinely on main, or it
# ships NOTHING and says why. The stub's log is the proof — an empty log means
# the ssh was never reached.
#
# The four refusals/passes below map to the four ways an operator can get this
# wrong: no value at all, a value that resolves to nothing, a value that
# resolves to a commit that never reached main, and the one good case.
#
# Run:  .chug/tasks/rollback.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/rollback.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$WORK/bin"
mkdir -p "$BIN"
LOG="$WORK/ssh.log"

# Stub ssh: log the whole invocation (the remote `update.sh <sha>` command is the
# last argument) and succeed. Its presence in the log means the external effect
# would have happened for real.
cat > "$BIN/ssh" <<EOF
#!/bin/sh
echo "ssh \$*" >> "$LOG"
exit 0
EOF
chmod +x "$BIN/ssh"

# A dummy key file so deploy.sh (which rollback.sh hands off to) takes the ssh
# path without materializing a key.
KEYF="$WORK/key"
: > "$KEYF"

# --- fixture: an upstream with main = A..B and an off-main commit C -----------
export GIT_AUTHOR_NAME="test" GIT_AUTHOR_EMAIL="test@example.com"
export GIT_COMMITTER_NAME="test" GIT_COMMITTER_EMAIL="test@example.com"
UP="$WORK/upstream"
git init -q -b main "$UP"
git -C "$UP" config uploadpack.allowfilter true   # rollback.sh fetches --filter=blob:none
echo a > "$UP/f"; git -C "$UP" add f; git -C "$UP" commit -qm "commit A"
SHA_A="$(git -C "$UP" rev-parse HEAD)"
echo b > "$UP/f"; git -C "$UP" commit -qam "commit B"
SHA_B="$(git -C "$UP" rev-parse HEAD)"
git -C "$UP" checkout -q -b side
echo c > "$UP/f"; git -C "$UP" commit -qam "commit C (never on main)"
SHA_C="$(git -C "$UP" rev-parse HEAD)"
git -C "$UP" checkout -q main

PROJ="$WORK/proj"
git clone -q "$UP" "$PROJ"

fail=0
pass=0

run_rollback() { # run_rollback [sha]; unset input when no argument. Sets RC/OUT.
  OUT="$WORK/out"
  : > "$LOG"
  set +e
  if [ "$#" -eq 0 ]; then
    ( cd "$PROJ" && PATH="$BIN:$PATH" MINI_DEPLOY_KEY_FILE="$KEYF" \
      sh -c 'unset CHUG_INPUT_SHA; exec "$0"' "$SUT" ) >"$OUT" 2>&1
  else
    ( cd "$PROJ" && PATH="$BIN:$PATH" MINI_DEPLOY_KEY_FILE="$KEYF" \
      CHUG_INPUT_SHA="$1" "$SUT" ) >"$OUT" 2>&1
  fi
  RC=$?
  set -e
}

check_refused() { # check_refused <label> <expected substring>
  if [ "$RC" -ne 0 ] && grep -qF "$2" "$OUT"; then
    echo "ok   - $1 (rc=$RC)"
    pass=$((pass + 1))
  else
    echo "FAIL - $1"
    cat "$OUT"
    fail=$((fail + 1))
  fi
  if [ -s "$LOG" ]; then
    echo "FAIL - $1: refused rollback must NOT reach ssh (the effect is external and irreversible)"
    fail=$((fail + 1))
  else
    echo "ok   - $1: never reached ssh"
    pass=$((pass + 1))
  fi
}

# ── Case 1: no input at all ⇒ `set -u` aborts before anything external ────────
# An unresolved input injects no env key (#311 "absent means absent"), so this is
# the real shape of a missing required value, not a contrived one.
run_rollback
check_refused "unset CHUG_INPUT_SHA aborts" "CHUG_INPUT_SHA"

# ── Case 2: a well-formed SHA that is not a commit here ⇒ refuse ─────────────
# The Mini's update.sh would silently fall back to origin/main for this one.
run_rollback "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
check_refused "unresolvable SHA is refused" "is not a commit in this repository"

# ── Case 3: a real commit that never reached main ⇒ refuse ──────────────────
run_rollback "$SHA_C"
check_refused "off-main commit is refused" "is not on main's history"

# ── Case 4: an abbreviated SHA of a real main ancestor ⇒ ship the FULL sha ───
SHORT_A="$(printf '%s' "$SHA_A" | cut -c1-7)"
run_rollback "$SHORT_A"
if [ "$RC" -eq 0 ] && grep -qF "$SHA_A" "$LOG"; then
  echo "ok   - main ancestor ships, abbreviation expanded to the full SHA"
  pass=$((pass + 1))
else
  echo "FAIL - main ancestor must ship as the resolved full SHA"
  cat "$OUT"; cat "$LOG" 2>/dev/null || true
  fail=$((fail + 1))
fi
if grep -qF "1 commit(s) behind main ($SHA_B)" "$OUT"; then
  echo "ok   - announced the target's distance from main before shipping"
  pass=$((pass + 1))
else
  echo "FAIL - the run must say what it is about to do (distance from main)"
  cat "$OUT"
  fail=$((fail + 1))
fi

# ── Case 5: deploy.sh with NO argument still ships HEAD ─────────────────────
# rollback.sh hands its resolved SHA to deploy.sh as $1, which is the only reason
# deploy.sh takes an argument at all. Assert the default path is untouched: a
# plain `deploy` job passes nothing and must still ship the checkout's HEAD.
: > "$LOG"
set +e
( cd "$PROJ" && PATH="$BIN:$PATH" MINI_DEPLOY_KEY_FILE="$KEYF" sh "$HERE/deploy.sh" ) \
  >"$WORK/out" 2>&1
RC=$?
set -e
if [ "$RC" -eq 0 ] && grep -qF "$SHA_B" "$LOG"; then
  echo "ok   - deploy.sh with no argument still ships HEAD"
  pass=$((pass + 1))
else
  echo "FAIL - deploy.sh must ship HEAD when called with no argument"
  cat "$WORK/out"
  fail=$((fail + 1))
fi

echo
echo "rollback.test: $pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1

#!/bin/sh
# Shell test for the two launchd wrappers, run-dispatcher.sh and run-api.sh —
# no launchd, no NATS, no built binary.
#
# What it pins is the LOG LEVEL the services are actually started with (#493).
# The binary filters on RUST_LOG and its default directive is ERROR, so a
# service started without one discards every warn! and info! it emits — the
# failure ticket #270 fixed for the worker daemon and left standing on the
# Mini's two services. So per wrapper: the default is applied when
# chuggernaut.env declares no RUST_LOG (this case fails against the unfixed
# wrappers, which hand the binary nothing), and an env-file value WINS over it
# (this one fails against the obvious wrong fix — a hard `export RUST_LOG=…`
# that shadows the operator). Both are read off the exec'd process's own
# environment, so a value that is set but not exported fails too.
#
# It also asserts the two decisions that keep the three services reading one
# way: the string is the same one build-worker.sh writes into a worker node's
# environment file (extracted from that script, not restated here), and the
# plists carry no RUST_LOG — launchd's EnvironmentVariables would shadow
# chuggernaut.env rather than yield to it.
#
# Run:  deploy/prod/run-wrappers.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A fake checkout. The wrappers derive HERE from their own path and REPO from
# HERE/../.., so copying them into one makes every path they touch ours: the
# env file they source, the readiness gate they run, the binary they exec.
FAKE="$WORK/deploy/prod"
mkdir -p "$FAKE" "$WORK/target/release" "$WORK/ui"
cp "$HERE/run-dispatcher.sh" "$HERE/run-api.sh" "$FAKE/"

cat > "$FAKE/wait-nats.sh" <<'EOF'
#!/bin/sh
exit 0
EOF

# The exec'd "binary" reports the environment it was handed. It is a CHILD
# process, so a variable reaches it only if the wrapper exported it.
cat > "$WORK/target/release/chuggernaut" <<'EOF'
#!/bin/sh
echo "argv=$*"
echo "RUST_LOG=${RUST_LOG-<unset>}"
EOF

chmod +x "$FAKE/wait-nats.sh" "$FAKE/run-dispatcher.sh" "$FAKE/run-api.sh" \
  "$WORK/target/release/chuggernaut"

# The fleet-wide default, read out of the script that writes it into every
# worker node's run spec — so this suite fails if the two halves ever diverge
# rather than agreeing with a copy of the old string.
WORKER_DEFAULT="$(sed -n 's/^spec_line RUST_LOG ".*:-\(.*\)}"$/\1/p' "$HERE/build-worker.sh")"
[ -n "$WORKER_DEFAULT" ] || {
  echo "FAIL - could not read the worker's RUST_LOG default out of build-worker.sh" >&2
  exit 1
}

pass=0
fail=0
check() { # <name> <expected-line> <output-file>
  name="$1"; needle="$2"; out="$3"
  if grep -qFx -- "$needle" "$out"; then
    echo "ok   - $name"
    pass=$((pass + 1))
  else
    echo "FAIL - $name: expected a line reading exactly: $needle"
    echo "----- output -----"; cat "$out"; echo "------------------"
    fail=$((fail + 1))
  fi
}

# Run one wrapper against a chuggernaut.env holding <env-body>. RUST_LOG is
# unset in the harness first: launchd hands these services HOME and PATH and
# nothing else, so an ambient value would model a machine prod is not.
run_wrapper() { # <dispatcher|api> <env-body> -> OUT
  printf '%s\n' "UI_ROOT=$WORK/ui" "$2" > "$FAKE/chuggernaut.env"
  OUT="$WORK/out"
  (
    unset RUST_LOG
    HOME="$WORK" PATH="$PATH" sh "$FAKE/run-$1.sh"
  ) > "$OUT" 2>&1
}

# ── Case 1: no RUST_LOG in the env file ⇒ the wrapper supplies the default ────
run_wrapper dispatcher ""
check "dispatcher: exec'd as the dispatcher" "argv=dispatcher" "$OUT"
check "dispatcher: default level applied when the env file declares none" \
  "RUST_LOG=$WORKER_DEFAULT" "$OUT"

run_wrapper api ""
check "api: exec'd as the api" "argv=api" "$OUT"
check "api: default level applied when the env file declares none" \
  "RUST_LOG=$WORKER_DEFAULT" "$OUT"

# ── Case 2: an env-file value WINS ────────────────────────────────────────────
# The operator keeps control: the default is a floor for a fresh install, never
# an override of what chuggernaut.env says.
run_wrapper dispatcher "RUST_LOG=info,dispatcher=debug"
check "dispatcher: an env-file RUST_LOG wins over the default" \
  "RUST_LOG=info,dispatcher=debug" "$OUT"

run_wrapper api "RUST_LOG=warn"
check "api: an env-file RUST_LOG wins over the default" "RUST_LOG=warn" "$OUT"

# ── Case 3: the plists declare no RUST_LOG ───────────────────────────────────
# launchd's EnvironmentVariables are applied to the process the wrapper runs in,
# so a default declared there would shadow an operator's chuggernaut.env instead
# of yielding to it — which is why the wrappers own it.
for label in dispatcher api; do
  tmpl="$HERE/launchd/com.chuggernaut.$label.plist.template"
  if grep -q "RUST_LOG" "$tmpl"; then
    echo "FAIL - $label: RUST_LOG must not be set in the plist (it would shadow chuggernaut.env)"
    fail=$((fail + 1))
  else
    echo "ok   - $label: the plist declares no RUST_LOG"
    pass=$((pass + 1))
  fi
done

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

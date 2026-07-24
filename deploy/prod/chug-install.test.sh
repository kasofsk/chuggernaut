#!/bin/sh
# Shell test for chug-install.sh preflight — no real deps, no live stack.
#
# It builds an isolated fake repo tree (chug-install.sh derives REPO from its own
# location), drops in a stub `chuggernaut` binary whose `validate` FAILS, plus a
# jobs/*.yaml to validate, and stubs the required deps on PATH. Then it asserts
# the #186 contract: a config-validation failure is FATAL unless --force.
#
# Run:  deploy/prod/chug-install.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Isolated fake repo: SUT copied so its REPO ($HERE/../..) is our sandbox.
REPO="$WORK/repo"
mkdir -p "$REPO/deploy/prod" "$REPO/target/release" "$REPO/jobs"
SUT="$REPO/deploy/prod/chug-install.sh"
cp "$HERE/chug-install.sh" "$SUT"
chmod +x "$SUT"

# Stub chuggernaut binary: `validate` always fails, everything else succeeds.
cat > "$REPO/target/release/chuggernaut" <<'EOF'
#!/bin/sh
[ "$1" = validate ] && exit 1
exit 0
EOF
chmod +x "$REPO/target/release/chuggernaut"

# A job file to validate (not _defaults.yaml, which preflight skips).
printf 'job_type: demo\n' > "$REPO/jobs/demo.yaml"

# Required deps as no-op stubs so preflight's dependency gate passes.
BIN="$WORK/bin"
mkdir -p "$BIN"
for d in git docker node age curl; do
  printf '#!/bin/sh\nexit 0\n' > "$BIN/$d"
  chmod +x "$BIN/$d"
done

# Minimal env file with the vars preflight looks for (avoids unrelated warnings).
ENVF="$WORK/chuggernaut.env"
cat > "$ENVF" <<EOF
NATS_URL=nats://localhost:4222
REPO_URL_BASE=ssh://git@localhost:2222
AGENT_PROVIDER_DEFAULT=anthropic
REPOS_ROOT=$WORK/repos
KEYS_DIR=$WORK/keys
EOF

pass=0
fail=0
run() { # <label> ...args-to-SUT-> RC/OUT
  OUT="$WORK/out"
  set +e
  PATH="$BIN:$PATH" sh "$SUT" "$@" >"$OUT" 2>&1
  RC=$?
  set -e
}

# ── Case 1: validation failure WITHOUT --force ⇒ FATAL (non-zero, loud) ────────
run --env "$ENVF" preflight
if [ "$RC" -ne 0 ] && grep -qF "validation FAILED" "$OUT"; then
  echo "ok   - config validation failure is fatal without --force (rc=$RC)"
  pass=$((pass + 1))
else
  echo "FAIL - validation failure must be fatal without --force (rc=$RC)"
  cat "$OUT"
  fail=$((fail + 1))
fi

# ── Case 2: same failure WITH --force ⇒ downgraded to a warning, exit 0 ────────
run --force --env "$ENVF" preflight
if [ "$RC" -eq 0 ] && grep -qF "preflight OK" "$OUT"; then
  echo "ok   - --force downgrades the validation failure to a warning (rc=0, preflight OK)"
  pass=$((pass + 1))
else
  echo "FAIL - --force should let preflight complete (rc=$RC)"
  cat "$OUT"
  fail=$((fail + 1))
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

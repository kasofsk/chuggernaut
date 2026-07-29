#!/bin/sh
# Shell test for chug-install.sh preflight + project-import — no live stack.
#
# It builds an isolated fake repo tree (chug-install.sh derives REPO from its own
# location), drops in a stub `chuggernaut` binary whose `validate` FAILS, plus a
# .chug/jobs/*.yaml to validate, and stubs the required deps on PATH. Then it asserts
# the #186 contract: a config-validation failure is FATAL unless --force; and the
# #276 contract: importing the platform's own source repo records SELF_REPO in
# the env file (an ordinary project does not, an existing value is kept, and a
# failing mirror step downstream cannot cost the install its SELF_REPO line).
#
# `git` is NOT stubbed — the import cases exercise real clone/push against local
# repos, which is what the SELF_REPO detection reads.
#
# Run:  deploy/prod/chug-install.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Isolated fake repo: SUT copied so its REPO ($HERE/../..) is our sandbox.
REPO="$WORK/repo"
mkdir -p "$REPO/deploy/prod" "$REPO/target/release" "$REPO/.chug/jobs"
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
printf 'job_type: demo\n' > "$REPO/.chug/jobs/demo.yaml"

# Required deps as no-op stubs so preflight's dependency gate passes. `git` is
# deliberately absent: the import cases need the real thing.
BIN="$WORK/bin"
mkdir -p "$BIN"
for d in docker node age curl; do
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

# ── project-import: SELF_REPO recording (#276) ────────────────────────────────
# The mirror installer is out of scope here — stub it where the SUT looks.
printf '#!/bin/sh\nexit 0\n' > "$REPO/deploy/prod/chug-mirror-install.sh"
chmod +x "$REPO/deploy/prod/chug-mirror-install.sh"

# make_src <dir> <path-to-commit> — a one-commit source repo on `main`.
make_src() {
  mkdir -p "$(dirname "$1/$2")"
  git init -q "$1"
  git -C "$1" symbolic-ref HEAD refs/heads/main
  printf 'x\n' > "$1/$2"
  git -C "$1" add -A
  git -C "$1" -c user.name=t -c user.email=t@example.com commit -q -m init
}

# make_bare <owner> <name> — the platform-owned bare repo `admin project create`
# would have made, so the import skips creation (the stub binary makes nothing).
make_bare() {
  git init -q --bare "$WORK/repos/$1/$2.git"
}

# A chuggernaut checkout is recognised by its dispatcher crate; anything else is
# an ordinary project.
make_src "$WORK/src-platform" crates/dispatcher/Cargo.toml
make_src "$WORK/src-plain" README.md
make_bare acme chuggernaut
make_bare acme widgets

# ── Case 3: importing the platform's own repo records SELF_REPO ───────────────
run --env "$ENVF" project-import "$WORK/src-platform" --owner acme --name chuggernaut
if [ "$RC" -eq 0 ] && grep -qx "SELF_REPO=acme/chuggernaut" "$ENVF"; then
  echo "ok   - importing the platform repo records SELF_REPO in the env file"
  pass=$((pass + 1))
else
  echo "FAIL - platform-repo import must record SELF_REPO (rc=$RC)"
  cat "$OUT"
  fail=$((fail + 1))
fi

# ── Case 4: a re-import keeps the existing value (no duplicate lines) ─────────
run --env "$ENVF" project-import "$WORK/src-platform" --owner acme --name chuggernaut
if [ "$RC" -eq 0 ] && [ "$(grep -c '^SELF_REPO=' "$ENVF")" -eq 1 ] &&
  grep -qF "SELF_REPO already set" "$OUT"; then
  echo "ok   - re-import leaves the recorded SELF_REPO alone"
  pass=$((pass + 1))
else
  echo "FAIL - re-import must not rewrite SELF_REPO (rc=$RC)"
  cat "$OUT"
  fail=$((fail + 1))
fi

# ── Case 5: an ordinary project import records nothing ────────────────────────
ENVF2="$WORK/plain.env"
cp "$ENVF" "$ENVF2"
sed '/^SELF_REPO=/d' "$ENVF2" > "$ENVF2.tmp" && mv "$ENVF2.tmp" "$ENVF2"
run --env "$ENVF2" project-import "$WORK/src-plain" --owner acme --name widgets
if [ "$RC" -eq 0 ] && ! grep -q '^SELF_REPO=' "$ENVF2"; then
  echo "ok   - an ordinary project import does not claim SELF_REPO"
  pass=$((pass + 1))
else
  echo "FAIL - only the platform repo may set SELF_REPO (rc=$RC)"
  cat "$OUT"
  fail=$((fail + 1))
fi

# ── Case 6: SELF_REPO survives a failing mirror step ──────────────────────────
# The mirror install is the one unguarded step in project-import, so under
# `set -eu` it aborts the whole subcommand. Recording must therefore not sit
# downstream of it: a fresh install whose mirror fails still gets SELF_REPO.
printf '#!/bin/sh\nexit 1\n' > "$REPO/deploy/prod/chug-mirror-install.sh"
ENVF3="$WORK/mirror-fail.env"
sed '/^SELF_REPO=/d' "$ENVF" > "$ENVF3"
run --env "$ENVF3" project-import "$WORK/src-platform" --owner acme --name chuggernaut
if grep -qx "SELF_REPO=acme/chuggernaut" "$ENVF3"; then
  echo "ok   - a failing mirror step does not cost the install its SELF_REPO"
  pass=$((pass + 1))
else
  echo "FAIL - SELF_REPO must be recorded before the mirror step (rc=$RC)"
  cat "$OUT"
  fail=$((fail + 1))
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

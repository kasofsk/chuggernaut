#!/bin/sh
# Shell test for deploy-health.sh — no network required.
#
# It drives deploy-health.sh with a fake `curl` on PATH that writes a canned
# body (FAKE_BODY) to curl's `-o` file and prints "<code> <content-type>" from
# FAKE_CODE / FAKE_CTYPE. The point of the gate is that a *200 with an HTML
# body* (the SPA fallback masquerading as health — the #77/#81 bug) must FAIL,
# and only the real {"dispatcher":"ok",..} JSON passes.
#
# Run:  tasks/deploy-health.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/deploy-health.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Fake curl: write $FAKE_BODY to the `-o <file>` target, print "code ctype".
mkdir -p "$WORK/bin"
cat > "$WORK/bin/curl" <<'EOF'
#!/bin/sh
out=""; prev=""
for a in "$@"; do
  [ "$prev" = "-o" ] && out="$a"
  prev="$a"
done
[ -n "$out" ] && printf '%s' "${FAKE_BODY:-}" > "$out"
printf '%s %s' "${FAKE_CODE:-000}" "${FAKE_CTYPE:-}"
EOF
chmod +x "$WORK/bin/curl"

pass=0
fail=0
# <name> <code> <ctype> <body> <expected-rc> <must-contain>
run_case() {
  name="$1"; code="$2"; ctype="$3"; body="$4"; want="$5"; needle="$6"
  out="$WORK/out"
  set +e
  PATH="$WORK/bin:$PATH" \
  HEALTH_TIMEOUT_SECS=0 HEALTH_INTERVAL_SECS=0 \
  FAKE_CODE="$code" FAKE_CTYPE="$ctype" FAKE_BODY="$body" \
    "$SUT" >"$out" 2>&1
  got=$?
  set -e
  if [ "$got" = "$want" ] && grep -qF "$needle" "$out"; then
    echo "ok   - $name (rc=$got)"
    pass=$((pass + 1))
  else
    echo "FAIL - $name: rc want=$want got=$got; expected output to contain: $needle"
    echo "----- output -----"; cat "$out"; echo "------------------"
    fail=$((fail + 1))
  fi
}

# 1. Real health JSON → PASS.
run_case "genuine health json passes" \
  200 "application/json" '{"dispatcher":"ok","version":"0.1.0"}' 0 "PASS"

# 2. SPA fallback masquerade: 200 but text/html → HARD FAIL (the core bug).
run_case "html body is rejected (SPA masquerade)" \
  200 "text/html" '<!doctype html><html><body>app</body></html>' 1 "SPA fallback"

# 3. Dispatcher down → 503 → FAIL.
run_case "503 fails" \
  503 "application/json" '{"dispatcher":"error","error":"no responders"}' 1 "status 503"

# 4. 200 + JSON but the wrong document (e.g. some other route) → FAIL.
run_case "non-health json fails" \
  200 "application/json" '{"jobs":[]}' 1 "not the health JSON"

# 5. Connection failure (no response) → FAIL.
run_case "connection failure fails" \
  "" "" "" 1 "no response"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

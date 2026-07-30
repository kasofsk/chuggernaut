#!/bin/sh
# Shell test for deploy-health.sh — no network required.
#
# It drives deploy-health.sh with a fake `curl` on PATH that writes a canned
# body to curl's `-o` file and prints "<code> <content-type>". The fake answers
# the two probed endpoints separately: FAKE_CODE/FAKE_CTYPE/FAKE_BODY for
# /api/v1/health, FAKE_FLEET_* for /api/v1/platform/fleet.
#
# Two things must hold, and the cases below pin both:
#   - a *200 with an HTML body* (the SPA fallback masquerading as health — the
#     #77/#81 bug) must FAIL, and only the real {"dispatcher":"ok",..} JSON
#     passes; and
#   - a healthy dispatcher with an EMPTY fleet must FAIL (deploy #267, where
#     both worker daemons were dead, the gate passed, and the deploy hung in
#     Evaluation) — while a worker bouncing mid-deploy must NOT fail it.
#
# Run:  .chug/tasks/deploy-health.test.sh   (exits 0 iff all cases pass)
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SUT="$HERE/deploy-health.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A live single-node fleet: one available node with slots to spare. The default
# for cases whose subject is the dispatcher probe.
FLEET_LIVE='{"nodes":[{"name":"nuc","slots":4,"occupied":1,"available":true,"version":"0.1.0","running":[{"project":"a/b","job_seq":1,"task_id":2,"task_kind":"eval","job_type":"deploy","phase":"evaluation","started_at":null}]}],"queue_depth":0}'
HEALTH_OK='{"dispatcher":"ok","version":"0.1.0"}'

# Fake curl: write the per-endpoint body to the `-o <file>` target, print
# "code ctype". The URL is the last argument. A per-run counter file lets a case
# answer differently on later attempts (the restarting-worker case):
# FAKE_FLEET_BODY_2, when set, is served from the second fleet probe onward.
#
# It also records each probe's argv to `$FAKE_STATE/argv.<endpoint>`, one
# argument per line (the bearer header embeds a space). Without that record a
# case cannot tell a correctly-built Authorization header from a missing one —
# the 403 case below passes either way — and the header is the whole reason the
# fleet check works against a platform-admin-only route.
mkdir -p "$WORK/bin" "$WORK/state"
cat > "$WORK/bin/curl" <<'EOF'
#!/bin/sh
out=""; prev=""; url=""
for a in "$@"; do
  [ "$prev" = "-o" ] && out="$a"
  case "$a" in http://*|https://*) url="$a" ;; esac
  prev="$a"
done
case "$url" in
*/platform/fleet)
  tag=fleet
  n=$(( $(cat "$FAKE_STATE/fleet.n" 2>/dev/null || echo 0) + 1 ))
  echo "$n" > "$FAKE_STATE/fleet.n"
  code="${FAKE_FLEET_CODE:-000}"; ctype="${FAKE_FLEET_CTYPE:-}"
  body="${FAKE_FLEET_BODY:-}"
  if [ "$n" -ge 2 ] && [ -n "${FAKE_FLEET_BODY_2:-}" ]; then
    body="$FAKE_FLEET_BODY_2"
  fi
  ;;
*)
  tag=health
  code="${FAKE_CODE:-000}"; ctype="${FAKE_CTYPE:-}"; body="${FAKE_BODY:-}"
  ;;
esac
: > "$FAKE_STATE/argv.$tag"
for a in "$@"; do printf '%s\n' "$a" >> "$FAKE_STATE/argv.$tag"; done
[ -n "$out" ] && printf '%s' "$body" > "$out"
printf '%s %s' "$code" "$ctype"
EOF
chmod +x "$WORK/bin/curl"

FAKE_STATE="$WORK/state"
export FAKE_STATE

# Clear every knob (and export the names once) so a case sets only what it is
# about and nothing leaks from the previous one.
fake_reset() {
  FAKE_CODE=""; FAKE_CTYPE=""; FAKE_BODY=""
  FAKE_FLEET_CODE=""; FAKE_FLEET_CTYPE=""; FAKE_FLEET_BODY=""; FAKE_FLEET_BODY_2=""
  # Unset by default: the gate must probe (and fail loudly) without a token
  # rather than skip the fleet check.
  DEPLOY_HEALTH_API_TOKEN=""
  # Single-attempt by default — a case that wants a retry raises its own window.
  HEALTH_TIMEOUT_SECS=0; HEALTH_INTERVAL_SECS=0
  FLEET_TIMEOUT_SECS=0; FLEET_INTERVAL_SECS=0
  export FAKE_CODE FAKE_CTYPE FAKE_BODY \
         FAKE_FLEET_CODE FAKE_FLEET_CTYPE FAKE_FLEET_BODY FAKE_FLEET_BODY_2 \
         DEPLOY_HEALTH_API_TOKEN \
         HEALTH_TIMEOUT_SECS HEALTH_INTERVAL_SECS \
         FLEET_TIMEOUT_SECS FLEET_INTERVAL_SECS
}

pass=0
fail=0
# <name> <expected-rc> <must-contain> — run the gate against the knobs as set.
run_gate() {
  name="$1"; want="$2"; needle="$3"
  out="$WORK/out"
  rm -f "$WORK/state/fleet.n" "$WORK/state/argv.health" "$WORK/state/argv.fleet"
  set +e
  PATH="$WORK/bin:$PATH" "$SUT" >"$out" 2>&1
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

# <name> <has|lacks> <needle> — assert what the LAST run_gate actually printed.
# `lacks` is the load-bearing direction: a failure must not borrow another
# failure's message (#335 — an unreadable body reported as #267).
assert_output() {
  if grep -qF -- "$3" "$WORK/out"; then got=has; else got=lacks; fi
  if [ "$got" = "$2" ]; then
    echo "ok   - $1"
    pass=$((pass + 1))
  else
    echo "FAIL - $1: output $got '$3', want $2"
    echo "----- output -----"; cat "$WORK/out"; echo "------------------"
    fail=$((fail + 1))
  fi
}

# <name> <health|fleet> <has|lacks> <needle> — assert what the last probe of
# that endpoint actually put on curl's command line, which the gate's own output
# never shows.
assert_argv() {
  argv_file="$WORK/state/argv.$2"
  if [ ! -f "$argv_file" ]; then
    echo "FAIL - $1: no $2 probe was recorded"
    fail=$((fail + 1))
    return
  fi
  if grep -qF -- "$4" "$argv_file"; then got=has; else got=lacks; fi
  if [ "$got" = "$3" ]; then
    echo "ok   - $1"
    pass=$((pass + 1))
  else
    echo "FAIL - $1: $2 argv $got '$4', want $3"
    echo "----- argv -----"; cat "$argv_file"; echo "----------------"
    fail=$((fail + 1))
  fi
}

# <name> <code> <ctype> <body> <expected-rc> <must-contain> — a health-probe
# case; the fleet answers live so the dispatcher assertion is what decides.
run_case() {
  fake_reset
  FAKE_CODE="$2"; FAKE_CTYPE="$3"; FAKE_BODY="$4"
  FAKE_FLEET_CODE=200; FAKE_FLEET_CTYPE="application/json"; FAKE_FLEET_BODY="$FLEET_LIVE"
  run_gate "$1" "$5" "$6"
}

# <name> <fleet-code> <fleet-ctype> <fleet-body> <expected-rc> <must-contain> —
# a fleet-probe case; the dispatcher answers healthy so the fleet decides.
run_fleet_case() {
  fake_reset
  FAKE_CODE=200; FAKE_CTYPE="application/json"; FAKE_BODY="$HEALTH_OK"
  FAKE_FLEET_CODE="$2"; FAKE_FLEET_CTYPE="$3"; FAKE_FLEET_BODY="$4"
  run_gate "$1" "$5" "$6"
}

# ── the dispatcher probe (unchanged behaviour — #59, #77/#81) ────────────────

# 1. Real health JSON (and a live fleet) → PASS.
run_case "genuine health json passes" \
  200 "application/json" "$HEALTH_OK" 0 "PASS"

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

# ── the fleet probe (#267) ──────────────────────────────────────────────────

# 6. THE #267 SCENARIO: dispatcher healthy, zero workers registered → FAIL, with
#    a reason naming the empty fleet rather than blaming the dispatcher.
run_fleet_case "empty fleet fails the gate (#267)" \
  200 "application/json" '{"nodes":[],"queue_depth":0}' \
  1 "empty fleet: no worker node is registered"

# 7. Nodes registered but every one of them down → FAIL.
run_fleet_case "registered but dead nodes fail" \
  200 "application/json" \
  '{"nodes":[{"name":"nuc","slots":4,"occupied":0,"available":false,"version":null,"running":[]},{"name":"air","slots":2,"occupied":0,"available":false,"version":null,"running":[]}],"queue_depth":0}' \
  1 "2 node(s) registered, none reporting alive"

# 8. Alive but zero usable slots — prod's `local|unix://…|0` docker-endpoint node
#    with the worker fleet gone. Registered is not the same as capacity.
run_fleet_case "alive node with zero slots is not capacity" \
  200 "application/json" \
  '{"nodes":[{"name":"local","slots":0,"occupied":0,"available":true,"version":null,"running":[]}],"queue_depth":0}' \
  1 "0 usable slots"

# 9. A node known only from a running container (slots null → cap unknown) is
#    not provable capacity either.
run_fleet_case "unknown-capacity node is not capacity" \
  200 "application/json" \
  '{"nodes":[{"name":"ghost","slots":null,"occupied":1,"available":true,"version":null,"running":[{"project":"a/b","job_seq":1,"task_id":2,"task_kind":"work","job_type":"code","phase":"work","started_at":null}]}],"queue_depth":0}' \
  1 "0 usable slots"

# 10. A live fleet passes, and the PASS line reports what it saw. A node's
#     `running` occupants must not be miscounted as nodes.
run_fleet_case "live fleet passes" \
  200 "application/json" "$FLEET_LIVE" 0 "1/1 node(s) alive, 4 slot(s)"

# 11. One node down, one alive with slots → PASS: the gate wants capacity, not a
#     perfect fleet.
run_fleet_case "a partially down fleet with capacity passes" \
  200 "application/json" \
  '{"nodes":[{"name":"air","slots":2,"occupied":0,"available":false,"version":null,"running":[]},{"name":"nuc","slots":4,"occupied":0,"available":true,"version":"0.1.0","running":[]}],"queue_depth":0}' \
  0 "1/2 node(s) alive, 4 slot(s)"

# 12. The fleet endpoint is platform-admin only: no/invalid token → FAIL naming
#     the credential, never a pass. An unreadable fleet is not a live one.
run_fleet_case "403 on the fleet endpoint fails" \
  403 "application/json" '{"error":"platform admin required"}' 1 "DEPLOY_HEALTH_API_TOKEN"

# 13. SPA fallback on the fleet route (an api too old to serve it) → FAIL.
run_fleet_case "html on the fleet endpoint is rejected" \
  200 "text/html" '<!doctype html><html><body>app</body></html>' 1 "SPA fallback"

# 14. A worker restarting mid-deploy: the first probe sees an empty fleet, the
#     retry sees it back. Must PASS — the refresh swaps each node's daemon, so a
#     transient gap is normal and must not flake every deploy.
fake_reset
FAKE_CODE=200; FAKE_CTYPE="application/json"; FAKE_BODY="$HEALTH_OK"
FAKE_FLEET_CODE=200; FAKE_FLEET_CTYPE="application/json"
FAKE_FLEET_BODY='{"nodes":[],"queue_depth":0}'
FAKE_FLEET_BODY_2="$FLEET_LIVE"
FLEET_TIMEOUT_SECS=5; FLEET_INTERVAL_SECS=1
run_gate "worker restarting mid-deploy does not fail the gate" 0 "PASS"

# ── the bearer header (what makes the fleet probe work in prod) ──────────────

# 15. The token rides the fleet probe and ONLY the fleet probe: /api/v1/health is
#     unauthenticated by design and must not be handed a credential. Asserted on
#     argv because the gate's output looks identical either way.
fake_reset
FAKE_CODE=200; FAKE_CTYPE="application/json"; FAKE_BODY="$HEALTH_OK"
FAKE_FLEET_CODE=200; FAKE_FLEET_CTYPE="application/json"; FAKE_FLEET_BODY="$FLEET_LIVE"
DEPLOY_HEALTH_API_TOKEN="tok-abc123"; export DEPLOY_HEALTH_API_TOKEN
run_gate "live fleet passes with a token set" 0 "PASS"
assert_argv "bearer token is sent on the fleet probe" \
  fleet has "Authorization: Bearer tok-abc123"
assert_argv "no credential on the unauthenticated health probe" \
  health lacks "Authorization"

# 16. Token unset (the secret missing or empty): no header is invented, the gate
#     still probes, and it fails loudly on the 403 rather than skipping the
#     check. Fail-closed is the point — an unverified fleet is #267's blind spot.
fake_reset
FAKE_CODE=200; FAKE_CTYPE="application/json"; FAKE_BODY="$HEALTH_OK"
FAKE_FLEET_CODE=403; FAKE_FLEET_CTYPE="application/json"
FAKE_FLEET_BODY='{"error":"platform admin required"}'
run_gate "missing token still probes and fails closed" 1 "refused our credentials"
assert_argv "no bearer header when the token is unset" \
  fleet lacks "Authorization"

# ── the fleet body parser (#335) ────────────────────────────────────────────
#
# The api serves the dispatcher's fleet.status snapshot VERBATIM
# (crates/api/src/routes.rs), so the body's key order and key set move without
# any api change: serde_json emits object keys alphabetically, and #293 added
# capacity_observed_at / capacity_source, which sort ahead of `name`. The old
# parser split on the literal `{"name":` and read the real fleet as EMPTY.

# 17. THE #335 REGRESSION: the body prod actually served on 2026-07-30 —
#     alphabetical keys, capacity_* present, `name` fourth. Two available nodes
#     with capacity, so this must PASS. It fails with "empty fleet ... (#267)"
#     against the pre-fix parser.
run_fleet_case "real fleet body with alphabetical keys passes" \
  200 "application/json" \
  '{"nodes":[{"available":true,"capacity_observed_at":"2026-07-30T20:45:57Z","capacity_source":"node","name":"air","occupied":0,"running":[],"slots":2,"version":"0.1.0+833f1a7"},{"available":true,"capacity_observed_at":"2026-07-30T20:45:57Z","capacity_source":"node","name":"nuc","occupied":0,"running":[],"slots":4,"version":"0.1.0+833f1a7"}],"queue_depth":0}' \
  0 "2/2 node(s) alive, 6 slot(s)"

# 18. A genuinely empty fleet — and with no `queue_depth` either, pinning that
#     the parser reads `nodes` rather than a whole expected field set — still
#     fails with #267 intact. That message is reserved for exactly this.
run_fleet_case "bare empty nodes array still fails with #267" \
  200 "application/json" '{"nodes":[]}' \
  1 "empty fleet: no worker node is registered"

# 19. Valid JSON, but not a fleet snapshot (an api serving some other document).
#     FAIL — with its own reason and a quoted excerpt, never #267: an
#     unrecognized shape is not evidence that nothing can run work.
run_fleet_case "json of the wrong shape fails with its own reason" \
  200 "application/json" '{"fleet":{"air":{"slots":2},"nuc":{"slots":4}}}' \
  1 "unreadable fleet snapshot"
assert_output "wrong-shape body quotes what it received" \
  has '{"fleet":{"air":{"slots":2}'
assert_output "wrong-shape body is not blamed on #267" lacks "#267"

# 20. A body that is not JSON at all, mislabelled application/json (so the
#     content-type check passes and the parser is what has to catch it).
run_fleet_case "non-json body fails with its own reason" \
  200 "application/json" '<!doctype html><html><body>app</body></html>' \
  1 "unreadable fleet snapshot"
assert_output "non-json body is not blamed on #267" lacks "#267"

# 21. The excerpt is BOUNDED — it lands in a job event an operator reads, so a
#     whole SPA page must not. Truncated at FLEET_EXCERPT_CHARS with a marker.
fake_reset
FAKE_CODE=200; FAKE_CTYPE="application/json"; FAKE_BODY="$HEALTH_OK"
FAKE_FLEET_CODE=200; FAKE_FLEET_CTYPE="application/json"
FAKE_FLEET_BODY='<!doctype html>'
n=0
while [ "$n" -lt 30 ]; do
  FAKE_FLEET_BODY="$FAKE_FLEET_BODY<div>spa</div>"
  n=$((n + 1))
done
FAKE_FLEET_BODY="$FAKE_FLEET_BODY<!--TAIL-MARKER-->"
run_gate "an oversized body is excerpted, not dumped" 1 "unreadable fleet snapshot"
assert_output "the excerpt is truncated" has "…"
assert_output "the excerpt stops before the tail" lacks "TAIL-MARKER"

# 22. Registered, alive, zero usable slots — in the real key order. Still the
#     "no usable slots" branch (#267), not a parse failure: the shape was read
#     fine, the fleet just cannot run anything.
run_fleet_case "zero-slot node in real key order is not capacity" \
  200 "application/json" \
  '{"nodes":[{"available":true,"capacity_source":"docker-endpoint","name":"local","occupied":0,"running":[],"slots":0,"version":null}],"queue_depth":0}' \
  1 "0 usable slots"

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

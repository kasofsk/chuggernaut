#!/bin/sh
# Deploy health gate — the *real* evaluator for a `deploy` job (jobs/deploy.yaml).
#
# A deploy job carries no commits, so the appended project ci default
# (jobs/_defaults.yaml) has an empty diff and glob-skips its cargo work in
# seconds — it proves nothing about whether the release actually came up. This
# script is the gate that does: it runs in an eval container on the worker node
# and checks the just-deployed platform over the tailnet, exactly as an outside
# client would.
#
# No secrets: both endpoints are hit token-free. Use the tailnet URL, never
# 127.0.0.1 — the api runs on the Mini, not in this eval container.
set -eu

BASE="https://gumbo-mini-0.tail20c474.ts.net"
HEALTH_URL="$BASE/api/v1/health"
SPA_URL="$BASE/"

# Retry loop rides out the api container recreate that a deploy triggers:
# ~30 attempts x 2s ≈ up to a minute of downtime before we give up.
ATTEMPTS=30
SLEEP=2

# curl the URL; echo the HTTP status code, or empty on connection failure.
http_status() {
  curl -sS -o /dev/null --max-time 10 -w '%{http_code}' "$1" 2>/dev/null || true
}

echo "deploy-health: checking $HEALTH_URL (up to $ATTEMPTS attempts, ${SLEEP}s apart)"
code=""
i=1
while [ "$i" -le "$ATTEMPTS" ]; do
  code="$(http_status "$HEALTH_URL")"
  if [ "$code" = "200" ]; then
    echo "deploy-health: health OK (200) on attempt $i"
    break
  fi
  echo "deploy-health: attempt $i/$ATTEMPTS — health not ready (got '${code:-no response}')"
  i=$((i + 1))
  [ "$i" -le "$ATTEMPTS" ] && sleep "$SLEEP"
done

if [ "$code" != "200" ]; then
  echo "deploy-health: FAIL — $HEALTH_URL never returned 200 after $ATTEMPTS attempts" >&2
  exit 1
fi

# Second, independent proof of NATS↔dispatcher↔api wiring: the SPA root must
# serve (200). This is a real read path, not the liveness endpoint, and it
# stays token-free.
echo "deploy-health: checking $SPA_URL (SPA root)"
spa_code="$(http_status "$SPA_URL")"
if [ "$spa_code" != "200" ]; then
  echo "deploy-health: FAIL — $SPA_URL returned '${spa_code:-no response}', expected 200" >&2
  exit 1
fi
echo "deploy-health: SPA root OK (200)"

echo "deploy-health: PASS — health endpoint and SPA root are both live at $BASE"

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
# It hits ONE endpoint — GET /api/v1/health — which round-trips the dispatcher's
# core actor and returns {"dispatcher":"ok","version"} as application/json only
# when a live dispatcher answers (crates/api/src/routes.rs, spec §6.x). A pass
# requires ALL of: HTTP 200, an application/json content-type, and that health
# JSON. A `text/html` body is an AUTOMATIC FAIL: that is the SPA fallback
# masquerading as health — the exact bug that let deploy #59 report Done while
# the dispatcher crash-looped on 2026-07-22 (#77/#81). No route → SPA 200 → and
# now we reject it.
#
# No secrets: the endpoint is hit token-free (it leaks only liveness + version).
# Use the tailnet URL, never 127.0.0.1 — the api runs on the Mini, not in this
# eval container. `curl` is resolved via PATH so the shell test can inject a fake.
set -eu

BASE="${DEPLOY_HEALTH_BASE:-https://gumbo-mini-0.tail20c474.ts.net}"
HEALTH_URL="$BASE/api/v1/health"

# Poll budget — aligned with restart-verify.sh (deploy/prod, #77): ~60s of retry
# at a 3s cadence, long enough to ride out the api container recreate a deploy
# triggers (an old api answering before the new one binds). Overridable for the
# shell test.
HEALTH_TIMEOUT_SECS="${HEALTH_TIMEOUT_SECS:-60}"
HEALTH_INTERVAL_SECS="${HEALTH_INTERVAL_SECS:-3}"

# One probe. Sets CODE, CTYPE, BODY from a single curl; CODE is empty on a
# connection failure. `%{content_type}` is the raw response header — the tell
# that distinguishes real health JSON from the SPA fallback's index.html.
probe_once() {
  body_file="$(mktemp)"
  meta="$(curl -sS --max-time 10 -o "$body_file" \
            -w '%{http_code} %{content_type}' "$HEALTH_URL" 2>/dev/null || true)"
  CODE="${meta%% *}"
  case "$meta" in
    *' '*) CTYPE="${meta#* }" ;;
    *) CTYPE="" ;;
  esac
  BODY="$(cat "$body_file")"
  rm -f "$body_file"
}

# Is the last probe a genuine dispatcher-proving health response? Sets REASON on
# failure. A non-JSON (e.g. text/html) content-type is an automatic fail — that
# is exactly the SPA fallback masquerading as health, which must never pass.
is_healthy() {
  if [ "$CODE" != "200" ]; then
    # curl prints 000 (and an empty content-type) when it never got an HTTP
    # response — a refused connection or the api being down mid-recreate.
    case "$CODE" in
    "" | 000) REASON="no response (want 200)" ;;
    *) REASON="status $CODE (want 200)" ;;
    esac
    return 1
  fi
  case "$CTYPE" in
    application/json*) : ;;
    *)
      REASON="content-type '${CTYPE:-none}' is not application/json (SPA fallback?)"
      return 1
      ;;
  esac
  # The api serializes {"dispatcher":"ok","version":...} with no spaces.
  if ! printf '%s' "$BODY" | grep -q '"dispatcher":"ok"'; then
    REASON="body is not the health JSON: ${BODY}"
    return 1
  fi
  return 0
}

echo "deploy-health: probing $HEALTH_URL for a live dispatcher" \
     "(up to ${HEALTH_TIMEOUT_SECS}s, ${HEALTH_INTERVAL_SECS}s apart)"
REASON=""
deadline=$(( $(date +%s) + HEALTH_TIMEOUT_SECS ))
attempt=0
while :; do
  attempt=$((attempt + 1))
  probe_once
  if is_healthy; then
    echo "deploy-health: PASS — dispatcher healthy on attempt $attempt ($BODY)"
    exit 0
  fi
  echo "deploy-health: attempt $attempt — not healthy yet: $REASON"
  if [ "$(date +%s)" -ge "$deadline" ]; then
    break
  fi
  sleep "$HEALTH_INTERVAL_SECS"
done

echo "deploy-health: FAIL — $HEALTH_URL never proved a live dispatcher within" \
     "${HEALTH_TIMEOUT_SECS}s (last: $REASON)" >&2
exit 1

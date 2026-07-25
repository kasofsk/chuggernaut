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
# It asserts TWO things, in order, and a deploy passes only when both hold.
#
# 1. A LIVE DISPATCHER — GET /api/v1/health, which round-trips the dispatcher's
#    core actor and returns {"dispatcher":"ok","version"} as application/json
#    only when a live dispatcher answers (crates/api/src/routes.rs, spec §6.6).
#    A pass requires ALL of: HTTP 200, an application/json content-type, and
#    that health JSON. A `text/html` body is an AUTOMATIC FAIL: that is the SPA
#    fallback masquerading as health — the exact bug that let deploy #59 report
#    Done while the dispatcher crash-looped on 2026-07-22 (#77/#81). No route →
#    SPA 200 → and now we reject it.
#
# 2. A LIVE FLEET — GET /api/v1/platform/fleet, the dispatcher's `fleet.status`
#    snapshot (spec §3.1). Added after deploy #267 (2026-07-25), where both
#    worker daemons were dead: the dispatcher was genuinely healthy the whole
#    time, so check 1 passed, and the deploy then HUNG in Evaluation because no
#    worker was alive to report its container's exit. Hanging is worse than
#    failing — a failed deploy escalates and surfaces, a hung one sits there
#    looking busy. Workers are separate processes on separate hosts, so check 1
#    can never see them. A pass requires at least one node reporting
#    `"available":true` AND non-zero total slots across those nodes: a node
#    registered with zero usable slots is not capacity (prod's `local`
#    docker-endpoint node is exactly that — `local|unix://…|0`), and a fleet
#    that can run nothing must never gate green.
#
# The health endpoint is unauthenticated by design; the fleet endpoint is
# platform-admin only (spec §6.6: "if project-liveness detail is ever added,
# gate the endpoint and update the deploy gate to authenticate"). So the fleet
# probe sends `Authorization: Bearer $DEPLOY_HEALTH_API_TOKEN`, injected as a
# declared secret on this evaluator (jobs/deploy.yaml; provision per deploy/prod
# README.md §3). The name deliberately avoids the reserved `CHUG_` prefix, which
# release validation rejects outright (spec §11). A fleet we cannot READ is a
# FAIL, never a pass — an unverified fleet is precisely the blind spot #267
# exploited.
#
# No secrets in check 1: the health endpoint is hit token-free (it leaks only
# liveness + version). Use the tailnet URL, never 127.0.0.1 — the api runs on
# the Mini, not in this eval container. `curl` is resolved via PATH so the shell
# test can inject a fake.
set -eu

BASE="${DEPLOY_HEALTH_BASE:-https://gumbo-mini-0.tail20c474.ts.net}"
HEALTH_URL="$BASE/api/v1/health"
FLEET_URL="$BASE/api/v1/platform/fleet"

# Poll budget — aligned with restart-verify.sh (deploy/prod, #77): ~60s of retry
# at a 3s cadence, long enough to ride out the api container recreate a deploy
# triggers (an old api answering before the new one binds). Overridable for the
# shell test.
HEALTH_TIMEOUT_SECS="${HEALTH_TIMEOUT_SECS:-60}"
HEALTH_INTERVAL_SECS="${HEALTH_INTERVAL_SECS:-3}"

# Fleet poll budget — its own window, spent only after the dispatcher answers.
# Workers legitimately restart DURING a deploy (update.sh step 3 swaps each
# node's daemon), so the fleet must be given a fair chance to come back before
# we call it empty. 90s is sized off the two constants that bound a legitimate
# gap: a worker re-announces every ANNOUNCE_INTERVAL (15s,
# crates/worker/src/daemon.rs) and the dispatcher only gates a node once its
# announce has lapsed for WORKER_HEARTBEAT_TIMEOUT (60s,
# crates/dispatcher/src/scan.rs). A node that bounces mid-gate is therefore back
# in the snapshot within 60+15 = 75s worst case; 90s covers that plus the
# republish, and no more — the whole point is to fail LOUDLY and soon when the
# fleet is genuinely gone, so the window stays under two minutes.
FLEET_TIMEOUT_SECS="${FLEET_TIMEOUT_SECS:-90}"
FLEET_INTERVAL_SECS="${FLEET_INTERVAL_SECS:-5}"

# Iteration cap for the node-record walk below (STYLE.md tier-2 rule 3). Our
# fleet is single digits; anything past this is a malformed body, not a fleet.
FLEET_NODES_MAX="${FLEET_NODES_MAX:-64}"

# One probe of $1. Sets CODE, CTYPE, BODY from a single curl; CODE is empty on a
# connection failure. `%{content_type}` is the raw response header — the tell
# that distinguishes real health JSON from the SPA fallback's index.html. With
# `auth` as $2 the bearer token rides along (the fleet endpoint needs it; the
# health endpoint is unauthenticated and stays that way).
probe_once() {
  probe_url="$1"
  probe_auth="${2:-}"
  body_file="$(mktemp)"
  # Built positionally so the header's embedded space survives quoting.
  set -- -sS --max-time 10 -o "$body_file" -w '%{http_code} %{content_type}'
  if [ "$probe_auth" = auth ] && [ -n "${DEPLOY_HEALTH_API_TOKEN:-}" ]; then
    set -- "$@" -H "Authorization: Bearer $DEPLOY_HEALTH_API_TOKEN"
  fi
  meta="$(curl "$@" "$probe_url" 2>/dev/null || true)"
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

# Tally one node record — the slice of the fleet body from this node's `{"name":`
# up to the next one. Adds to FLEET_NODES / FLEET_ALIVE / FLEET_SLOTS. A record
# we cannot read counts as registered but neither alive nor capacity: an
# unreadable fleet must fail the gate, never flatter it.
fleet_tally_record() {
  record="$1"
  FLEET_NODES=$((FLEET_NODES + 1))
  # FleetNode serializes name,slots,occupied,available,version,…,running — so a
  # key's value runs to the next comma. `slots` is null for a node seen only
  # through a running container (cap unknown), which is not capacity either.
  key_available='"available":'
  key_slots='"slots":'
  available="${record#*"$key_available"}"
  available="${available%%,*}"
  [ "$available" = "true" ] || return 0
  FLEET_ALIVE=$((FLEET_ALIVE + 1))
  slots="${record#*"$key_slots"}"
  slots="${slots%%,*}"
  case "$slots" in
    "" | *[!0-9]*) return 0 ;;
  esac
  FLEET_SLOTS=$((FLEET_SLOTS + slots))
}

# Walk the last probe's body, tallying one record per node. Only FleetNode
# carries a "name" key (SlotOccupant and RefreshOutcome do not), so splitting on
# it isolates the nodes without a JSON parser — the eval image has no jq.
fleet_scan() {
  FLEET_NODES=0
  FLEET_ALIVE=0
  FLEET_SLOTS=0
  sep='{"name":'
  rest="$BODY"
  while [ "${rest#*"$sep"}" != "$rest" ]; do
    if [ "$FLEET_NODES" -ge "$FLEET_NODES_MAX" ]; then
      REASON="fleet snapshot lists more than $FLEET_NODES_MAX nodes — refusing to parse"
      return 1
    fi
    rest="${rest#*"$sep"}"
    fleet_tally_record "${rest%%"$sep"*}"
  done
  return 0
}

# Does the last fleet probe prove a fleet that can actually run work? Sets
# REASON on failure. The three failing shapes are distinguished deliberately:
# an operator reading the FAIL line should know whether nothing registered,
# everything is down, or the only survivors have no usable slots.
fleet_is_live() {
  case "$CODE" in
    401 | 403)
      REASON="fleet endpoint refused our credentials (status $CODE) — set the DEPLOY_HEALTH_API_TOKEN secret (deploy/prod/README.md §3)"
      return 1
      ;;
    "" | 000) REASON="no response (want 200)" ; return 1 ;;
    200) : ;;
    *) REASON="status $CODE (want 200)" ; return 1 ;;
  esac
  case "$CTYPE" in
    application/json*) : ;;
    *)
      REASON="content-type '${CTYPE:-none}' is not application/json (SPA fallback?)"
      return 1
      ;;
  esac
  fleet_scan || return 1
  if [ "$FLEET_NODES" -eq 0 ]; then
    REASON="empty fleet: no worker node is registered — nothing can run work (#267)"
    return 1
  fi
  if [ "$FLEET_ALIVE" -eq 0 ]; then
    REASON="dead fleet: $FLEET_NODES node(s) registered, none reporting alive — nothing can run work (#267)"
    return 1
  fi
  if [ "$FLEET_SLOTS" -eq 0 ]; then
    REASON="no fleet capacity: $FLEET_ALIVE alive node(s) but 0 usable slots (#267)"
    return 1
  fi
  return 0
}

echo "deploy-health: probing $HEALTH_URL for a live dispatcher" \
     "(up to ${HEALTH_TIMEOUT_SECS}s, ${HEALTH_INTERVAL_SECS}s apart)"
REASON=""
healthy=0
deadline=$(( $(date +%s) + HEALTH_TIMEOUT_SECS ))
attempt=0
while :; do
  attempt=$((attempt + 1))
  probe_once "$HEALTH_URL"
  if is_healthy; then
    echo "deploy-health: dispatcher healthy on attempt $attempt ($BODY)"
    healthy=1
    break
  fi
  echo "deploy-health: attempt $attempt — not healthy yet: $REASON"
  if [ "$(date +%s)" -ge "$deadline" ]; then
    break
  fi
  sleep "$HEALTH_INTERVAL_SECS"
done

if [ "$healthy" -ne 1 ]; then
  echo "deploy-health: FAIL — $HEALTH_URL never proved a live dispatcher within" \
       "${HEALTH_TIMEOUT_SECS}s (last: $REASON)" >&2
  exit 1
fi

echo "deploy-health: probing $FLEET_URL for a live fleet" \
     "(up to ${FLEET_TIMEOUT_SECS}s, ${FLEET_INTERVAL_SECS}s apart)"
live=0
deadline=$(( $(date +%s) + FLEET_TIMEOUT_SECS ))
attempt=0
while :; do
  attempt=$((attempt + 1))
  probe_once "$FLEET_URL" auth
  if fleet_is_live; then
    echo "deploy-health: PASS — dispatcher healthy and fleet live on attempt" \
         "$attempt ($FLEET_ALIVE/$FLEET_NODES node(s) alive, $FLEET_SLOTS slot(s))"
    live=1
    break
  fi
  echo "deploy-health: attempt $attempt — fleet not live yet: $REASON"
  if [ "$(date +%s)" -ge "$deadline" ]; then
    break
  fi
  sleep "$FLEET_INTERVAL_SECS"
done

if [ "$live" -ne 1 ]; then
  echo "deploy-health: FAIL — $FLEET_URL never proved a live fleet within" \
       "${FLEET_TIMEOUT_SECS}s (last: $REASON)" >&2
  exit 1
fi
exit 0

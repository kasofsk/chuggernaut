#!/usr/bin/env bash
# Mock review: outputs a JSON review decision.
# Controlled by MOCK_REVIEW_DECISION env var (default: "approved").
# First invocation returns "changes_requested", subsequent return "approved"
# when MOCK_REVIEW_CYCLE=true.
set -euo pipefail

log() { echo "mock-review: $*" >&2; }

DECISION="${MOCK_REVIEW_DECISION:-approved}"
FEEDBACK="${MOCK_REVIEW_FEEDBACK:-Looks good}"

log "mode=$DECISION feedback=$FEEDBACK"
log "review_level=${CHUGGERNAUT_REVIEW_LEVEL:-unknown}"
log "diff length=${#CHUGGERNAUT_PR_DIFF:-0}"

# Output the decision as JSON (worker parses last JSON line from stdout)
echo "{\"decision\": \"${DECISION}\", \"feedback\": \"${FEEDBACK}\"}"

log "exiting"
exit 0

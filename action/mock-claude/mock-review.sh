#!/usr/bin/env bash
# Mock review: outputs a JSON review decision.
# Controlled by MOCK_REVIEW_DECISION env var (default: "approved").
#
# Accepts --print <prompt> (prompt is ignored — decision comes from env).
set -euo pipefail

log() { echo "mock-review: $*" >&2; }

# Skip --print arg if present
if [ "${1:-}" = "--print" ]; then
    log "print mode — prompt received (${#2} chars)"
fi

DECISION="${MOCK_REVIEW_DECISION:-approved}"
FEEDBACK="${MOCK_REVIEW_FEEDBACK:-Looks good}"

log "mode=$DECISION feedback=$FEEDBACK"

# Output the decision as JSON (worker parses last JSON line from stdout)
echo "{\"decision\": \"${DECISION}\", \"feedback\": \"${FEEDBACK}\"}"

log "exiting"
exit 0

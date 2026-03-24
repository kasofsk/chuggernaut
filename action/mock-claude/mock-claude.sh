#!/usr/bin/env bash
# Mock Claude script for E2E testing.
# Does simple git work and exits. No MCP needed.
set -euo pipefail

echo "mock-claude: starting" >&2

# Do git work: create a file, commit
echo "Hello from mock Claude" > mock-work.txt
git add -A
git commit -m "mock: automated work" || true
git push -u origin HEAD 2>&1 || echo "push failed" >&2

echo "mock-claude: done" >&2
exit 0

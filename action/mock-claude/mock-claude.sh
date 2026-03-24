#!/usr/bin/env bash
# Mock Claude script for E2E testing.
# Speaks MCP JSON-RPC on stdin/stdout, does simple git work, exits.
set -euo pipefail

# MCP initialize handshake
read -r request
id=$(echo "$request" | jq -r '.id')
echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"mock-claude","version":"0.1.0"}}}'

# Read notifications/initialized (ignore)
read -r _notification

# Read tools/list request
read -r request
id=$(echo "$request" | jq -r '.id')
echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"tools":[]}}'

# Do the actual work: create a file, commit, push
echo "mock-claude: doing git work" >&2
echo "Hello from mock Claude" > mock-work.txt
git add -A
git commit -m "mock: automated work" || true
git push -u origin HEAD || true

# Call update_status tool via stdout (the worker will forward to channel MCP)
# In a real scenario, Claude would call MCP tools. For the mock, we just exit.
echo "mock-claude: work complete, exiting" >&2
exit 0

#!/usr/bin/env bash
# Mock Claude: MCP client that exercises the channel tools, does git work, exits.
# Speaks JSON-RPC 2.0 on stdout (requests) and reads responses on stdin.
set -euo pipefail

log() { echo "mock-claude: $*" >&2; }

# Global request ID counter
REQ_ID=0

# Send a JSON-RPC request on stdout, read response on stdin, store in LAST_RESPONSE
send_request() {
    local method="$1"
    local params="$2"
    REQ_ID=$((REQ_ID + 1))
    local req="{\"jsonrpc\":\"2.0\",\"id\":${REQ_ID},\"method\":\"${method}\",\"params\":${params}}"
    log ">>> SEND $method (id=$REQ_ID): $req"
    echo "$req"
    log "    waiting for response on stdin..."
    read -r LAST_RESPONSE
    log "<<< RECV: $LAST_RESPONSE"
}

# Send a notification (no response expected)
send_notification() {
    local method="$1"
    local params="${2:-{}}"
    local notif="{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":${params}}"
    log ">>> NOTIFY $method: $notif"
    echo "$notif"
}

# Call an MCP tool
call_tool() {
    local name="$1"
    local args="$2"
    send_request "tools/call" "{\"name\":\"${name}\",\"arguments\":${args}}"
}

log "starting MCP handshake"

# 1. Initialize
send_request "initialize" '{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"mock-claude","version":"0.1.0"}}'
log "initialized"

# 2. Notify initialized
send_notification "notifications/initialized"

# 3. List tools
send_request "tools/list" '{}'
log "tools listed"

# 4. Update status: starting
call_tool "update_status" '{"status":"starting work"}'

# 5. Git work
log "doing git work in $(pwd)"
log "git status: $(git status --short 2>&1)"
echo "Hello from mock Claude via MCP" > mock-work.txt
git add -A
log "git commit..."
git commit -m "mock: automated work via MCP" 2>&1 || log "commit failed or nothing to commit"
log "git push..."
git push -u origin HEAD 2>&1 || log "push failed (may be ok)"
log "git work done"

# 6. Send channel message
call_tool "channel_send" '{"message":"work complete"}'

# 7. Update status: done
call_tool "update_status" '{"status":"done","progress":100}'

log "exiting"
exit 0

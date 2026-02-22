#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Integration tests: Conversational Agent Gateway (v0.7.0)
# Tests chat sessions, message routing, reply queue, and dashboard endpoints.
# Requires: beads-bridge on :3310, status-api on :3312, a2a-gateway on :3313,
#           Postgres with chat_sessions schema (migration 019)
# Run: bash tests/chat-integration.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
STATUS_URL="${STATUS_URL:-http://127.0.0.1:3312}"
GATEWAY_URL="${GATEWAY_URL:-http://127.0.0.1:3313}"
HUGO_URL="${HUGO_URL:-http://localhost:1313}"
STATUS_API_KEY="${STATUS_API_KEY:-dev-api-key}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"
PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

# Load JWT
if [ ! -f "$JWT_FILE" ]; then
  echo "ERROR: JWT token not found at $JWT_FILE"
  exit 1
fi
JWT=$(cat "$JWT_FILE")

# Helper: call a beads-bridge tool
call_tool() {
  local tool="$1"
  local params="$2"
  local desc="$3"
  local expected_key="${4:-}"

  local response
  response=$(curl -sf -X POST \
    -H "Authorization: Bearer $JWT" \
    -H "Content-Type: application/json" \
    -d "{\"params\": $params}" \
    "$BRIDGE_URL/api/v1/tool/$tool" 2>&1) || {
    fail "$desc: HTTP error calling $tool"
    return
  }

  local success
  success=$(echo "$response" | python3 -c "import sys,json; print(json.load(sys.stdin).get('success', False))" 2>/dev/null) || {
    fail "$desc: invalid JSON response"
    return
  }

  if [ "$success" != "True" ]; then
    fail "$desc: success=$success"
    return
  fi

  if [ -n "$expected_key" ]; then
    local has_key
    has_key=$(echo "$response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
print('yes' if '$expected_key' in d else 'no')
" 2>/dev/null)
    if [ "$has_key" != "yes" ]; then
      fail "$desc: missing expected key '$expected_key'"
      return
    fi
  fi

  pass "$desc"
  echo "$response"
}

echo "=== Chat Integration Tests (v0.7.0) ==="
echo ""

# -----------------------------------------------
# 1. Create chat session via Slack webhook mock
# -----------------------------------------------
echo "--- 1. Slack Webhook → Chat Session ---"

# Use unique channel/thread IDs per test run to avoid stale session reuse
TEST_TS=$(date +%s)
TEST_CHANNEL="C-TEST-CHAT-${TEST_TS}"
TEST_THREAD="${TEST_TS}.000001"

# Simulate a Slack Events API message (JSON body = event_callback)
SLACK_RESP=$(curl -sf -X POST \
  -H "Content-Type: application/json" \
  -d "{
    \"type\": \"event_callback\",
    \"event\": {
      \"type\": \"message\",
      \"channel\": \"$TEST_CHANNEL\",
      \"user\": \"U-TEST-USER-001\",
      \"text\": \"Hello Broodlink, can you help me?\",
      \"thread_ts\": \"$TEST_THREAD\"
    }
  }" \
  "$GATEWAY_URL/webhook/slack" 2>&1) || SLACK_RESP=""

if [ -n "$SLACK_RESP" ]; then
  pass "Slack webhook accepted chat message (200)"
else
  fail "Slack webhook rejected chat message"
fi

# Give the system a moment to process
sleep 2

echo ""

# -----------------------------------------------
# 2. Verify task created with chat metadata
# -----------------------------------------------
echo "--- 2. Task Created with Chat Metadata ---"

TASKS_RESP=$(curl -sf -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
  "$STATUS_URL/api/v1/tasks" 2>&1) || TASKS_RESP=""

has_chat_task=$(echo "$TASKS_RESP" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
recent = d.get('recent', [])
chat_tasks = [t for t in recent if t.get('title', '').startswith('Chat:')]
print('yes' if chat_tasks else 'no')
" 2>/dev/null || echo "no")

if [ "$has_chat_task" = "yes" ]; then
  pass "Task created with 'Chat:' prefix title"
else
  fail "No task with 'Chat:' prefix found in task queue"
fi

echo ""

# -----------------------------------------------
# 3. List sessions via status-api
# -----------------------------------------------
echo "--- 3. Status-API: List Chat Sessions ---"

SESSIONS_RESP=$(curl -sf -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
  "$STATUS_URL/api/v1/chat/sessions" 2>&1) || SESSIONS_RESP=""

has_sessions=$(echo "$SESSIONS_RESP" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
sessions = d.get('sessions', [])
print('yes' if sessions else 'no')
" 2>/dev/null || echo "no")

if [ "$has_sessions" = "yes" ]; then
  pass "Status-API returns chat sessions"
else
  fail "Status-API returned no chat sessions"
fi

# Extract session_id for later tests
SESSION_ID=$(echo "$SESSIONS_RESP" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
sessions = d.get('sessions', [])
slack_sessions = [s for s in sessions if s.get('channel_id') == '$TEST_CHANNEL']
print(slack_sessions[0]['id'] if slack_sessions else '')
" 2>/dev/null || echo "")

echo ""

# -----------------------------------------------
# 4. Chat stats endpoint
# -----------------------------------------------
echo "--- 4. Status-API: Chat Stats ---"

STATS_RESP=$(curl -sf -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
  "$STATUS_URL/api/v1/chat/stats" 2>&1) || STATS_RESP=""

stats_ok=$(echo "$STATS_RESP" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
has_fields = all(k in d for k in ['active_sessions', 'messages_today', 'pending_replies', 'platforms'])
print('yes' if has_fields else 'no')
" 2>/dev/null || echo "no")

if [ "$stats_ok" = "yes" ]; then
  pass "Chat stats endpoint returns all required fields"
else
  fail "Chat stats missing required fields"
fi

echo ""

# -----------------------------------------------
# 5. Reply to chat via beads-bridge tool
# -----------------------------------------------
echo "--- 5. Reply to Chat ---"

if [ -n "$SESSION_ID" ]; then
  call_tool "reply_to_chat" "{\"session_id\": \"$SESSION_ID\", \"content\": \"Test reply from integration test\"}" \
    "reply_to_chat queues reply" "status"
else
  fail "reply_to_chat: no session_id available (skipped)"
fi

echo ""

# -----------------------------------------------
# 6. List sessions via beads-bridge tool
# -----------------------------------------------
echo "--- 6. list_chat_sessions Tool ---"

call_tool "list_chat_sessions" "{\"platform\": \"slack\"}" \
  "list_chat_sessions returns slack sessions" "sessions"

echo ""

# -----------------------------------------------
# 7. Close session via status-api
# -----------------------------------------------
echo "--- 7. Status-API: Close Chat Session ---"

if [ -n "$SESSION_ID" ]; then
  CLOSE_RESP=$(curl -sf -X POST \
    -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
    -H "Content-Type: application/json" \
    -d '{}' \
    "$STATUS_URL/api/v1/chat/sessions/$SESSION_ID/close" 2>&1) || CLOSE_RESP=""

  close_ok=$(echo "$CLOSE_RESP" | python3 -c "
import sys, json
print(json.load(sys.stdin).get('status', ''))
" 2>/dev/null || echo "")

  if [ "$close_ok" = "ok" ]; then
    pass "Close session returns status=ok"
  else
    fail "Close session failed: $CLOSE_RESP"
  fi
else
  fail "Close session: no session_id available (skipped)"
fi

echo ""

# -----------------------------------------------
# 8. Slash command still routes correctly
# -----------------------------------------------
echo "--- 8. Slash Command Coexistence ---"

CMD_RESP=$(curl -sf -X POST \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "text=agents&response_url=https://hooks.slack.com/actions/test/test/test" \
  "$GATEWAY_URL/webhook/slack" 2>&1) || CMD_RESP=""

if [ -n "$CMD_RESP" ]; then
  pass "Slash command /broodlink agents accepted via webhook"
else
  fail "Slash command rejected by webhook"
fi

echo ""

# -----------------------------------------------
# 9. Dashboard chat page returns 200
# -----------------------------------------------
echo "--- 9. Dashboard Chat Page ---"

CHAT_PAGE=$(curl -sf -o /dev/null -w "%{http_code}" "$HUGO_URL/chat/" 2>/dev/null || echo "000")
if [ "$CHAT_PAGE" = "200" ]; then
  pass "Dashboard /chat/ page returns 200"
else
  fail "Dashboard /chat/ page returned HTTP $CHAT_PAGE"
fi

echo ""

# -----------------------------------------------
# Summary
# -----------------------------------------------
echo "============================================"
echo "  Results: $PASS/$((PASS + FAIL)) passed, $FAIL failed"
echo "============================================"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

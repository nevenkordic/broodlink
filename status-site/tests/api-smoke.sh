#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Smoke tests for Broodlink status-api endpoints.
# Validates that all endpoints the frontend relies on are reachable and return valid JSON.
# Run: bash status-site/tests/api-smoke.sh

set -euo pipefail

API_URL="${STATUS_API_URL:-http://127.0.0.1:3312}"
API_KEY="${STATUS_API_KEY:-dev-api-key}"
PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

check_endpoint() {
  local path="$1"
  local desc="$2"
  local expected_key="${3:-}"

  local response
  response=$(curl -sf -H "X-Broodlink-Api-Key: $API_KEY" "$API_URL$path" 2>&1) || {
    fail "$desc ($path): HTTP error or unreachable"
    return
  }

  # Must be valid JSON
  if ! echo "$response" | python3 -c "import sys,json; json.load(sys.stdin)" 2>/dev/null; then
    fail "$desc ($path): invalid JSON response"
    return
  fi

  # Check status field
  local status
  status=$(echo "$response" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status',''))" 2>/dev/null)
  if [ "$status" != "ok" ]; then
    fail "$desc ($path): status='$status' (expected 'ok')"
    return
  fi

  # Optionally check for a specific key in the data
  if [ -n "$expected_key" ]; then
    local has_key
    has_key=$(echo "$response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
print('yes' if '$expected_key' in d else 'no')
" 2>/dev/null)
    if [ "$has_key" != "yes" ]; then
      fail "$desc ($path): missing expected key '$expected_key' in data"
      return
    fi
  fi

  pass "$desc ($path)"
}

echo "=== Status-API Smoke Tests ==="
echo "Target: $API_URL"
echo ""

# Auth check: unauthenticated should return 401
status_code=$(curl -s -o /dev/null -w "%{http_code}" "$API_URL/api/v1/health" 2>/dev/null) || true
if [ "$status_code" = "401" ]; then
  pass "Unauthenticated request returns 401"
else
  fail "Unauthenticated request returned $status_code (expected 401)"
fi

# All endpoints used by frontend
check_endpoint "/api/v1/health"       "Health check"           "dependencies"
check_endpoint "/api/v1/agents"       "Agents list"            "agents"
check_endpoint "/api/v1/tasks"        "Task queue"             "recent"
check_endpoint "/api/v1/decisions"    "Decisions"              "decisions"
check_endpoint "/api/v1/activity"     "Activity feed"          "activity"
check_endpoint "/api/v1/memory/stats" "Memory stats"           "top_topics"
check_endpoint "/api/v1/commits"      "Dolt commits"           "commits"
check_endpoint "/api/v1/summary"      "Daily summary"          "summaries"
check_endpoint "/api/v1/audit"        "Audit log"              "audit"

# v0.2.0 endpoints
check_endpoint "/api/v1/approvals"         "Approvals"              "approvals"
check_endpoint "/api/v1/approval-policies" "Approval policies"      "policies"
check_endpoint "/api/v1/agent-metrics"     "Agent metrics"          "metrics"
check_endpoint "/api/v1/delegations"       "Delegations"            "delegations"
check_endpoint "/api/v1/guardrails"        "Guardrails"             "policies"
check_endpoint "/api/v1/violations"        "Violations"             "violations"
check_endpoint "/api/v1/streams"           "Streams"                "streams"

# v0.3.0 endpoints
check_endpoint "/api/v1/a2a/tasks"           "A2A tasks"
check_endpoint "/api/v1/a2a/card"            "A2A AgentCard"

# v0.5.0 knowledge graph endpoints
check_endpoint "/api/v1/kg/stats"            "KG stats"               "total_entities"
check_endpoint "/api/v1/kg/entities"         "KG entities"            "entities"
check_endpoint "/api/v1/kg/edges"            "KG edges"               "edges"

# v0.7.0 chat endpoints
check_endpoint "/api/v1/chat/sessions"       "Chat sessions"          "sessions"
check_endpoint "/api/v1/chat/stats"          "Chat stats"             "active_sessions"

# v0.7.0 formula registry endpoints
check_endpoint "/api/v1/formulas"            "Formula list"           "formulas"

# v0.7.0 user management endpoints (API key = admin)
check_endpoint "/api/v1/users"               "User list"              "users"

echo ""
echo "=== API Response Structure Tests ==="

# Tasks must return counts_by_status with pending count
check_endpoint "/api/v1/tasks"        "Tasks: counts_by_status"  "counts_by_status"

# Verify counts_by_status.pending is a number (not missing)
tasks_response=$(curl -sf -H "X-Broodlink-Api-Key: $API_KEY" "$API_URL/api/v1/tasks" 2>&1) || true
has_pending=$(echo "$tasks_response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
cbs = d.get('counts_by_status', {})
print('yes' if isinstance(cbs.get('pending'), int) else 'no')
" 2>/dev/null)
if [ "$has_pending" = "yes" ]; then
  pass "Tasks: counts_by_status.pending is integer"
else
  fail "Tasks: counts_by_status.pending missing or not integer"
fi

# Tasks recent items must have assigned_agent field
has_assigned=$(echo "$tasks_response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
recent = d.get('recent', [])
if not recent:
    print('yes')  # empty list is ok
else:
    print('yes' if 'assigned_agent' in recent[0] else 'no')
" 2>/dev/null)
if [ "$has_assigned" = "yes" ]; then
  pass "Tasks: recent items include assigned_agent field"
else
  fail "Tasks: recent items missing assigned_agent (dashboard needs this)"
fi

# Memory stats must return agents_count and topics_count
check_endpoint "/api/v1/memory/stats" "Memory: agents_count"     "agents_count"
check_endpoint "/api/v1/memory/stats" "Memory: topics_count"     "topics_count"

# Memory stats top_topics must have content_length (not raw content)
mem_response=$(curl -sf -H "X-Broodlink-Api-Key: $API_KEY" "$API_URL/api/v1/memory/stats" 2>&1) || true
has_content_length=$(echo "$mem_response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
topics = d.get('top_topics', [])
if not topics:
    print('yes')  # empty is ok
else:
    t = topics[0]
    has_len = 'content_length' in t
    no_content = 'content' not in t
    print('yes' if has_len and no_content else 'no')
" 2>/dev/null)
if [ "$has_content_length" = "yes" ]; then
  pass "Memory: top_topics use content_length (not raw content)"
else
  fail "Memory: top_topics should have content_length and NOT send raw content blob"
fi

# Chat stats must return required fields
chat_stats_response=$(curl -sf -H "X-Broodlink-Api-Key: $API_KEY" "$API_URL/api/v1/chat/stats" 2>&1) || true
has_chat_fields=$(echo "$chat_stats_response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
fields = ['active_sessions', 'messages_today', 'pending_replies', 'platforms']
print('yes' if all(k in d for k in fields) else 'no')
" 2>/dev/null)
if [ "$has_chat_fields" = "yes" ]; then
  pass "Chat stats: has all required fields (active_sessions, messages_today, pending_replies, platforms)"
else
  fail "Chat stats: missing required fields"
fi

# Chat sessions must return sessions array
chat_sessions_response=$(curl -sf -H "X-Broodlink-Api-Key: $API_KEY" "$API_URL/api/v1/chat/sessions" 2>&1) || true
has_sessions_key=$(echo "$chat_sessions_response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
print('yes' if 'sessions' in d and isinstance(d['sessions'], list) else 'no')
" 2>/dev/null)
if [ "$has_sessions_key" = "yes" ]; then
  pass "Chat sessions: returns sessions array"
else
  fail "Chat sessions: missing sessions array in response"
fi

echo ""
echo "=== Beads-Bridge Smoke Test ==="
bridge_status=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:3310/health" 2>/dev/null) || true
if [ "$bridge_status" = "200" ]; then
  pass "Beads-bridge health (port 3310)"
else
  fail "Beads-bridge health returned $bridge_status (expected 200)"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

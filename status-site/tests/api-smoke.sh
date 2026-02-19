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

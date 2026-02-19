#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Error case tests for beads-bridge tool dispatch.
# Verifies proper error responses for invalid inputs.
# Run: bash tests/bridge-error-cases.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"
PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

if [ ! -f "$JWT_FILE" ]; then
  echo "ERROR: JWT token not found at $JWT_FILE"
  exit 1
fi
JWT=$(cat "$JWT_FILE")

# Helper: call tool and check for non-success response
expect_error() {
  local tool="$1"
  local params="$2"
  local desc="$3"

  local http_code body
  body=$(curl -s -w "\n%{http_code}" -X POST \
    -H "Authorization: Bearer $JWT" \
    -H "Content-Type: application/json" \
    -d "{\"params\": $params}" \
    "$BRIDGE_URL/api/v1/tool/$tool" 2>/dev/null) || true

  http_code=$(echo "$body" | tail -1)
  body=$(echo "$body" | sed '$d')

  # Should NOT return 200 with success=true
  local success
  success=$(echo "$body" | python3 -c "import sys,json; print(json.load(sys.stdin).get('success', 'N/A'))" 2>/dev/null) || success="parse_error"

  if [ "$success" = "True" ]; then
    fail "$desc: expected error but got success=True (HTTP $http_code)"
  else
    pass "$desc (HTTP $http_code)"
  fi
}

echo "=== Bridge Error Case Tests ==="
echo "Target: $BRIDGE_URL"
echo ""

# 1. Unknown tool name
expect_error "nonexistent_tool_xyz" '{}' \
  "Unknown tool returns error"

# 2. store_memory: missing topic
expect_error "store_memory" '{"content": "x"}' \
  "store_memory: missing topic"

# 3. store_memory: missing content
expect_error "store_memory" '{"topic": "x"}' \
  "store_memory: missing content"

# 4. create_task: missing title
expect_error "create_task" '{"description": "no title"}' \
  "create_task: missing title"

# 5. get_task: nonexistent UUID
expect_error "get_task" '{"task_id": "00000000-0000-0000-0000-000000000000"}' \
  "get_task: nonexistent task_id"

# 6. get_agent: nonexistent agent
expect_error "get_agent" '{"agent_id": "_nonexistent_agent_xyz"}' \
  "get_agent: nonexistent agent_id"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

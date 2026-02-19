#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Auth rejection tests for beads-bridge JWT middleware.
# Run: bash tests/bridge-auth-rejection.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"
PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

echo "=== Bridge Auth Rejection Tests ==="
echo "Target: $BRIDGE_URL"
echo ""

# 1. No Authorization header
status=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
  -H "Content-Type: application/json" \
  -d '{"params": {}}' \
  "$BRIDGE_URL/api/v1/tool/ping" 2>/dev/null) || true
if [ "$status" = "401" ]; then
  pass "No auth header returns 401"
else
  fail "No auth header returned $status (expected 401)"
fi

# 2. Invalid Bearer token
status=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
  -H "Authorization: Bearer invalid-garbage-token-12345" \
  -H "Content-Type: application/json" \
  -d '{"params": {}}' \
  "$BRIDGE_URL/api/v1/tool/ping" 2>/dev/null) || true
if [ "$status" = "401" ]; then
  pass "Invalid Bearer token returns 401"
else
  fail "Invalid Bearer token returned $status (expected 401)"
fi

# 3. Missing Bearer prefix
status=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
  -H "Authorization: not-a-bearer-token" \
  -H "Content-Type: application/json" \
  -d '{"params": {}}' \
  "$BRIDGE_URL/api/v1/tool/ping" 2>/dev/null) || true
if [ "$status" = "401" ]; then
  pass "Missing Bearer prefix returns 401"
else
  fail "Missing Bearer prefix returned $status (expected 401)"
fi

# 4. Valid JWT succeeds
if [ ! -f "$JWT_FILE" ]; then
  fail "Valid JWT test: token file not found at $JWT_FILE"
else
  JWT=$(cat "$JWT_FILE")
  status=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Authorization: Bearer $JWT" \
    -H "Content-Type: application/json" \
    -d '{"params": {}}' \
    "$BRIDGE_URL/api/v1/tool/ping" 2>/dev/null) || true
  if [ "$status" = "200" ]; then
    pass "Valid JWT returns 200"
  else
    fail "Valid JWT returned $status (expected 200)"
  fi
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

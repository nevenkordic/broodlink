#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Tests for A2A (Agent-to-Agent) protocol gateway.
# Part 1: Code structure. Part 2: Build + unit tests. Part 3: Integration.
# Run: bash tests/a2a-protocol.sh

set -euo pipefail

A2A_URL="${A2A_URL:-http://127.0.0.1:3313}"
BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
PASS=0
FAIL=0
SKIP=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }
skip() { SKIP=$((SKIP + 1)); printf "  SKIP: %s\n" "$1"; }

echo "=== A2A Protocol Gateway Tests ==="
echo ""

# ---------------------------------------------------------------------------
# Part 1: Code structure verification
# ---------------------------------------------------------------------------

echo "--- Part 1: Code structure ---"

GW_MAIN="rust/a2a-gateway/src/main.rs"
BB_MAIN="rust/beads-bridge/src/main.rs"
SA_MAIN="rust/status-api/src/main.rs"

# A2A gateway crate exists
if [ -f "$GW_MAIN" ]; then
  pass "a2a-gateway crate exists"
else
  fail "a2a-gateway crate missing"
fi

# AgentCard endpoint
if grep -q "agent_card_handler" "$GW_MAIN"; then
  pass "AgentCard handler exists"
else
  fail "AgentCard handler missing"
fi

# tasks/send endpoint
if grep -q "tasks_send_handler" "$GW_MAIN"; then
  pass "tasks/send handler exists"
else
  fail "tasks/send handler missing"
fi

# tasks/get endpoint
if grep -q "tasks_get_handler" "$GW_MAIN"; then
  pass "tasks/get handler exists"
else
  fail "tasks/get handler missing"
fi

# tasks/cancel endpoint
if grep -q "tasks_cancel_handler" "$GW_MAIN"; then
  pass "tasks/cancel handler exists"
else
  fail "tasks/cancel handler missing"
fi

# tasks/sendSubscribe (SSE streaming)
if grep -q "tasks_send_subscribe_handler" "$GW_MAIN"; then
  pass "tasks/sendSubscribe (SSE) handler exists"
else
  fail "tasks/sendSubscribe handler missing"
fi

# Bearer token auth
if grep -q "check_auth" "$GW_MAIN"; then
  pass "bearer token auth check exists"
else
  fail "bearer token auth missing"
fi

# beads-bridge: a2a_discover tool
if grep -q "a2a_discover" "$BB_MAIN"; then
  pass "beads-bridge has a2a_discover tool"
else
  fail "beads-bridge missing a2a_discover tool"
fi

# beads-bridge: a2a_delegate tool
if grep -q "a2a_delegate" "$BB_MAIN"; then
  pass "beads-bridge has a2a_delegate tool"
else
  fail "beads-bridge missing a2a_delegate tool"
fi

# status-api: A2A tasks endpoint
if grep -q "handler_a2a_tasks" "$SA_MAIN"; then
  pass "status-api has A2A tasks handler"
else
  fail "status-api missing A2A tasks handler"
fi

# status-api: A2A card endpoint
if grep -q "handler_a2a_card" "$SA_MAIN"; then
  pass "status-api has A2A card handler"
else
  fail "status-api missing A2A card handler"
fi

# Migration file
if [ -f "migrations/009_a2a_gateway.sql" ]; then
  pass "A2A migration file exists"
else
  fail "A2A migration file missing"
fi

# ---------------------------------------------------------------------------
# Part 2: Build + unit tests
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 2: Build + unit tests ---"

if cargo test -p a2a-gateway 2>&1 | grep -q "test result: ok"; then
  pass "cargo test -p a2a-gateway: all tests pass"
else
  fail "cargo test -p a2a-gateway: some tests failed"
fi

A2A_TESTS=$(cargo test -p a2a-gateway -- --list 2>&1 | grep -c "test_" || true)
if [ "$A2A_TESTS" -ge 8 ]; then
  pass "a2a-gateway has $A2A_TESTS unit tests (>= 8)"
else
  fail "a2a-gateway has only $A2A_TESTS unit tests (expected >= 8)"
fi

# Workspace in Cargo.toml
if grep -q "rust/a2a-gateway" "Cargo.toml"; then
  pass "a2a-gateway in workspace members"
else
  fail "a2a-gateway not in workspace"
fi

# ---------------------------------------------------------------------------
# Part 3: Integration tests (require running services)
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 3: Integration tests (services required) ---"

if curl -sf "$A2A_URL/health" > /dev/null 2>&1; then
  # Health endpoint
  HEALTH=$(curl -sf "$A2A_URL/health")
  if echo "$HEALTH" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('status')=='ok'" 2>/dev/null; then
    pass "GET /health returns ok"
  else
    fail "GET /health unexpected response"
  fi

  # AgentCard
  CARD=$(curl -sf "$A2A_URL/.well-known/agent.json" 2>&1) || true
  if echo "$CARD" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('name')=='Broodlink'" 2>/dev/null; then
    pass "AgentCard returns name=Broodlink"
  else
    fail "AgentCard unexpected response"
  fi

  if echo "$CARD" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('capabilities',{}).get('streaming')==True" 2>/dev/null; then
    pass "AgentCard advertises streaming capability"
  else
    fail "AgentCard missing streaming capability"
  fi

  # tasks/send — requires bridge to be running
  if curl -sf "$BRIDGE_URL/health" > /dev/null 2>&1; then
    SEND_RESP=$(curl -sf -X POST \
      -H "Content-Type: application/json" \
      -d '{"jsonrpc":"2.0","id":1,"method":"tasks/send","params":{"message":{"role":"user","parts":[{"type":"text","text":"test task"}]}}}' \
      "$A2A_URL/a2a/tasks/send" 2>&1) || true

    if echo "$SEND_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('result',{}).get('status',{}).get('state')=='submitted'" 2>/dev/null; then
      pass "tasks/send creates task with status=submitted"

      # Extract external_id for tasks/get
      EXT_ID=$(echo "$SEND_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['id'])" 2>/dev/null) || true

      if [ -n "$EXT_ID" ]; then
        GET_RESP=$(curl -sf -X POST \
          -H "Content-Type: application/json" \
          -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tasks/get\",\"params\":{\"id\":\"$EXT_ID\"}}" \
          "$A2A_URL/a2a/tasks/get" 2>&1) || true

        if echo "$GET_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('result',{}).get('id')=='$EXT_ID'" 2>/dev/null; then
          pass "tasks/get returns correct external_id"
        else
          fail "tasks/get returned unexpected response"
        fi
      fi
    else
      fail "tasks/send failed or returned unexpected status"
    fi
  else
    skip "beads-bridge not reachable — skipping task submission tests"
  fi
else
  skip "A2A gateway not reachable at $A2A_URL — skipping integration tests"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "=== Results ==="
echo "  Passed:  $PASS"
echo "  Failed:  $FAIL"
echo "  Skipped: $SKIP"
echo ""

if [ "$FAIL" -gt 0 ]; then
  echo "FAIL: $FAIL test(s) failed"
  exit 1
else
  echo "OK: All $PASS tests passed ($SKIP skipped)"
fi

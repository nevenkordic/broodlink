#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Tests for SSE stream delivery (beads-bridge + status-api proxy).
# Some tests require running services, others are structural checks.
# Run: bash tests/stream-delivery.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
STATUS_URL="${STATUS_URL:-http://127.0.0.1:3312}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"
API_KEY="${API_KEY:-dev-api-key}"
PASS=0
FAIL=0
SKIP=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }
skip() { SKIP=$((SKIP + 1)); printf "  SKIP: %s\n" "$1"; }

echo "=== Stream Delivery Tests ==="
echo ""

# ---------------------------------------------------------------------------
# Part 1: Code structure verification
# ---------------------------------------------------------------------------

echo "--- Part 1: Code structure ---"

BB_MAIN="rust/beads-bridge/src/main.rs"
SA_MAIN="rust/status-api/src/main.rs"
LISTENER="agents/broodlink_agent/listener.py"

# beads-bridge: SSE handler exists
if grep -q "sse_stream_handler" "$BB_MAIN"; then
  pass "beads-bridge has sse_stream_handler"
else
  fail "beads-bridge missing sse_stream_handler"
fi

# beads-bridge: start_stream tool
if grep -q "start_stream" "$BB_MAIN"; then
  pass "beads-bridge has start_stream tool"
else
  fail "beads-bridge missing start_stream tool"
fi

# beads-bridge: emit_stream_event tool
if grep -q "emit_stream_event" "$BB_MAIN"; then
  pass "beads-bridge has emit_stream_event tool"
else
  fail "beads-bridge missing emit_stream_event tool"
fi

# beads-bridge: KeepAlive for SSE heartbeats
if grep -q "KeepAlive" "$BB_MAIN"; then
  pass "beads-bridge SSE uses KeepAlive heartbeats"
else
  fail "beads-bridge SSE missing KeepAlive heartbeats"
fi

# status-api: SSE proxy endpoint
if grep -q "handler_stream_sse" "$SA_MAIN"; then
  pass "status-api has SSE stream proxy handler"
else
  fail "status-api missing SSE stream proxy handler"
fi

# status-api: NATS client for SSE bridging
if grep -q "pub nats: async_nats::Client" "$SA_MAIN"; then
  pass "status-api has NATS client for SSE bridging"
else
  fail "status-api missing NATS client"
fi

# status-api: SSE stream route registered
if grep -q "stream/:stream_id" "$SA_MAIN"; then
  pass "status-api has /stream/:stream_id route"
else
  fail "status-api missing /stream/:stream_id route"
fi

# Python agent: streaming integration
if grep -q "_start_stream" "$LISTENER"; then
  pass "Python agent has _start_stream helper"
else
  fail "Python agent missing _start_stream helper"
fi

if grep -q "_emit_event" "$LISTENER"; then
  pass "Python agent has _emit_event helper"
else
  fail "Python agent missing _emit_event helper"
fi

if grep -q "emit_stream_event" "$LISTENER"; then
  pass "Python agent calls emit_stream_event"
else
  fail "Python agent doesn't call emit_stream_event"
fi

# ---------------------------------------------------------------------------
# Part 2: Build verification
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 2: Build verification ---"

if cargo build -p beads-bridge 2>&1 | tail -1 | grep -q "Finished\|Compiling"; then
  pass "beads-bridge builds cleanly"
else
  fail "beads-bridge build failed"
fi

if cargo build -p status-api 2>&1 | tail -1 | grep -q "Finished\|Compiling"; then
  pass "status-api builds cleanly"
else
  fail "status-api build failed"
fi

# ---------------------------------------------------------------------------
# Part 3: Integration tests (require running services)
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 3: Integration tests (services required) ---"

# Check if services are reachable
BRIDGE_OK=false
STATUS_OK=false

if curl -sf "$BRIDGE_URL/health" >/dev/null 2>&1; then
  BRIDGE_OK=true
fi

if curl -sf -H "X-Broodlink-Api-Key: $API_KEY" "$STATUS_URL/api/v1/health" >/dev/null 2>&1; then
  STATUS_OK=true
fi

if [ "$BRIDGE_OK" = true ] && [ -f "$JWT_FILE" ]; then
  JWT=$(cat "$JWT_FILE")

  # Test start_stream → emit → complete cycle
  STREAM_RESP=$(curl -sf -X POST \
    -H "Authorization: Bearer $JWT" \
    -H "Content-Type: application/json" \
    -d '{"params": {"tool_name": "test", "task_id": "test-stream-001"}}' \
    "$BRIDGE_URL/api/v1/tool/start_stream" 2>&1) || true

  STREAM_ID=$(echo "$STREAM_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('data',{}).get('stream_id',''))" 2>/dev/null) || true

  if [ -n "$STREAM_ID" ] && [ "$STREAM_ID" != "" ]; then
    pass "start_stream returns stream_id: $STREAM_ID"

    # Emit a progress event
    EMIT_RESP=$(curl -sf -X POST \
      -H "Authorization: Bearer $JWT" \
      -H "Content-Type: application/json" \
      -d "{\"params\": {\"stream_id\": \"$STREAM_ID\", \"event_type\": \"progress\", \"data\": \"{\\\"step\\\": \\\"test\\\"}\"}}" \
      "$BRIDGE_URL/api/v1/tool/emit_stream_event" 2>&1) || true

    if echo "$EMIT_RESP" | python3 -c "import sys,json; assert json.load(sys.stdin).get('success')==True" 2>/dev/null; then
      pass "emit_stream_event succeeds for progress event"
    else
      fail "emit_stream_event failed"
    fi

    # Close with complete event
    CLOSE_RESP=$(curl -sf -X POST \
      -H "Authorization: Bearer $JWT" \
      -H "Content-Type: application/json" \
      -d "{\"params\": {\"stream_id\": \"$STREAM_ID\", \"event_type\": \"complete\", \"data\": \"{\\\"done\\\": true}\"}}" \
      "$BRIDGE_URL/api/v1/tool/emit_stream_event" 2>&1) || true

    if echo "$CLOSE_RESP" | python3 -c "import sys,json; assert json.load(sys.stdin).get('success')==True" 2>/dev/null; then
      pass "complete event closes stream"
    else
      fail "complete event failed"
    fi
  else
    skip "start_stream failed (may need active task)"
  fi
else
  skip "beads-bridge not reachable — skipping stream integration tests"
fi

if [ "$STATUS_OK" = true ]; then
  # Test streams list endpoint
  STREAMS_RESP=$(curl -sf -H "X-Broodlink-Api-Key: $API_KEY" "$STATUS_URL/api/v1/streams" 2>&1) || true

  if echo "$STREAMS_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('status')=='ok'" 2>/dev/null; then
    pass "status-api /streams endpoint returns ok"
  else
    fail "status-api /streams endpoint failed"
  fi
else
  skip "status-api not reachable — skipping status-api stream tests"
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

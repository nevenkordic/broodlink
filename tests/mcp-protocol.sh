#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Tests for MCP Streamable HTTP transport (single /mcp endpoint).
# Part 1: Code structure (always runs). Part 2: Build. Part 3: Integration (requires service).
# Run: bash tests/mcp-protocol.sh

set -euo pipefail

MCP_URL="${MCP_URL:-http://127.0.0.1:3311}"
PASS=0
FAIL=0
SKIP=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }
skip() { SKIP=$((SKIP + 1)); printf "  SKIP: %s\n" "$1"; }

echo "=== MCP Streamable HTTP Protocol Tests ==="
echo ""

# ---------------------------------------------------------------------------
# Part 1: Code structure verification
# ---------------------------------------------------------------------------

echo "--- Part 1: Code structure ---"

SRC="rust/mcp-server/src"

# streamable_http module exists
if [ -f "$SRC/streamable_http.rs" ]; then
  pass "streamable_http.rs module exists"
else
  fail "streamable_http.rs module missing"
fi

# Session management
if grep -q "RwLock<HashMap<String, Session>>" "$SRC/streamable_http.rs"; then
  pass "session management uses RwLock<HashMap>"
else
  fail "session management missing"
fi

# POST /mcp handler
if grep -q "mcp_post_handler" "$SRC/streamable_http.rs"; then
  pass "POST /mcp handler exists"
else
  fail "POST /mcp handler missing"
fi

# GET /mcp handler
if grep -q "mcp_get_handler" "$SRC/streamable_http.rs"; then
  pass "GET /mcp handler exists"
else
  fail "GET /mcp handler missing"
fi

# Origin validation
if grep -q "validate_origin" "$SRC/streamable_http.rs"; then
  pass "origin validation function exists"
else
  fail "origin validation missing"
fi

# Session reaper
if grep -q "reaped expired sessions" "$SRC/streamable_http.rs"; then
  pass "session reaper implemented"
else
  fail "session reaper missing"
fi

# Mcp-Session-Id header
if grep -q "mcp-session-id" "$SRC/streamable_http.rs"; then
  pass "Mcp-Session-Id header used"
else
  fail "Mcp-Session-Id header missing"
fi

# Transport selector in main.rs
if grep -q '"http"' "$SRC/main.rs" && grep -q "streamable_http::run_http_transport" "$SRC/main.rs"; then
  pass "main.rs routes to streamable_http on transport=http"
else
  fail "main.rs transport routing missing"
fi

# CORS layer
if grep -q "build_cors_layer" "$SRC/streamable_http.rs"; then
  pass "CORS layer builder exists"
else
  fail "CORS layer missing"
fi

# Config transport field
if grep -q "transport" "crates/broodlink-config/src/lib.rs"; then
  pass "config has transport field"
else
  fail "config transport field missing"
fi

# ---------------------------------------------------------------------------
# Part 2: Build + unit tests
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 2: Build + unit tests ---"

if cargo test -p mcp-server 2>&1 | grep -q "test result: ok"; then
  pass "cargo test -p mcp-server: all tests pass"
else
  fail "cargo test -p mcp-server: some tests failed"
fi

# Check streamable_http tests exist
HTTP_TESTS=$(cargo test -p mcp-server -- --list 2>&1 | grep -c "streamable_http::tests" || true)
if [ "$HTTP_TESTS" -ge 4 ]; then
  pass "streamable_http module has $HTTP_TESTS unit tests (>= 4)"
else
  fail "streamable_http module has only $HTTP_TESTS unit tests (expected >= 4)"
fi

# ---------------------------------------------------------------------------
# Part 3: Integration tests (require running MCP server)
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 3: Integration tests (service required) ---"

if curl -sf "$MCP_URL/health" > /dev/null 2>&1; then
  # Health endpoint
  HEALTH=$(curl -sf "$MCP_URL/health")
  if echo "$HEALTH" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('status')=='ok'" 2>/dev/null; then
    pass "GET /health returns ok"
  else
    fail "GET /health unexpected response"
  fi

  # Initialize via POST /mcp
  INIT_RESP=$(curl -sf -X POST \
    -H "Content-Type: application/json" \
    -D /tmp/mcp-headers.txt \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
    "$MCP_URL/mcp" 2>&1) || true

  SESSION_ID=$(grep -i "mcp-session-id" /tmp/mcp-headers.txt 2>/dev/null | tr -d '\r' | awk '{print $2}') || true

  if [ -n "$SESSION_ID" ]; then
    pass "POST /mcp initialize returns Mcp-Session-Id header"
  else
    fail "POST /mcp initialize missing Mcp-Session-Id"
  fi

  if echo "$INIT_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['result']['protocolVersion']" 2>/dev/null; then
    pass "initialize response has protocolVersion"
  else
    fail "initialize response missing protocolVersion"
  fi

  # Use session for tools/list
  if [ -n "$SESSION_ID" ]; then
    TOOLS_RESP=$(curl -sf -X POST \
      -H "Content-Type: application/json" \
      -H "Mcp-Session-Id: $SESSION_ID" \
      -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
      "$MCP_URL/mcp" 2>&1) || true

    if echo "$TOOLS_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert len(d['result']['tools'])>0" 2>/dev/null; then
      pass "tools/list returns tools with valid session"
    else
      fail "tools/list failed with valid session"
    fi
  fi

  # Request without session should fail (non-initialize)
  NO_SESSION_RESP=$(curl -s -o /dev/null -w "%{http_code}" -X POST \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}' \
    "$MCP_URL/mcp" 2>&1) || true

  if [ "$NO_SESSION_RESP" = "400" ]; then
    pass "POST /mcp without session returns 400"
  else
    fail "POST /mcp without session returned $NO_SESSION_RESP (expected 400)"
  fi

  # GET /mcp without session returns 405
  GET_STATUS=$(curl -s -o /dev/null -w "%{http_code}" "$MCP_URL/mcp" 2>&1) || true
  if [ "$GET_STATUS" = "405" ]; then
    pass "GET /mcp without session returns 405"
  else
    fail "GET /mcp without session returned $GET_STATUS (expected 405)"
  fi
else
  skip "MCP server not reachable at $MCP_URL — skipping integration tests"
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

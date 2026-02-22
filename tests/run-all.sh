#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Master test runner — executes all test suites and reports aggregate results.
# Run: bash tests/run-all.sh

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

TOTAL_SUITES=0
PASSED_SUITES=0
FAILED_SUITES=0
FAILED_NAMES=()

run_suite() {
  local name="$1"
  local cmd="$2"
  TOTAL_SUITES=$((TOTAL_SUITES + 1))

  echo "=================================================================="
  echo "  Suite: $name"
  echo "=================================================================="
  echo ""

  if eval "$cmd"; then
    PASSED_SUITES=$((PASSED_SUITES + 1))
    echo ""
    echo "  >> $name: PASSED"
  else
    FAILED_SUITES=$((FAILED_SUITES + 1))
    FAILED_NAMES+=("$name")
    echo ""
    echo "  >> $name: FAILED"
  fi
  echo ""
}

echo ""
echo "========================================"
echo "  Broodlink Full Test Suite"
echo "========================================"
echo ""

# --- Rust unit tests (no external services needed) ---
run_suite "Rust unit tests (cargo test)" \
  "cargo test --workspace 2>&1"

# --- Offline shell tests ---
run_suite "LaunchAgent portability" \
  "bash tests/launchagent-portability.sh"

run_suite "Security audit" \
  "bash tests/security-audit.sh"

# --- Online tests (require running services) ---
# Check if beads-bridge is reachable before running integration tests
if curl -sf http://127.0.0.1:3310/health > /dev/null 2>&1; then
  run_suite "Bridge auth rejection" \
    "bash tests/bridge-auth-rejection.sh"

  sleep 15  # let rate-limit bucket refill between bridge test suites

  run_suite "Bridge error cases" \
    "bash tests/bridge-error-cases.sh"

  sleep 15  # let rate-limit bucket refill before the big round-trip suite

  run_suite "Bridge tool round-trips" \
    "bash tests/bridge-tool-roundtrips.sh"
else
  echo "  SKIP: beads-bridge not running — skipping integration tests"
  echo ""
fi

# Check if status-api is reachable (routes are under /api/v1, auth required)
if curl -sf -H "X-Broodlink-Api-Key: dev-api-key" http://127.0.0.1:3312/api/v1/health > /dev/null 2>&1; then
  run_suite "Status API smoke tests" \
    "bash status-site/tests/api-smoke.sh"
else
  echo "  SKIP: status-api not running — skipping smoke tests"
  echo ""
fi

# Frontend smoke tests (require Hugo dev server)
if curl -sf http://127.0.0.1:1313/ > /dev/null 2>&1; then
  run_suite "Frontend smoke tests" \
    "bash status-site/tests/frontend-smoke.sh"
else
  echo "  SKIP: Hugo dev server not running — skipping frontend tests"
  echo ""
fi

# JS regression tests
if [ -f "status-site/tests/js-regression.sh" ]; then
  run_suite "JS regression tests" \
    "bash status-site/tests/js-regression.sh"
fi

# DOM cross-check tests (JS getElementById ↔ HTML id alignment)
if [ -f "status-site/tests/dom-crosscheck.sh" ]; then
  run_suite "DOM ID cross-check tests" \
    "bash status-site/tests/dom-crosscheck.sh"
fi

# Control panel tests (HTML, CSS, JS, API endpoints, design elements)
if [ -f "status-site/tests/control-panel.sh" ] && curl -sf http://127.0.0.1:1313/ > /dev/null 2>&1; then
  run_suite "Control panel tests" \
    "bash status-site/tests/control-panel.sh"
fi

# Agent regression tests
if [ -f "tests/agent-regression.sh" ]; then
  run_suite "Agent regression tests" \
    "bash tests/agent-regression.sh"
fi

# v0.3.0 routing tests
if [ -f "tests/coordinator-routing.sh" ]; then
  run_suite "Coordinator routing tests" \
    "bash tests/coordinator-routing.sh"
fi

# v0.3.0 stream delivery tests
if [ -f "tests/stream-delivery.sh" ]; then
  run_suite "Stream delivery tests" \
    "bash tests/stream-delivery.sh"
fi

# v0.3.0 MCP protocol tests
if [ -f "tests/mcp-protocol.sh" ]; then
  run_suite "MCP Streamable HTTP tests" \
    "bash tests/mcp-protocol.sh"
fi

# v0.3.0 A2A protocol tests
if [ -f "tests/a2a-protocol.sh" ]; then
  run_suite "A2A protocol tests" \
    "bash tests/a2a-protocol.sh"
fi

# v0.5.0 Knowledge Graph tests
if [ -f "tests/knowledge-graph.sh" ]; then
  sleep 10  # let rate-limit bucket refill before KG tests
  run_suite "Knowledge Graph tests" \
    "bash tests/knowledge-graph.sh"
fi

# v0.6.0 regression tests
if [ -f "tests/v060-regression.sh" ]; then
  sleep 15  # let rate-limit bucket refill before v0.6.0 tests
  run_suite "v0.6.0 regression tests" \
    "bash tests/v060-regression.sh"
fi

# v0.7.0 chat integration tests
if [ -f "tests/chat-integration.sh" ]; then
  sleep 10  # let rate-limit bucket refill before chat tests
  run_suite "Chat integration tests" \
    "bash tests/chat-integration.sh"
fi

# v0.7.0 formula registry tests
if [ -f "tests/formula-registry.sh" ]; then
  sleep 10  # let rate-limit bucket refill before formula tests
  run_suite "Formula registry tests" \
    "bash tests/formula-registry.sh"
fi

# v0.7.0 dashboard auth tests
if [ -f "tests/dashboard-auth.sh" ]; then
  run_suite "Dashboard auth tests" \
    "bash tests/dashboard-auth.sh"
fi

# v0.3.0 deployment tests
if [ -f "tests/deployment.sh" ]; then
  run_suite "Deployment configuration tests" \
    "bash tests/deployment.sh"
fi

# --- Summary ---
echo "========================================"
echo "  Summary: $PASSED_SUITES/$TOTAL_SUITES suites passed"
echo "========================================"

if [ ${#FAILED_NAMES[@]} -gt 0 ]; then
  echo ""
  echo "  Failed suites:"
  for name in "${FAILED_NAMES[@]}"; do
    echo "    - $name"
  done
fi

echo ""
[ "$FAILED_SUITES" -eq 0 ] && exit 0 || exit 1

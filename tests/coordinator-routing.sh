#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Unit + integration tests for coordinator smart routing algorithm.
# Unit tests run via `cargo test -p coordinator`.
# Integration tests require: coordinator, beads-bridge, NATS, Dolt, Postgres running.
# Run: bash tests/coordinator-routing.sh

set -euo pipefail

PASS=0
FAIL=0
SKIP=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }
skip() { SKIP=$((SKIP + 1)); printf "  SKIP: %s\n" "$1"; }

echo "=== Coordinator Routing Tests ==="
echo ""

# ---------------------------------------------------------------------------
# Part 1: Cargo unit tests (always available)
# ---------------------------------------------------------------------------

echo "--- Part 1: Unit tests (cargo test) ---"

if cargo test -p coordinator 2>&1 | grep -q "test result: ok"; then
  pass "cargo test -p coordinator: all unit tests pass"
else
  fail "cargo test -p coordinator: some tests failed"
fi

# Check that scoring tests exist
SCORING_TESTS=$(cargo test -p coordinator -- --list 2>&1 | grep -c "test_compute_score\|test_rank_agents\|test_cost_tier_score\|test_recency_score" || true)
if [ "$SCORING_TESTS" -ge 8 ]; then
  pass "scoring algorithm has $SCORING_TESTS unit tests (>= 8)"
else
  fail "scoring algorithm has only $SCORING_TESTS unit tests (expected >= 8)"
fi

# Check routing decision payload test exists
if cargo test -p coordinator -- --list 2>&1 | grep -q "test_routing_decision_payload"; then
  pass "routing_decision_payload serialization test exists"
else
  fail "routing_decision_payload serialization test missing"
fi

# ---------------------------------------------------------------------------
# Part 2: Code structure verification
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 2: Code structure ---"

MAIN_RS="rust/coordinator/src/main.rs"

# Check compute_score function exists
if grep -q "fn compute_score" "$MAIN_RS"; then
  pass "compute_score function exists"
else
  fail "compute_score function missing"
fi

# Check rank_agents function exists
if grep -q "fn rank_agents" "$MAIN_RS"; then
  pass "rank_agents function exists"
else
  fail "rank_agents function missing"
fi

# Check fetch_agent_metrics function exists
if grep -q "fn fetch_agent_metrics" "$MAIN_RS"; then
  pass "fetch_agent_metrics function exists"
else
  fail "fetch_agent_metrics function missing"
fi

# Check routing decision NATS publish
if grep -q "routing_decision" "$MAIN_RS"; then
  pass "routing_decision NATS publish present"
else
  fail "routing_decision NATS publish missing"
fi

# Check try-next-candidate pattern (NOT backoff on same agent)
if grep -q "trying next candidate" "$MAIN_RS"; then
  pass "try-next-candidate pattern implemented"
else
  fail "try-next-candidate pattern missing"
fi

# Check weighted scoring formula uses config weights
if grep -q "weights.capability" "$MAIN_RS" && grep -q "weights.success_rate" "$MAIN_RS"; then
  pass "scoring uses configurable weights"
else
  fail "scoring does not use configurable weights"
fi

# Check agent_metrics Postgres query
if grep -q "agent_metrics" "$MAIN_RS"; then
  pass "agent_metrics table queried"
else
  fail "agent_metrics table not queried"
fi

# Check new_agent_bonus is used
if grep -q "new_agent_bonus" "$MAIN_RS"; then
  pass "new_agent_bonus parameter used"
else
  fail "new_agent_bonus parameter missing"
fi

# ---------------------------------------------------------------------------
# Part 3: Integration tests (require running services)
# ---------------------------------------------------------------------------

echo ""
echo "--- Part 3: Integration tests (services required) ---"

NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"
PG_URL="${PG_URL:-postgres://postgres:changeme@127.0.0.1:5432/broodlink_hot}"

# Check if NATS is reachable
if command -v nats >/dev/null 2>&1 && nats server check connection --server="$NATS_URL" 2>/dev/null; then
  NATS_OK=true
else
  NATS_OK=false
fi

if [ "$NATS_OK" = true ]; then
  # Subscribe and check for routing_decision messages
  skip "routing_decision NATS publish (requires live task dispatch)"
else
  skip "NATS not reachable — skipping integration tests"
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

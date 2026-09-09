#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Offline regression tests for operator setup, formula drafts, and runtimes.
# Run: bash tests/capability.sh

set -euo pipefail

cd "$(dirname "$0")/.." || exit 1

PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

TMPDIR_CAP=$(mktemp -d)
trap 'rm -rf "$TMPDIR_CAP"' EXIT
CFG="$TMPDIR_CAP/config.toml"

echo "=== Capability plan regression ==="
echo ""

echo "--- broodctl setup (non-interactive) ---"
if ./broodctl setup --non-interactive --model gemma4:e4b --tools core,files,web --config "$CFG" >/dev/null; then
  pass "setup writes a config file"
else
  fail "setup failed"
fi

if grep -q 'chat_model = "gemma4:e4b"' "$CFG"; then
  pass "setup set chat_model"
else
  fail "setup did not set chat_model"
fi

if grep -q 'file_tools_enabled = true' "$CFG" && grep -q 'web_search_enabled = true' "$CFG"; then
  pass "setup enabled files + web"
else
  fail "setup tool groups not applied"
fi

if grep -q 'command_tools_enabled = false' "$CFG"; then
  pass "setup left commands disabled"
else
  fail "commands should be off when omitted from --tools"
fi

echo ""
echo "--- broodctl model / tools ---"
CURRENT=$(./broodctl model --config "$CFG")
if [[ "$CURRENT" == "gemma4:e4b" ]]; then
  pass "model prints current value"
else
  fail "model print: expected gemma4:e4b got $CURRENT"
fi

./broodctl model gemma4:31b --config "$CFG" >/dev/null
if grep -q 'chat_model = "gemma4:31b"' "$CFG"; then
  pass "model updates chat_model"
else
  fail "model did not update chat_model"
fi

./broodctl tools core,commands --config "$CFG" >/dev/null
if grep -q 'command_tools_enabled = true' "$CFG" && grep -q 'file_tools_enabled = false' "$CFG"; then
  pass "tools updates groups"
else
  fail "tools did not flip file/command flags"
fi

TOOL_GROUPS=$(./broodctl tools --config "$CFG")
if [[ "$TOOL_GROUPS" == "core,commands" ]]; then
  pass "tools prints enabled groups"
else
  fail "tools print: expected core,commands got $TOOL_GROUPS"
fi

if ./broodctl setup --non-interactive --tools core,bogus --config "$CFG" >/dev/null 2>&1; then
  fail "setup should reject unknown tool groups"
else
  pass "setup rejects unknown tool groups"
fi

echo ""
echo "--- help lists new commands ---"
HELP=$(./broodctl help || true)
if echo "$HELP" | grep -q setup && echo "$HELP" | grep -q model && echo "$HELP" | grep -q tools && echo "$HELP" | grep -q drafts; then
  pass "help lists setup/model/tools/drafts"
else
  fail "help missing new commands"
fi

echo ""
echo "--- worker script is env-driven ---"
if grep -q 'BROODLINK_WORKER_JWT' scripts/broodlink-worker.sh && ! grep -qE 'eval |\$\(' scripts/broodlink-worker.sh; then
  pass "worker script uses env JWT and no eval"
else
  fail "worker script looks unsafe or incomplete"
fi

echo ""
echo "--- migration 032 present ---"
if grep -q 'CREATE TABLE IF NOT EXISTS formula_drafts' migrations/032_formula_drafts_workers.sql \
  && grep -q 'CREATE TABLE IF NOT EXISTS workers' migrations/032_formula_drafts_workers.sql; then
  pass "032 defines formula_drafts and workers"
else
  fail "032 missing expected tables"
fi

if grep -q '032_formula_drafts_workers' scripts/db-setup.sh; then
  pass "db-setup applies 032"
else
  fail "db-setup does not apply 032"
fi

echo ""
echo "--- formula crate never marks drafts system ---"
if grep -q 'is_system' crates/broodlink-formulas/src/lib.rs; then
  # draft helpers must not set is_system
  if grep -n 'is_system' crates/broodlink-formulas/src/lib.rs | grep -v '^#' | grep -q 'true'; then
    fail "formulas crate must not set is_system = true"
  else
    pass "formulas crate does not publish system formulas"
  fi
else
  pass "formulas crate has no is_system flag on drafts"
fi

echo ""
echo "========================================"
echo "  Results: $PASS passed, $FAIL failed"
echo "========================================"

[[ "$FAIL" -eq 0 ]]

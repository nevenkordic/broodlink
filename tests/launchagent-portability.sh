#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Portability tests for LaunchAgent plist templates.
# Ensures no hardcoded paths exist in template files.
# Run: bash tests/launchagent-portability.sh

set -euo pipefail

PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

PLIST_DIR="launchagents"

echo "=== LaunchAgent Portability Tests ==="
echo ""

echo "--- Plist Template Placeholders ---"

for plist in com.broodlink.beads-bridge.plist \
             com.broodlink.coordinator.plist \
             com.broodlink.heartbeat.plist \
             com.broodlink.embedding-worker.plist \
             com.broodlink.status-api.plist \
             com.broodlink.hugo-status.plist; do
  file="$PLIST_DIR/$plist"
  short="${plist#com.broodlink.}"
  short="${short%.plist}"

  if [ ! -f "$file" ]; then
    fail "$short: plist not found at $file"
    continue
  fi

  # Must use __BROOD_DIR__ placeholder
  if grep -q '__BROOD_DIR__' "$file"; then
    pass "$short: uses __BROOD_DIR__"
  else
    fail "$short: missing __BROOD_DIR__ placeholder"
  fi

  # Must use __LOG_DIR__ placeholder
  if grep -q '__LOG_DIR__' "$file"; then
    pass "$short: uses __LOG_DIR__"
  else
    fail "$short: missing __LOG_DIR__ placeholder"
  fi

  # Must NOT contain hardcoded /Users/ paths
  if grep -qE '/Users/[a-z]+/' "$file"; then
    fail "$short: contains hardcoded /Users/ path"
  else
    pass "$short: no hardcoded paths"
  fi
done

echo ""
echo "--- Install Script ---"

SCRIPT="scripts/launchagents.sh"

if [ ! -f "$SCRIPT" ]; then
  fail "launchagents.sh: not found"
else
  # Must do sed substitution for all 3 placeholders
  if grep -q '__BROOD_DIR__' "$SCRIPT"; then
    pass "launchagents.sh: substitutes __BROOD_DIR__"
  else
    fail "launchagents.sh: missing __BROOD_DIR__ substitution"
  fi

  if grep -q '__LOG_DIR__' "$SCRIPT"; then
    pass "launchagents.sh: substitutes __LOG_DIR__"
  else
    fail "launchagents.sh: missing __LOG_DIR__ substitution"
  fi

  if grep -q '__HOMEBREW_PREFIX__' "$SCRIPT"; then
    pass "launchagents.sh: substitutes __HOMEBREW_PREFIX__"
  else
    fail "launchagents.sh: missing __HOMEBREW_PREFIX__ substitution"
  fi

  # Must detect homebrew prefix dynamically
  if grep -q 'brew --prefix' "$SCRIPT"; then
    pass "launchagents.sh: detects homebrew prefix"
  else
    fail "launchagents.sh: missing brew --prefix detection"
  fi
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

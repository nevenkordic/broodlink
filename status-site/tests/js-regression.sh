#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Regression tests for Broodlink status-site JavaScript modules.
# Validates that JS files reference the correct global namespace and API fields.
# Run: bash status-site/tests/js-regression.sh

set -euo pipefail

PASS=0
FAIL=0
JS_DIR="status-site/themes/broodlink-status/static/js"

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

echo "=== JS Namespace Regression Tests ==="

# Every JS module that references window.Broodlink must NOT reference window.BL
for f in agents.js audit.js commits.js decisions.js memory.js dashboard.js beads.js; do
  file="$JS_DIR/$f"
  if [ ! -f "$file" ]; then
    fail "$f: file not found"
    continue
  fi

  if grep -q 'window\.BL\b' "$file" 2>/dev/null; then
    fail "$f: references window.BL (should be window.Broodlink)"
  else
    pass "$f: no window.BL references"
  fi
done

echo ""
echo "=== JS API Field Regression Tests ==="

# agents.js must use latest_work (not latest_action)
if grep -q 'latest_action' "$JS_DIR/agents.js" 2>/dev/null; then
  fail "agents.js: references latest_action (API returns latest_work)"
else
  pass "agents.js: uses latest_work"
fi

# agents.js must use updated_at for last seen (not last_seen)
if grep -q 'a\.last_seen' "$JS_DIR/agents.js" 2>/dev/null; then
  fail "agents.js: references a.last_seen (API returns updated_at)"
else
  pass "agents.js: uses updated_at"
fi

# audit.js must handle data.audit (not just data.entries)
if grep -q 'data\.audit' "$JS_DIR/audit.js" 2>/dev/null; then
  pass "audit.js: handles data.audit key"
else
  fail "audit.js: missing data.audit handler (API returns audit key)"
fi

# decisions.js must handle decision field (not just title)
if grep -q 'd\.decision' "$JS_DIR/decisions.js" 2>/dev/null; then
  pass "decisions.js: handles d.decision field"
else
  fail "decisions.js: missing d.decision (API returns decision, not title)"
fi

# decisions.js must handle agent_id (not just agent_name)
if grep -q 'agent_id' "$JS_DIR/decisions.js" 2>/dev/null; then
  pass "decisions.js: handles agent_id field"
else
  fail "decisions.js: missing agent_id (API returns agent_id, not agent_name)"
fi

# decisions.js must handle reasoning field (not just rationale)
if grep -q 'reasoning' "$JS_DIR/decisions.js" 2>/dev/null; then
  pass "decisions.js: handles reasoning field"
else
  fail "decisions.js: missing reasoning (API returns reasoning, not rationale)"
fi

# memory.js must handle max_updated_at
if grep -q 'max_updated_at' "$JS_DIR/memory.js" 2>/dev/null; then
  pass "memory.js: handles max_updated_at"
else
  fail "memory.js: missing max_updated_at (API returns max_updated_at)"
fi

# commits.js must use REFRESH_INTERVAL (not refreshInterval)
if grep -q 'REFRESH_INTERVAL' "$JS_DIR/commits.js" 2>/dev/null; then
  pass "commits.js: uses REFRESH_INTERVAL"
else
  fail "commits.js: missing REFRESH_INTERVAL constant reference"
fi

# beads.js must use REFRESH_INTERVAL
if grep -q 'REFRESH_INTERVAL' "$JS_DIR/beads.js" 2>/dev/null; then
  pass "beads.js: uses REFRESH_INTERVAL"
else
  fail "beads.js: missing REFRESH_INTERVAL constant reference"
fi

# beads.js must use statusDot for badges
if grep -q 'statusDot' "$JS_DIR/beads.js" 2>/dev/null; then
  pass "beads.js: uses statusDot for badges"
else
  fail "beads.js: missing statusDot (should use BL.statusDot for status badges)"
fi

# beads.js must use fetchApi (not raw fetch)
if grep -q 'fetchApi' "$JS_DIR/beads.js" 2>/dev/null; then
  pass "beads.js: uses fetchApi"
else
  fail "beads.js: missing fetchApi (should use BL.fetchApi)"
fi

echo ""
echo "=== Sidebar Logo Regression ==="

SIDEBAR="status-site/themes/broodlink-status/layouts/partials/sidebar.html"
if grep -q 'broodlink-logo\.svg' "$SIDEBAR" 2>/dev/null; then
  pass "sidebar: references broodlink-logo.svg"
else
  fail "sidebar: missing logo SVG reference"
fi

if grep -q 'sidebar-logo' "$SIDEBAR" 2>/dev/null; then
  pass "sidebar: uses sidebar-logo CSS class"
else
  fail "sidebar: missing sidebar-logo CSS class"
fi

CSS="status-site/themes/broodlink-status/static/css/style.css"
if grep -q 'sidebar-logo' "$CSS" 2>/dev/null; then
  pass "style.css: has sidebar-logo rules"
else
  fail "style.css: missing sidebar-logo CSS rules"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

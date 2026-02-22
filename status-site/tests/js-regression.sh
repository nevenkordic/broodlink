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
for f in agents.js audit.js commits.js decisions.js memory.js dashboard.js beads.js knowledge-graph.js chat.js auth.js; do
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
echo "=== Dashboard Field Regression Tests ==="

# dashboard.js must use counts_by_status for pending metric (not filter recent array)
if grep -q 'counts_by_status' "$JS_DIR/dashboard.js" 2>/dev/null; then
  pass "dashboard.js: uses counts_by_status for pending metric"
else
  fail "dashboard.js: missing counts_by_status (must use server-provided count, not filter recent)"
fi

# dashboard.js must use assigned_agent for task table agent column
if grep -q 'assigned_agent' "$JS_DIR/dashboard.js" 2>/dev/null; then
  pass "dashboard.js: uses assigned_agent field"
else
  fail "dashboard.js: missing assigned_agent (API returns assigned_agent, not agent_id)"
fi

# dashboard.js must use REFRESH_INTERVAL
if grep -q 'REFRESH_INTERVAL' "$JS_DIR/dashboard.js" 2>/dev/null; then
  pass "dashboard.js: uses REFRESH_INTERVAL"
else
  fail "dashboard.js: missing REFRESH_INTERVAL constant reference"
fi

echo ""
echo "=== Memory Field Regression Tests ==="

# memory.js must use content_length (not raw content string length)
if grep -q 'content_length' "$JS_DIR/memory.js" 2>/dev/null; then
  pass "memory.js: uses content_length field from API"
else
  fail "memory.js: missing content_length (should use API-provided content_length, not content.length)"
fi

# memory.js chart must specify unit label
if grep -q "unit.*chars\|unit:.*'chars'" "$JS_DIR/memory.js" 2>/dev/null; then
  pass "memory.js: chart specifies unit label"
else
  fail "memory.js: chart missing unit label (bars show raw numbers without context)"
fi

echo ""
echo "=== Chat Field Regression Tests ==="

# chat.js must use fetchApi (not raw fetch)
if grep -q 'fetchApi' "$JS_DIR/chat.js" 2>/dev/null; then
  pass "chat.js: uses fetchApi"
else
  fail "chat.js: missing fetchApi (should use BL.fetchApi)"
fi

# chat.js must use REFRESH_INTERVAL
if grep -q 'REFRESH_INTERVAL' "$JS_DIR/chat.js" 2>/dev/null; then
  pass "chat.js: uses REFRESH_INTERVAL"
else
  fail "chat.js: missing REFRESH_INTERVAL constant reference"
fi

# chat.js must handle session status rendering
if grep -q 'active_sessions\|message_count\|last_message_at' "$JS_DIR/chat.js" 2>/dev/null; then
  pass "chat.js: handles session fields (active_sessions/message_count/last_message_at)"
else
  fail "chat.js: missing expected chat session field references"
fi

# chat.js must reference the platform filter
if grep -q 'platform' "$JS_DIR/chat.js" 2>/dev/null; then
  pass "chat.js: handles platform filtering"
else
  fail "chat.js: missing platform filter logic"
fi

# chat.js must handle message direction (inbound/outbound)
if grep -q 'direction.*inbound\|inbound.*outbound' "$JS_DIR/chat.js" 2>/dev/null; then
  pass "chat.js: handles message direction (inbound/outbound)"
else
  fail "chat.js: missing inbound/outbound message direction handling"
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
echo "=== Auth JS Field Regression Tests ==="

# auth.js must use sessionStorage (not localStorage)
if grep -q 'localStorage' "$JS_DIR/auth.js" 2>/dev/null; then
  fail "auth.js: uses localStorage (should use sessionStorage)"
else
  pass "auth.js: uses sessionStorage"
fi

# auth.js must export BLAuth namespace
if grep -q 'window\.BLAuth' "$JS_DIR/auth.js" 2>/dev/null; then
  pass "auth.js: exports BLAuth namespace"
else
  fail "auth.js: missing BLAuth namespace"
fi

# auth.js must send X-Broodlink-Session header
if grep -q 'X-Broodlink-Session' "$JS_DIR/auth.js" 2>/dev/null; then
  pass "auth.js: sends X-Broodlink-Session header"
else
  fail "auth.js: missing X-Broodlink-Session header"
fi

# utils.js must inject session token
if grep -q 'broodlink_session_token' "$JS_DIR/utils.js" 2>/dev/null; then
  pass "utils.js: injects session token in fetchApi"
else
  fail "utils.js: missing session token injection"
fi

# control.js must have enforceRoles function
if grep -q 'enforceRoles' "$JS_DIR/control.js" 2>/dev/null; then
  pass "control.js: has enforceRoles function"
else
  fail "control.js: missing enforceRoles"
fi

# control.js must export user management functions
for fn in createUser changeRole toggleUser resetPassword; do
  if grep -q "$fn" "$JS_DIR/control.js" 2>/dev/null; then
    pass "control.js: exports $fn"
  else
    fail "control.js: missing $fn"
  fi
done

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

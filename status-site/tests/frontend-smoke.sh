#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Frontend smoke tests — verify dashboard pages load and contain expected elements.
# Requires: Hugo dev server running on port 1313
# Run: bash status-site/tests/frontend-smoke.sh

set -euo pipefail

SITE_URL="${SITE_URL:-http://127.0.0.1:1313}"
PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

# Fetch a page, check HTTP status and grep for expected content
check_page() {
  local path="$1"
  local desc="$2"
  shift 2
  # remaining args are grep patterns that must all be present

  local body
  body=$(curl -sf "$SITE_URL$path" 2>&1) || {
    fail "$desc ($path): HTTP error or unreachable"
    return
  }

  for pattern in "$@"; do
    if ! echo "$body" | grep -q "$pattern"; then
      fail "$desc ($path): missing '$pattern'"
      return
    fi
  done

  pass "$desc ($path)"
}

echo "=== Frontend Smoke Tests ==="
echo "Target: $SITE_URL"
echo ""

# -----------------------------------------------------------------------
# Page loads (HTTP 200 + basic structure)
# -----------------------------------------------------------------------
echo "--- Page Loads ---"

check_page "/" "Dashboard" \
  "Broodlink Operations" \
  "utils.js"

check_page "/agents/" "Agents" \
  "aria-current" \
  "agents"

check_page "/decisions/" "Decisions" \
  "aria-current"

check_page "/memory/" "Memory" \
  "aria-current"

check_page "/commits/" "Commits" \
  "aria-current"

check_page "/audit/" "Audit Log" \
  "aria-current"

check_page "/beads/" "Beads" \
  "aria-current"

check_page "/approvals/" "Approvals" \
  "aria-current"

check_page "/delegations/" "Delegations" \
  "aria-current"

check_page "/guardrails/" "Guardrails" \
  "aria-current"

echo ""

# -----------------------------------------------------------------------
# Navigation structure
# -----------------------------------------------------------------------
echo "--- Navigation ---"

check_page "/" "Sidebar nav links" \
  'href="/agents/"' \
  'href="/decisions/"' \
  'href="/approvals/"' \
  'href="/guardrails/"'

check_page "/" "Version badge" \
  "v0.6.0"

check_page "/" "Logo" \
  "broodlink-logo.svg"

echo ""

# -----------------------------------------------------------------------
# Approvals page — policy editor
# -----------------------------------------------------------------------
echo "--- Approvals Page ---"

check_page "/approvals/" "Status filter" \
  "approval-status-filter" \
  "pending" \
  "auto_approved" \
  "expired"

check_page "/approvals/" "Policy editor form" \
  "policy-editor" \
  "btn-new-policy" \
  "policy-form" \
  "pf-name" \
  "pf-gate-type"

check_page "/approvals/" "Severity checkboxes" \
  'value="low"' \
  'value="medium"' \
  'value="high"' \
  'value="critical"'

check_page "/approvals/" "Policy form fields" \
  "pf-agent-ids" \
  "pf-tool-names" \
  "pf-auto-approve" \
  "pf-threshold" \
  "pf-expiry"

check_page "/approvals/" "Policy actions" \
  "Save Policy" \
  "btn-cancel-policy"

check_page "/approvals/" "Policies table container" \
  "approval-policies"

# -----------------------------------------------------------------------
# v0.3.0 pages
# -----------------------------------------------------------------------
echo "--- v0.3.0 Pages ---"

check_page "/a2a/" "A2A page" \
  "A2A Gateway" \
  "a2a.js"

# -----------------------------------------------------------------------
# v0.5.0 Knowledge Graph page
# -----------------------------------------------------------------------
echo "--- v0.5.0 Pages ---"

check_page "/knowledge-graph/" "Knowledge Graph page" \
  "Knowledge Graph" \
  "knowledge-graph.js" \
  "charts.js" \
  "kg-total-entities" \
  "kg-types-chart" \
  "kg-connected-tbody" \
  "kg-edges-tbody"

echo ""

# -----------------------------------------------------------------------
# v0.6.0 Control Panel page
# -----------------------------------------------------------------------
echo "--- v0.6.0 Pages ---"

check_page "/control/" "Control Panel page" \
  "Control Panel" \
  "control.js" \
  "tab-agents" \
  "tab-budgets" \
  "tab-webhooks" \
  "ctrl-metric-agents" \
  "ctrl-toast-container"

echo ""

# -----------------------------------------------------------------------
# v0.3.0 nav
# -----------------------------------------------------------------------
echo "--- v0.3.0 Navigation ---"

check_page "/" "A2A nav link" \
  'href="/a2a/"'

check_page "/" "Knowledge Graph nav link" \
  'href="/knowledge-graph/"'

check_page "/" "Control Panel nav link" \
  'href="/control/"'

echo ""

# -----------------------------------------------------------------------
# v0.3.0 agent metrics CSS
# -----------------------------------------------------------------------
echo "--- v0.3.0 Agent Metrics ---"

check_page "/agents/" "Agent metrics JS" \
  "agents.js"

echo ""

# -----------------------------------------------------------------------
# Help text — page descriptions and tooltips
# -----------------------------------------------------------------------
echo "--- Help Text ---"

check_page "/" "Dashboard page description" \
  "page-desc" \
  "Live overview"

check_page "/" "Dashboard metric tooltips" \
  "data-tooltip"

check_page "/agents/" "Agents page description" \
  "page-desc" \
  "Registered AI agents"

check_page "/beads/" "Beads column tooltips" \
  'data-tooltip="Unique issue identifier"' \
  'data-tooltip="Group of related issues'

check_page "/approvals/" "Approvals form tooltips" \
  'data-tooltip="When the gate triggers' \
  'data-tooltip="Minimum confidence'

check_page "/commits/" "Commits hash tooltip" \
  'data-tooltip="Dolt commit hash'

echo ""

# -----------------------------------------------------------------------
# JS and CSS loaded
# -----------------------------------------------------------------------
echo "--- Assets ---"

check_page "/" "Dashboard JS" \
  "utils.js"

check_page "/" "Stylesheet" \
  "style.css"

check_page "/approvals/" "Approvals JS" \
  "approvals.js"

echo ""

# -----------------------------------------------------------------------
# API meta tags (used by JS to connect to status-api)
# -----------------------------------------------------------------------
echo "--- API Config ---"

check_page "/" "Status API URL meta" \
  "broodlink:statusApiUrl"

check_page "/" "Status API key meta" \
  "broodlink:statusApiKey"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

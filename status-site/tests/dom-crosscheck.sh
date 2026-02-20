#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# DOM cross-check: verify every getElementById('X') call in JS has a matching
# id="X" in the corresponding HTML layout. Catches silent null-element bugs
# where JS queries an ID that doesn't exist in the template.
#
# Run: bash status-site/tests/dom-crosscheck.sh

set -euo pipefail

PASS=0
FAIL=0
JS_DIR="status-site/themes/broodlink-status/static/js"
LAYOUT_DIR="status-site/themes/broodlink-status/layouts"

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

echo "=== DOM ID Cross-Check Tests ==="
echo ""

# Map each JS file to the HTML layout(s) that include it.
# Format: "js_file:layout_file1 layout_file2 ..."
# dashboard.js is loaded on index.html (the homepage)
declare -a JS_LAYOUT_MAP=(
  "dashboard.js:${LAYOUT_DIR}/index.html"
  "agents.js:${LAYOUT_DIR}/agents/list.html"
  "audit.js:${LAYOUT_DIR}/audit/list.html"
  "decisions.js:${LAYOUT_DIR}/decisions/list.html"
  "memory.js:${LAYOUT_DIR}/memory/list.html"
  "commits.js:${LAYOUT_DIR}/commits/list.html"
  "beads.js:${LAYOUT_DIR}/beads/list.html"
  "approvals.js:${LAYOUT_DIR}/approvals/list.html"
  "delegations.js:${LAYOUT_DIR}/delegations/list.html"
  "guardrails.js:${LAYOUT_DIR}/guardrails/list.html"
  "knowledge-graph.js:${LAYOUT_DIR}/knowledge-graph/list.html"
  "a2a.js:${LAYOUT_DIR}/a2a/list.html"
)

page_fail=0

for mapping in "${JS_LAYOUT_MAP[@]}"; do
  js_name="${mapping%%:*}"
  layout_files="${mapping#*:}"
  js_file="$JS_DIR/$js_name"

  if [ ! -f "$js_file" ]; then
    fail "$js_name: JS file not found"
    continue
  fi

  # Extract all getElementById('...') and getElementById("...") calls
  # Handles both single and double quotes
  ids=$(grep -oE "getElementById\(['\"][^'\"]+['\"]\)" "$js_file" 2>/dev/null \
    | sed -E "s/getElementById\(['\"]([^'\"]+)['\"]\)/\1/" \
    | sort -u)

  if [ -z "$ids" ]; then
    pass "$js_name: no getElementById calls (utility module)"
    continue
  fi

  # Collect all id="..." from the layout file(s)
  all_html_ids=""
  for lf in $layout_files; do
    if [ -f "$lf" ]; then
      file_ids=$(grep -oE 'id="[^"]+"' "$lf" 2>/dev/null \
        | sed -E 's/id="([^"]+)"/\1/' || true)
      all_html_ids="$all_html_ids $file_ids"
    fi
  done

  # Check each JS-referenced ID exists in HTML
  missing=0
  for eid in $ids; do
    # Word-boundary match in the collected HTML IDs (-- prevents IDs from being treated as options)
    if ! echo "$all_html_ids" | grep -qw -- "$eid" 2>/dev/null; then
      fail "$js_name: getElementById('$eid') has NO matching id=\"$eid\" in HTML"
      missing=$((missing + 1))
    fi
  done

  if [ "$missing" -eq 0 ]; then
    id_count=$(echo "$ids" | wc -w | tr -d ' ')
    pass "$js_name: all $id_count getElementById calls match HTML ($layout_files)"
  fi
done

echo ""
echo "=== Reverse Check: Critical HTML IDs Have JS Consumers ==="

# Verify that key interactive elements (containers JS populates) are actually
# referenced by JS. Only check IDs that end in -tbody, -list, -table, -feed,
# -roster, -grid, -card, -filter — these are dynamic containers that MUST have
# JS populating them.
DYNAMIC_SUFFIXES="tbody|list|table|feed|roster|grid|card|filter"

# Layout-only containers: CSS wrappers whose children are individually populated
# by JS — the container ID itself is not referenced and that's intentional.
LAYOUT_EXCEPTIONS="metrics-grid"

for mapping in "${JS_LAYOUT_MAP[@]}"; do
  js_name="${mapping%%:*}"
  layout_files="${mapping#*:}"
  js_file="$JS_DIR/$js_name"

  if [ ! -f "$js_file" ]; then continue; fi

  for lf in $layout_files; do
    if [ ! -f "$lf" ]; then continue; fi

    # Get dynamic container IDs from HTML
    dynamic_ids=$(grep -oE 'id="[^"]+"' "$lf" 2>/dev/null \
      | sed -E 's/id="([^"]+)"/\1/' \
      | grep -E -- "-(${DYNAMIC_SUFFIXES})$" || true)

    for did in $dynamic_ids; do
      # Skip known layout-only containers
      if echo "$LAYOUT_EXCEPTIONS" | grep -qw -- "$did" 2>/dev/null; then
        continue
      fi
      if grep -q -- "$did" "$js_file" 2>/dev/null; then
        pass "$js_name: references dynamic container '$did'"
      else
        fail "$js_name: HTML has dynamic container id=\"$did\" but JS never references it"
      fi
    done
  done
done

echo ""
echo "=== Version Consistency ==="

# The version in hugo.toml must match what the frontend smoke test expects
HUGO_VERSION=$(sed -n 's/.*version.*=.*"\([^"]*\)".*/\1/p' status-site/hugo.toml 2>/dev/null || echo "MISSING")
SMOKE_VERSION=$(sed -n 's/.*"v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)".*/\1/p' status-site/tests/frontend-smoke.sh 2>/dev/null | head -1 || echo "MISSING")

if [ "$HUGO_VERSION" = "MISSING" ]; then
  fail "hugo.toml: version not found"
elif [ "$SMOKE_VERSION" = "MISSING" ]; then
  fail "frontend-smoke.sh: version check not found"
elif [ "$HUGO_VERSION" = "$SMOKE_VERSION" ]; then
  pass "version consistency: hugo.toml ($HUGO_VERSION) matches frontend-smoke.sh ($SMOKE_VERSION)"
else
  fail "version mismatch: hugo.toml=$HUGO_VERSION vs frontend-smoke.sh=$SMOKE_VERSION"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

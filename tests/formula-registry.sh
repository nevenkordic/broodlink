#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Integration tests: Formula Registry (v0.7.0)
# Tests formula CRUD via beads-bridge tools and status-api endpoints.
# Requires: beads-bridge on :3310, status-api on :3312, Postgres with formula_registry schema
# Run: bash tests/formula-registry.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
STATUS_URL="${STATUS_URL:-http://127.0.0.1:3312}"
HUGO_URL="${HUGO_URL:-http://localhost:1313}"
STATUS_API_KEY="${STATUS_API_KEY:-dev-api-key}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"
PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

# Load JWT
if [ ! -f "$JWT_FILE" ]; then
  echo "ERROR: JWT token not found at $JWT_FILE"
  exit 1
fi
JWT=$(cat "$JWT_FILE")

call_tool() {
  local tool="$1"
  local params="$2"
  local desc="$3"
  local expected_key="${4:-}"

  local response
  response=$(curl -sf -X POST \
    -H "Authorization: Bearer $JWT" \
    -H "Content-Type: application/json" \
    -d "{\"params\": $params}" \
    "$BRIDGE_URL/api/v1/tool/$tool" 2>&1) || {
    fail "$desc: HTTP error calling $tool"
    return 1
  }

  local success
  success=$(echo "$response" | python3 -c "import sys,json; print(json.load(sys.stdin).get('success', False))" 2>/dev/null) || {
    fail "$desc: invalid JSON response"
    return 1
  }

  if [ "$success" != "True" ]; then
    fail "$desc: success=$success"
    return 1
  fi

  if [ -n "$expected_key" ]; then
    local has_key
    has_key=$(echo "$response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
print('yes' if '$expected_key' in d else 'no')
" 2>/dev/null)
    if [ "$has_key" != "yes" ]; then
      fail "$desc: missing expected key '$expected_key'"
      return 1
    fi
  fi

  pass "$desc"
  echo "$response"
  return 0
}

echo "=== Formula Registry Integration Tests (v0.7.0) ==="
echo ""

# -----------------------------------------------
# 1. Seed script idempotent
# -----------------------------------------------
echo "--- 1. Seed Script Idempotent ---"

if bash scripts/seed-formulas.sh 2>&1 | grep -q "Seeded.*formulas"; then
  pass "Seed script runs successfully (idempotent)"
else
  fail "Seed script failed"
fi

# Run again to verify idempotency (match end-of-output to avoid SIGPIPE with pipefail)
if bash scripts/seed-formulas.sh 2>&1 | grep -q "Seeded.*formulas"; then
  pass "Seed script is idempotent (re-run succeeds)"
else
  fail "Seed script not idempotent"
fi

echo ""

# -----------------------------------------------
# 2. List formulas includes seeded ones
# -----------------------------------------------
echo "--- 2. List Formulas ---"

LIST_RESP=$(call_tool "list_formulas" '{}' "list_formulas returns seeded formulas" "formulas" 2>&1) || true

has_research=$(echo "$LIST_RESP" | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        formulas = d.get('data', {}).get('formulas', [])
        names = [f.get('name') for f in formulas]
        print('yes' if 'research' in names else 'no')
        break
    except: continue
print('no')
" 2>/dev/null | head -1)

if [ "$has_research" = "yes" ]; then
  pass "Seeded 'research' formula present in list"
else
  fail "Seeded 'research' formula missing from list"
fi

echo ""

# -----------------------------------------------
# 3. Create formula via bridge tool
# -----------------------------------------------
echo "--- 3. Create Formula ---"

DEFINITION=$(python3 -c "
import json
d = {
    'formula': {'name': 'test-registry', 'description': 'Integration test formula', 'version': '1'},
    'parameters': [{'name': 'input', 'type': 'string', 'required': True}],
    'steps': [
        {'name': 'process', 'agent_role': 'worker', 'tools': ['recall_memory'], 'prompt': 'Process {{input}}', 'output': 'result'}
    ],
    'on_failure': None
}
print(json.dumps(json.dumps(d)))
" 2>/dev/null)

call_tool "create_formula" "{\"name\": \"test-registry\", \"display_name\": \"Test Registry Formula\", \"definition\": $DEFINITION}" \
  "create_formula creates new formula" "status" || true

echo ""

# -----------------------------------------------
# 4. Get formula includes new one
# -----------------------------------------------
echo "--- 4. Get Formula ---"

call_tool "get_formula" '{"name": "test-registry"}' "get_formula returns created formula" "definition" || true

echo ""

# -----------------------------------------------
# 5. Update formula increments version
# -----------------------------------------------
echo "--- 5. Update Formula ---"

call_tool "update_formula" '{"name": "test-registry", "description": "Updated description"}' \
  "update_formula updates description" "status" || true

# Update definition to trigger version bump
NEW_DEF=$(python3 -c "
import json
d = {
    'formula': {'name': 'test-registry', 'description': 'Updated', 'version': '1'},
    'steps': [
        {'name': 'process_v2', 'agent_role': 'worker', 'prompt': 'Process v2 {{input}}', 'output': 'result'}
    ]
}
print(json.dumps(json.dumps(d)))
" 2>/dev/null)

VER_RESP=$(call_tool "update_formula" "{\"name\": \"test-registry\", \"definition\": $NEW_DEF}" \
  "update_formula with definition bumps version" "version_bumped" 2>&1) || true

echo ""

# -----------------------------------------------
# 6. Disable formula
# -----------------------------------------------
echo "--- 6. Toggle Formula ---"

call_tool "update_formula" '{"name": "test-registry", "enabled": false}' \
  "update_formula disables formula" "status" || true

echo ""

# -----------------------------------------------
# 7. Status-API formula endpoints
# -----------------------------------------------
echo "--- 7. Status-API Endpoints ---"

FORMULAS_RESP=$(curl -sf -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
  "$STATUS_URL/api/v1/formulas" 2>&1) || FORMULAS_RESP=""

status_ok=$(echo "$FORMULAS_RESP" | python3 -c "
import sys, json
d = json.load(sys.stdin)
print('yes' if d.get('status') == 'ok' and 'formulas' in d.get('data', {}) else 'no')
" 2>/dev/null || echo "no")

if [ "$status_ok" = "yes" ]; then
  pass "GET /api/v1/formulas returns ok with formulas array"
else
  fail "GET /api/v1/formulas failed"
fi

# Get single formula
SINGLE_RESP=$(curl -sf -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
  "$STATUS_URL/api/v1/formulas/research" 2>&1) || SINGLE_RESP=""

single_ok=$(echo "$SINGLE_RESP" | python3 -c "
import sys, json
d = json.load(sys.stdin)
print('yes' if d.get('status') == 'ok' and 'definition' in d.get('data', {}) else 'no')
" 2>/dev/null || echo "no")

if [ "$single_ok" = "yes" ]; then
  pass "GET /api/v1/formulas/research returns full definition"
else
  fail "GET /api/v1/formulas/research failed"
fi

# Toggle formula
TOGGLE_RESP=$(curl -sf -X POST \
  -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{}' \
  "$STATUS_URL/api/v1/formulas/test-registry/toggle" 2>&1) || TOGGLE_RESP=""

toggle_ok=$(echo "$TOGGLE_RESP" | python3 -c "
import sys, json
d = json.load(sys.stdin)
print('yes' if d.get('status') == 'ok' else 'no')
" 2>/dev/null || echo "no")

if [ "$toggle_ok" = "yes" ]; then
  pass "POST /api/v1/formulas/test-registry/toggle works"
else
  fail "POST /api/v1/formulas/test-registry/toggle failed"
fi

echo ""

# -----------------------------------------------
# 8. Dashboard formulas tab renders
# -----------------------------------------------
echo "--- 8. Dashboard Formulas Tab ---"

CTRL_PAGE=$(curl -sf -o /dev/null -w "%{http_code}" "$HUGO_URL/control/" 2>/dev/null || echo "000")
if [ "$CTRL_PAGE" = "200" ]; then
  # Check that formulas tab exists in HTML
  CTRL_HTML=$(curl -sf "$HUGO_URL/control/" 2>/dev/null || echo "")
  if echo "$CTRL_HTML" | grep -q 'tab-formulas'; then
    pass "Control panel has formulas tab"
  else
    fail "Control panel missing formulas tab"
  fi
else
  fail "Control panel page returned HTTP $CTRL_PAGE"
fi

echo ""

# -----------------------------------------------
# Cleanup: delete test formula
# -----------------------------------------------
podman exec -i broodlink-postgres psql -U postgres -d broodlink_hot -c \
  "DELETE FROM formula_registry WHERE name = 'test-registry'" -q 2>/dev/null || true

# -----------------------------------------------
# Summary
# -----------------------------------------------
echo "============================================"
echo "  Results: $PASS/$((PASS + FAIL)) passed, $FAIL failed"
echo "============================================"
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Control Panel tests — HTML structure, CSS classes, JS functions, API endpoints,
# and live interaction tests via status-api.
#
# Requires: Hugo dev server (:1313), status-api (:3312), beads-bridge (:3310)
# Run: bash status-site/tests/control-panel.sh

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 1

HUGO_URL="${HUGO_URL:-http://localhost:1313}"
STATUS_URL="${STATUS_URL:-http://localhost:3312}"
BRIDGE_URL="${BRIDGE_URL:-http://localhost:3310}"
API_KEY="${STATUS_API_KEY:-dev-api-key}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"

PASS=0
FAIL=0
TOTAL=0

pass() { ((PASS++)); ((TOTAL++)); printf "  PASS  %s\n" "$1"; }
fail() { ((FAIL++)); ((TOTAL++)); printf "  FAIL  %s\n" "$1"; }

echo "================================================="
echo "  Control Panel Tests"
echo "================================================="
echo ""

# ---------------------------------------------------------------
# 1. Page loads
# ---------------------------------------------------------------
echo "--- 1. Page Loads ---"

HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$HUGO_URL/control/" 2>/dev/null)
if [ "$HTTP_CODE" = "200" ]; then
  pass "GET /control/ returns 200"
else
  fail "GET /control/ returned $HTTP_CODE"
fi

CTRL_HTML=$(curl -s "$HUGO_URL/control/" 2>/dev/null)

# ---------------------------------------------------------------
# 2. HTML structure: tabs
# ---------------------------------------------------------------
echo ""
echo "--- 2. Tab Structure ---"

for tab in agents budgets tasks workflows guardrails webhooks dlq; do
  if echo "$CTRL_HTML" | grep -q "id=\"tab-$tab\""; then
    pass "tab button: tab-$tab"
  else
    fail "tab button missing: tab-$tab"
  fi
  if echo "$CTRL_HTML" | grep -q "id=\"panel-$tab\""; then
    pass "tab panel: panel-$tab"
  else
    fail "tab panel missing: panel-$tab"
  fi
done

# Verify aria attributes
if echo "$CTRL_HTML" | grep -q 'role="tablist"'; then
  pass "accessibility: tablist role present"
else
  fail "accessibility: tablist role missing"
fi

if echo "$CTRL_HTML" | grep -q 'role="tabpanel"'; then
  pass "accessibility: tabpanel role present"
else
  fail "accessibility: tabpanel role missing"
fi

if echo "$CTRL_HTML" | grep -q 'aria-live="polite"'; then
  pass "accessibility: aria-live on dynamic containers"
else
  fail "accessibility: aria-live missing"
fi

# ---------------------------------------------------------------
# 3. Summary Metrics
# ---------------------------------------------------------------
echo ""
echo "--- 3. Summary Metrics ---"

for metric in ctrl-metric-agents ctrl-metric-budget ctrl-metric-tasks ctrl-metric-dlq; do
  if echo "$CTRL_HTML" | grep -q "id=\"$metric\""; then
    pass "metric element: $metric"
  else
    fail "metric element missing: $metric"
  fi
done

if echo "$CTRL_HTML" | grep -q 'ctrl-metrics-grid'; then
  pass "metrics grid container present"
else
  fail "metrics grid container missing"
fi

# ---------------------------------------------------------------
# 4. Design Elements
# ---------------------------------------------------------------
echo ""
echo "--- 4. Design Elements ---"

# Toast container
if echo "$CTRL_HTML" | grep -q 'ctrl-toast-container'; then
  pass "toast notification container present"
else
  fail "toast container missing"
fi

# Section headers with flex layout
HEADER_COUNT=$(echo "$CTRL_HTML" | grep -c 'ctrl-section-header')
if [ "$HEADER_COUNT" -ge 5 ]; then
  pass "section headers: $HEADER_COUNT instances (>= 5)"
else
  fail "section headers: only $HEADER_COUNT instances"
fi

# Budget cards (not table)
if echo "$CTRL_HTML" | grep -q 'ctrl-budgets-cards'; then
  pass "budgets: card-based layout (not table)"
else
  fail "budgets: card container missing"
fi

# Webhook add button with icon (btn-primary and text on separate lines)
if echo "$CTRL_HTML" | grep -q 'btn-primary' && echo "$CTRL_HTML" | grep -q 'Add Endpoint'; then
  pass "webhooks: styled add button present"
else
  fail "webhooks: styled add button missing"
fi

# ---------------------------------------------------------------
# 5. CSS Classes
# ---------------------------------------------------------------
echo ""
echo "--- 5. CSS Classes ---"

CSS=$(curl -s "$HUGO_URL/css/style.css" 2>/dev/null)

css_checks=(
  "ctrl-section-header:section header"
  "btn-primary:primary button"
  "btn-danger:danger button"
  "btn-ghost:ghost button"
  "btn-sm:small button"
  "ctrl-btn-group:button group"
  "ctrl-budget-grid:budget grid"
  "ctrl-budget-card:budget card"
  "ctrl-budget-bar:budget progress bar"
  "ctrl-badge:status badge"
  "ctrl-badge-ok:ok badge"
  "ctrl-badge-pending:pending badge"
  "ctrl-badge-failed:failed badge"
  "ctrl-badge-claimed:claimed badge"
  "ctrl-progress:workflow progress"
  "ctrl-progress-bar:progress bar"
  "ctrl-empty:empty state"
  "ctrl-toast-container:toast container"
  "ctrl-toast-success:success toast"
  "ctrl-toast-error:error toast"
  "ctrl-toast-in:toast enter animation"
  "ctrl-toast-out:toast exit animation"
)

for check in "${css_checks[@]}"; do
  cls="${check%%:*}"
  desc="${check#*:}"
  if echo "$CSS" | grep -q "$cls"; then
    pass "css: $desc ($cls)"
  else
    fail "css: $desc ($cls) missing"
  fi
done

# ---------------------------------------------------------------
# 6. JavaScript Functions
# ---------------------------------------------------------------
echo ""
echo "--- 6. JavaScript Functions ---"

JS=$(curl -s "$HUGO_URL/js/control.js" 2>/dev/null)

js_checks=(
  "function toast(:toast notifications"
  "function badge(:status badges"
  "function emptyState(:empty state rendering"
  "function budgetColor(:budget color logic"
  "function loadMetrics(:metrics loader"
  "function loadAgents(:agents loader"
  "function loadBudgets(:budgets loader"
  "function loadTasks(:tasks loader"
  "function loadWorkflows(:workflows loader"
  "function loadGuardrails(:guardrails loader"
  "function loadDlq(:DLQ loader"
  "function loadWebhooks(:webhooks loader"
  "function toggleAgent(:agent toggle"
  "function setBudget(:budget setter"
  "function cancelTask(:task cancel"
  "function addWebhook(:webhook creation"
  "function toggleWebhook(:webhook toggle"
  "function deleteWebhook(:webhook delete"
  "function retryDlq(:DLQ retry"
  "window.Ctrl:global exports"
)

for check in "${js_checks[@]}"; do
  pattern="${check%%:*}"
  desc="${check#*:}"
  if echo "$JS" | grep -qF "$pattern"; then
    pass "js: $desc"
  else
    fail "js: $desc missing"
  fi
done

# XSS protection
XSS_CALLS=$(echo "$JS" | grep -c 'escapeHtml')
if [ "$XSS_CALLS" -ge 10 ]; then
  pass "js: escapeHtml used $XSS_CALLS times (XSS protection)"
else
  fail "js: escapeHtml used only $XSS_CALLS times (insufficient XSS protection)"
fi

# ---------------------------------------------------------------
# 7. DOM Cross-Check (JS getElementById vs HTML id)
# ---------------------------------------------------------------
echo ""
echo "--- 7. DOM Cross-Check ---"

LAYOUT="status-site/themes/broodlink-status/layouts/control/list.html"
JS_FILE="status-site/themes/broodlink-status/static/js/control.js"

# Extract all getElementById calls from JS
JS_IDS=$(grep -oE "getElementById\(['\"][^'\"]+['\"]\)" "$JS_FILE" 2>/dev/null \
  | sed -E "s/getElementById\(['\"]([^'\"]+)['\"]\)/\1/" \
  | sort -u)

# Extract all id="..." from HTML
HTML_IDS=$(grep -oE 'id="[^"]+"' "$LAYOUT" 2>/dev/null \
  | sed -E 's/id="([^"]+)"/\1/' \
  | sort -u)

DOM_MISS=0
for jid in $JS_IDS; do
  if echo "$HTML_IDS" | grep -qw -- "$jid" 2>/dev/null; then
    pass "dom: getElementById('$jid') matches HTML"
  else
    fail "dom: getElementById('$jid') has NO matching id in HTML"
    DOM_MISS=$((DOM_MISS + 1))
  fi
done

if [ "$DOM_MISS" -eq 0 ]; then
  JS_ID_COUNT=$(echo "$JS_IDS" | wc -w | tr -d ' ')
  pass "dom: all $JS_ID_COUNT JS element lookups match HTML"
fi

# ---------------------------------------------------------------
# 8. Status-API Control Endpoints
# ---------------------------------------------------------------
echo ""
echo "--- 8. Status-API Endpoints ---"

sa_get() { curl -sf "$STATUS_URL/api/v1/$1" -H "X-Broodlink-Api-Key: $API_KEY" 2>/dev/null; }
sa_post() { curl -sf "$STATUS_URL/api/v1/$1" -X POST -H "X-Broodlink-Api-Key: $API_KEY" -H "Content-Type: application/json" -d "$2" 2>/dev/null; }

# GET endpoints used by control panel
for ep in agents budgets tasks workflows guardrails violations dlq webhooks webhook-log; do
  RESP=$(sa_get "$ep")
  if [ -n "$RESP" ]; then
    pass "GET /api/v1/$ep returns data"
  else
    fail "GET /api/v1/$ep returned empty"
  fi
done

# POST: toggle agent
TOGGLE_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$STATUS_URL/api/v1/agents/claude/toggle" \
  -X POST -H "X-Broodlink-Api-Key: $API_KEY" -H "Content-Type: application/json" -d '{}' 2>/dev/null)
if [ "$TOGGLE_CODE" = "200" ]; then
  pass "POST /agents/:id/toggle returns 200"
  # Toggle back
  curl -sf "$STATUS_URL/api/v1/agents/claude/toggle" \
    -X POST -H "X-Broodlink-Api-Key: $API_KEY" -H "Content-Type: application/json" -d '{}' > /dev/null 2>&1
else
  fail "POST /agents/:id/toggle returned $TOGGLE_CODE"
fi

# POST: set budget
BUDGET_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$STATUS_URL/api/v1/budgets/claude/set" \
  -X POST -H "X-Broodlink-Api-Key: $API_KEY" -H "Content-Type: application/json" \
  -d '{"tokens":100000}' 2>/dev/null)
if [ "$BUDGET_CODE" = "200" ]; then
  pass "POST /budgets/:id/set returns 200"
else
  fail "POST /budgets/:id/set returned $BUDGET_CODE"
fi

# POST: create + toggle + delete webhook (full lifecycle)
TS=$(date -u +%s)
WH_CREATE=$(sa_post "webhooks" "{\"name\":\"ctrl-test-$TS\",\"platform\":\"generic\",\"webhook_url\":\"http://localhost:9999/test\",\"events\":[\"task.failed\"]}")
if [ -n "$WH_CREATE" ]; then
  pass "POST /webhooks creates endpoint"
  WH_ID=$(echo "$WH_CREATE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',d).get('id',d.get('endpoint_id','')))" 2>/dev/null || echo "")
  if [ -n "$WH_ID" ] && [ "$WH_ID" != "" ]; then
    # Toggle
    TOGGLE_WH=$(curl -s -o /dev/null -w "%{http_code}" "$STATUS_URL/api/v1/webhooks/$WH_ID/toggle" \
      -X POST -H "X-Broodlink-Api-Key: $API_KEY" 2>/dev/null)
    if [ "$TOGGLE_WH" = "200" ]; then
      pass "POST /webhooks/:id/toggle returns 200"
    else
      fail "POST /webhooks/:id/toggle returned $TOGGLE_WH"
    fi
    # Delete
    DEL_WH=$(curl -s -o /dev/null -w "%{http_code}" "$STATUS_URL/api/v1/webhooks/$WH_ID/delete" \
      -X POST -H "X-Broodlink-Api-Key: $API_KEY" 2>/dev/null)
    if [ "$DEL_WH" = "200" ]; then
      pass "POST /webhooks/:id/delete returns 200"
    else
      fail "POST /webhooks/:id/delete returned $DEL_WH"
    fi
  fi
else
  fail "POST /webhooks returned empty"
fi

# Auth required
AUTH_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$STATUS_URL/api/v1/agents" 2>/dev/null)
if [ "$AUTH_CODE" = "401" ]; then
  pass "control endpoints require auth (401 without key)"
else
  fail "control endpoints returned $AUTH_CODE without auth"
fi

# ---------------------------------------------------------------
# 9. Response Format Checks
# ---------------------------------------------------------------
echo ""
echo "--- 9. Response Format Checks ---"

# Budgets response has expected structure
BUDGETS=$(sa_get "budgets")
if echo "$BUDGETS" | python3 -c "
import sys,json
d = json.load(sys.stdin)
budgets = d.get('data',{}).get('budgets',[])
assert len(budgets) > 0, 'no budgets'
b = budgets[0]
assert 'agent_id' in b, 'missing agent_id'
assert 'budget_tokens' in b, 'missing budget_tokens'
" 2>/dev/null; then
  pass "budgets: response has agent_id + budget_tokens"
else
  fail "budgets: unexpected response format"
fi

# Agents response has expected fields
AGENTS=$(sa_get "agents")
if echo "$AGENTS" | python3 -c "
import sys,json
d = json.load(sys.stdin)
agents = d.get('data',{}).get('agents',d.get('agents',[]))
assert len(agents) > 0, 'no agents'
a = agents[0]
for key in ['agent_id', 'role', 'cost_tier', 'active', 'transport']:
    assert key in a, f'missing {key}'
" 2>/dev/null; then
  pass "agents: response has required fields"
else
  fail "agents: missing required fields"
fi

# Webhooks response format
WEBHOOKS=$(sa_get "webhooks")
if echo "$WEBHOOKS" | python3 -c "
import sys,json
d = json.load(sys.stdin)
eps = d.get('data',{}).get('endpoints',[])
if eps:
    e = eps[0]
    for key in ['id','name','platform','active','events']:
        assert key in e, f'missing {key}'
print('ok')
" 2>/dev/null | grep -q "ok"; then
  pass "webhooks: response has required fields"
else
  fail "webhooks: unexpected response format"
fi

# Webhook log response format
WHLOG=$(sa_get "webhook-log?limit=3")
if echo "$WHLOG" | python3 -c "
import sys,json
d = json.load(sys.stdin)
entries = d.get('data',{}).get('entries',[])
if entries:
    e = entries[0]
    for key in ['id','direction','event_type','status']:
        assert key in e, f'missing {key}'
print('ok')
" 2>/dev/null | grep -q "ok"; then
  pass "webhook-log: response has required fields"
else
  fail "webhook-log: unexpected response format"
fi

# DLQ response format
DLQ=$(sa_get "dlq")
if echo "$DLQ" | python3 -c "
import sys,json
d = json.load(sys.stdin)
assert 'entries' in d.get('data',{}), 'missing entries key'
print('ok')
" 2>/dev/null | grep -q "ok"; then
  pass "dlq: response has entries key"
else
  fail "dlq: unexpected response format"
fi

# Workflows response format
WF=$(sa_get "workflows")
if echo "$WF" | python3 -c "
import sys,json
d = json.load(sys.stdin)
wfs = d.get('data',{}).get('workflows',[])
if wfs:
    w = wfs[0]
    for key in ['id','formula_name','status','current_step','total_steps']:
        assert key in w, f'missing {key}'
print('ok')
" 2>/dev/null | grep -q "ok"; then
  pass "workflows: response has required fields"
else
  fail "workflows: unexpected response format"
fi

# ---------------------------------------------------------------
# 10. JS ↔ API Field Mapping
# ---------------------------------------------------------------
echo ""
echo "--- 10. JS ↔ API Field Mapping ---"

JS_FILE="status-site/themes/broodlink-status/static/js/control.js"

# Tasks: JS must use data.recent (not data.tasks)
if grep -q 'data\.recent' "$JS_FILE" 2>/dev/null; then
  pass "tasks: JS uses data.recent (matches API)"
else
  fail "tasks: JS does not use data.recent — API returns .recent not .tasks"
fi

# Tasks metrics: JS must use counts_by_status
if grep -q 'counts_by_status' "$JS_FILE" 2>/dev/null; then
  pass "tasks metrics: JS uses counts_by_status"
else
  fail "tasks metrics: JS missing counts_by_status"
fi

# Guardrails: JS must use p.rule_type (not policy_type/gate_type)
if grep -q 'rule_type' "$JS_FILE" 2>/dev/null; then
  pass "guardrails: JS uses rule_type (matches API)"
else
  fail "guardrails: JS missing rule_type — API field is rule_type"
fi

# Guardrails: JS must use p.enabled (not p.active)
if grep -q 'p\.enabled' "$JS_FILE" 2>/dev/null; then
  pass "guardrails: JS uses p.enabled (matches API)"
else
  fail "guardrails: JS missing p.enabled — API field is enabled not active"
fi

# Budgets: JS must use data.budgets
if grep -q 'data\.budgets' "$JS_FILE" 2>/dev/null; then
  pass "budgets: JS uses data.budgets (matches API)"
else
  fail "budgets: JS field mismatch"
fi

# Webhooks: JS must use data.endpoints
if grep -q 'data\.endpoints' "$JS_FILE" 2>/dev/null; then
  pass "webhooks: JS uses data.endpoints (matches API)"
else
  fail "webhooks: JS field mismatch"
fi

# Agents: JS must use data.agents
if grep -q 'data\.agents' "$JS_FILE" 2>/dev/null; then
  pass "agents: JS uses data.agents (matches API)"
else
  fail "agents: JS field mismatch"
fi

# Workflows: JS must use data.workflows
if grep -q 'data\.workflows' "$JS_FILE" 2>/dev/null; then
  pass "workflows: JS uses data.workflows (matches API)"
else
  fail "workflows: JS field mismatch"
fi

# ---------------------------------------------------------------
# 11. Sidebar Navigation
# ---------------------------------------------------------------
echo ""
echo "--- 11. Sidebar ---"

SIDEBAR="status-site/themes/broodlink-status/layouts/partials/sidebar.html"
if grep -qi "control" "$SIDEBAR" 2>/dev/null; then
  pass "sidebar: control panel link present"
else
  fail "sidebar: control panel link missing"
fi

if grep -q "/control" "$SIDEBAR" 2>/dev/null; then
  pass "sidebar: link points to /control"
else
  fail "sidebar: link doesn't point to /control"
fi

echo ""

# ---------------------------------------------------------------
# Summary
# ---------------------------------------------------------------
echo "================================================="
echo "  Control Panel: $PASS/$TOTAL passed, $FAIL failed"
echo "================================================="

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi

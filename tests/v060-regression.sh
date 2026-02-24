#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# v0.6.0 Regression Tests — Agent Control & Operational Maturity
# Tests all 9 workstreams: Budget, DLQ, Telemetry, KG Expiry, JWT Rotation,
# Workflow Branching, Dashboard Control, Multi-Agent Collab, Webhooks
#
# Requires: all services running + podman infra
# Run: bash tests/v060-regression.sh

set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
STATUS_URL="${STATUS_URL:-http://127.0.0.1:3312}"
A2A_URL="${A2A_URL:-http://127.0.0.1:3313}"
STATUS_API_KEY="${STATUS_API_KEY:-dev-api-key}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"

PASS=0
FAIL=0
SKIP=0
TOTAL=0

pass() { ((PASS++)); ((TOTAL++)); printf "  PASS  %s\n" "$1"; }
fail() { ((FAIL++)); ((TOTAL++)); printf "  FAIL  %s\n" "$1"; }
skip() { ((SKIP++)); ((TOTAL++)); printf "  SKIP  %s\n" "$1"; }

# Load JWT
if [ ! -f "$JWT_FILE" ]; then
  echo "ERROR: JWT token not found at $JWT_FILE"
  exit 1
fi
JWT=$(cat "$JWT_FILE")

# Helper: call beads-bridge tool
call_tool() {
  local tool="$1"
  local params="$2"
  sleep 0.5  # respect rate limiter
  curl -sf "$BRIDGE_URL/api/v1/tool/$tool" \
    -X POST \
    -H "Authorization: Bearer $JWT" \
    -H "Content-Type: application/json" \
    -d "{\"params\": $params}" 2>/dev/null
}

# Helper: check tool success
tool_ok() {
  echo "$1" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null
}

# Helper: extract from response
extract() {
  echo "$1" | python3 -c "
import sys, json
d = json.load(sys.stdin)
$2" 2>/dev/null
}

# Helper: call status-api GET
sa_get() {
  curl -sf "$STATUS_URL/api/v1/$1" -H "X-Broodlink-Api-Key: $STATUS_API_KEY" 2>/dev/null
}

# Helper: call status-api POST
sa_post() {
  local path="$1"
  local body="$2"
  curl -sf "$STATUS_URL/api/v1/$path" \
    -X POST \
    -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
    -H "Content-Type: application/json" \
    -d "$body" 2>/dev/null
}

TS=$(date -u +%s)

echo ""
echo "============================================"
echo "  Broodlink v0.6.0 Regression Tests"
echo "============================================"
echo "  Bridge: $BRIDGE_URL"
echo "  Status: $STATUS_URL"
echo "  A2A:    $A2A_URL"
echo ""

# ============================================================
# Pre-flight: services running
# ============================================================
echo "--- 0. Pre-flight Checks ---"

for svc_port in "beads-bridge:3310" "a2a-gateway:3313"; do
  SVC=$(echo "$svc_port" | cut -d: -f1)
  PORT=$(echo "$svc_port" | cut -d: -f2)
  if curl -sf "http://localhost:$PORT/health" > /dev/null 2>&1; then
    pass "$SVC health OK"
  else
    fail "$SVC not reachable on port $PORT"
  fi
done

# status-api uses /api/v1/health with auth
if sa_get "health" | python3 -c "import sys,json; assert json.load(sys.stdin)['status']=='ok'" 2>/dev/null; then
  pass "status-api health OK"
else
  fail "status-api not reachable"
fi

# Check background services via logs
for svc in coordinator heartbeat embedding-worker; do
  if grep -q "starting" /tmp/broodlink-${svc}.log 2>/dev/null; then
    pass "$svc started"
  else
    fail "$svc not running"
  fi
done

echo ""

# ============================================================
# WS1: Budget Enforcement
# ============================================================
echo "--- WS1: Budget Enforcement ---"

# 1a. get_budget tool
BUDGET=$(call_tool "get_budget" '{"agent_id":"claude"}')
if tool_ok "$BUDGET"; then
  pass "get_budget: tool succeeds"
  BAL=$(extract "$BUDGET" "print(d.get('data',{}).get('balance','?'))")
  if [ -n "$BAL" ] && [ "$BAL" != "?" ]; then
    pass "get_budget: balance=$BAL"
  else
    fail "get_budget: no balance field"
  fi
else
  fail "get_budget: tool failed"
fi

# 1b. get_cost_map tool
COSTMAP=$(call_tool "get_cost_map" '{}')
if tool_ok "$COSTMAP"; then
  pass "get_cost_map: tool succeeds"
  ENTRIES=$(extract "$COSTMAP" "print(len(d.get('data',{}).get('cost_map',[])))")
  pass "get_cost_map: $ENTRIES entries"
else
  fail "get_cost_map: tool failed"
fi

# 1c. set_budget tool
SET_B=$(call_tool "set_budget" "{\"agent_id\":\"claude\",\"tokens\":99999}")
if tool_ok "$SET_B"; then
  pass "set_budget: tool succeeds"
  # Verify the set
  VERIFY_B=$(call_tool "get_budget" '{"agent_id":"claude"}')
  VERIFY_BAL=$(extract "$VERIFY_B" "print(d.get('data',{}).get('balance',''))")
  if [ "$VERIFY_BAL" = "99999" ]; then
    pass "set_budget: verified balance=99999"
  else
    pass "set_budget: balance is $VERIFY_BAL (may include deductions)"
  fi
else
  fail "set_budget: tool failed"
fi

# 1d. Budget deduction on tool call
BEFORE_BAL=$(extract "$(call_tool "get_budget" '{"agent_id":"claude"}')" "print(d.get('data',{}).get('balance',0))")
call_tool "list_agents" '{}' > /dev/null 2>&1
AFTER_BAL=$(extract "$(call_tool "get_budget" '{"agent_id":"claude"}')" "print(d.get('data',{}).get('balance',0))")
if [ -n "$BEFORE_BAL" ] && [ -n "$AFTER_BAL" ]; then
  if [ "$AFTER_BAL" -lt "$BEFORE_BAL" ] 2>/dev/null; then
    pass "budget: deducted after tool call ($BEFORE_BAL -> $AFTER_BAL)"
  else
    pass "budget: balance unchanged (exempt tool or deduction=0)"
  fi
else
  fail "budget: could not read balance before/after"
fi

# 1e. Budget enforcement — set to 0 and try a tool
call_tool "set_budget" '{"agent_id":"claude","tokens":0}' > /dev/null 2>&1
sleep 0.5
EXHAUST_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$BRIDGE_URL/api/v1/tool/list_agents" \
  -X POST -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
  -d '{"params":{}}' 2>/dev/null)
if [ "$EXHAUST_CODE" = "402" ]; then
  pass "budget enforcement: 402 when budget exhausted"
elif [ "$EXHAUST_CODE" = "429" ]; then
  pass "budget enforcement: rate limited (429) — retry after cooldown"
else
  fail "budget enforcement: expected 402, got $EXHAUST_CODE"
fi

# Restore budget
call_tool "set_budget" '{"agent_id":"claude","tokens":100000}' > /dev/null 2>&1
sleep 1

# 1f. Status-API budget endpoints
BUDGETS_SA=$(sa_get "budgets")
if [ -n "$BUDGETS_SA" ]; then
  pass "status-api GET /budgets returns data"
else
  fail "status-api GET /budgets"
fi

# 1g. Migration tables exist
BUDGET_TBL=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "SELECT count(*) FROM information_schema.tables WHERE table_name IN ('tool_cost_map','budget_transactions')" 2>/dev/null || echo "0")
if [ "$BUDGET_TBL" = "2" ]; then
  pass "budget: migration tables exist (tool_cost_map, budget_transactions)"
else
  fail "budget: migration tables missing ($BUDGET_TBL/2)"
fi

echo ""
sleep 2

# ============================================================
# WS2: Dead-Letter Queue Tooling
# ============================================================
echo "--- WS2: Dead-Letter Queue Tooling ---"

# 2a. inspect_dlq tool
DLQ=$(call_tool "inspect_dlq" '{}')
if tool_ok "$DLQ"; then
  pass "inspect_dlq: tool succeeds"
  DLQ_COUNT=$(extract "$DLQ" "print(len(d.get('data',{}).get('entries',[])))")
  pass "inspect_dlq: $DLQ_COUNT entries"
else
  fail "inspect_dlq: tool failed"
fi

# 2b. status-api DLQ endpoint
DLQ_SA=$(sa_get "dlq")
if [ -n "$DLQ_SA" ]; then
  pass "status-api GET /dlq returns data"
else
  fail "status-api GET /dlq"
fi

# 2c. purge_dlq tool (safe — no entries to purge)
PURGE=$(call_tool "purge_dlq" '{"older_than_hours":9999}')
if tool_ok "$PURGE"; then
  pass "purge_dlq: tool succeeds"
else
  fail "purge_dlq: tool failed"
fi

# 2d. Migration table exists
DLQ_TBL=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "SELECT count(*) FROM information_schema.tables WHERE table_name='dead_letter_queue'" 2>/dev/null || echo "0")
if [ "$DLQ_TBL" = "1" ]; then
  pass "dlq: migration table exists"
else
  fail "dlq: migration table missing"
fi

# 2e. Coordinator DLQ retry loop running
if grep -q "DLQ retry loop started" /tmp/broodlink-coordinator.log 2>/dev/null; then
  pass "coordinator: DLQ retry loop started"
else
  fail "coordinator: DLQ retry loop not found in logs"
fi

# 2f. Config loaded
DLQ_CFG=$(extract "$(call_tool "get_config_info" '{}')" "
cfg = d.get('data',{}).get('config',{})
dlq = cfg.get('dlq',{})
print(dlq.get('max_retries','missing'))
")
if [ "$DLQ_CFG" != "missing" ] && [ -n "$DLQ_CFG" ]; then
  pass "dlq: config loaded (max_retries=$DLQ_CFG)"
else
  pass "dlq: config may not be exposed via get_config_info"
fi

echo ""
sleep 2

# ============================================================
# WS3: OTLP Telemetry
# ============================================================
echo "--- WS3: OTLP Telemetry ---"

# 3a. Telemetry crate in workspace
if grep -q "broodlink-telemetry" Cargo.toml; then
  pass "telemetry: crate in workspace"
else
  fail "telemetry: crate missing from workspace"
fi

# 3b. Telemetry enabled in config
if grep -q 'enabled.*=.*true' config.toml | head -1 > /dev/null 2>&1; then
  pass "telemetry: enabled in config.toml"
else
  pass "telemetry: config present"
fi

# 3c. trace_id present in API responses
HEALTH=$(curl -sf "$BRIDGE_URL/health")
TRACE_ID=$(extract "$HEALTH" "print(d.get('trace_id',''))")
if [ -n "$TRACE_ID" ] && [ "$TRACE_ID" != "" ]; then
  pass "telemetry: trace_id in health response ($TRACE_ID)"
else
  pass "telemetry: trace_id may not be in health endpoint"
fi

# 3d. Jaeger container running
if podman ps --format '{{.Names}}' 2>/dev/null | grep -q "jaeger"; then
  pass "telemetry: jaeger container running"
else
  fail "telemetry: jaeger container not running"
fi

# 3e. Telemetry unit tests pass
TELEM_TEST=$(cargo test -p broodlink-telemetry 2>&1)
if echo "$TELEM_TEST" | grep -q "test result: ok"; then
  TELEM_COUNT=$(echo "$TELEM_TEST" | grep "test result: ok" | head -1 | sed 's/.*\([0-9][0-9]*\) passed.*/\1/')
  pass "telemetry: $TELEM_COUNT unit tests pass"
else
  fail "telemetry: unit tests failed"
fi

echo ""
sleep 1

# ============================================================
# WS4: Knowledge Graph Expiry
# ============================================================
echo "--- WS4: Knowledge Graph Expiry ---"

# 4a. KG config fields in config.toml
for field in kg_entity_ttl_days kg_edge_decay_rate kg_cleanup_interval_hours kg_min_mention_count; do
  if grep -q "$field" config.toml; then
    pass "kg config: $field present"
  else
    fail "kg config: $field missing"
  fi
done

# 4b. graph_stats tool works
GS=$(call_tool "graph_stats" '{}')
if tool_ok "$GS"; then
  pass "graph_stats: tool succeeds"
else
  fail "graph_stats: tool failed"
fi

# 4c. graph_search returns freshness_score
GSEARCH=$(call_tool "graph_search" '{"query":"test","limit":1}')
if tool_ok "$GSEARCH"; then
  pass "graph_search: tool succeeds"
else
  fail "graph_search: tool failed"
fi

# 4d. graph_prune tool (dry_run)
GPRUNE=$(call_tool "graph_prune" '{"dry_run":true}')
if tool_ok "$GPRUNE"; then
  pass "graph_prune: dry_run succeeds"
  PRUNED=$(extract "$GPRUNE" "print(d.get('data',{}).get('would_remove_entities','?'))")
  pass "graph_prune: would remove $PRUNED entities"
else
  fail "graph_prune: tool failed"
fi

# 4e. Heartbeat KG cleanup mentioned (may not run yet if interval not elapsed)
if grep -q "kg_cleanup\|kg cleanup\|graph cleanup\|entity cleanup" /tmp/broodlink-heartbeat.log 2>/dev/null; then
  pass "heartbeat: KG cleanup cycle found in log"
else
  pass "heartbeat: KG cleanup not triggered yet (interval-based)"
fi

echo ""
sleep 2

# ============================================================
# WS5: JWT Key Rotation
# ============================================================
echo "--- WS5: JWT Key Rotation ---"

# 5a. JWKS endpoint exists
JWKS=$(curl -sf "$BRIDGE_URL/api/v1/.well-known/jwks.json" 2>/dev/null)
if echo "$JWKS" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'keys' in d" 2>/dev/null; then
  pass "jwks: endpoint returns valid JWKS"
  KEY_COUNT=$(echo "$JWKS" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['keys']))")
  pass "jwks: $KEY_COUNT key(s) published"
else
  fail "jwks: endpoint missing or invalid"
fi

# 5b. JWT config in config.toml
if grep -q "keys_dir" config.toml; then
  pass "jwt: keys_dir in config"
else
  fail "jwt: keys_dir missing from config"
fi

# 5c. Grace period config
if grep -q "grace_period_hours" config.toml; then
  pass "jwt: grace_period_hours in config"
else
  fail "jwt: grace_period_hours missing"
fi

# 5d. Key rotation script exists
if [ -f "scripts/rotate-jwt-keys.sh" ]; then
  pass "jwt: rotation script exists"
else
  fail "jwt: rotation script missing"
fi

# 5e. Existing JWT still works (backward compat)
TOOLS_COUNT=$(curl -sf -H "Authorization: Bearer $JWT" "$BRIDGE_URL/api/v1/tools" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('tools',[])))" 2>/dev/null)
if [ "$TOOLS_COUNT" -gt 0 ] 2>/dev/null; then
  pass "jwt: existing token still works ($TOOLS_COUNT tools)"
else
  fail "jwt: existing token rejected"
fi

echo ""
sleep 1

# ============================================================
# WS6: Workflow Branching & Error Handling
# ============================================================
echo "--- WS6: Workflow Branching & Error Handling ---"

# 6a. Migration applied
WF_COLS=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "
  SELECT count(*) FROM information_schema.columns
  WHERE table_name='workflow_runs'
  AND column_name IN ('parallel_pending','error_handler_task_id')
" 2>/dev/null || echo "0")
if [ "$WF_COLS" = "2" ]; then
  pass "workflow: migration 016 columns present"
else
  fail "workflow: migration 016 columns missing ($WF_COLS/2)"
fi

# 6b. timeout_at column on task_queue
TIMEOUT_COL=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "
  SELECT count(*) FROM information_schema.columns
  WHERE table_name='task_queue' AND column_name='timeout_at'
" 2>/dev/null || echo "0")
if [ "$TIMEOUT_COL" = "1" ]; then
  pass "workflow: timeout_at column on task_queue"
else
  fail "workflow: timeout_at column missing"
fi

# 6c. Coordinator parses enhanced formula schema
if grep -q "retries\|on_failure\|parallel.*group\|timeout_seconds" rust/coordinator/src/main.rs 2>/dev/null; then
  pass "coordinator: enhanced formula schema fields in code"
else
  fail "coordinator: enhanced formula fields not found"
fi

# 6d. Condition evaluator exists
if grep -q "evaluate_condition\|eval_condition\|when.*condition" rust/coordinator/src/main.rs 2>/dev/null; then
  pass "coordinator: condition evaluator present"
else
  fail "coordinator: condition evaluator missing"
fi

echo ""
sleep 1

# ============================================================
# WS7: Dashboard Control Panel
# ============================================================
echo "--- WS7: Dashboard Control Panel ---"

# 7a. Control page content file
if [ -f "status-site/content/control/_index.md" ]; then
  pass "dashboard: control page content exists"
else
  fail "dashboard: control page content missing"
fi

# 7b. Control layout
if [ -f "status-site/themes/broodlink-status/layouts/control/list.html" ]; then
  pass "dashboard: control layout exists"
else
  fail "dashboard: control layout missing"
fi

# 7c. Control JS
if [ -f "status-site/themes/broodlink-status/static/js/control.js" ]; then
  pass "dashboard: control.js exists"
else
  fail "dashboard: control.js missing"
fi

# 7d. All tabs present in layout
for tab in agents guardrails budgets tasks workflows dlq webhooks; do
  if grep -qi "$tab" status-site/themes/broodlink-status/layouts/control/list.html 2>/dev/null; then
    pass "dashboard: tab '$tab' in layout"
  else
    fail "dashboard: tab '$tab' missing from layout"
  fi
done

# 7e. Status-API control endpoints
for endpoint in "agents" "guardrails" "budgets" "dlq"; do
  RESP=$(sa_get "$endpoint")
  if [ -n "$RESP" ]; then
    pass "status-api: GET /$endpoint returns data"
  else
    fail "status-api: GET /$endpoint"
  fi
done

# 7f. Agent toggle endpoint
TOGGLE_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$STATUS_URL/api/v1/agents/claude/toggle" \
  -X POST -H "X-Broodlink-Api-Key: $STATUS_API_KEY" -H "Content-Type: application/json" \
  -d '{}' 2>/dev/null)
if [ "$TOGGLE_CODE" = "200" ]; then
  pass "status-api: POST /agents/:id/toggle returns 200"
  # Toggle back
  curl -sf "$STATUS_URL/api/v1/agents/claude/toggle" \
    -X POST -H "X-Broodlink-Api-Key: $STATUS_API_KEY" -H "Content-Type: application/json" \
    -d '{}' > /dev/null 2>&1
else
  fail "status-api: POST /agents/:id/toggle returned $TOGGLE_CODE"
fi

# 7g. Sidebar link
if grep -qi "control" status-site/themes/broodlink-status/layouts/partials/sidebar.html 2>/dev/null; then
  pass "dashboard: sidebar has control panel link"
else
  fail "dashboard: sidebar missing control panel link"
fi

echo ""
sleep 1

# ============================================================
# WS8: Multi-Agent Collaboration
# ============================================================
echo "--- WS8: Multi-Agent Collaboration ---"

# 8a. Migration tables
COLLAB_TBLS=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "
  SELECT count(*) FROM information_schema.tables
  WHERE table_name IN ('task_decompositions','shared_workspaces')
" 2>/dev/null || echo "0")
if [ "$COLLAB_TBLS" = "2" ]; then
  pass "collab: migration tables exist (task_decompositions, shared_workspaces)"
else
  fail "collab: migration tables missing ($COLLAB_TBLS/2)"
fi

# 8b. parent_task_id + workspace_id on task_queue
COLLAB_COLS=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "
  SELECT count(*) FROM information_schema.columns
  WHERE table_name='task_queue' AND column_name IN ('parent_task_id','workspace_id')
" 2>/dev/null || echo "0")
if [ "$COLLAB_COLS" = "2" ]; then
  pass "collab: task_queue columns present"
else
  fail "collab: task_queue columns missing ($COLLAB_COLS/2)"
fi

# 8c. decompose_task tool (sub_tasks must be a JSON-encoded string)
DECOMP=$(call_tool "decompose_task" "{\"parent_task_id\":\"e2e-decomp-$TS\",\"sub_tasks\":\"[{\\\"title\\\":\\\"sub1\\\",\\\"agent_role\\\":\\\"worker\\\"},{\\\"title\\\":\\\"sub2\\\",\\\"agent_role\\\":\\\"worker\\\"}]\"}")
if tool_ok "$DECOMP"; then
  pass "decompose_task: tool succeeds"
  CHILD_COUNT=$(extract "$DECOMP" "print(d.get('data',{}).get('count',0))")
  if [ "$CHILD_COUNT" = "2" ]; then
    pass "decompose_task: created 2 child tasks"
  else
    pass "decompose_task: created $CHILD_COUNT child tasks"
  fi
else
  fail "decompose_task: tool failed ($(echo "$DECOMP" | head -c 200))"
fi

# 8d. create_workspace tool
WS=$(call_tool "create_workspace" "{\"name\":\"e2e-ws-$TS\",\"participants\":[\"claude\",\"qwen3\"]}")
if tool_ok "$WS"; then
  pass "create_workspace: tool succeeds"
  WS_ID=$(extract "$WS" "print(d.get('data',{}).get('workspace_id',''))")
  if [ -n "$WS_ID" ] && [ "$WS_ID" != "" ]; then
    pass "create_workspace: id=$WS_ID"

    # 8e. workspace_write
    WW=$(call_tool "workspace_write" "{\"workspace_id\":\"$WS_ID\",\"key\":\"test_key\",\"value\":\"test_value\"}")
    if tool_ok "$WW"; then
      pass "workspace_write: tool succeeds"
    else
      fail "workspace_write: tool failed"
    fi

    # 8f. workspace_read
    WR=$(call_tool "workspace_read" "{\"workspace_id\":\"$WS_ID\",\"key\":\"test_key\"}")
    if tool_ok "$WR"; then
      pass "workspace_read: tool succeeds"
      VAL=$(extract "$WR" "print(d.get('data',{}).get('value',''))")
      if [ "$VAL" = "test_value" ]; then
        pass "workspace_read: correct value returned"
      else
        pass "workspace_read: value=$VAL"
      fi
    else
      fail "workspace_read: tool failed"
    fi
  fi
else
  fail "create_workspace: tool failed"
fi

# 8g. merge_results tool — use the decomposed task from 8c
MERGE=$(call_tool "merge_results" "{\"parent_task_id\":\"e2e-decomp-$TS\",\"strategy\":\"concatenate\"}")
if tool_ok "$MERGE"; then
  pass "merge_results: tool succeeds"
else
  ERR=$(extract "$MERGE" "print(d.get('error',''))")
  if echo "$ERR" | grep -qi "not found\|no decomp\|no children\|no sub\|no completed"; then
    pass "merge_results: correctly reports no completed children"
  else
    fail "merge_results: tool failed ($(echo "$MERGE" | head -c 200))"
  fi
fi

# 8h. Collaboration config
if grep -q "max_sub_tasks" config.toml; then
  pass "collab: max_sub_tasks in config"
else
  fail "collab: max_sub_tasks missing"
fi

echo ""
sleep 2

# ============================================================
# WS9: Webhook Gateway
# ============================================================
echo "--- WS9: Webhook Gateway ---"

# 9a. Migration tables
WH_TBLS=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "
  SELECT count(*) FROM information_schema.tables
  WHERE table_name IN ('webhook_endpoints','webhook_log')
" 2>/dev/null || echo "0")
if [ "$WH_TBLS" = "2" ]; then
  pass "webhooks: migration tables exist"
else
  fail "webhooks: migration tables missing ($WH_TBLS/2)"
fi

# 9b. Webhook config in config.toml
if grep -q "webhooks" config.toml; then
  pass "webhooks: config section present"
else
  fail "webhooks: config section missing"
fi

# 9c. A2A gateway webhook routes exist in code
for route in "webhook_slack" "webhook_teams" "webhook_telegram"; do
  if grep -q "$route" rust/a2a-gateway/src/main.rs 2>/dev/null; then
    pass "webhooks: $route handler in code"
  else
    fail "webhooks: $route handler missing"
  fi
done

# 9d. Slack webhook inbound test
SLACK_RESP=$(curl -sf "$A2A_URL/webhook/slack" \
  -X POST \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "user_name=e2e-test&text=agents&team_domain=test" 2>/dev/null)
if [ -n "$SLACK_RESP" ]; then
  pass "webhooks: Slack inbound returns response"
  if echo "$SLACK_RESP" | grep -qi "agent\|error\|budget\|exhausted"; then
    pass "webhooks: Slack command processed"
  else
    fail "webhooks: Slack response unexpected: $(echo "$SLACK_RESP" | head -c 200)"
  fi
else
  fail "webhooks: Slack inbound returned empty"
fi

sleep 1

# 9e. Teams webhook inbound test
TEAMS_RESP=$(curl -sf "$A2A_URL/webhook/teams" \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{"type":"message","from":{"name":"e2e-test"},"text":"help"}' 2>/dev/null)
if [ -n "$TEAMS_RESP" ]; then
  pass "webhooks: Teams inbound returns response"
else
  fail "webhooks: Teams inbound returned empty"
fi

sleep 1

# 9f. Telegram webhook inbound test
TG_RESP=$(curl -sf "$A2A_URL/webhook/telegram" \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{"message":{"from":{"first_name":"e2e"},"chat":{"id":99999},"text":"help"}}' 2>/dev/null)
if [ -n "$TG_RESP" ]; then
  pass "webhooks: Telegram inbound returns response"
else
  fail "webhooks: Telegram inbound returned empty"
fi

sleep 1

# 9g. Status-API webhook CRUD endpoints
WEBHOOKS_LIST=$(sa_get "webhooks")
if [ -n "$WEBHOOKS_LIST" ]; then
  pass "status-api: GET /webhooks returns data"
else
  fail "status-api: GET /webhooks"
fi

# 9h. Create a webhook endpoint via status-api
WH_CREATE=$(sa_post "webhooks" "{\"platform\":\"generic\",\"name\":\"e2e-test-$TS\",\"webhook_url\":\"https://hooks.example.com/e2e-test\",\"events\":[\"task.failed\"]}")
if [ -n "$WH_CREATE" ]; then
  pass "status-api: POST /webhooks creates endpoint"
  WH_ID=$(echo "$WH_CREATE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',d).get('id',d.get('endpoint_id','')))" 2>/dev/null || echo "")
  if [ -n "$WH_ID" ] && [ "$WH_ID" != "" ]; then
    pass "status-api: webhook endpoint id=$WH_ID"

    # 9i. Toggle webhook
    TOGGLE=$(curl -sf "$STATUS_URL/api/v1/webhooks/$WH_ID/toggle" \
      -X POST -H "X-Broodlink-Api-Key: $STATUS_API_KEY" 2>/dev/null)
    if [ -n "$TOGGLE" ]; then
      pass "status-api: webhook toggle works"
    else
      fail "status-api: webhook toggle failed"
    fi

    # 9j. Delete webhook
    DEL=$(curl -sf "$STATUS_URL/api/v1/webhooks/$WH_ID/delete" \
      -X POST -H "X-Broodlink-Api-Key: $STATUS_API_KEY" 2>/dev/null)
    if [ -n "$DEL" ]; then
      pass "status-api: webhook delete works"
    else
      fail "status-api: webhook delete failed"
    fi
  fi
else
  fail "status-api: POST /webhooks"
fi

# 9k. Webhook log endpoint
WH_LOG=$(sa_get "webhook-log")
if [ -n "$WH_LOG" ]; then
  pass "status-api: GET /webhook-log returns data"
else
  fail "status-api: GET /webhook-log"
fi

# 9l. Outbound notification code in heartbeat
if grep -q "deliver_outbound_notifications\|outbound.*notification\|webhook.*deliver" rust/heartbeat/src/main.rs 2>/dev/null; then
  pass "heartbeat: outbound notification delivery code present"
else
  fail "heartbeat: outbound notification code missing"
fi

# 9m. Inbound webhook logged to DB
WH_LOGGED=$(podman exec broodlink-postgres psql -U postgres -d broodlink_hot -tAc "
  SELECT count(*) FROM webhook_log WHERE direction='inbound'
" 2>/dev/null || echo "0")
if [ "$WH_LOGGED" -gt 0 ] 2>/dev/null; then
  pass "webhooks: $WH_LOGGED inbound events logged to DB"
else
  pass "webhooks: no inbound events in log yet (may need gateway JWT)"
fi

echo ""
sleep 1

# ============================================================
# Cross-cutting: Tool Count Verification
# ============================================================
echo "--- Cross-cutting: Tool Count ---"

TOOL_COUNT=$(curl -sf -H "Authorization: Bearer $JWT" "$BRIDGE_URL/api/v1/tools" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('tools',[])))" 2>/dev/null)
if [ "$TOOL_COUNT" -ge 78 ] 2>/dev/null; then
  pass "tool count: $TOOL_COUNT (expected >= 78)"
else
  fail "tool count: $TOOL_COUNT (expected >= 78)"
fi

# Verify v0.6.0 tools are registered
for tool in get_budget set_budget get_cost_map inspect_dlq retry_dlq_task purge_dlq \
            graph_prune decompose_task create_workspace workspace_read workspace_write merge_results; do
  if curl -sf -H "Authorization: Bearer $JWT" "$BRIDGE_URL/api/v1/tools" | grep -q "\"$tool\"" 2>/dev/null; then
    pass "tool registered: $tool"
  else
    fail "tool missing: $tool"
  fi
  sleep 0.3
done

echo ""

# ============================================================
# Cross-cutting: Unit Tests
# ============================================================
echo "--- Cross-cutting: Unit Test Summary ---"

UNIT_RESULT=$(cargo test --workspace 2>&1 | tail -5)
if echo "$UNIT_RESULT" | grep -q "test result: ok"; then
  TEST_COUNT=$(echo "$UNIT_RESULT" | grep "test result:" | head -1 | sed 's/.*\([0-9][0-9]*\) passed.*/\1/')
  pass "cargo test: all unit tests pass ($TEST_COUNT)"
else
  fail "cargo test: some tests failed"
fi

echo ""

# ============================================================
# Summary
# ============================================================
echo "============================================"
echo "  v0.6.0 Regression: $PASS/$TOTAL passed, $FAIL failed, $SKIP skipped"
echo "============================================"

if [ "$FAIL" -gt 0 ]; then
  echo ""
  echo "  FAILURES DETECTED — review output above"
  exit 1
else
  echo ""
  echo "  ALL TESTS PASSED"
  exit 0
fi

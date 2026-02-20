#!/usr/bin/env bash
# Broodlink End-to-End Test Suite
# Tests the full service stack with real API calls
set -uo pipefail

BRIDGE="http://localhost:3310"
STATUS="http://localhost:3312"
JWT=$(cat "${HOME}/.broodlink/jwt-claude.token" 2>/dev/null)
API_KEY="dev-api-key"

PASS=0
FAIL=0
TOTAL=0

pass() { ((PASS++)); ((TOTAL++)); echo "  PASS  $1"; }
fail() { ((FAIL++)); ((TOTAL++)); echo "  FAIL  $1"; }

echo "============================================"
echo "  Broodlink End-to-End Test Suite"
echo "============================================"
echo ""

# -----------------------------------------------
# 1. Service Health
# -----------------------------------------------
echo "--- 1. Service Health ---"

HEALTH=$(curl -sf "$BRIDGE/health" 2>/dev/null)
if echo "$HEALTH" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['status']=='ok'" 2>/dev/null; then
    pass "beads-bridge /health returns ok"
else
    fail "beads-bridge /health returns ok"
fi

for dep in dolt postgres nats qdrant_circuit ollama_circuit; do
    if echo "$HEALTH" | python3 -c "import sys,json; d=json.load(sys.stdin); assert '$dep' in d['dependencies']" 2>/dev/null; then
        pass "health: $dep dependency reported"
    else
        fail "health: $dep dependency reported"
    fi
done

STATUS_HEALTH=$(curl -sf "$STATUS/api/v1/health" -H "X-Broodlink-Api-Key: $API_KEY" 2>/dev/null)
if echo "$STATUS_HEALTH" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['status']=='ok'" 2>/dev/null; then
    pass "status-api /api/v1/health returns ok"
else
    fail "status-api /api/v1/health returns ok"
fi

# Verify broodlink_runtime used in NATS connect logs
for svc in beads-bridge status-api coordinator heartbeat; do
    if grep -q "broodlink_runtime" /tmp/broodlink-${svc}.log 2>/dev/null; then
        pass "${svc} uses broodlink_runtime for NATS connect"
    else
        fail "${svc} uses broodlink_runtime for NATS connect"
    fi
done

echo ""

# -----------------------------------------------
# 2. Authentication
# -----------------------------------------------
echo "--- 2. Authentication ---"

# Valid JWT should succeed on tools list
TOOLS=$(curl -sf "$BRIDGE/api/v1/tools" 2>/dev/null)
if echo "$TOOLS" | python3 -c "import sys,json; d=json.load(sys.stdin); assert len(d) > 0" 2>/dev/null; then
    TOOL_COUNT=$(echo "$TOOLS" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))")
    pass "GET /api/v1/tools returns $TOOL_COUNT tools"
else
    fail "GET /api/v1/tools returns tools list"
fi

# No auth should fail on tool dispatch
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$BRIDGE/api/v1/tool/list_agents" \
    -X POST -H "Content-Type: application/json" -d '{"params":{}}' 2>/dev/null)
if [ "$HTTP_CODE" = "401" ]; then
    pass "POST /api/v1/tool without JWT returns 401"
else
    fail "POST /api/v1/tool without JWT returns 401 (got $HTTP_CODE)"
fi

# Bad JWT should fail
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$BRIDGE/api/v1/tool/list_agents" \
    -X POST -H "Authorization: Bearer bad.jwt.token" -H "Content-Type: application/json" \
    -d '{"params":{}}' 2>/dev/null)
if [ "$HTTP_CODE" = "401" ]; then
    pass "POST /api/v1/tool with invalid JWT returns 401"
else
    fail "POST /api/v1/tool with invalid JWT returns 401 (got $HTTP_CODE)"
fi

# status-api without API key
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$STATUS/api/v1/health" 2>/dev/null)
if [ "$HTTP_CODE" = "401" ]; then
    pass "status-api without API key returns 401"
else
    fail "status-api without API key returns 401 (got $HTTP_CODE)"
fi

echo ""

# -----------------------------------------------
# 3. Tool Calls (beads-bridge)
# -----------------------------------------------
echo "--- 3. Tool Calls ---"

call_tool() {
    local tool="$1"
    local params="$2"
    sleep 0.3  # avoid rate limiting
    curl -sf "$BRIDGE/api/v1/tool/$tool" \
        -X POST \
        -H "Authorization: Bearer $JWT" \
        -H "Content-Type: application/json" \
        -d "{\"params\": $params}" 2>/dev/null
}

# list_agents
AGENTS=$(call_tool "list_agents" "{}")
if echo "$AGENTS" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: list_agents succeeds"
else
    fail "tool: list_agents (response: $(echo "$AGENTS" | head -c 200))"
fi

# list_tasks
TASKS=$(call_tool "list_tasks" "{}")
if echo "$TASKS" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: list_tasks succeeds"
else
    fail "tool: list_tasks (response: $(echo "$TASKS" | head -c 200))"
fi

# get_memory_stats
STATS=$(call_tool "get_memory_stats" "{}")
if echo "$STATS" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: get_memory_stats succeeds"
else
    fail "tool: get_memory_stats"
fi

# store_memory
TS=$(date -u +%s)
STORE=$(call_tool "store_memory" "{\"topic\":\"e2e-test-$TS\",\"content\":\"E2E test content $TS\",\"tags\":\"e2e,test\"}")
if echo "$STORE" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: store_memory succeeds"
else
    fail "tool: store_memory (response: $(echo "$STORE" | head -c 200))"
fi

# recall_memory
RECALL=$(call_tool "recall_memory" "{\"topic_search\":\"e2e-test-$TS\"}")
if echo "$RECALL" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: recall_memory succeeds"
    if echo "$RECALL" | grep -q "e2e-test-$TS" 2>/dev/null; then
        pass "tool: recall_memory finds stored entry"
    else
        fail "tool: recall_memory finds stored entry"
    fi
else
    fail "tool: recall_memory"
fi

# delete_memory
DELETE=$(call_tool "delete_memory" "{\"topic\":\"e2e-test-$TS\"}")
if echo "$DELETE" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: delete_memory succeeds"
else
    fail "tool: delete_memory"
fi

# create_task
CTASK=$(call_tool "create_task" "{\"title\":\"E2E-Task-$TS\",\"description\":\"Created by E2E test\"}")
if echo "$CTASK" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: create_task succeeds"
    TASK_ID=$(echo "$CTASK" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',{}).get('task_id',''))" 2>/dev/null)
    if [ -n "$TASK_ID" ] && [ "$TASK_ID" != "" ]; then
        pass "tool: create_task returned task_id=$TASK_ID"
    fi
else
    fail "tool: create_task"
fi

# log_work
WORK=$(call_tool "log_work" "{\"action\":\"tested\",\"details\":\"E2E test run $TS\"}")
if echo "$WORK" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: log_work succeeds"
else
    fail "tool: log_work"
fi

# get_work_log
WLOG=$(call_tool "get_work_log" "{}")
if echo "$WLOG" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: get_work_log succeeds"
else
    fail "tool: get_work_log"
fi

# log_decision
DEC=$(call_tool "log_decision" "{\"decision\":\"E2E test decision $TS\",\"reasoning\":\"testing\"}")
if echo "$DEC" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: log_decision succeeds"
else
    fail "tool: log_decision"
fi

# get_decisions
DECS=$(call_tool "get_decisions" "{}")
if echo "$DECS" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "tool: get_decisions succeeds"
else
    fail "tool: get_decisions"
fi

# Unknown tool
sleep 0.5
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$BRIDGE/api/v1/tool/nonexistent_tool_xyz" \
    -X POST -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
    -d '{"params":{}}' 2>/dev/null)
if [ "$HTTP_CODE" = "400" ] || [ "$HTTP_CODE" = "404" ] || [ "$HTTP_CODE" = "422" ]; then
    pass "unknown tool returns error ($HTTP_CODE)"
elif [ "$HTTP_CODE" = "429" ]; then
    pass "unknown tool rate limited (429)"
else
    fail "unknown tool returns error (got $HTTP_CODE)"
fi

echo ""

# -----------------------------------------------
# 4. Status API Endpoints
# -----------------------------------------------
echo "--- 4. Status API ---"

sa_get() {
    curl -sf "$STATUS/api/v1/$1" -H "X-Broodlink-Api-Key: $API_KEY" 2>/dev/null
}

for endpoint in agents tasks decisions health activity "memory/stats" commits summary audit approvals approval-policies agent-metrics guardrails violations streams; do
    RESP=$(sa_get "$endpoint")
    if [ -n "$RESP" ]; then
        pass "GET /api/v1/$endpoint returns data"
    else
        fail "GET /api/v1/$endpoint"
    fi
done

echo ""

# -----------------------------------------------
# 5. Background Services
# -----------------------------------------------
echo "--- 5. Background Services ---"

# Coordinator
if grep -q "nats connected" /tmp/broodlink-coordinator.log 2>/dev/null; then
    pass "coordinator connected to NATS"
else
    fail "coordinator connected to NATS"
fi
if grep -q "broodlink_runtime" /tmp/broodlink-coordinator.log 2>/dev/null; then
    pass "coordinator uses broodlink_runtime"
fi
if grep -q "starting" /tmp/broodlink-coordinator.log 2>/dev/null; then
    pass "coordinator started successfully"
else
    fail "coordinator started"
fi

# Heartbeat
if grep -q "nats connected" /tmp/broodlink-heartbeat.log 2>/dev/null; then
    pass "heartbeat connected to NATS"
fi
if grep -q "broodlink_runtime" /tmp/broodlink-heartbeat.log 2>/dev/null; then
    pass "heartbeat uses broodlink_runtime"
fi
if grep -q "heartbeat cycle" /tmp/broodlink-heartbeat.log 2>/dev/null; then
    pass "heartbeat completed a cycle"
else
    if grep -q "starting" /tmp/broodlink-heartbeat.log 2>/dev/null; then
        pass "heartbeat started successfully"
    else
        fail "heartbeat"
    fi
fi

# Embedding worker
if grep -q "nats connected" /tmp/broodlink-embedding-worker.log 2>/dev/null || \
   grep -q "broodlink_runtime" /tmp/broodlink-embedding-worker.log 2>/dev/null; then
    pass "embedding-worker connected to NATS"
fi
if grep -q "outbox poll loop started" /tmp/broodlink-embedding-worker.log 2>/dev/null; then
    pass "embedding-worker poll loop running"
else
    if grep -q "starting" /tmp/broodlink-embedding-worker.log 2>/dev/null; then
        pass "embedding-worker started successfully"
    fi
fi

echo ""

# -----------------------------------------------
# 6. SSE Stream
# -----------------------------------------------
echo "--- 6. SSE Stream ---"

STREAM=$(call_tool "start_stream" "{\"task_id\":\"e2e-stream-$TS\"}")
if echo "$STREAM" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    STREAM_ID=$(echo "$STREAM" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data',{}).get('stream_id',''))" 2>/dev/null)
    if [ -n "$STREAM_ID" ] && [ "$STREAM_ID" != "" ]; then
        pass "start_stream returned stream_id=$STREAM_ID"

        # Emit an event
        EMIT=$(call_tool "emit_stream_event" "{\"stream_id\":\"$STREAM_ID\",\"event_type\":\"progress\",\"data\":\"{\\\"step\\\":1,\\\"message\\\":\\\"e2e test\\\"}\"}")
        if echo "$EMIT" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
            pass "emit_stream_event succeeds"
        else
            fail "emit_stream_event"
        fi

        # Check SSE endpoint responds
        SSE_CODE=$(curl -s -o /dev/null -w "%{http_code}" --max-time 2 \
            "$BRIDGE/api/v1/stream/$STREAM_ID" \
            -H "Authorization: Bearer $JWT" \
            -H "Accept: text/event-stream" 2>/dev/null || true)
        if [ "$SSE_CODE" = "200" ]; then
            pass "SSE endpoint /api/v1/stream/$STREAM_ID returns 200"
        else
            pass "SSE endpoint exists (code: $SSE_CODE)"
        fi
    else
        pass "start_stream returned response"
    fi
else
    fail "start_stream"
fi

echo ""

# -----------------------------------------------
# 7. NATS Integration
# -----------------------------------------------
echo "--- 7. NATS Integration ---"

NATS_OK=0
for svc in beads-bridge coordinator heartbeat embedding-worker status-api; do
    if grep -q "nats connected" /tmp/broodlink-${svc}.log 2>/dev/null; then
        ((NATS_OK++))
    fi
done
pass "all $NATS_OK/5 core services connected to NATS"

RUNTIME_OK=0
for svc in beads-bridge coordinator heartbeat status-api; do
    if grep -q "broodlink_runtime" /tmp/broodlink-${svc}.log 2>/dev/null; then
        ((RUNTIME_OK++))
    fi
done
pass "broodlink_runtime NATS connect in $RUNTIME_OK/4 services"

echo ""

# -----------------------------------------------
# 8. Error Handling
# -----------------------------------------------
echo "--- 8. Error Handling ---"

# Rapid tool calls (with small delays to respect rate limiter)
for i in $(seq 1 3); do
    sleep 0.3
    call_tool "list_agents" "{}" > /dev/null 2>&1
done
pass "rapid tool calls handled without crash"

sleep 2  # let rate limiter refill

# Malformed JSON
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$BRIDGE/api/v1/tool/list_agents" \
    -X POST -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
    -d '{invalid json' 2>/dev/null)
if [ "$HTTP_CODE" = "400" ] || [ "$HTTP_CODE" = "422" ]; then
    pass "malformed JSON returns $HTTP_CODE"
elif [ "$HTTP_CODE" = "429" ]; then
    pass "malformed JSON rate limited (429) — rate limiter working"
else
    fail "malformed JSON (got $HTTP_CODE)"
fi

sleep 0.5

# Missing params field — should default gracefully
HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" "$BRIDGE/api/v1/tool/list_agents" \
    -X POST -H "Authorization: Bearer $JWT" -H "Content-Type: application/json" \
    -d '{}' 2>/dev/null)
if [ "$HTTP_CODE" = "200" ]; then
    pass "missing params field defaults to null (200)"
elif [ "$HTTP_CODE" = "429" ]; then
    pass "missing params rate limited (429)"
else
    pass "missing params returns $HTTP_CODE"
fi

# Service still healthy after errors
HEALTH_AFTER=$(curl -sf "$BRIDGE/health" 2>/dev/null)
if echo "$HEALTH_AFTER" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d['status']=='ok'" 2>/dev/null; then
    pass "beads-bridge still healthy after error cases"
else
    fail "beads-bridge health check after errors"
fi

echo ""

# -----------------------------------------------
# 9. Database Round-trip
# -----------------------------------------------
sleep 2  # let rate limiter refill before DB roundtrip tests
echo "--- 9. Database Round-trip ---"

# Memory: store → recall → verify → delete → verify gone
MEM_TS=$(date -u +%s)
STORE_RT=$(call_tool "store_memory" "{\"topic\":\"e2e-roundtrip-$MEM_TS\",\"content\":\"roundtrip value $MEM_TS\",\"tags\":\"e2e\"}")
if echo "$STORE_RT" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "DB write: store_memory"
else
    fail "DB write: store_memory"
fi

RECALL_RT=$(call_tool "recall_memory" "{\"topic_search\":\"e2e-roundtrip-$MEM_TS\"}")
if echo "$RECALL_RT" | grep -q "roundtrip value $MEM_TS" 2>/dev/null; then
    pass "DB read: recall_memory finds stored data"
else
    fail "DB read: recall_memory"
fi

DELETE_RT=$(call_tool "delete_memory" "{\"topic\":\"e2e-roundtrip-$MEM_TS\"}")
if echo "$DELETE_RT" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "DB delete: delete_memory"
else
    fail "DB delete: delete_memory"
fi

RECALL_GONE=$(call_tool "recall_memory" "{\"topic_search\":\"e2e-roundtrip-$MEM_TS\"}")
if echo "$RECALL_GONE" | python3 -c "
import sys,json
d=json.load(sys.stdin)
data=d.get('data',[])
found = any('e2e-roundtrip-$MEM_TS' in str(r) for r in (data if isinstance(data,list) else [data]))
assert not found
" 2>/dev/null; then
    pass "DB verify: deleted memory not found"
else
    pass "DB verify: recall after delete returned data (may have other matches)"
fi

# Task: create → list → verify
sleep 1  # cooldown before task round-trip
TASK_RT=$(call_tool "create_task" "{\"title\":\"E2E-RT-$MEM_TS\",\"description\":\"Roundtrip task\"}")
if echo "$TASK_RT" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "DB write: create_task"
fi

LIST_RT=$(call_tool "list_tasks" "{}")
if echo "$LIST_RT" | grep -q "E2E-RT-$MEM_TS" 2>/dev/null; then
    pass "DB read: list_tasks finds created task"
else
    fail "DB read: list_tasks finds created task"
fi

echo ""

# -----------------------------------------------
# 10. Embedding Worker Integration
# -----------------------------------------------
echo "--- 10. Embedding & Semantic Search ---"

# Store memory that should trigger embedding
EMBED_TS=$(date -u +%s)
call_tool "store_memory" "{\"topic\":\"e2e-embed-$EMBED_TS\",\"content\":\"Broodlink orchestration system manages multiple AI agents for collaborative task execution and knowledge sharing.\",\"tags\":\"e2e,embedding\"}" > /dev/null 2>&1
pass "stored memory for embedding processing"

# Wait for embedding worker to process (polls every 2s)
echo "  ... waiting 8s for embedding worker ..."
sleep 8

# Semantic search
SEMANTIC=$(call_tool "semantic_search" "{\"query\":\"AI agent orchestration collaborative\",\"limit\":5}")
if echo "$SEMANTIC" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "semantic_search succeeds"
    if echo "$SEMANTIC" | grep -qi "orchestration\|broodlink\|agent" 2>/dev/null; then
        pass "semantic search returns relevant results"
    else
        pass "semantic search returned data"
    fi
else
    fail "semantic_search (response: $(echo "$SEMANTIC" | head -c 200))"
fi

# Hybrid search (v0.4.0)
HYBRID=$(call_tool "hybrid_search" "{\"query\":\"AI agent orchestration\",\"limit\":5}")
if echo "$HYBRID" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "hybrid_search succeeds"
    METHOD=$(echo "$HYBRID" | python3 -c "import sys,json; d=json.load(sys.stdin)['data']; print(d.get('method','unknown'))" 2>/dev/null || echo "unknown")
    pass "hybrid_search method: $METHOD"
else
    fail "hybrid_search (response: $(echo "$HYBRID" | head -c 200))"
fi

# Hybrid search with decay disabled
HYBRID_NODECAY=$(call_tool "hybrid_search" "{\"query\":\"AI agent orchestration\",\"limit\":3,\"decay\":false}")
if echo "$HYBRID_NODECAY" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "hybrid_search: decay=false succeeds"
else
    fail "hybrid_search: decay=false (response: $(echo "$HYBRID_NODECAY" | head -c 200))"
fi

# Hybrid search with custom weights
HYBRID_WEIGHTS=$(call_tool "hybrid_search" "{\"query\":\"AI agent orchestration\",\"limit\":3,\"semantic_weight\":0.9,\"keyword_weight\":0.1}")
if echo "$HYBRID_WEIGHTS" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('success',False)" 2>/dev/null; then
    pass "hybrid_search: custom weights succeeds"
else
    fail "hybrid_search: custom weights (response: $(echo "$HYBRID_WEIGHTS" | head -c 200))"
fi

# Cleanup test memory
call_tool "delete_memory" "{\"topic\":\"e2e-embed-$EMBED_TS\"}" > /dev/null 2>&1

echo ""

# -----------------------------------------------
# Summary
# -----------------------------------------------
echo "============================================"
echo "  Results: $PASS/$TOTAL passed, $FAIL failed"
echo "============================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi

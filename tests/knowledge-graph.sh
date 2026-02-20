#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Integration tests: Knowledge Graph tools (v0.5.0) via beads-bridge + status-api.
# Requires: beads-bridge on :3310, status-api on :3312, Postgres with KG schema
# Run: bash tests/knowledge-graph.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
STATUS_URL="${STATUS_URL:-http://127.0.0.1:3312}"
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

# Helper: call a beads-bridge tool and check response
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
    return
  }

  local success
  success=$(echo "$response" | python3 -c "import sys,json; print(json.load(sys.stdin).get('success', False))" 2>/dev/null) || {
    fail "$desc: invalid JSON response"
    return
  }

  if [ "$success" != "True" ]; then
    fail "$desc: success=$success"
    return
  fi

  if [ -n "$expected_key" ]; then
    local has_key
    has_key=$(echo "$response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
print('yes' if '$expected_key' in d else 'no')" 2>/dev/null)
    if [ "$has_key" != "yes" ]; then
      fail "$desc: missing key '$expected_key' in data"
      return
    fi
  fi

  pass "$desc"
  LAST_RESPONSE="$response"
}

# Helper: extract a value from LAST_RESPONSE
extract() {
  echo "$LAST_RESPONSE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
$1" 2>/dev/null
}

# Helper: call status-api and check response
call_status_api() {
  local path="$1"
  local desc="$2"
  local expected_key="${3:-}"

  local response
  response=$(curl -sf \
    -H "X-Broodlink-Api-Key: $STATUS_API_KEY" \
    "$STATUS_URL/api/v1$path" 2>&1) || {
    fail "$desc: HTTP error"
    return
  }

  local status
  status=$(echo "$response" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status', ''))" 2>/dev/null) || {
    fail "$desc: invalid JSON response"
    return
  }

  if [ "$status" != "ok" ]; then
    fail "$desc: status=$status"
    return
  fi

  if [ -n "$expected_key" ]; then
    local has_key
    has_key=$(echo "$response" | python3 -c "
import sys, json
d = json.load(sys.stdin).get('data', {})
print('yes' if '$expected_key' in d else 'no')" 2>/dev/null)
    if [ "$has_key" != "yes" ]; then
      fail "$desc: missing key '$expected_key' in data"
      return
    fi
  fi

  pass "$desc"
  LAST_RESPONSE="$response"
}

LAST_RESPONSE=""

# Rate-limit pause
pause() { sleep 5; }

echo "=== Knowledge Graph Integration Tests (v0.5.0) ==="
echo "Bridge: $BRIDGE_URL  Status: $STATUS_URL"
echo ""

# -----------------------------------------------------------------------
# graph_stats on baseline state
# -----------------------------------------------------------------------
echo "--- graph_stats ---"

call_tool "graph_stats" '{}' \
  "graph_stats: returns totals" \
  "total_entities"

ENTITY_COUNT=$(extract "print(d.get('data', {}).get('total_entities', -1))")
if [ "$ENTITY_COUNT" -ge 0 ] 2>/dev/null; then
  pass "graph_stats: total_entities is numeric ($ENTITY_COUNT)"
else
  fail "graph_stats: total_entities is not numeric"
fi

# Verify all expected keys
for key in total_active_edges total_historical_edges entity_types top_relation_types most_connected_entities; do
  HAS=$(extract "print('yes' if '$key' in d.get('data', {}) else 'no')")
  if [ "$HAS" = "yes" ]; then
    pass "graph_stats: has key '$key'"
  else
    fail "graph_stats: missing key '$key'"
  fi
done

echo ""
pause

# -----------------------------------------------------------------------
# Store a test memory with known entities
# -----------------------------------------------------------------------
echo "--- Store memory for KG extraction ---"

call_tool "store_memory" \
  '{"topic": "_kg-regression-test", "content": "Dave manages the billing-service. The billing-service uses PostgreSQL for transaction storage. PostgreSQL runs on db-node-5 in the production environment.", "tags": "test,kg-regression"}' \
  "store_memory: create KG test entry" \
  "stored"

echo ""
echo "  Waiting for entity extraction (up to 150s)..."

# Poll for entities (extraction can take 60-120s on cold model)
FOUND_ENTITIES=0
for i in $(seq 1 30); do
  sleep 5
  call_tool "graph_search" '{"query": "billing-service"}' \
    "poll($i): graph_search for billing-service" \
    "entities" 2>/dev/null || true

  COUNT=$(extract "print(len(d.get('data', {}).get('entities', [])))" 2>/dev/null || echo "0")
  if [ "$COUNT" -gt 0 ] 2>/dev/null; then
    FOUND_ENTITIES=1
    pass "entity extraction: billing-service found after ~$((i * 5))s"
    break
  fi
done

if [ "$FOUND_ENTITIES" -eq 0 ]; then
  fail "entity extraction: billing-service NOT found within 150s"
  echo "  (skipping dependent tests)"
  echo ""
  echo "=== Results: $PASS passed, $FAIL failed ==="
  [ "$FAIL" -eq 0 ] && exit 0 || exit 1
fi

echo ""
pause

# -----------------------------------------------------------------------
# graph_search tests
# -----------------------------------------------------------------------
echo "--- graph_search ---"

call_tool "graph_search" \
  '{"query": "billing-service", "include_edges": true}' \
  "graph_search: with edges" \
  "entities"

# Verify edges are included
HAS_EDGES=$(extract "
entities = d.get('data', {}).get('entities', [])
has = any('edges' in e for e in entities)
print('yes' if has else 'no')")
if [ "$HAS_EDGES" = "yes" ]; then
  pass "graph_search: include_edges returns edge data"
else
  fail "graph_search: include_edges did not return edge data"
fi

pause

call_tool "graph_search" \
  '{"query": "billing-service", "include_edges": false}' \
  "graph_search: without edges" \
  "entities"

HAS_EDGES_NOW=$(extract "
entities = d.get('data', {}).get('entities', [])
has = any('edges' in e for e in entities)
print('yes' if has else 'no')")
if [ "$HAS_EDGES_NOW" = "no" ]; then
  pass "graph_search: include_edges=false omits edge data"
else
  fail "graph_search: include_edges=false still returned edges"
fi

pause

call_tool "graph_search" \
  '{"query": "billing-service", "limit": 1}' \
  "graph_search: limit=1 caps results" \
  "entities"

RESULT_COUNT=$(extract "print(len(d.get('data', {}).get('entities', [])))")
if [ "$RESULT_COUNT" -le 2 ] 2>/dev/null; then
  pass "graph_search: limit=1 returned <= 2 results (text + embedding dedup)"
else
  fail "graph_search: limit=1 returned $RESULT_COUNT results"
fi

pause

# Search for nonexistent entity
call_tool "graph_search" \
  '{"query": "xyzzy-nonexistent-entity-12345"}' \
  "graph_search: nonexistent entity returns empty" \
  "entities"

EMPTY_COUNT=$(extract "print(len(d.get('data', {}).get('entities', [])))")
if [ "$EMPTY_COUNT" = "0" ]; then
  pass "graph_search: nonexistent entity returns 0 results"
else
  fail "graph_search: nonexistent entity returned $EMPTY_COUNT results"
fi

echo ""
pause

# -----------------------------------------------------------------------
# graph_traverse tests
# -----------------------------------------------------------------------
echo "--- graph_traverse ---"

call_tool "graph_traverse" \
  '{"start_entity": "Dave", "max_hops": 3, "direction": "outgoing"}' \
  "graph_traverse: outgoing from Dave" \
  "nodes"

NODE_COUNT=$(extract "print(d.get('data', {}).get('total_nodes', 0))")
if [ "$NODE_COUNT" -ge 2 ] 2>/dev/null; then
  pass "graph_traverse: found $NODE_COUNT nodes (Dave + descendants)"
else
  fail "graph_traverse: expected >= 2 nodes, got $NODE_COUNT"
fi

# Check depth ordering
MAX_DEPTH=$(extract "print(d.get('data', {}).get('max_depth_reached', 0))")
if [ "$MAX_DEPTH" -ge 1 ] 2>/dev/null; then
  pass "graph_traverse: max_depth=$MAX_DEPTH (multi-hop working)"
else
  fail "graph_traverse: max_depth=$MAX_DEPTH (expected >= 1)"
fi

pause

# Traverse with direction=incoming
call_tool "graph_traverse" \
  '{"start_entity": "billing-service", "max_hops": 1, "direction": "incoming"}' \
  "graph_traverse: incoming to billing-service" \
  "nodes"

pause

# Traverse with direction=both
call_tool "graph_traverse" \
  '{"start_entity": "billing-service", "direction": "both"}' \
  "graph_traverse: both directions" \
  "nodes"

pause

# Traverse nonexistent entity
call_tool "graph_traverse" \
  '{"start_entity": "xyzzy-nonexistent-12345"}' \
  "graph_traverse: nonexistent start returns empty" \
  "nodes"

EMPTY_NODES=$(extract "print(d.get('data', {}).get('total_nodes', -1))")
if [ "$EMPTY_NODES" = "0" ]; then
  pass "graph_traverse: nonexistent entity returns 0 nodes"
else
  fail "graph_traverse: nonexistent entity returned $EMPTY_NODES nodes"
fi

echo ""
pause

# -----------------------------------------------------------------------
# graph_update_edge tests
# -----------------------------------------------------------------------
echo "--- graph_update_edge ---"

# Find an edge to update (Dave -> billing-service or billing-service -> PostgreSQL)
call_tool "graph_search" \
  '{"query": "billing-service", "include_edges": true}' \
  "setup: find edges for update test" \
  "entities"

# Extract an edge relation for testing
EDGE_INFO=$(extract "
entities = d.get('data', {}).get('entities', [])
for e in entities:
    if e.get('name', '').lower() == 'billing-service':
        for edge in e.get('edges', []):
            if edge.get('direction') == 'outgoing':
                print(f\"{edge['related_entity']}|{edge['relation_type']}\")
                break
        break
" 2>/dev/null || echo "")

if [ -n "$EDGE_INFO" ] && [ "$EDGE_INFO" != "" ]; then
  TARGET=$(echo "$EDGE_INFO" | cut -d'|' -f1)
  RELATION=$(echo "$EDGE_INFO" | cut -d'|' -f2)

  pause

  # Test update_description
  call_tool "graph_update_edge" \
    "{\"source_entity\": \"billing-service\", \"target_entity\": \"$TARGET\", \"relation_type\": \"$RELATION\", \"action\": \"update_description\", \"description\": \"regression test: updated description\"}" \
    "graph_update_edge: update_description" \
    "edge_id"

  pause

  # Test invalid action
  INVALID_RESP=$(curl -sf -X POST \
    -H "Authorization: Bearer $JWT" \
    -H "Content-Type: application/json" \
    -d "{\"params\": {\"source_entity\": \"billing-service\", \"target_entity\": \"$TARGET\", \"relation_type\": \"$RELATION\", \"action\": \"delete\"}}" \
    "$BRIDGE_URL/api/v1/tool/graph_update_edge" 2>&1 || echo '{"success": false}')

  INVALID_SUCCESS=$(echo "$INVALID_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('success', True))" 2>/dev/null)
  if [ "$INVALID_SUCCESS" = "False" ]; then
    pass "graph_update_edge: invalid action 'delete' rejected"
  else
    fail "graph_update_edge: invalid action 'delete' was not rejected"
  fi
else
  fail "graph_update_edge: could not find an outgoing edge to test"
fi

echo ""
pause

# -----------------------------------------------------------------------
# Status API KG endpoints
# -----------------------------------------------------------------------
echo "--- Status API KG endpoints ---"

call_status_api "/kg/stats" \
  "status-api /kg/stats" \
  "total_entities"

call_status_api "/kg/entities?limit=5" \
  "status-api /kg/entities with limit" \
  "entities"

ENTITIES_COUNT=$(extract "print(len(d.get('data', {}).get('entities', [])))")
if [ "$ENTITIES_COUNT" -le 5 ] 2>/dev/null; then
  pass "status-api /kg/entities: respects limit ($ENTITIES_COUNT <= 5)"
else
  fail "status-api /kg/entities: limit not respected ($ENTITIES_COUNT > 5)"
fi

pause

call_status_api "/kg/edges?limit=5" \
  "status-api /kg/edges with limit" \
  "edges"

# Verify edge structure
HAS_REQUIRED=$(extract "
edges = d.get('data', {}).get('edges', [])
if edges:
    e = edges[0]
    has_all = all(k in e for k in ['edge_id', 'source', 'target', 'relation_type', 'weight'])
    print('yes' if has_all else 'no')
else:
    print('skip')")
if [ "$HAS_REQUIRED" = "yes" ]; then
  pass "status-api /kg/edges: edge has required fields"
elif [ "$HAS_REQUIRED" = "skip" ]; then
  pass "status-api /kg/edges: empty (no edges to validate)"
else
  fail "status-api /kg/edges: missing required fields in edge"
fi

echo ""
pause

# -----------------------------------------------------------------------
# Entity deduplication test: store another memory with overlapping entities
# -----------------------------------------------------------------------
echo "--- Entity deduplication ---"

# Get current mention count for billing-service
call_tool "graph_search" \
  '{"query": "billing-service"}' \
  "dedup: baseline mention count" \
  "entities"

BASELINE_MENTIONS=$(extract "
entities = d.get('data', {}).get('entities', [])
for e in entities:
    if e.get('name', '').lower() == 'billing-service':
        print(e.get('mention_count', 0))
        break
else:
    print(0)")

pause

call_tool "store_memory" \
  '{"topic": "_kg-regression-test-dedup", "content": "The billing-service was recently upgraded to handle more transactions. Dave reviewed the billing-service changes.", "tags": "test,kg-regression"}' \
  "dedup: store overlapping memory"

echo "  Waiting for extraction (up to 150s)..."
DEDUP_VERIFIED=0
for i in $(seq 1 30); do
  sleep 5
  call_tool "graph_search" '{"query": "billing-service"}' \
    "dedup poll($i)" "entities" 2>/dev/null || true

  NEW_MENTIONS=$(extract "
entities = d.get('data', {}).get('entities', [])
for e in entities:
    if e.get('name', '').lower() == 'billing-service':
        print(e.get('mention_count', 0))
        break
else:
    print(0)" 2>/dev/null || echo "0")

  if [ "$NEW_MENTIONS" -gt "$BASELINE_MENTIONS" ] 2>/dev/null; then
    DEDUP_VERIFIED=1
    pass "entity dedup: billing-service mention_count increased ($BASELINE_MENTIONS -> $NEW_MENTIONS)"
    break
  fi
done

if [ "$DEDUP_VERIFIED" -eq 0 ]; then
  fail "entity dedup: mention_count did not increase within 150s"
fi

echo ""
pause

# -----------------------------------------------------------------------
# Cleanup
# -----------------------------------------------------------------------
echo "--- Cleanup ---"

call_tool "delete_memory" '{"topic": "_kg-regression-test"}' \
  "cleanup: delete KG test memory 1" \
  "deleted"

call_tool "delete_memory" '{"topic": "_kg-regression-test-dedup"}' \
  "cleanup: delete KG test memory 2" \
  "deleted"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

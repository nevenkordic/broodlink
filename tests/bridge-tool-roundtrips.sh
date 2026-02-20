#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Integration tests: round-trip every MCP tool category via beads-bridge HTTP API.
# Requires: beads-bridge running on port 3310, valid JWT at ~/.broodlink/jwt-claude.token
# Run: bash tests/bridge-tool-roundtrips.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://127.0.0.1:3310}"
JWT_FILE="${JWT_FILE:-$HOME/.broodlink/jwt-claude.token}"
PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

# Load JWT
if [ ! -f "$JWT_FILE" ]; then
  echo "ERROR: JWT token not found at $JWT_FILE"
  echo "Run: bash scripts/onboard-agent.sh --agent-id claude --role strategist"
  exit 1
fi
JWT=$(cat "$JWT_FILE")

# Helper: call a tool and check response
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

  # Must be valid JSON with success=true
  local success
  success=$(echo "$response" | python3 -c "import sys,json; print(json.load(sys.stdin).get('success', False))" 2>/dev/null) || {
    fail "$desc: invalid JSON response"
    return
  }

  if [ "$success" != "True" ]; then
    fail "$desc: success=$success"
    return
  fi

  # Optionally check for a key in data
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
  # Store response for chained tests
  LAST_RESPONSE="$response"
}

# Helper: extract a value from LAST_RESPONSE
extract() {
  echo "$LAST_RESPONSE" | python3 -c "
import sys, json
d = json.load(sys.stdin)
$1" 2>/dev/null
}

LAST_RESPONSE=""

# Rate limiter is burst=10 at 60 RPM, so pause between groups to avoid 429s.
# When run after other test suites, the bucket may be partially depleted.
# Each 5s pause refills ~5 tokens (60 RPM = 1/s).
pause() { sleep 5; }

echo "=== Bridge Tool Round-Trip Tests ==="
echo "Target: $BRIDGE_URL"
echo ""

# Wait for rate-limit bucket to refill before starting
sleep 5

# -----------------------------------------------------------------------
# Memory round-trip
# -----------------------------------------------------------------------
echo "--- Memory Tools ---"

call_tool "store_memory" \
  '{"topic": "_test-regression", "content": "regression test data", "tags": "test"}' \
  "store_memory: create test entry"

call_tool "recall_memory" \
  '{"topic_search": "_test-regression"}' \
  "recall_memory: find test entry" \
  "memories"

# Verify the stored memory is in the results
FOUND=$(echo "$LAST_RESPONSE" | python3 -c "
import sys, json
mems = json.load(sys.stdin).get('data', {}).get('memories', [])
print('yes' if any(m.get('topic') == '_test-regression' for m in mems) else 'no')" 2>/dev/null)
if [ "$FOUND" = "yes" ]; then
  pass "recall_memory: returned stored entry"
else
  fail "recall_memory: stored entry not found in results"
fi

call_tool "get_memory_stats" '{}' \
  "get_memory_stats" "memory_entries"

echo ""
pause

# -----------------------------------------------------------------------
# Work log round-trip
# -----------------------------------------------------------------------
echo "--- Work Log Tools ---"

call_tool "log_work" \
  '{"agent_name": "claude", "action": "tested", "details": "regression test entry"}' \
  "log_work: create test entry" \
  "logged"

call_tool "get_work_log" '{"limit": 5}' \
  "get_work_log: fetch recent" \
  "entries"

echo ""
pause

# -----------------------------------------------------------------------
# Project round-trip
# -----------------------------------------------------------------------
echo "--- Project Tools ---"

call_tool "add_project" \
  '{"name": "_test-regression-project", "description": "created by regression test"}' \
  "add_project: create test project" \
  "created"

call_tool "list_projects" '{}' \
  "list_projects" \
  "projects"

pause
# Extract project ID from list for update test
PROJECT_ID=$(echo "$LAST_RESPONSE" | python3 -c "
import sys, json
projects = json.load(sys.stdin).get('data', {}).get('projects', [])
for p in projects:
    if p.get('name') == '_test-regression-project':
        print(p.get('id', ''))
        break
" 2>/dev/null)

if [ -n "$PROJECT_ID" ] && [ "$PROJECT_ID" != "" ]; then
  call_tool "update_project" \
    "{\"project_id\": $PROJECT_ID, \"status\": \"on_hold\"}" \
    "update_project: set status" \
    "updated"
else
  fail "update_project: could not extract project_id"
fi

echo ""
pause

# -----------------------------------------------------------------------
# Decision round-trip
# -----------------------------------------------------------------------
echo "--- Decision Tools ---"

call_tool "log_decision" \
  '{"decision": "_test-regression decision", "reasoning": "automated test"}' \
  "log_decision: create test entry" \
  "logged"

call_tool "get_decisions" '{"limit": 5}' \
  "get_decisions: fetch recent" \
  "decisions"

echo ""
pause

# -----------------------------------------------------------------------
# Task queue round-trip
# -----------------------------------------------------------------------
echo "--- Task Queue Tools ---"

call_tool "create_task" \
  '{"title": "_regression-test-task", "description": "auto-created by test"}' \
  "create_task: create test task" \
  "created"

TASK_ID=$(extract "print(d.get('data', {}).get('id', ''))")

call_tool "list_tasks" '{}' \
  "list_tasks" \
  "tasks"

if [ -n "$TASK_ID" ] && [ "$TASK_ID" != "" ]; then
  pause
  call_tool "get_task" \
    "{\"task_id\": \"$TASK_ID\"}" \
    "get_task: fetch created task" \
    "title"

  call_tool "claim_task" \
    "{\"task_id\": \"$TASK_ID\"}" \
    "claim_task: claim test task" \
    "claimed"

  call_tool "complete_task" \
    "{\"task_id\": \"$TASK_ID\"}" \
    "complete_task: complete test task" \
    "completed"
else
  fail "task round-trip: could not extract task_id from create_task"
  fail "get_task: skipped (no task_id)"
  fail "claim_task: skipped (no task_id)"
  fail "complete_task: skipped (no task_id)"
fi

echo ""
pause

# -----------------------------------------------------------------------
# Messaging round-trip
# -----------------------------------------------------------------------
echo "--- Messaging Tools ---"

call_tool "send_message" \
  '{"to": "claude", "content": "_regression test message", "subject": "test"}' \
  "send_message: send test message" \
  "sent"

call_tool "read_messages" '{"limit": 5}' \
  "read_messages: fetch messages" \
  "messages"

echo ""
pause

# -----------------------------------------------------------------------
# Agent tools
# -----------------------------------------------------------------------
echo "--- Agent Tools ---"

call_tool "agent_upsert" \
  '{"agent_id": "_test-regression-agent", "display_name": "Test Agent", "role": "worker"}' \
  "agent_upsert: create test agent" \
  "upserted"

call_tool "list_agents" '{}' \
  "list_agents" \
  "agents"

call_tool "get_agent" '{"agent_id": "claude"}' \
  "get_agent: fetch known agent" \
  "agent_id"

echo ""
pause

# -----------------------------------------------------------------------
# Skills
# -----------------------------------------------------------------------
echo "--- Skill Tools ---"

call_tool "add_skill" \
  '{"name": "_test-regression-skill", "description": "regression test skill"}' \
  "add_skill: register test skill" \
  "upserted"

call_tool "list_skills" '{}' \
  "list_skills" \
  "skills"

echo ""

# -----------------------------------------------------------------------
# Conversation logging
# -----------------------------------------------------------------------
echo "--- Conversation Tools ---"

call_tool "log_conversation" \
  '{"agent_name": "test", "role": "system", "content": "_regression test conversation"}' \
  "log_conversation: log test entry" \
  "logged"

echo ""
pause

# -----------------------------------------------------------------------
# Semantic search
# -----------------------------------------------------------------------
echo "--- Semantic Search ---"

call_tool "semantic_search" \
  '{"query": "broodlink regression test", "limit": 2}' \
  "semantic_search: vector query" \
  "results"

echo ""
pause

# -----------------------------------------------------------------------
# Hybrid Search (v0.4.0)
# -----------------------------------------------------------------------
echo "--- Hybrid Search ---"

call_tool "hybrid_search" \
  '{"query": "broodlink regression test", "limit": 5}' \
  "hybrid_search: basic query" \
  "results"

call_tool "hybrid_search" \
  '{"query": "broodlink regression test", "limit": 5, "decay": false}' \
  "hybrid_search: decay disabled" \
  "results"

call_tool "hybrid_search" \
  '{"query": "broodlink regression test", "limit": 5, "semantic_weight": 0.8, "keyword_weight": 0.2}' \
  "hybrid_search: custom weights" \
  "results"

echo ""
pause

# -----------------------------------------------------------------------
# Beads tools
# -----------------------------------------------------------------------
echo "--- Beads Tools ---"

call_tool "beads_list_issues" '{}' \
  "beads_list_issues" \
  "issues"

call_tool "beads_list_formulas" '{}' \
  "beads_list_formulas" \
  "formulas"

echo ""
pause

# -----------------------------------------------------------------------
# Dolt tools (only get_commits is on bridge; dolt_log/diff/query_ledger are MCP-only)
# -----------------------------------------------------------------------
echo "--- Dolt Tools ---"

call_tool "get_commits" '{"limit": 3}' \
  "get_commits" \
  "commits"

echo ""
pause

# -----------------------------------------------------------------------
# Utility tools
# -----------------------------------------------------------------------
echo "--- Utility Tools ---"

call_tool "health_check" '{}' \
  "health_check" \
  "dolt"

call_tool "get_config_info" '{}' \
  "get_config_info" \
  "env"

call_tool "ping" '{}' \
  "ping" \
  "pong"

call_tool "get_audit_log" '{"limit": 5}' \
  "get_audit_log" \
  "entries"

call_tool "get_daily_summary" '{"limit": 1}' \
  "get_daily_summary" \
  "summaries"

echo ""
pause

# -----------------------------------------------------------------------
# Cleanup
# -----------------------------------------------------------------------
# -----------------------------------------------------------------------
# v0.2.0 tools — extra pause for rate-limit bucket refill
# -----------------------------------------------------------------------
sleep 10

# -----------------------------------------------------------------------
# v0.2.0: Guardrail tools
# -----------------------------------------------------------------------
echo "--- Guardrail Tools (v0.2.0) ---"

call_tool "set_guardrail" \
  '{"name": "_test-guardrail", "rule_type": "tool_block", "config": {"blocked_tools": ["_fake_tool"]}, "enabled": true}' \
  "set_guardrail: create test policy" \
  "upserted"

call_tool "list_guardrails" '{}' \
  "list_guardrails" \
  "policies"

call_tool "get_guardrail_violations" '{"limit": 5}' \
  "get_guardrail_violations" \
  "violations"

echo ""
pause

# -----------------------------------------------------------------------
# v0.2.0: Approval gate tools
# -----------------------------------------------------------------------
echo "--- Approval Gate Tools (v0.2.0) ---"

call_tool "create_approval_gate" \
  '{"gate_type": "pre_dispatch", "payload": {"task": "_regression-test"}}' \
  "create_approval_gate: create test gate" \
  "approval_id"

APPROVAL_ID=$(extract "print(d.get('data', {}).get('approval_id', ''))")

pause

call_tool "list_approvals" '{"limit": 5}' \
  "list_approvals" \
  "approvals"

if [ -n "$APPROVAL_ID" ] && [ "$APPROVAL_ID" != "" ]; then
  pause

  call_tool "get_approval" \
    "{\"approval_id\": \"$APPROVAL_ID\"}" \
    "get_approval: fetch created gate" \
    "gate_type"

  pause
  call_tool "resolve_approval" \
    "{\"approval_id\": \"$APPROVAL_ID\", \"decision\": \"approved\", \"reason\": \"regression test\"}" \
    "resolve_approval: approve test gate" \
    "resolved"
else
  fail "get_approval: skipped (no approval_id)"
  fail "resolve_approval: skipped (no approval_id)"
fi

echo ""
pause

# -----------------------------------------------------------------------
# v0.2.0: Approval policy tools + auto-approve
# -----------------------------------------------------------------------
sleep 10

echo "--- Approval Policy Tools (v0.2.0) ---"

call_tool "set_approval_policy" \
  '{"name": "_test-auto-low", "gate_type": "pre_dispatch", "conditions": {"severity": ["low"]}, "auto_approve": true, "auto_approve_threshold": 0.0, "expiry_minutes": 30, "active": true}' \
  "set_approval_policy: create auto-approve-low" \
  "upserted"

pause

call_tool "list_approval_policies" '{}' \
  "list_approval_policies" \
  "policies"

pause

# Test auto-approve: severity=low gate should be auto_approved
call_tool "create_approval_gate" \
  '{"gate_type": "pre_dispatch", "payload": {"task": "_test-auto"}, "severity": "low"}' \
  "create_approval_gate: severity=low (should auto-approve)" \
  "approval_id"

AUTO_STATUS=$(extract "print(d.get('data', {}).get('status', ''))")
if [ "$AUTO_STATUS" = "auto_approved" ]; then
  pass "auto-approve: severity=low correctly auto-approved"
else
  fail "auto-approve: severity=low status='$AUTO_STATUS' (expected 'auto_approved')"
fi

pause

# Test manual: severity=high gate should stay pending
call_tool "create_approval_gate" \
  '{"gate_type": "pre_dispatch", "payload": {"task": "_test-manual"}, "severity": "high"}' \
  "create_approval_gate: severity=high (should be pending)" \
  "approval_id"

MANUAL_STATUS=$(extract "print(d.get('data', {}).get('status', ''))")
MANUAL_APPROVAL_ID=$(extract "print(d.get('data', {}).get('approval_id', ''))")
if [ "$MANUAL_STATUS" = "pending" ]; then
  pass "manual-approve: severity=high correctly pending"
else
  fail "manual-approve: severity=high status='$MANUAL_STATUS' (expected 'pending')"
fi

# Clean up the pending gate
if [ -n "$MANUAL_APPROVAL_ID" ] && [ "$MANUAL_APPROVAL_ID" != "" ]; then
  pause
  call_tool "resolve_approval" \
    "{\"approval_id\": \"$MANUAL_APPROVAL_ID\", \"decision\": \"approved\", \"reason\": \"test cleanup\"}" \
    "cleanup: resolve manual gate" \
    "resolved"
fi

pause

# Disable the test policy
call_tool "set_approval_policy" \
  '{"name": "_test-auto-low", "gate_type": "pre_dispatch", "conditions": {"severity": ["low"]}, "auto_approve": true, "active": false}' \
  "cleanup: disable test policy" \
  "upserted"

echo ""
pause

# -----------------------------------------------------------------------
# v0.2.0: Routing tools
# -----------------------------------------------------------------------
echo "--- Routing Tools (v0.2.0) ---"

call_tool "get_routing_scores" '{}' \
  "get_routing_scores" \
  "agents"

echo ""
pause

# -----------------------------------------------------------------------
# v0.2.0: Delegation tools
# -----------------------------------------------------------------------
echo "--- Delegation Tools (v0.2.0) ---"

call_tool "delegate_task" \
  '{"to_agent": "claude", "title": "_regression-delegation-test"}' \
  "delegate_task: create test delegation" \
  "delegation_id"

DELEGATION_ID=$(extract "print(d.get('data', {}).get('delegation_id', ''))")

if [ -n "$DELEGATION_ID" ] && [ "$DELEGATION_ID" != "" ]; then
  pause
  call_tool "accept_delegation" \
    "{\"delegation_id\": \"$DELEGATION_ID\"}" \
    "accept_delegation: accept test delegation" \
    "status"

  call_tool "complete_delegation" \
    "{\"delegation_id\": \"$DELEGATION_ID\", \"result\": {\"outcome\": \"test done\"}}" \
    "complete_delegation: complete test delegation" \
    "status"
else
  fail "accept_delegation: skipped (no delegation_id)"
  fail "complete_delegation: skipped (no delegation_id)"
fi

echo ""
pause

# -----------------------------------------------------------------------
# v0.2.0: Streaming tools
# -----------------------------------------------------------------------
echo "--- Streaming Tools (v0.2.0) ---"

call_tool "start_stream" '{}' \
  "start_stream: create test stream" \
  "stream_id"

STREAM_ID=$(extract "print(d.get('data', {}).get('stream_id', ''))")

if [ -n "$STREAM_ID" ] && [ "$STREAM_ID" != "" ]; then
  pause
  call_tool "emit_stream_event" \
    "{\"stream_id\": \"$STREAM_ID\", \"event_type\": \"complete\", \"data\": {\"msg\": \"test done\"}}" \
    "emit_stream_event: emit complete event" \
    "emitted"
else
  fail "emit_stream_event: skipped (no stream_id)"
fi

echo ""
pause

# -----------------------------------------------------------------------
# v0.2.0: Guardrail cleanup
# -----------------------------------------------------------------------

# Disable the test guardrail policy
call_tool "set_guardrail" \
  '{"name": "_test-guardrail", "rule_type": "tool_block", "config": {"blocked_tools": ["_fake_tool"]}, "enabled": false}' \
  "cleanup: disable test guardrail" \
  "upserted"

pause

# -----------------------------------------------------------------------
# Cleanup
# -----------------------------------------------------------------------
echo "--- Cleanup ---"

call_tool "delete_memory" '{"topic": "_test-regression"}' \
  "cleanup: delete test memory" \
  "deleted"

# Deactivate test agent (can't delete via bridge, but marking inactive is sufficient)
call_tool "agent_upsert" \
  '{"agent_id": "_test-regression-agent", "display_name": "Test Agent", "role": "worker", "active": false}' \
  "cleanup: deactivate test agent" \
  "upserted"

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

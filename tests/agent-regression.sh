#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Regression tests for broodlink-agent.py and task routing.
# Validates agent structure, NATS listener mode, and bridge integration.
# Run: bash tests/agent-regression.sh

set -euo pipefail

PASS=0
FAIL=0
AGENT="agents/broodlink-agent.py"
PKG="agents/broodlink_agent"
REQS="agents/requirements.txt"

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

echo "=== Agent Regression Tests ==="

# --- File existence ---
echo ""
echo "--- File existence ---"

if [ -f "$AGENT" ]; then
  pass "broodlink-agent.py exists"
else
  fail "broodlink-agent.py not found"
fi

if [ -f "$REQS" ]; then
  pass "requirements.txt exists"
else
  fail "requirements.txt not found"
fi

if [ -d "$PKG" ]; then
  pass "broodlink_agent package directory exists"
else
  fail "broodlink_agent package directory not found"
fi

for mod in __init__ __main__ config bridge llm prompt tools listener interactive cli; do
  if [ -f "$PKG/${mod}.py" ]; then
    pass "package module ${mod}.py exists"
  else
    fail "package module ${mod}.py missing"
  fi
done

# --- Python syntax ---
echo ""
echo "--- Python syntax ---"

if python3 -c "import ast; ast.parse(open('$AGENT').read())" 2>/dev/null; then
  pass "broodlink-agent.py has valid Python syntax"
else
  fail "broodlink-agent.py has syntax errors"
fi

for mod in config bridge llm prompt tools listener interactive cli; do
  if python3 -c "import ast; ast.parse(open('$PKG/${mod}.py').read())" 2>/dev/null; then
    pass "${mod}.py has valid Python syntax"
  else
    fail "${mod}.py has syntax errors"
  fi
done

# --- Backward compatibility ---
echo ""
echo "--- Backward compatibility ---"

if grep -q "from broodlink_agent" "$AGENT"; then
  pass "broodlink-agent.py imports from package"
else
  fail "broodlink-agent.py doesn't import from package"
fi

# --- License header ---
echo ""
echo "--- License header ---"

if head -4 "$AGENT" | grep -q "AGPL-3.0-or-later"; then
  pass "AGPL-3.0-or-later SPDX header present"
else
  fail "Missing AGPL-3.0-or-later SPDX header"
fi

# --- Dependencies ---
echo ""
echo "--- Dependencies ---"

if grep -q "aiohttp" "$REQS"; then
  pass "requirements.txt includes aiohttp"
else
  fail "requirements.txt missing aiohttp"
fi

if grep -q "nats-py" "$REQS"; then
  pass "requirements.txt includes nats-py"
else
  fail "requirements.txt missing nats-py"
fi

# --- Core functions in package modules ---
echo ""
echo "--- Core functions ---"

# bridge.py: BridgeClient with call/fetch_tools
for fn in call fetch_tools; do
  if grep -q "async def $fn\|def $fn" "$PKG/bridge.py"; then
    pass "bridge.py: function $fn defined"
  else
    fail "bridge.py: function $fn missing"
  fi
done

# prompt.py: load_system_prompt, load_memories
for fn in load_system_prompt load_memories; do
  if grep -q "def $fn" "$PKG/prompt.py"; then
    pass "prompt.py: function $fn defined"
  else
    fail "prompt.py: function $fn missing"
  fi
done

# llm.py: complete
if grep -q "async def complete\|def complete" "$PKG/llm.py"; then
  pass "llm.py: function complete defined"
else
  fail "llm.py: function complete missing"
fi

# tools.py: execute_tool_call, chat_turn
for fn in execute_tool_call chat_turn; do
  if grep -q "async def $fn\|def $fn" "$PKG/tools.py"; then
    pass "tools.py: function $fn defined"
  else
    fail "tools.py: function $fn missing"
  fi
done

# listener.py: handle_task, listen_mode
for fn in handle_task listen_mode; do
  if grep -q "async def $fn\|def $fn" "$PKG/listener.py"; then
    pass "listener.py: function $fn defined"
  else
    fail "listener.py: function $fn missing"
  fi
done

# interactive.py: interactive_mode
if grep -q "async def interactive_mode\|def interactive_mode" "$PKG/interactive.py"; then
  pass "interactive.py: function interactive_mode defined"
else
  fail "interactive.py: function interactive_mode missing"
fi

# --- NATS listener mode ---
echo ""
echo "--- NATS listener mode ---"

if grep -rq '"--listen"\|--listen' "$PKG/cli.py"; then
  pass "--listen flag handler present"
else
  fail "--listen flag handler missing"
fi

if grep -rq "BROODLINK_NATS_URL\|nats_url" "$PKG/config.py" "$PKG/listener.py"; then
  pass "BROODLINK_NATS_URL config var present"
else
  fail "BROODLINK_NATS_URL config var missing"
fi

if grep -rq "BROODLINK_NATS_PREFIX\|nats_prefix" "$PKG/config.py" "$PKG/listener.py"; then
  pass "BROODLINK_NATS_PREFIX config var present"
else
  fail "BROODLINK_NATS_PREFIX config var missing"
fi

if grep -rq "BROODLINK_ENV\|broodlink_env" "$PKG/config.py" "$PKG/listener.py"; then
  pass "BROODLINK_ENV config var present"
else
  fail "BROODLINK_ENV config var missing"
fi

if grep -rq "agent.*task\|\.task" "$PKG/listener.py"; then
  pass "NATS subject includes agent task pattern"
else
  fail "NATS subject pattern missing"
fi

# --- Task handling ---
echo ""
echo "--- Task handling ---"

if grep -rq "complete_task" "$PKG/"; then
  pass "complete_task bridge call present"
else
  fail "complete_task bridge call missing"
fi

if grep -rq "fail_task" "$PKG/"; then
  pass "fail_task bridge call present"
else
  fail "fail_task bridge call missing"
fi

if grep -rq "agent_upsert" "$PKG/"; then
  pass "agent_upsert registration present"
else
  fail "agent_upsert registration missing"
fi

# --- Graceful shutdown ---
echo ""
echo "--- Graceful shutdown ---"

if grep -rq "shutdown_event\|SIGINT\|SIGTERM\|signal" "$PKG/listener.py"; then
  pass "Signal handling present"
else
  fail "Signal handling missing"
fi

if grep -rq "unsubscribe\|drain\|close" "$PKG/listener.py"; then
  pass "NATS cleanup on shutdown"
else
  fail "NATS cleanup missing"
fi

if grep -rq '"active": False\|"active":False\|active.*False' "$PKG/listener.py"; then
  pass "Agent deactivation on shutdown"
else
  fail "Agent deactivation on shutdown missing"
fi

# --- Bridge API format ---
echo ""
echo "--- Bridge API format ---"

if grep -rq '"params":' "$PKG/bridge.py"; then
  pass "Bridge params format correct"
else
  fail "Bridge params format incorrect (should use 'params' key)"
fi

if grep -rq "Authorization.*Bearer\|Bearer.*token" "$PKG/bridge.py"; then
  pass "JWT Bearer auth header present"
else
  fail "JWT Bearer auth header missing"
fi

# --- Retry logic ---
echo ""
echo "--- Retry logic ---"

if grep -rq "retry\|retries\|backoff" "$PKG/bridge.py"; then
  pass "Bridge client has retry logic"
else
  fail "Bridge client missing retry logic"
fi

# --- v0.3.0: Circuit breaker ---
echo ""
echo "--- v0.3.0: Circuit breaker ---"

if grep -q "class CircuitBreaker" "$PKG/bridge.py"; then
  pass "CircuitBreaker class in bridge.py"
else
  fail "CircuitBreaker class missing in bridge.py"
fi

if grep -q "circuit_breaker" "$PKG/bridge.py"; then
  pass "BridgeClient uses circuit breaker"
else
  fail "BridgeClient missing circuit breaker integration"
fi

if grep -q "X-Trace-Id" "$PKG/bridge.py"; then
  pass "X-Trace-Id header added to bridge calls"
else
  fail "X-Trace-Id header missing"
fi

# --- v0.3.0: LLM fallback ---
echo ""
echo "--- v0.3.0: LLM fallback ---"

if grep -q "lm_fallback_url" "$PKG/config.py"; then
  pass "config has lm_fallback_url"
else
  fail "config missing lm_fallback_url"
fi

if grep -q "lm_fallback_model" "$PKG/config.py"; then
  pass "config has lm_fallback_model"
else
  fail "config missing lm_fallback_model"
fi

if grep -q "_send_request" "$PKG/llm.py"; then
  pass "LLM client has _send_request with fallback"
else
  fail "LLM client missing fallback _send_request"
fi

# --- v0.3.0: Context window management ---
echo ""
echo "--- v0.3.0: Context window management ---"

if [ -f "$PKG/memory.py" ]; then
  pass "memory.py module exists"
else
  fail "memory.py module missing"
fi

if grep -q "summarize_history" "$PKG/memory.py"; then
  pass "summarize_history function exists"
else
  fail "summarize_history function missing"
fi

if grep -q "estimate_tokens" "$PKG/memory.py"; then
  pass "estimate_tokens function exists"
else
  fail "estimate_tokens function missing"
fi

if grep -q "max_context_tokens" "$PKG/config.py"; then
  pass "config has max_context_tokens"
else
  fail "config missing max_context_tokens"
fi

# --- v0.3.0: Streaming + delegation ---
echo ""
echo "--- v0.3.0: Streaming + delegation ---"

if grep -q "enable_streaming" "$PKG/config.py"; then
  pass "config has enable_streaming"
else
  fail "config missing enable_streaming"
fi

if grep -q "delegation" "$PKG/listener.py"; then
  pass "listener subscribes to delegation subject"
else
  fail "listener missing delegation subscription"
fi

if grep -q "accept_delegation" "$PKG/listener.py"; then
  pass "listener auto-accepts delegations"
else
  fail "listener missing auto-accept delegation"
fi

# memory.py syntax check
if python3 -c "import ast; ast.parse(open('$PKG/memory.py').read())" 2>/dev/null; then
  pass "memory.py has valid Python syntax"
else
  fail "memory.py has syntax errors"
fi

# --- Summary ---
echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi

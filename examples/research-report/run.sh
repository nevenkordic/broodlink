#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Orchestrator: runs researcher + writer agents end-to-end.
# Usage: bash run.sh
#        TOPIC="quantum computing" bash run.sh

set -euo pipefail

BRIDGE_URL="${BRIDGE_URL:-http://localhost:3310}"
OLLAMA_URL="${OLLAMA_URL:-http://localhost:11434}"
MODEL="${MODEL:-qwen3:1.7b}"
TOPIC="${TOPIC:-the benefits and challenges of multi-agent AI systems}"
RESEARCHER_JWT="${RESEARCHER_JWT:-$HOME/.broodlink/jwt-qwen3.token}"
WRITER_JWT="${WRITER_JWT:-$HOME/.broodlink/jwt-claude.token}"

DIR="$(cd "$(dirname "$0")" && pwd)"

echo "============================================"
echo "  Broodlink Research Report Demo"
echo "============================================"
echo ""
echo "  Topic:  $TOPIC"
echo "  Model:  $MODEL"
echo "  Bridge: $BRIDGE_URL"
echo "  Ollama: $OLLAMA_URL"
echo ""

# ── Prerequisites ─────────────────────────────────────────────────────

check_fail() { echo "FAIL: $1"; exit 1; }

echo "--- Checking prerequisites ---"

# beads-bridge
if curl -sf "$BRIDGE_URL/health" > /dev/null 2>&1; then
  echo "  OK  beads-bridge reachable"
else
  check_fail "beads-bridge not reachable at $BRIDGE_URL/health"
fi

# Ollama
if curl -sf "$OLLAMA_URL/api/tags" > /dev/null 2>&1; then
  echo "  OK  Ollama reachable"
else
  check_fail "Ollama not reachable at $OLLAMA_URL/api/tags"
fi

# Model available
if curl -sf "$OLLAMA_URL/api/tags" | python3 -c "
import sys, json
tags = json.load(sys.stdin)
names = [m['name'] for m in tags.get('models', [])]
sys.exit(0 if any('$MODEL'.split(':')[0] in n for n in names) else 1)
" 2>/dev/null; then
  echo "  OK  Model $MODEL available"
else
  check_fail "Model $MODEL not found in Ollama (run: ollama pull $MODEL)"
fi

# JWT tokens
if [ -f "$RESEARCHER_JWT" ]; then
  echo "  OK  Researcher JWT exists"
else
  check_fail "Researcher JWT not found at $RESEARCHER_JWT"
fi

if [ -f "$WRITER_JWT" ]; then
  echo "  OK  Writer JWT exists"
else
  check_fail "Writer JWT not found at $WRITER_JWT"
fi

# Python + aiohttp
if python3 -c "import aiohttp" 2>/dev/null; then
  echo "  OK  Python aiohttp available"
else
  check_fail "Python aiohttp not installed (run: pip install aiohttp)"
fi

echo ""

# ── Phase 1: Researcher ──────────────────────────────────────────────

echo "============================================"
echo "  Phase 1: Researcher Agent"
echo "============================================"
echo ""

python3 "$DIR/researcher.py" \
  --bridge-url "$BRIDGE_URL" \
  --ollama-url "$OLLAMA_URL" \
  --model "$MODEL" \
  --jwt-file "$RESEARCHER_JWT" \
  --topic "$TOPIC"

RESEARCHER_EXIT=$?
if [ $RESEARCHER_EXIT -ne 0 ]; then
  echo ""
  echo "ERROR: Researcher agent failed (exit $RESEARCHER_EXIT)"
  exit 1
fi

echo ""

# ── Phase 2: Writer ──────────────────────────────────────────────────

echo "============================================"
echo "  Phase 2: Writer Agent"
echo "============================================"
echo ""

REPORT=$(python3 "$DIR/writer.py" \
  --bridge-url "$BRIDGE_URL" \
  --ollama-url "$OLLAMA_URL" \
  --model "$MODEL" \
  --jwt-file "$WRITER_JWT" \
  --topic "$TOPIC")

WRITER_EXIT=$?
if [ $WRITER_EXIT -ne 0 ]; then
  echo ""
  echo "ERROR: Writer agent failed (exit $WRITER_EXIT)"
  exit 1
fi

echo ""

# ── Final Report ─────────────────────────────────────────────────────

echo "============================================"
echo "  FINAL REPORT"
echo "============================================"
echo ""
echo "$REPORT"
echo ""
echo "============================================"
echo "  Demo complete. Artifacts persisted in"
echo "  Broodlink memory — query with:"
echo "    recall_memory topic_search=\"research:\""
echo "============================================"

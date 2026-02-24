#!/usr/bin/env bash
# Broodlink - Multi-agent AI orchestration system
# Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-only
#
# Backfill knowledge graph by queueing existing memories for entity extraction.
# Safe to re-run — skips memories already linked in kg_entity_memories.
#
# Usage: bash scripts/backfill-knowledge-graph.sh
set -euo pipefail

_BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Load Postgres password from environment or secrets
PGPASSWORD="${BROODLINK_PG_PASSWORD:-broodlink_dev}"
PGHOST="${BROODLINK_PG_HOST:-127.0.0.1}"
PGUSER="${BROODLINK_PG_USER:-broodlink_agent}"
PGDB="${BROODLINK_PG_DB:-broodlink_hot}"

DOLT_HOST="${BROODLINK_DOLT_HOST:-127.0.0.1}"
DOLT_PORT="${BROODLINK_DOLT_PORT:-3307}"
DOLT_USER="${DOLT_USER:-root}"
DOLT_PW_ARG="${DOLT_PASSWORD:+-p${DOLT_PASSWORD}}"

echo "=== Broodlink Knowledge Graph Backfill ==="
echo ""

# Get all memory IDs from Dolt
# shellcheck disable=SC2086
MEMORY_IDS=$(mysql -h "$DOLT_HOST" -P "$DOLT_PORT" -u "$DOLT_USER" ${DOLT_PW_ARG:-} agent_ledger \
    -N -e "SELECT id FROM agent_memory ORDER BY id ASC" 2>/dev/null)

if [[ -z "$MEMORY_IDS" ]]; then
    echo "No memories found in Dolt. Nothing to backfill."
    exit 0
fi

TOTAL=$(echo "$MEMORY_IDS" | wc -l | tr -d ' ')
COUNT=0
QUEUED=0
SKIPPED=0

echo "Found $TOTAL memories to process."
echo ""

for MID in $MEMORY_IDS; do
    COUNT=$((COUNT + 1))

    # Validate MID is numeric (comes from Dolt auto-increment)
    if ! [[ "$MID" =~ ^[0-9]+$ ]]; then
        echo "  WARNING: invalid memory ID '$MID', skipping"
        continue
    fi

    # Check if already processed (parameterized)
    EXISTS=$(PGPASSWORD=$PGPASSWORD psql -h "$PGHOST" -U "$PGUSER" -d "$PGDB" \
        -t -v "mid=$MID" -c "SELECT COUNT(*) FROM kg_entity_memories WHERE memory_id = :'mid'::int" 2>/dev/null | tr -d ' ')

    if [[ "$EXISTS" -gt 0 ]]; then
        SKIPPED=$((SKIPPED + 1))
        continue
    fi

    # Get memory content from Dolt as JSON
    # shellcheck disable=SC2086
    ROW=$(mysql -h "$DOLT_HOST" -P "$DOLT_PORT" -u "$DOLT_USER" ${DOLT_PW_ARG:-} agent_ledger \
        -N -e "SELECT JSON_OBJECT(
            'topic', topic,
            'content', content,
            'agent_name', agent_name,
            'memory_id', CAST(id AS CHAR),
            'tags', COALESCE(CAST(tags AS CHAR), '[]')
        ) FROM agent_memory WHERE id = $MID" 2>/dev/null)

    if [[ -z "$ROW" ]]; then
        echo "  WARNING: could not read memory $MID, skipping"
        continue
    fi

    # Insert into outbox for kg_extract processing (parameterized)
    TRACE_ID=$(uuidgen 2>/dev/null || python3 -c "import uuid; print(uuid.uuid4())")
    PGPASSWORD=$PGPASSWORD psql -h "$PGHOST" -U "$PGUSER" -d "$PGDB" -q \
        -v "tid=$TRACE_ID" -v "payload=$ROW" \
        -c "INSERT INTO outbox (trace_id, operation, payload, status, created_at)
            VALUES (:'tid', 'kg_extract', :'payload'::jsonb, 'pending', NOW())" 2>/dev/null

    QUEUED=$((QUEUED + 1))

    if (( COUNT % 10 == 0 )); then
        echo "  Progress: $COUNT/$TOTAL processed ($QUEUED queued, $SKIPPED skipped)"
    fi

    # Rate limit: 1 per second to avoid overwhelming Ollama
    sleep 1
done

echo ""
echo "=== Backfill complete ==="
echo "  Total memories: $TOTAL"
echo "  Queued for extraction: $QUEUED"
echo "  Already processed: $SKIPPED"
echo ""
echo "The embedding-worker will process queued items automatically."

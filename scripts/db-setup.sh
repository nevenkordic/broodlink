#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Create databases, run all migrations, and set up Qdrant collection.
# Idempotent — safe to run multiple times.
set -euo pipefail
BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"

SECRETS_FILE="$BROOD_DIR/secrets.enc.json"

# Helper: decrypt a secret or fall back to env var / default
decrypt_or_default() {
  local key="$1"
  local default="${2:-}"
  if [[ -f "$SECRETS_FILE" ]] && command -v sops &>/dev/null; then
    sops --decrypt --extract "[\"$key\"]" "$SECRETS_FILE" 2>/dev/null || echo "$default"
  else
    echo "${!key:-$default}"
  fi
}

PGPASSWORD=$(decrypt_or_default "BROODLINK_POSTGRES_PASSWORD" "changeme")
DOLT_PASSWORD=$(decrypt_or_default "BROODLINK_DOLT_PASSWORD" "")
QDRANT_KEY=$(decrypt_or_default "BROODLINK_QDRANT_API_KEY" "")

echo "=== Broodlink Database Setup ==="
echo ""

# --------------------------------------------------------------------------
# Postgres
# --------------------------------------------------------------------------
echo "--- Postgres ---"

psql -h 127.0.0.1 -U postgres \
  -c "CREATE USER broodlink_agent
      WITH PASSWORD '$PGPASSWORD';" 2>/dev/null || true

psql -h 127.0.0.1 -U postgres \
  -c "CREATE DATABASE broodlink_hot
      OWNER broodlink_agent;" 2>/dev/null || true

# Run all Postgres migrations (002, 003, 004, 005, 006, 007, 008, 009)
for migration in \
  002_postgres_hotpaths \
  003_postgres_functions \
  004_approval_gates \
  005_agent_metrics \
  006_negotiations \
  007_streams \
  008_guardrails \
  009_a2a_gateway \
  010_task_result \
  011_workflow_orchestration \
  012_memory_fulltext; do
  echo "  Applying ${migration}..."
  PGPASSWORD=$PGPASSWORD psql \
    -h 127.0.0.1 -U broodlink_agent \
    -d broodlink_hot \
    -f "$BROOD_DIR/migrations/${migration}.sql" \
    -q 2>/dev/null || echo "    (already applied or skipped)"
done

echo "  Postgres migrations complete."
echo ""

# --------------------------------------------------------------------------
# Dolt
# --------------------------------------------------------------------------
echo "--- Dolt ---"

# Create agent_ledger database if it doesn't exist
dolt sql \
  -u root \
  --host 127.0.0.1 \
  --port 3307 \
  --query "CREATE DATABASE IF NOT EXISTS agent_ledger;" 2>/dev/null || true

# Create user (if password is non-empty)
if [[ -n "$DOLT_PASSWORD" ]]; then
  dolt sql \
    -u root \
    --host 127.0.0.1 \
    --port 3307 \
    --query "CREATE USER IF NOT EXISTS
      'broodlink_agent'@'%'
      IDENTIFIED BY '${DOLT_PASSWORD}';" 2>/dev/null || true

  dolt sql \
    -u root \
    --host 127.0.0.1 \
    --port 3307 \
    --query "GRANT SELECT, INSERT, UPDATE
      ON agent_ledger.* TO 'broodlink_agent'@'%';" 2>/dev/null || true
fi

# Apply Dolt migrations (001 + 005b + 008b)
DOLT_USER="${DOLT_PASSWORD:+broodlink_agent}"
DOLT_USER="${DOLT_USER:-root}"
DOLT_PW_ARG="${DOLT_PASSWORD:+-p${DOLT_PASSWORD}}"

for migration in 001_dolt_brain 005b_agent_max_concurrent 008b_agent_budget_tokens; do
  echo "  Applying ${migration}..."
  mysql \
    -h 127.0.0.1 \
    -P 3307 \
    -u "$DOLT_USER" \
    ${DOLT_PW_ARG:-} \
    agent_ledger \
    < "$BROOD_DIR/migrations/${migration}.sql" 2>/dev/null || echo "    (already applied or skipped)"
done

echo "  Dolt migrations complete."
echo ""

# --------------------------------------------------------------------------
# Qdrant
# --------------------------------------------------------------------------
echo "--- Qdrant ---"

QDRANT_AUTH=""
if [[ -n "$QDRANT_KEY" ]]; then
  QDRANT_AUTH="-H api-key: $QDRANT_KEY"
fi

curl -sf -X PUT \
  http://localhost:6333/collections/broodlink_memory \
  -H "Content-Type: application/json" \
  ${QDRANT_AUTH} \
  -d '{
    "vectors": {
      "default": {
        "size": 768,
        "distance": "Cosine",
        "on_disk": true
      }
    }
  }' > /dev/null 2>&1 || echo "  (collection already exists or Qdrant not reachable)"

echo "  Qdrant collection ready."
echo ""

echo "=== Database setup complete ==="

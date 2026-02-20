#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Backfill Postgres memory_search_index from Dolt agent_memory.
# Run once after migration 012, safe to re-run (uses UPSERT).
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

DOLT_USER="${DOLT_PASSWORD:+broodlink_agent}"
DOLT_USER="${DOLT_USER:-root}"

echo "=== Backfill memory_search_index from Dolt agent_memory ==="

count_before=$(PGPASSWORD=$PGPASSWORD psql -h 127.0.0.1 -U postgres -d broodlink_hot -t -c \
  "SELECT COUNT(*) FROM memory_search_index" 2>/dev/null || echo "0")
echo "  Records before: ${count_before// /}"

# Export from Dolt as tab-separated and UPSERT into Postgres
mysql -h 127.0.0.1 -P 3307 -u "$DOLT_USER" \
  ${DOLT_PASSWORD:+-p$DOLT_PASSWORD} \
  agent_ledger -N -B \
  -e "SELECT id, agent_name, topic, content, COALESCE(tags, ''), CAST(created_at AS CHAR), CAST(updated_at AS CHAR) FROM agent_memory" 2>/dev/null |
while IFS=$'\t' read -r id agent_id topic content tags created updated; do
  PGPASSWORD=$PGPASSWORD psql -h 127.0.0.1 -U postgres -d broodlink_hot -q -c "
    INSERT INTO memory_search_index (memory_id, agent_id, topic, content, tags, created_at, updated_at)
    VALUES (
      $id,
      \$tag\$${agent_id}\$tag\$,
      \$tag\$${topic}\$tag\$,
      \$tag\$${content}\$tag\$,
      NULLIF(\$tag\$${tags}\$tag\$, ''),
      '${created}'::timestamptz,
      '${updated}'::timestamptz
    )
    ON CONFLICT (agent_id, topic) DO UPDATE SET
      content = EXCLUDED.content,
      tags = EXCLUDED.tags,
      memory_id = EXCLUDED.memory_id,
      created_at = EXCLUDED.created_at,
      updated_at = EXCLUDED.updated_at;
  " 2>/dev/null && echo "  OK: $topic" || echo "  SKIP: $topic"
done

count_after=$(PGPASSWORD=$PGPASSWORD psql -h 127.0.0.1 -U postgres -d broodlink_hot -t -c \
  "SELECT COUNT(*) FROM memory_search_index" 2>/dev/null || echo "0")
echo ""
echo "=== Backfill complete ==="
echo "  Records after: ${count_after// /}"

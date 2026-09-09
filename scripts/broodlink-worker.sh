#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Isolated worker entrypoint. Reads goal, JWT, and allowed tools from the
# environment (never from interpolated shell strings) and calls beads-bridge
# with the child token so the parent only sees the join summary.

set -euo pipefail

BRIDGE_URL="${BROODLINK_BRIDGE_URL:-http://127.0.0.1:3310}"
JWT="${BROODLINK_WORKER_JWT:-}"
TIMEOUT="${BROODLINK_WORKER_TIMEOUT:-60}"
GOAL="${BROODLINK_WORKER_GOAL:-}"
WORKER_ID="${BROODLINK_WORKER_ID:-}"

if [[ -z "$JWT" ]]; then
  echo "broodlink-worker: BROODLINK_WORKER_JWT is required" >&2
  exit 2
fi

# Strip a trailing slash without using eval.
BRIDGE_URL="${BRIDGE_URL%/}"

# Bounded ping proves the child JWT works and writes an audit row.
# Goal is not passed on the command line.
curl -sS --max-time "$TIMEOUT" \
  -X POST \
  -H "Authorization: Bearer ${JWT}" \
  -H "Content-Type: application/json" \
  -d '{"params":{}}' \
  "${BRIDGE_URL}/api/v1/tool/ping" \
  || {
    echo "broodlink-worker: ping failed worker_id=${WORKER_ID}" >&2
    exit 1
  }

echo "broodlink-worker: ok worker_id=${WORKER_ID} goal_len=${#GOAL}"
exit 0

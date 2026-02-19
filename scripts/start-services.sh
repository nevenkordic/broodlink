#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Start all Broodlink services (native binaries, not containers).
# Logs go to /tmp/broodlink-<service>.log.
# Usage: bash scripts/start-services.sh [--stop]
set -euo pipefail
BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"

SERVICES=(beads-bridge coordinator heartbeat embedding-worker status-api mcp-server a2a-gateway)
CONFIG_PATH="${BROODLINK_CONFIG:-$BROOD_DIR/config.toml}"

stop_services() {
  echo "Stopping Broodlink services..."
  for svc in "${SERVICES[@]}"; do
    pkill -f "target/release/${svc}" 2>/dev/null || true
  done
  echo "All services stopped."
}

if [[ "${1:-}" == "--stop" ]]; then
  stop_services
  exit 0
fi

# Check binaries exist
MISSING=()
for svc in "${SERVICES[@]}"; do
  if [[ ! -f "$BROOD_DIR/target/release/${svc}" ]]; then
    MISSING+=("$svc")
  fi
done

if [[ ${#MISSING[@]} -gt 0 ]]; then
  echo "ERROR: Missing binaries: ${MISSING[*]}"
  echo "Run 'scripts/build.sh' first."
  exit 1
fi

# Stop any running instances
stop_services 2>/dev/null

# Export status API key for mcp-server (reads from env)
if [[ -f "$BROOD_DIR/.secrets/env" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$BROOD_DIR/.secrets/env"
  set +a
fi

echo "Starting Broodlink services..."
for svc in "${SERVICES[@]}"; do
  BROODLINK_CONFIG="$CONFIG_PATH" \
    nohup "$BROOD_DIR/target/release/${svc}" \
    > "/tmp/broodlink-${svc}.log" 2>&1 &
  echo "  Started ${svc} (PID $!)"
done

# Start Hugo dev server if available
if command -v hugo &>/dev/null && [[ -d "$BROOD_DIR/status-site" ]]; then
  if ! curl -sf http://localhost:1313/ > /dev/null 2>&1; then
    nohup hugo server --source "$BROOD_DIR/status-site" \
      --bind 0.0.0.0 --port 1313 --disableLiveReload \
      > /tmp/broodlink-hugo.log 2>&1 &
    echo "  Started Hugo dev server (PID $!)"
  fi
fi

echo ""
echo "All services started. Logs at /tmp/broodlink-*.log"
echo ""

# Health check
sleep 3
echo "Health checks:"
for port_svc in "3310:beads-bridge" "3312:status-api" "3311:mcp-server" "3313:a2a-gateway"; do
  port="${port_svc%%:*}"
  svc="${port_svc##*:}"
  if curl -sf "http://127.0.0.1:${port}/health" > /dev/null 2>&1; then
    echo "  ${svc} (${port}): OK"
  else
    echo "  ${svc} (${port}): not ready (check /tmp/broodlink-${svc}.log)"
  fi
done

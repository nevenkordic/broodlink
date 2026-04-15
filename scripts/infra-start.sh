#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Start/stop all native infrastructure services (no containers).
# Usage: bash scripts/infra-start.sh [--stop|--status]
set -euo pipefail

BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"
export BROOD_DIR
INFRA_DIR="${HOME}/.broodlink/infra"
LOG_DIR="/tmp/broodlink"

mkdir -p "$INFRA_DIR" "$LOG_DIR"

# ── Dolt setup ───────────────────────────────────────────────────
DOLT_DATA="${INFRA_DIR}/dolt-data"
DOLT_PORT=3307
export DOLT_PASSWORD="${BROODLINK_DOLT_PASSWORD:?Set BROODLINK_DOLT_PASSWORD — run scripts/secrets-init.sh}"

# ── Qdrant setup ─────────────────────────────────────────────────
QDRANT_DATA="${INFRA_DIR}/qdrant-storage"
QDRANT_PORT=6333

# ── Jaeger ───────────────────────────────────────────────────────
JAEGER_PORT=16686
export OTLP_PORT=4317

# ── Functions ────────────────────────────────────────────────────

start_postgres() {
  if brew services list | grep -q "postgresql@16.*started"; then
    echo "  PostgreSQL (5432): already running"
  else
    brew services start postgresql@16
    echo "  PostgreSQL (5432): started"
  fi

  # Wait for postgres to be ready, then create broodlink DB if needed
  sleep 2
  local pg_bin="/opt/homebrew/opt/postgresql@16/bin"
  if ! "$pg_bin/psql" -U postgres -lqt 2>/dev/null | cut -d\| -f1 | grep -qw broodlink_hot; then
    "$pg_bin/createdb" -U "$(whoami)" broodlink_hot 2>/dev/null || true
    echo "  PostgreSQL: created broodlink_hot database"
  fi
}

start_nats() {
  if brew services list | grep -q "nats-server.*started"; then
    echo "  NATS (4222): already running"
  else
    brew services start nats-server
    echo "  NATS (4222): started"
  fi
}

start_ollama() {
  if brew services list | grep -q "ollama.*started"; then
    echo "  Ollama (11434): already running"
  else
    brew services start ollama
    echo "  Ollama (11434): started"
  fi
}

start_dolt() {
  if pgrep -f "dolt sql-server" > /dev/null 2>&1; then
    echo "  Dolt (${DOLT_PORT}): already running"
    return
  fi

  mkdir -p "$DOLT_DATA"

  # Initialize Dolt repo if needed
  if [[ ! -d "$DOLT_DATA/.dolt" ]]; then
    (cd "$DOLT_DATA" && dolt init --name broodlink --email "broodlink@local")
    echo "  Dolt: initialized database at ${DOLT_DATA}"
  fi

  # Start Dolt SQL server
  (cd "$DOLT_DATA" && nohup dolt sql-server \
    --host 127.0.0.1 \
    --port "$DOLT_PORT" \
    > "${LOG_DIR}/dolt.log" 2>&1 &)
  echo "  Dolt (${DOLT_PORT}): started (log: ${LOG_DIR}/dolt.log)"

  # Wait for Dolt to be ready, then create database if needed
  sleep 2
  if command -v mysql &>/dev/null; then
    mysql -h 127.0.0.1 -P "$DOLT_PORT" -u root -e "CREATE DATABASE IF NOT EXISTS agent_ledger;" 2>/dev/null || true
  fi
}

start_qdrant() {
  if pgrep -f "qdrant" > /dev/null 2>&1; then
    echo "  Qdrant (${QDRANT_PORT}): already running"
    return
  fi

  if ! command -v qdrant &>/dev/null; then
    echo "  Qdrant: binary not found. Installing..."
    local arch
    arch="$(uname -m)"
    if [[ "$arch" == "arm64" ]]; then
      arch="aarch64"
    fi
    curl -fsSL "https://github.com/qdrant/qdrant/releases/latest/download/qdrant-${arch}-apple-darwin.tar.gz" \
      -o /tmp/qdrant.tar.gz
    tar xzf /tmp/qdrant.tar.gz -C /opt/homebrew/bin/
    rm /tmp/qdrant.tar.gz
    echo "  Qdrant: installed to /opt/homebrew/bin/qdrant"
  fi

  mkdir -p "$QDRANT_DATA"

  nohup qdrant --storage-path "$QDRANT_DATA" \
    > "${LOG_DIR}/qdrant.log" 2>&1 &
  echo "  Qdrant (${QDRANT_PORT}): started (log: ${LOG_DIR}/qdrant.log)"
}

start_jaeger() {
  if pgrep -f "jaeger" > /dev/null 2>&1; then
    echo "  Jaeger (${JAEGER_PORT}): already running"
    return
  fi

  if ! command -v jaeger-all-in-one &>/dev/null && ! command -v jaeger &>/dev/null; then
    echo "  Jaeger: not installed (optional — telemetry will queue locally)"
    return
  fi

  local jaeger_bin
  jaeger_bin="$(command -v jaeger-all-in-one 2>/dev/null || command -v jaeger 2>/dev/null)"

  COLLECTOR_OTLP_ENABLED=true nohup "$jaeger_bin" \
    > "${LOG_DIR}/jaeger.log" 2>&1 &
  echo "  Jaeger (${JAEGER_PORT}): started (log: ${LOG_DIR}/jaeger.log)"
}

stop_all() {
  echo "Stopping Broodlink infrastructure..."
  brew services stop postgresql@16 2>/dev/null || true
  brew services stop nats-server 2>/dev/null || true
  brew services stop ollama 2>/dev/null || true
  pkill -f "dolt sql-server" 2>/dev/null || true
  pkill -f "qdrant" 2>/dev/null || true
  pkill -f "jaeger" 2>/dev/null || true
  echo "All infrastructure stopped."
}

status_all() {
  echo "Broodlink infrastructure status:"
  echo ""

  for name_port in "PostgreSQL:5432" "NATS:4222" "Ollama:11434" "Dolt:${DOLT_PORT}" "Qdrant:${QDRANT_PORT}"; do
    name="${name_port%%:*}"
    port="${name_port##*:}"
    if curl -sf "http://127.0.0.1:${port}/" > /dev/null 2>&1 || \
       nc -z 127.0.0.1 "$port" 2>/dev/null; then
      echo "  ${name} (${port}): UP"
    else
      echo "  ${name} (${port}): DOWN"
    fi
  done

  if curl -sf "http://127.0.0.1:${JAEGER_PORT}/" > /dev/null 2>&1; then
    echo "  Jaeger (${JAEGER_PORT}): UP"
  else
    echo "  Jaeger (${JAEGER_PORT}): not running (optional)"
  fi
}

# ── Main ─────────────────────────────────────────────────────────

case "${1:-start}" in
  start)
    echo "Starting Broodlink infrastructure (native)..."
    echo ""
    start_postgres
    start_nats
    start_ollama
    start_dolt
    start_qdrant
    start_jaeger
    echo ""
    echo "Infrastructure ready. Run 'scripts/start-services.sh' to start Broodlink services."
    ;;
  stop|--stop)
    stop_all
    ;;
  status|--status)
    status_all
    ;;
  *)
    echo "Usage: infra-start.sh [start|stop|status]"
    ;;
esac

#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"

case "${1:-help}" in
  start)
    bash "$BROOD_DIR/scripts/infra-start.sh" start
    sleep 3
    bash "$BROOD_DIR/scripts/start-services.sh"
    if command -v hugo &>/dev/null && [[ -d "$BROOD_DIR/status-site" ]]; then
      hugo server \
        --source "$BROOD_DIR/status-site" \
        --bind 127.0.0.1 \
        --port 1313 &
    fi
    echo "Dev stack running."
    echo "  Dashboard: http://localhost:1313"
    echo "  Status API: http://localhost:3312"
    echo "  NATS monitor: http://localhost:8222"
    ;;
  stop)
    pkill -f "hugo server" || true
    bash "$BROOD_DIR/scripts/start-services.sh" --stop
    bash "$BROOD_DIR/scripts/infra-start.sh" stop
    ;;
  logs)
    tail -f /tmp/broodlink/broodlink-"${2:-"*"}".log 2>/dev/null || \
      tail -f /tmp/broodlink-"${2:-"*"}".log
    ;;
  status)
    bash "$BROOD_DIR/scripts/infra-start.sh" status
    ;;
  build)
    if command -v hugo &>/dev/null; then
      hugo --source "$BROOD_DIR/status-site" --minify
    fi
    ;;
  *)
    echo "Usage: dev.sh start|stop|logs|status|build"
    ;;
esac

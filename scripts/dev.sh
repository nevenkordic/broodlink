#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
case "${1:-help}" in
  start)
    podman-compose -f podman-compose.yaml up -d
    sleep 3
    hugo server \
      --source status-site \
      --bind 127.0.0.1 \
      --port 1313 &
    echo "Dev stack running."
    echo "  Dashboard: http://localhost:1313"
    echo "  Status API: http://localhost:3312"
    echo "  NATS monitor: http://localhost:8222"
    ;;
  stop)
    pkill -f "hugo server" || true
    podman-compose -f podman-compose.yaml down
    ;;
  logs)
    podman-compose -f podman-compose.yaml logs -f "${2:-}"
    ;;
  build)
    hugo --source status-site --minify
    ;;
  *)
    echo "Usage: dev.sh start|stop|logs|build"
    ;;
esac

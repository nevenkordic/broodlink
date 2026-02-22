#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
AGENTS=(beads-bridge coordinator heartbeat
        embedding-worker status-api mcp-server a2a-gateway)
PLIST_DIR="$HOME/Library/LaunchAgents"
BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR="$HOME/logs/broodlink"
HOMEBREW_PREFIX="$(brew --prefix 2>/dev/null || echo /opt/homebrew)"

case "${1:-help}" in
  install)
    mkdir -p "$LOG_DIR"
    mkdir -p "$PLIST_DIR"
    for agent in "${AGENTS[@]}"; do
      sed -e "s|__BROOD_DIR__|$BROOD_DIR|g" \
          -e "s|__LOG_DIR__|$LOG_DIR|g" \
          -e "s|__HOMEBREW_PREFIX__|$HOMEBREW_PREFIX|g" \
          "$BROOD_DIR/launchagents/com.broodlink.$agent.plist" \
          > "$PLIST_DIR/com.broodlink.$agent.plist"
      launchctl bootstrap \
        "gui/$(id -u)" \
        "$PLIST_DIR/com.broodlink.$agent.plist"
    done
    echo "LaunchAgents installed."
    ;;
  uninstall)
    for agent in "${AGENTS[@]}"; do
      launchctl bootout \
        "gui/$(id -u)" \
        "$PLIST_DIR/com.broodlink.$agent.plist" \
        2>/dev/null || true
      rm -f \
        "$PLIST_DIR/com.broodlink.$agent.plist"
    done
    echo "LaunchAgents removed."
    ;;
  status)
    for agent in "${AGENTS[@]}"; do
      echo "--- com.broodlink.$agent ---"
      launchctl print \
        "gui/$(id -u)/com.broodlink.$agent" \
        2>/dev/null || echo "not loaded"
    done
    ;;
  restart)
    TARGET="${2:-all}"
    if [ "$TARGET" = "all" ]; then
      TARGETS=("${AGENTS[@]}")
    else
      TARGETS=("$TARGET")
    fi
    for agent in "${TARGETS[@]}"; do
      launchctl bootout \
        "gui/$(id -u)" \
        "$PLIST_DIR/com.broodlink.$agent.plist" \
        2>/dev/null || true
      sed -e "s|__BROOD_DIR__|$BROOD_DIR|g" \
          -e "s|__LOG_DIR__|$LOG_DIR|g" \
          -e "s|__HOMEBREW_PREFIX__|$HOMEBREW_PREFIX|g" \
          "$BROOD_DIR/launchagents/com.broodlink.$agent.plist" \
          > "$PLIST_DIR/com.broodlink.$agent.plist"
      launchctl bootstrap \
        "gui/$(id -u)" \
        "$PLIST_DIR/com.broodlink.$agent.plist"
      echo "Restarted: $agent"
    done
    ;;
  *)
    echo "Usage: launchagents.sh install|uninstall|status|restart [service|all]"
    ;;
esac

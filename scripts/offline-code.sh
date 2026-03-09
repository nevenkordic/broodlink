#!/usr/bin/env bash
# Broodlink Offline Coding Launcher
# One command to start everything needed for offline AI-assisted development
# Usage: ./scripts/offline-code.sh [aider|continue|chat]
set -euo pipefail

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BOLD='\033[1m'
NC='\033[0m'

BROOD_DIR="$HOME/broodlink"

# Load env vars (NAS config, API keys) — .env is gitignored
[ -f "$BROOD_DIR/.env" ] && set -a && source "$BROOD_DIR/.env" && set +a

log()  { echo -e "${GREEN}[offline]${NC} $1"; }
warn() { echo -e "${YELLOW}[warn]${NC} $1"; }
err()  { echo -e "${RED}[error]${NC} $1"; }

# ── Check & start Ollama ──────────────────────────────────────
start_ollama() {
    if curl -s --max-time 2 http://localhost:11434/api/tags >/dev/null 2>&1; then
        log "Ollama is running"
    else
        log "Starting Ollama..."
        open -a Ollama 2>/dev/null || ollama serve &>/dev/null &
        for i in {1..30}; do
            curl -s --max-time 1 http://localhost:11434/api/tags >/dev/null 2>&1 && break
            sleep 1
        done
        curl -s --max-time 2 http://localhost:11434/api/tags >/dev/null 2>&1 \
            || { err "Ollama failed to start. Install from https://ollama.com"; exit 1; }
        log "Ollama started"
    fi
}

# ── Verify models exist ──────────────────────────────────────
check_models() {
    local models
    models=$(ollama list 2>/dev/null | tail -n +2 | awk '{print $1}')
    local required=("qwen3-coder:30b" "qwen3.5:4b")
    local missing=()

    for m in "${required[@]}"; do
        echo "$models" | grep -q "^$m$" || missing+=("$m")
    done

    if [ ${#missing[@]} -gt 0 ]; then
        warn "Missing models: ${missing[*]}"
        warn "Run: ./scripts/ai-backup.sh --restore  (to restore from NAS)"
        warn "Or:  ollama pull ${missing[0]}"
        return 1
    fi

    local count
    count=$(echo "$models" | wc -l | tr -d ' ')
    log "$count models available"
}

# ── Show model menu ───────────────────────────────────────────
show_status() {
    echo ""
    echo -e "${BOLD}╔══════════════════════════════════════════════╗${NC}"
    echo -e "${BOLD}║       BROODLINK OFFLINE CODING READY         ║${NC}"
    echo -e "${BOLD}╠══════════════════════════════════════════════╣${NC}"
    echo -e "${BOLD}║${NC} Models:                                      ${BOLD}║${NC}"
    ollama list 2>/dev/null | tail -n +2 | while read -r name id size rest; do
        printf "${BOLD}║${NC}   %-30s %8s   ${BOLD}║${NC}\n" "$name" "$size"
    done
    echo -e "${BOLD}╠══════════════════════════════════════════════╣${NC}"
    echo -e "${BOLD}║${NC} Tools:                                       ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   aider     — CLI coding agent (git-aware)   ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   continue  — VS Code sidebar (Cmd+L)        ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   chat      — Quick terminal chat             ${BOLD}║${NC}"
    echo -e "${BOLD}╠══════════════════════════════════════════════╣${NC}"
    echo -e "${BOLD}║${NC} Usage:                                       ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   offline-code aider    — start aider         ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   offline-code continue — open VS Code        ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   offline-code chat     — quick terminal chat ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   offline-code backup   — backup to NAS       ${BOLD}║${NC}"
    echo -e "${BOLD}║${NC}   offline-code restore  — restore from NAS    ${BOLD}║${NC}"
    echo -e "${BOLD}╚══════════════════════════════════════════════╝${NC}"
    echo ""
}

# ── Launch modes ──────────────────────────────────────────────
launch_aider() {
    log "Launching Aider with qwen3-coder:30b..."
    cd "$BROOD_DIR"
    export PATH="$HOME/.local/bin:$PATH"
    exec aider
}

launch_continue() {
    log "Opening VS Code with Continue.dev..."
    if command -v code &>/dev/null; then
        code "$BROOD_DIR"
    elif command -v cursor &>/dev/null; then
        cursor "$BROOD_DIR"
    elif [ -d "/Applications/Cursor.app" ]; then
        open -a "Cursor" "$BROOD_DIR"
    elif [ -d "/Applications/Visual Studio Code.app" ]; then
        open -a "Visual Studio Code" "$BROOD_DIR"
    else
        err "No VS Code or Cursor found. Open your editor manually."
        exit 1
    fi
    log "Use Cmd+L to open Continue chat panel"
}

launch_chat() {
    local model="${2:-qwen3.5:4b}"
    log "Terminal chat with $model (type /bye to exit)"
    exec ollama run "$model"
}

# ── Main ──────────────────────────────────────────────────────
start_ollama
check_models || true
show_status

case "${1:-}" in
    aider|a)     launch_aider ;;
    continue|c)  launch_continue ;;
    chat|q)      launch_chat "$@" ;;
    backup|b)    exec "$BROOD_DIR/scripts/ai-backup.sh" ;;
    restore|r)   exec "$BROOD_DIR/scripts/ai-backup.sh" --restore ;;
    "")          log "Pick a mode above, or just run: offline-code aider" ;;
    *)           err "Unknown mode: $1. Use: aider, continue, chat, backup, restore" ;;
esac

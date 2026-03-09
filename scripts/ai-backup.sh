#!/usr/bin/env bash
# Broodlink AI Backup — syncs all AI models and configs to NAS
# Usage: ./scripts/ai-backup.sh [--restore]
set -euo pipefail

# Configure these for your environment, or set via env vars:
#   BACKUP_NAS_SHARE=smb://your-nas/share  BACKUP_MOUNT=/Volumes/YourMount  ./scripts/ai-backup.sh
NAS_SHARE="${BACKUP_NAS_SHARE:?Set BACKUP_NAS_SHARE in .env (e.g. smb://your-nas/share)}"
MOUNT_POINT="${BACKUP_MOUNT:?Set BACKUP_MOUNT in .env (e.g. /Volumes/YourMount)}"
BACKUP_DIR="$MOUNT_POINT/broodlink-offline"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[backup]${NC} $1"; }
warn() { echo -e "${YELLOW}[warn]${NC} $1"; }
err()  { echo -e "${RED}[error]${NC} $1"; exit 1; }

# Ensure NAS is mounted
ensure_mount() {
    if mount | grep -q "$MOUNT_POINT"; then
        log "NAS already mounted at $MOUNT_POINT"
    else
        log "Mounting $NAS_SHARE..."
        open "$NAS_SHARE" 2>/dev/null
        sleep 3
        mount | grep -q "$MOUNT_POINT" || err "Failed to mount $NAS_SHARE — open Finder and connect manually"
    fi
}

do_backup() {
    ensure_mount
    mkdir -p "$BACKUP_DIR"/{ollama-models,continue-config,aider-config,beads-formulas}

    log "Backing up Ollama models (~100 GB, this may take a while)..."
    rsync -ah --progress --delete \
        "$HOME/.ollama/models/" \
        "$BACKUP_DIR/ollama-models/"

    log "Backing up Continue.dev config..."
    rsync -ah --progress \
        "$HOME/.continue/config.json" \
        "$BACKUP_DIR/continue-config/"

    log "Backing up Aider config..."
    rsync -ah --progress \
        "$HOME/broodlink/.aider.conf.yml" \
        "$BACKUP_DIR/aider-config/"

    log "Backing up Beads formulas..."
    rsync -ah --progress \
        "$HOME/broodlink/.beads/formulas/" \
        "$BACKUP_DIR/beads-formulas/"

    # Save model inventory for quick reference
    ollama list > "$BACKUP_DIR/model-inventory.txt" 2>/dev/null || true
    date '+%Y-%m-%d %H:%M:%S' > "$BACKUP_DIR/last-backup.txt"

    log "Backup complete! Inventory:"
    cat "$BACKUP_DIR/model-inventory.txt"
    echo ""
    log "Last backup: $(cat "$BACKUP_DIR/last-backup.txt")"
    du -sh "$BACKUP_DIR"
}

do_restore() {
    ensure_mount
    [ -d "$BACKUP_DIR" ] || err "No backup found at $BACKUP_DIR"

    log "Restoring Ollama models..."
    mkdir -p "$HOME/.ollama/models"
    rsync -ah --progress \
        "$BACKUP_DIR/ollama-models/" \
        "$HOME/.ollama/models/"

    log "Restoring Continue.dev config..."
    mkdir -p "$HOME/.continue"
    rsync -ah --progress \
        "$BACKUP_DIR/continue-config/config.json" \
        "$HOME/.continue/config.json"

    log "Restoring Aider config..."
    rsync -ah --progress \
        "$BACKUP_DIR/aider-config/.aider.conf.yml" \
        "$HOME/broodlink/.aider.conf.yml"

    log "Restoring Beads formulas..."
    mkdir -p "$HOME/broodlink/.beads/formulas"
    rsync -ah --progress \
        "$BACKUP_DIR/beads-formulas/" \
        "$HOME/broodlink/.beads/formulas/"

    log "Restore complete! Restart Ollama to pick up models."
}

case "${1:-backup}" in
    --restore|-r|restore) do_restore ;;
    *) do_backup ;;
esac

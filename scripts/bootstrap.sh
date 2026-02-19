#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# One-shot bootstrap: takes a fresh clone to a running system.
# Usage: bash scripts/bootstrap.sh
set -euo pipefail
BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"

echo "=========================================="
echo "  Broodlink Bootstrap"
echo "=========================================="
echo ""

# --------------------------------------------------------------------------
# 0. Check prerequisites
# --------------------------------------------------------------------------
echo "--- Checking prerequisites ---"
MISSING=()
for cmd in rustc cargo hugo age sops openssl podman podman-compose psql mysql dolt curl python3; do
  if ! command -v "$cmd" &>/dev/null; then
    MISSING+=("$cmd")
  fi
done

if [[ ${#MISSING[@]} -gt 0 ]]; then
  echo ""
  echo "ERROR: Missing required tools: ${MISSING[*]}"
  echo ""
  echo "Install them first:"
  echo "  brew install rustup hugo age sops openssl podman dolt curl python3"
  echo "  brew install libpq       # for psql"
  echo "  brew install mysql-client # for mysql CLI"
  echo "  cargo install cargo-deny"
  echo ""
  exit 1
fi

echo "  All prerequisites found."
echo ""

# --------------------------------------------------------------------------
# 1. Start infrastructure
# --------------------------------------------------------------------------
echo "--- Starting infrastructure (podman-compose) ---"
cd "$BROOD_DIR"
podman-compose up -d dolt postgres nats qdrant ollama 2>/dev/null || {
  echo "WARNING: podman-compose failed. Make sure podman machine is running."
  echo "  podman machine start"
  exit 1
}
echo "  Waiting for services to be ready..."
sleep 10

# --------------------------------------------------------------------------
# 2. Pull Ollama embedding model
# --------------------------------------------------------------------------
echo "--- Pulling Ollama embedding model ---"
podman exec -it broodlink-ollama ollama pull nomic-embed-text 2>/dev/null || \
  curl -sf http://localhost:11434/api/pull -d '{"name":"nomic-embed-text"}' > /dev/null 2>&1 || \
  echo "  WARNING: Could not pull nomic-embed-text (Ollama may not be ready yet)"
echo ""

# --------------------------------------------------------------------------
# 3. Secrets initialization
# --------------------------------------------------------------------------
echo "--- Initializing secrets ---"
bash "$BROOD_DIR/scripts/secrets-init.sh"
echo ""

# If secrets.skeleton.json was created, auto-encrypt it for dev
if [[ -f "$BROOD_DIR/secrets.skeleton.json" ]] && ! [[ -f "$BROOD_DIR/secrets.enc.json" ]]; then
  echo "  Auto-encrypting secrets.skeleton.json for local dev..."
  sops --encrypt "$BROOD_DIR/secrets.skeleton.json" > "$BROOD_DIR/secrets.enc.json"
  rm -f "$BROOD_DIR/secrets.skeleton.json"
  # Regenerate .secrets/env with real encrypted values
  bash "$BROOD_DIR/scripts/secrets-init.sh" 2>/dev/null || true
fi

# --------------------------------------------------------------------------
# 4. Database setup
# --------------------------------------------------------------------------
echo "--- Setting up databases ---"
bash "$BROOD_DIR/scripts/db-setup.sh"
echo ""

# --------------------------------------------------------------------------
# 5. Build
# --------------------------------------------------------------------------
echo "--- Building Rust services ---"
bash "$BROOD_DIR/scripts/build.sh"
echo ""

# --------------------------------------------------------------------------
# 6. Onboard default agent
# --------------------------------------------------------------------------
echo "--- Onboarding default agent ---"
bash "$BROOD_DIR/scripts/onboard-agent.sh" claude \
  --role strategist --display-name "Claude" --cost-tier high 2>/dev/null || true
echo ""

# --------------------------------------------------------------------------
# 7. Start services
# --------------------------------------------------------------------------
echo "--- Starting Broodlink services ---"
bash "$BROOD_DIR/scripts/start-services.sh"
echo ""

echo "=========================================="
echo "  Bootstrap complete!"
echo "=========================================="
echo ""
echo "  Dashboard:     http://localhost:1313"
echo "  beads-bridge:  http://localhost:3310/health"
echo "  status-api:    http://localhost:3312/api/v1/health"
echo "  MCP server:    http://localhost:3311/health"
echo "  A2A gateway:   http://localhost:3313/health"
echo ""
echo "  Run tests:     bash tests/run-all.sh"

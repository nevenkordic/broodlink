#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

echo "→ Checking licenses..."
cargo deny check

echo "→ Running tests..."
cargo test --workspace

echo "→ Building release binaries..."
cargo build --release --workspace

echo "→ Copying binaries..."
mkdir -p bin
for svc in beads-bridge coordinator heartbeat \
           embedding-worker status-api; do
  cp "target/release/$svc" "bin/$svc"
  echo "  ✓ bin/$svc"
done

echo "→ Building Hugo site..."
hugo --source status-site --minify

echo "Build complete."

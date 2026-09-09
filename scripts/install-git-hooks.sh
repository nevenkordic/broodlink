#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Point this clone at the tracked hooks in .githooks/ so every commit
# runs the secret-leak audit before it can leave the machine.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ ! -d .git ]]; then
  echo "Not a git repository." >&2
  exit 1
fi

if [[ ! -f .githooks/pre-commit ]]; then
  echo "Missing .githooks/pre-commit" >&2
  exit 1
fi

chmod +x .githooks/pre-commit
git config core.hooksPath .githooks
echo "Installed git hooks (core.hooksPath=.githooks)"
echo "Pre-commit will run tests/security-audit.sh"

#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Create or update an admin user for the dashboard.
# Usage: bash scripts/create-admin.sh --username admin --password <password>
# Idempotent — safe to run multiple times.
set -euo pipefail

BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SECRETS_FILE="$BROOD_DIR/secrets.enc.json"

# Defaults
USERNAME=""
PASSWORD=""
ROLE="admin"
DISPLAY_NAME=""
BCRYPT_COST=12

usage() {
  echo "Usage: $0 --username <name> --password <pw> [--role <role>] [--display-name <name>] [--cost <N>]"
  echo ""
  echo "  --username      Dashboard username (required)"
  echo "  --password      Password (required)"
  echo "  --role          User role: viewer, operator, admin (default: admin)"
  echo "  --display-name  Human-readable display name"
  echo "  --cost          Bcrypt cost factor (default: 12)"
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --username)   USERNAME="$2"; shift 2 ;;
    --password)   PASSWORD="$2"; shift 2 ;;
    --role)       ROLE="$2"; shift 2 ;;
    --display-name) DISPLAY_NAME="$2"; shift 2 ;;
    --cost)       BCRYPT_COST="$2"; shift 2 ;;
    -h|--help)    usage ;;
    *)            echo "Unknown option: $1"; usage ;;
  esac
done

if [[ -z "$USERNAME" || -z "$PASSWORD" ]]; then
  echo "Error: --username and --password are required."
  usage
fi

# Validate role
if [[ "$ROLE" != "viewer" && "$ROLE" != "operator" && "$ROLE" != "admin" ]]; then
  echo "Error: invalid role '$ROLE'. Must be viewer, operator, or admin."
  exit 1
fi

# Helper: decrypt a secret or fall back to env var / default
decrypt_or_default() {
  local key="$1"
  local default="${2:-}"
  if [[ -f "$SECRETS_FILE" ]] && command -v sops &>/dev/null; then
    sops --decrypt --extract "[\"$key\"]" "$SECRETS_FILE" 2>/dev/null || echo "$default"
  else
    echo "${!key:-$default}"
  fi
}

# Helper: run SQL against Postgres (tries local psql, then podman)
run_sql() {
  local sql="$1"
  local flags="${2:--q}"
  if command -v psql &>/dev/null; then
    PGPASSWORD=$PGPASSWORD psql -h 127.0.0.1 -U postgres -d broodlink_hot $flags -c "$sql" 2>/dev/null
  else
    podman exec -i broodlink-postgres psql -U postgres -d broodlink_hot $flags -c "$sql" 2>/dev/null
  fi
}

PGPASSWORD=$(decrypt_or_default "BROODLINK_POSTGRES_PASSWORD" "changeme")

# Ensure pgcrypto extension exists (needed for Postgres-based bcrypt fallback)
run_sql "CREATE EXTENSION IF NOT EXISTS pgcrypto;" "-q" 2>/dev/null || true

# Generate bcrypt hash — try Python bcrypt, then passlib, then Postgres pgcrypto
HASH=""

# Method 1: Python bcrypt
if [[ -z "$HASH" ]]; then
  HASH=$(python3 -c "
import bcrypt, sys
pw = sys.argv[1].encode('utf-8')
cost = int(sys.argv[2])
h = bcrypt.hashpw(pw, bcrypt.gensalt(rounds=cost))
print(h.decode('utf-8'))
" "$PASSWORD" "$BCRYPT_COST" 2>/dev/null) || true
fi

# Method 2: Python passlib
if [[ -z "$HASH" ]]; then
  HASH=$(python3 -c "
from passlib.hash import bcrypt
import sys
print(bcrypt.using(rounds=int(sys.argv[2])).hash(sys.argv[1]))
" "$PASSWORD" "$BCRYPT_COST" 2>/dev/null) || true
fi

# Method 3: Postgres pgcrypto (no Python dependency needed)
if [[ -z "$HASH" ]]; then
  SAFE_PW="${PASSWORD//\'/\'\'}"
  HASH=$(run_sql "SELECT crypt('$SAFE_PW', gen_salt('bf', $BCRYPT_COST));" "-tA") || true
fi

if [[ -z "$HASH" ]]; then
  echo "Error: Could not generate bcrypt hash."
  echo "Install Python bcrypt (pip install bcrypt) or ensure Postgres pgcrypto extension is available."
  exit 1
fi

USER_ID=$(python3 -c "import uuid; print(str(uuid.uuid4()))")

echo "Creating dashboard user: $USERNAME (role=$ROLE)"

SAFE_USER="${USERNAME//\'/\'\'}"
SAFE_HASH="${HASH//\'/\'\'}"
SAFE_ROLE="${ROLE//\'/\'\'}"
SAFE_DISPLAY="${DISPLAY_NAME//\'/\'\'}"
DISPLAY_SQL=$([ -n "$DISPLAY_NAME" ] && echo "'$SAFE_DISPLAY'" || echo "NULL")
run_sql "INSERT INTO dashboard_users (id, username, password_hash, role, display_name)
    VALUES ('$USER_ID', '$SAFE_USER', '$SAFE_HASH', '$SAFE_ROLE', $DISPLAY_SQL)
    ON CONFLICT (username) DO NOTHING;" "-q"

echo "Done. User '$USERNAME' is ready (ON CONFLICT = no-op if exists)."

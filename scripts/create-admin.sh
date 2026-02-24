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

if [[ -z "$USERNAME" ]]; then
  echo "Error: --username is required."
  usage
fi

# Prompt for password interactively if not provided via CLI
if [[ -z "$PASSWORD" ]]; then
  if [[ -t 0 ]]; then
    read -rs -p "Password: " PASSWORD
    echo ""
    read -rs -p "Confirm password: " PASSWORD_CONFIRM
    echo ""
    if [[ "$PASSWORD" != "$PASSWORD_CONFIRM" ]]; then
      echo "Error: passwords do not match." >&2
      exit 1
    fi
  else
    echo "Error: --password is required (or run interactively to be prompted)."
    usage
  fi
fi

# Validate password strength
if [[ ${#PASSWORD} -lt 12 ]]; then
  echo "Error: password must be at least 12 characters." >&2
  exit 1
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
# Usage: run_sql "SQL" [flags] [-v var=val ...]
run_sql() {
  local sql="$1"; shift
  local flags="${1:--q}"; shift || true
  if command -v psql &>/dev/null; then
    # shellcheck disable=SC2086
    PGPASSWORD=$PGPASSWORD psql -h 127.0.0.1 -U postgres -d broodlink_hot $flags "$@" -c "$sql" 2>/dev/null
  else
    # podman exec doesn't pass psql -v bindings correctly,
    # so convert -v key=value args to \set statements piped via stdin.
    local set_cmds=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        -v)
          local kv="$2"
          local key="${kv%%=*}"
          local val="${kv#*=}"
          # Escape single quotes for psql \set
          val="${val//\'/\'\'}"
          set_cmds="${set_cmds}\\set ${key} '${val}'
"
          shift 2
          ;;
        *) shift ;;
      esac
    done
    # shellcheck disable=SC2086
    echo "${set_cmds}${sql}" | podman exec -i broodlink-postgres psql -U postgres -d broodlink_hot $flags 2>/dev/null
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
# Note: psql -v variable binding doesn't work through podman exec,
# so we pipe SQL with \set to bind the password safely via stdin.
if [[ -z "$HASH" ]]; then
  # Validate BCRYPT_COST is a safe integer (prevent injection)
  if ! [[ "$BCRYPT_COST" =~ ^[0-9]+$ ]]; then
    echo "Error: invalid bcrypt cost '$BCRYPT_COST'" >&2
    exit 1
  fi
  # Escape single quotes for psql \set ('' is the escape sequence)
  PW_SET_ESCAPED="${PASSWORD//\'/\'\'}"
  PG_SQL="\\set pw '${PW_SET_ESCAPED}'
SELECT crypt(:'pw', gen_salt('bf', ${BCRYPT_COST}));"
  if command -v psql &>/dev/null; then
    HASH=$(echo "$PG_SQL" | PGPASSWORD=$PGPASSWORD psql -h 127.0.0.1 -U postgres -d broodlink_hot -tA 2>/dev/null) || true
  else
    HASH=$(echo "$PG_SQL" | podman exec -i broodlink-postgres psql -U postgres -d broodlink_hot -tA 2>/dev/null) || true
  fi
fi

if [[ -z "$HASH" ]]; then
  echo "Error: Could not generate bcrypt hash."
  echo "Install Python bcrypt (pip install bcrypt) or ensure Postgres pgcrypto extension is available."
  exit 1
fi

USER_ID=$(python3 -c "import uuid; print(str(uuid.uuid4()))")

echo "Creating dashboard user: $USERNAME (role=$ROLE)"

DISPLAY_VAL="${DISPLAY_NAME:-}"
run_sql "INSERT INTO dashboard_users (id, username, password_hash, role, display_name)
    VALUES (:'uid', :'uname', :'phash', :'urole', NULLIF(:'dname', ''))
    ON CONFLICT (username) DO NOTHING;" "-q" \
    -v "uid=$USER_ID" -v "uname=$USERNAME" -v "phash=$HASH" -v "urole=$ROLE" -v "dname=$DISPLAY_VAL"

echo "Done. User '$USERNAME' is ready (ON CONFLICT = no-op if exists)."

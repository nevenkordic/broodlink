#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Seed the formula_registry table from .beads/formulas/*.formula.toml files.
# Idempotent: ON CONFLICT (name) DO NOTHING.
# Run: bash scripts/seed-formulas.sh

set -euo pipefail

BROOD_DIR="${BROOD_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"
FORMULA_DIR="$BROOD_DIR/.beads/formulas"
PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PG_PORT:-5432}"
PG_DB="${PG_DB:-broodlink_hot}"
PG_USER="${PG_USER:-postgres}"
PGPASSWORD="${PGPASSWORD:-${BROODLINK_POSTGRES_PASSWORD:-postgres}}"
export PGPASSWORD

if ! command -v python3 &>/dev/null; then
  echo "ERROR: python3 is required for TOML → JSON conversion"
  exit 1
fi

echo "=== Seeding Formula Registry ==="
echo "Source: $FORMULA_DIR"
echo ""

COUNT=0

for toml_file in "$FORMULA_DIR"/*.formula.toml; do
  [ -f "$toml_file" ] || continue
  filename=$(basename "$toml_file")
  echo "  Processing: $filename"

  # Convert TOML to JSONB definition using Python
  definition=$(python3 -c "
import tomllib, json, sys

with open('$toml_file', 'rb') as f:
    data = tomllib.load(f)

formula = data.get('formula', {})
steps = data.get('steps', [])

# Convert parameters from {name: {type, required, ...}} to [{name, type, required, ...}]
raw_params = formula.pop('parameters', {})
parameters = []
for pname, pval in raw_params.items():
    p = {'name': pname}
    if isinstance(pval, dict):
        p.update(pval)
    else:
        p['type'] = 'string'
    parameters.append(p)

# Build the definition JSONB
defn = {
    'formula': {
        'name': formula.get('name', ''),
        'description': formula.get('description', ''),
        'version': formula.get('version', '1'),
    },
    'parameters': parameters,
    'steps': [],
    'on_failure': data.get('on_failure', None),
}

for step in steps:
    defn['steps'].append({
        'name': step.get('name', ''),
        'agent_role': step.get('agent_role', ''),
        'tools': step.get('tools', []),
        'prompt': step.get('prompt', ''),
        'output': step.get('output', None),
        'input': step.get('input', None),
        'when': step.get('when', None),
        'retries': step.get('retries', 0),
        'timeout_seconds': step.get('timeout_seconds', None),
        'group': step.get('group', None),
    })

print(json.dumps(defn))
" 2>&1) || {
    echo "    ERROR: Failed to parse $filename"
    continue
  }

  # Extract metadata
  name=$(echo "$definition" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['formula']['name'])")
  description=$(echo "$definition" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['formula']['description'])")
  display_name=$(echo "$name" | sed 's/-/ /g' | python3 -c "import sys; print(sys.stdin.read().strip().title())")
  id=$(python3 -c "import uuid; print(str(uuid.uuid4()))")

  # Use parameterized queries to prevent SQL injection
  sql="INSERT INTO formula_registry (id, name, display_name, description, definition, is_system, author)
VALUES (:'fid', :'fname', :'fdisplay', :'fdesc', :'fdef'::jsonb, true, 'system')
ON CONFLICT (name) DO NOTHING;"

  psql_vars=(-v "fid=$id" -v "fname=$name" -v "fdisplay=$display_name" -v "fdesc=$description" -v "fdef=$definition")

  if podman exec -i broodlink-postgres psql -U "$PG_USER" -d "$PG_DB" -c "$sql" -q "${psql_vars[@]}" 2>/dev/null; then
    echo "    Seeded: $name"
    COUNT=$((COUNT + 1))
  else
    # Try direct psql if podman fails
    if psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$PG_DB" -c "$sql" -q "${psql_vars[@]}" 2>/dev/null; then
      echo "    Seeded: $name"
      COUNT=$((COUNT + 1))
    else
      echo "    SKIP: $name (already exists or error)"
    fi
  fi
done

echo ""
echo "=== Seeded $COUNT formulas ==="

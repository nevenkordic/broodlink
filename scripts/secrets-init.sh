#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Initialize secrets: age keypair, JWT RSA keypair, SOPS skeleton, and .secrets/env.
# Run once on first setup, before db-setup.sh.
set -euo pipefail
BROOD_DIR="$(cd "$(dirname "$0")/.." && pwd)"

echo "=== Broodlink Secrets Initialization ==="
echo ""

# --------------------------------------------------------------------------
# 1. Age keypair (for SOPS encryption)
# --------------------------------------------------------------------------
mkdir -p ~/.broodlink
if [[ -f ~/.broodlink/age-identity ]]; then
  echo "Age identity already exists at ~/.broodlink/age-identity — skipping."
  PUB=$(grep "public key" ~/.broodlink/age-identity | awk '{print $NF}')
else
  age-keygen -o ~/.broodlink/age-identity
  PUB=$(grep "public key" ~/.broodlink/age-identity | awk '{print $NF}')
  echo "Age keypair created."
fi
echo "  Public key: $PUB"
echo ""

# --------------------------------------------------------------------------
# 2. SOPS config (references secrets.enc.json to match config.toml)
# --------------------------------------------------------------------------
cat > "$BROOD_DIR/.sops.yaml" <<EOF
creation_rules:
  - path_regex: secrets\.enc\.json$
    age: $PUB
EOF
echo "Created .sops.yaml (encrypts secrets.enc.json)"

# --------------------------------------------------------------------------
# 3. RSA keypair for JWT signing — persisted to ~/.broodlink/
# --------------------------------------------------------------------------
if [[ -f ~/.broodlink/jwt-private.pem ]]; then
  echo "JWT keypair already exists at ~/.broodlink/ — skipping."
  JWT_PUB=$(cat ~/.broodlink/jwt-public.pem)
else
  openssl genrsa -out ~/.broodlink/jwt-private.pem 4096 2>/dev/null
  openssl rsa -in ~/.broodlink/jwt-private.pem -pubout -out ~/.broodlink/jwt-public.pem 2>/dev/null
  chmod 600 ~/.broodlink/jwt-private.pem
  chmod 644 ~/.broodlink/jwt-public.pem
  JWT_PUB=$(cat ~/.broodlink/jwt-public.pem)
  echo "JWT RSA keypair created at ~/.broodlink/jwt-{private,public}.pem"
fi
echo ""

# --------------------------------------------------------------------------
# 4. Secrets skeleton (JSON format to match config.toml sops_file reference)
# --------------------------------------------------------------------------
if [[ -f "$BROOD_DIR/secrets.enc.json" ]]; then
  echo "secrets.enc.json already exists — skipping skeleton."
else
  cat > "$BROOD_DIR/secrets.skeleton.json" <<SKEL
{
  "BROODLINK_DOLT_PASSWORD": "",
  "BROODLINK_POSTGRES_PASSWORD": "changeme",
  "BROODLINK_QDRANT_API_KEY": "",
  "BROODLINK_STATUS_API_KEY": "dev-api-key",
  "BROODLINK_JWT_PUBLIC_KEY": $(printf '%s' "$JWT_PUB" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read()))')
}
SKEL
  echo "Created secrets.skeleton.json"
  echo ""
  echo "  Edit secrets.skeleton.json if needed, then encrypt:"
  echo "    sops --encrypt secrets.skeleton.json > secrets.enc.json"
  echo "    rm secrets.skeleton.json"
fi
echo ""

# --------------------------------------------------------------------------
# 5. Generate .secrets/env for podman-compose
# --------------------------------------------------------------------------
mkdir -p "$BROOD_DIR/.secrets"

if [[ -f "$BROOD_DIR/secrets.enc.json" ]]; then
  # Extract from encrypted file
  echo "Generating .secrets/env from secrets.enc.json..."
  DOLT_PW=$(sops --decrypt --extract '["BROODLINK_DOLT_PASSWORD"]' "$BROOD_DIR/secrets.enc.json" 2>/dev/null || echo "")
  PG_PW=$(sops --decrypt --extract '["BROODLINK_POSTGRES_PASSWORD"]' "$BROOD_DIR/secrets.enc.json" 2>/dev/null || echo "changeme")
  API_KEY=$(sops --decrypt --extract '["BROODLINK_STATUS_API_KEY"]' "$BROOD_DIR/secrets.enc.json" 2>/dev/null || echo "dev-api-key")
else
  echo "No secrets.enc.json found — generating .secrets/env with dev defaults."
  DOLT_PW=""
  PG_PW="changeme"
  API_KEY="dev-api-key"
fi

cat > "$BROOD_DIR/.secrets/env" <<EOF
BROODLINK_CONFIG=/app/config.toml
BROODLINK_DOLT_PASSWORD=$DOLT_PW
BROODLINK_POSTGRES_PASSWORD=$PG_PW
BROODLINK_STATUS_API_KEY=$API_KEY
BROODLINK_QDRANT_API_KEY=
EOF
chmod 600 "$BROOD_DIR/.secrets/env"
echo "Created .secrets/env (used by podman-compose)"
echo ""

echo "=== Secrets initialization complete ==="
echo ""
echo "Next steps:"
echo "  1. scripts/db-setup.sh     # Create databases and run migrations"
echo "  2. scripts/build.sh        # Build all Rust services"
echo "  3. scripts/onboard-agent.sh <agent_id>  # Create JWT for an agent"

#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Onboard a new agent into Broodlink.
#
# Usage:
#   ./scripts/onboard-agent.sh <agent_id> [options]
#
# Options:
#   --role <role>           Agent role (default: worker)
#   --display-name <name>   Human-readable name (default: agent_id)
#   --cost-tier <tier>      low, medium, high (default: medium)
#   --bridge-url <url>      Bridge URL (default: http://localhost:3310)
#
# Prerequisites:
#   - ~/.broodlink/jwt-private.pem must exist (run scripts/secrets-init.sh first)
#   - beads-bridge must be running at the bridge URL
#
# Output:
#   Prints env vars to source: BROODLINK_AGENT_ID, BROODLINK_AGENT_JWT, BROODLINK_BRIDGE_URL

set -euo pipefail

BROOD_DIR="${HOME}/.broodlink"
PRIVATE_KEY="${BROOD_DIR}/jwt-private.pem"

# Defaults
ROLE="worker"
DISPLAY_NAME=""
COST_TIER="medium"
BRIDGE_URL="http://localhost:3310"

# Parse args
AGENT_ID="${1:-}"
shift || true

while [[ $# -gt 0 ]]; do
  case "$1" in
    --role)        ROLE="$2";         shift 2 ;;
    --display-name) DISPLAY_NAME="$2"; shift 2 ;;
    --cost-tier)   COST_TIER="$2";    shift 2 ;;
    --bridge-url)  BRIDGE_URL="$2";   shift 2 ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

if [[ -z "$AGENT_ID" ]]; then
  echo "Usage: $0 <agent_id> [--role worker] [--display-name Name] [--cost-tier medium]" >&2
  exit 1
fi

DISPLAY_NAME="${DISPLAY_NAME:-$AGENT_ID}"

# Check prerequisites
if [[ ! -f "$PRIVATE_KEY" ]]; then
  echo "ERROR: $PRIVATE_KEY not found." >&2
  echo "Run scripts/secrets-init.sh first to generate the RSA keypair." >&2
  exit 1
fi

if ! command -v openssl &>/dev/null; then
  echo "ERROR: openssl is required but not found." >&2
  exit 1
fi

if ! command -v curl &>/dev/null; then
  echo "ERROR: curl is required but not found." >&2
  exit 1
fi

echo "Onboarding agent: ${AGENT_ID}"
echo "  Role:         ${ROLE}"
echo "  Display name: ${DISPLAY_NAME}"
echo "  Cost tier:    ${COST_TIER}"
echo "  Bridge:       ${BRIDGE_URL}"
echo ""

# --- Generate RS256 JWT ---

b64url() {
  openssl base64 -e -A | tr '/+' '_-' | tr -d '='
}

NOW=$(date +%s)
EXP=$((NOW + 31536000))  # 1 year

HEADER=$(printf '{"alg":"RS256","typ":"JWT"}' | b64url)
PAYLOAD=$(printf '{"sub":"%s","agent_id":"%s","iat":%d,"exp":%d}' \
  "$AGENT_ID" "$AGENT_ID" "$NOW" "$EXP" | b64url)

SIGNATURE=$(printf '%s.%s' "$HEADER" "$PAYLOAD" \
  | openssl dgst -sha256 -sign "$PRIVATE_KEY" -binary \
  | b64url)

JWT="${HEADER}.${PAYLOAD}.${SIGNATURE}"

# Save token
TOKEN_FILE="${BROOD_DIR}/jwt-${AGENT_ID}.token"
echo "$JWT" > "$TOKEN_FILE"
chmod 600 "$TOKEN_FILE"
echo "JWT saved to: ${TOKEN_FILE}"

# --- Register agent via beads-bridge ---

echo "Registering agent with beads-bridge..."
HTTP_CODE=$(curl -s -o /tmp/broodlink-onboard-response.json -w '%{http_code}' \
  -X POST "${BRIDGE_URL}/api/v1/tool/agent_upsert" \
  -H "Authorization: Bearer ${JWT}" \
  -H "Content-Type: application/json" \
  -d "{\"params\":{\"agent_id\":\"${AGENT_ID}\",\"display_name\":\"${DISPLAY_NAME}\",\"role\":\"${ROLE}\",\"transport\":\"api\",\"cost_tier\":\"${COST_TIER}\",\"active\":true}}" \
  2>/dev/null) || true

if [[ "$HTTP_CODE" == "200" ]]; then
  echo "Agent registered successfully."
else
  echo "WARNING: Registration returned HTTP ${HTTP_CODE} (bridge may be offline)."
  echo "The JWT is still valid. Register manually later or restart with bridge running."
  if [[ -f /tmp/broodlink-onboard-response.json ]]; then
    cat /tmp/broodlink-onboard-response.json 2>/dev/null
    echo ""
  fi
fi

# --- Output env vars ---

echo ""
echo "=== Add these to your environment ==="
echo ""
echo "export BROODLINK_AGENT_ID=${AGENT_ID}"
echo "export BROODLINK_AGENT_JWT=${JWT}"
echo "export BROODLINK_BRIDGE_URL=${BRIDGE_URL}/api/v1/tool"
echo ""
echo "=== Quick start ==="
echo ""
echo "LM_STUDIO_URL=http://localhost:1234 python agents/broodlink-agent.py"

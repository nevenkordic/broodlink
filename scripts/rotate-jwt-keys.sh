#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Rotate JWT signing keys for Broodlink services.
# Generates a new RSA keypair, keeps old keys for a grace period.
#
# Usage: bash scripts/rotate-jwt-keys.sh [--force]
set -euo pipefail

BROODLINK_DIR="${HOME}/.broodlink"
TIMESTAMP=$(date +%s)

echo "=== Broodlink JWT Key Rotation ==="
echo ""

# 1. Generate new RSA keypair
NEW_PRIVATE="${BROODLINK_DIR}/jwt-private-${TIMESTAMP}.pem"
NEW_PUBLIC="${BROODLINK_DIR}/jwt-public-${TIMESTAMP}.pem"

openssl genpkey -algorithm RSA -out "$NEW_PRIVATE" -pkeyopt rsa_keygen_bits:2048 2>/dev/null
openssl rsa -in "$NEW_PRIVATE" -pubout -out "$NEW_PUBLIC" 2>/dev/null
chmod 600 "$NEW_PRIVATE"
chmod 644 "$NEW_PUBLIC"

echo "  Generated new keypair:"
echo "    Private: ${NEW_PRIVATE}"
echo "    Public:  ${NEW_PUBLIC}"

# 2. Archive old primary key (don't delete — grace period)
OLD_PRIVATE="${BROODLINK_DIR}/jwt-private.pem"
OLD_PUBLIC="${BROODLINK_DIR}/jwt-public.pem"

if [[ -f "$OLD_PRIVATE" ]]; then
    OLD_TS=$(stat -f %m "$OLD_PRIVATE" 2>/dev/null || stat -c %Y "$OLD_PRIVATE" 2>/dev/null || echo "prev")
    cp "$OLD_PRIVATE" "${BROODLINK_DIR}/jwt-private-${OLD_TS}.pem" 2>/dev/null || true
    cp "$OLD_PUBLIC" "${BROODLINK_DIR}/jwt-public-${OLD_TS}.pem" 2>/dev/null || true
    echo "  Archived old keys with timestamp ${OLD_TS}"
fi

# 3. Install new key as primary
cp "$NEW_PRIVATE" "$OLD_PRIVATE"
cp "$NEW_PUBLIC" "$OLD_PUBLIC"
echo "  Installed as primary key"

# 4. Re-generate agent tokens with new key
b64url() { openssl base64 -e -A | tr '/+' '_-' | tr -d '='; }

for agent in claude qwen3 mcp-server a2a-gateway; do
    NOW=$(date +%s)
    EXP=$((NOW + 31536000))
    HDR=$(printf '{"alg":"RS256","typ":"JWT"}' | b64url)
    PLD=$(printf '{"sub":"%s","agent_id":"%s","iat":%d,"exp":%d}' \
        "$agent" "$agent" "$NOW" "$EXP" | b64url)
    SIG=$(printf '%s.%s' "$HDR" "$PLD" \
        | openssl dgst -sha256 -sign "$OLD_PRIVATE" -binary | b64url)
    echo "${HDR}.${PLD}.${SIG}" > "${BROODLINK_DIR}/jwt-${agent}.token"
    chmod 600 "${BROODLINK_DIR}/jwt-${agent}.token"
    echo "  Re-generated token for ${agent}"
done

# 5. Update secrets (SOPS) with new public key
if command -v sops &>/dev/null && [[ -f "./secrets.enc.json" ]]; then
    echo ""
    echo "  NOTE: Update BROODLINK_JWT_PUBLIC_KEY in secrets.enc.json:"
    echo "    sops secrets.enc.json"
    echo "    # Set BROODLINK_JWT_PUBLIC_KEY to contents of ${NEW_PUBLIC}"
fi

echo ""
echo "Key rotation complete. Restart services to pick up new keys."
echo "Old keys remain valid for the configured grace period."
echo ""
echo "To clean up old keys after grace period:"
echo "  rm ${BROODLINK_DIR}/jwt-private-*.pem ${BROODLINK_DIR}/jwt-public-*.pem"

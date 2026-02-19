#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Generate self-signed TLS certificates for inter-service communication.
# Creates a CA + server cert/key pair in ./tls/certs/
#
# Usage: ./scripts/generate-tls.sh [--force]
#   --force    Overwrite existing certificates

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TLS_DIR="${PROJECT_DIR}/tls/certs"
CA_DAYS=3650      # 10 years
CERT_DAYS=365     # 1 year
KEY_BITS=4096
FORCE=false

for arg in "$@"; do
  case "$arg" in
    --force) FORCE=true ;;
    *) echo "Unknown argument: $arg"; exit 1 ;;
  esac
done

# Check for existing certs
if [ -f "${TLS_DIR}/server.crt" ] && [ "$FORCE" != "true" ]; then
  echo "Certificates already exist at ${TLS_DIR}/"
  echo "Use --force to regenerate."
  exit 0
fi

echo "=== Broodlink TLS Certificate Generator ==="
echo ""

# Create output directory
mkdir -p "${TLS_DIR}"

# Step 1: Generate CA private key and self-signed certificate
echo "[1/4] Generating CA private key..."
openssl genrsa -out "${TLS_DIR}/ca.key" "$KEY_BITS" 2>/dev/null

echo "[2/4] Generating CA certificate (${CA_DAYS} days)..."
openssl req -new -x509 \
  -key "${TLS_DIR}/ca.key" \
  -out "${TLS_DIR}/ca.crt" \
  -days "$CA_DAYS" \
  -subj "/CN=Broodlink Internal CA/O=Broodlink/OU=Infrastructure" \
  2>/dev/null

# Step 2: Generate server key and CSR
echo "[3/4] Generating server key and CSR..."
openssl genrsa -out "${TLS_DIR}/server.key" "$KEY_BITS" 2>/dev/null

# Create SAN config for the server cert
cat > "${TLS_DIR}/server.cnf" <<EOF
[req]
distinguished_name = req_dn
req_extensions = v3_req
prompt = no

[req_dn]
CN = broodlink-service
O = Broodlink
OU = Services

[v3_req]
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth, clientAuth
subjectAltName = @alt_names

[alt_names]
DNS.1 = localhost
DNS.2 = beads-bridge
DNS.3 = status-api
DNS.4 = a2a-gateway
DNS.5 = mcp-server
DNS.6 = coordinator
DNS.7 = heartbeat
DNS.8 = embedding-worker
DNS.9 = *.broodlink.local
IP.1 = 127.0.0.1
IP.2 = ::1
EOF

openssl req -new \
  -key "${TLS_DIR}/server.key" \
  -out "${TLS_DIR}/server.csr" \
  -config "${TLS_DIR}/server.cnf" \
  2>/dev/null

# Step 3: Sign server cert with CA
echo "[4/4] Signing server certificate (${CERT_DAYS} days)..."
openssl x509 -req \
  -in "${TLS_DIR}/server.csr" \
  -CA "${TLS_DIR}/ca.crt" \
  -CAkey "${TLS_DIR}/ca.key" \
  -CAcreateserial \
  -out "${TLS_DIR}/server.crt" \
  -days "$CERT_DAYS" \
  -extensions v3_req \
  -extfile "${TLS_DIR}/server.cnf" \
  2>/dev/null

# Clean up intermediates
rm -f "${TLS_DIR}/server.csr" "${TLS_DIR}/server.cnf" "${TLS_DIR}/ca.srl"

# Set restrictive permissions
chmod 600 "${TLS_DIR}/ca.key" "${TLS_DIR}/server.key"
chmod 644 "${TLS_DIR}/ca.crt" "${TLS_DIR}/server.crt"

echo ""
echo "=== TLS Certificates Generated ==="
echo ""
echo "  CA cert:      ${TLS_DIR}/ca.crt"
echo "  CA key:       ${TLS_DIR}/ca.key"
echo "  Server cert:  ${TLS_DIR}/server.crt"
echo "  Server key:   ${TLS_DIR}/server.key"
echo ""
echo "Add to config.toml:"
echo ""
echo "  [tls]"
echo "  cert_path = \"${TLS_DIR}/server.crt\""
echo "  key_path  = \"${TLS_DIR}/server.key\""
echo "  ca_path   = \"${TLS_DIR}/ca.crt\""
echo ""
echo "  [profile]"
echo "  tls_interservice = true"
echo ""

# Verify the cert
echo "Verification:"
openssl x509 -in "${TLS_DIR}/server.crt" -noout -subject -issuer -dates -ext subjectAltName 2>/dev/null
echo ""
echo "Done."

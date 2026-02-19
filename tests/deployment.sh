#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Deployment configuration tests — validates production compose,
# TLS setup, read replica config, and NATS clustering.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
PASS=0
FAIL=0

pass() { echo "  PASS  $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL  $1"; FAIL=$((FAIL + 1)); }

echo "=== Broodlink Deployment Tests ==="
echo ""

# ── 1. Production compose file ──────────────────────────────────

echo "--- Production Compose ---"

PROD_COMPOSE="${PROJECT_DIR}/podman-compose.prod.yaml"

if [ -f "$PROD_COMPOSE" ]; then
  pass "podman-compose.prod.yaml exists"
else
  fail "podman-compose.prod.yaml missing"
fi

# Check for 3-node NATS cluster
NATS_NODES=$(grep -c 'nats-[123]:' "$PROD_COMPOSE" 2>/dev/null || echo 0)
if [ "$NATS_NODES" -ge 3 ]; then
  pass "3-node NATS cluster defined"
else
  fail "Expected 3 NATS nodes, found $NATS_NODES"
fi

# Check for cluster routes
if grep -q 'cluster_name=broodlink' "$PROD_COMPOSE"; then
  pass "NATS cluster name configured"
else
  fail "NATS cluster name not found"
fi

# Check for Postgres primary + replica
if grep -q 'postgres-primary:' "$PROD_COMPOSE" && grep -q 'postgres-replica:' "$PROD_COMPOSE"; then
  pass "Postgres primary + replica defined"
else
  fail "Postgres primary/replica not found"
fi

# Check for WAL replication config
if grep -q 'wal_level=replica' "$PROD_COMPOSE"; then
  pass "Postgres WAL replication configured"
else
  fail "Postgres WAL replication not configured"
fi

# Check for TLS volume mounts
TLS_MOUNTS=$(grep -c 'tls/certs:/etc/broodlink/tls' "$PROD_COMPOSE" 2>/dev/null || echo 0)
if [ "$TLS_MOUNTS" -ge 3 ]; then
  pass "TLS volume mounts present ($TLS_MOUNTS services)"
else
  fail "Expected 3+ TLS volume mounts, found $TLS_MOUNTS"
fi

# Check for health checks on infrastructure
HEALTH_CHECKS=$(grep -c 'healthcheck:' "$PROD_COMPOSE" 2>/dev/null || echo 0)
if [ "$HEALTH_CHECKS" -ge 5 ]; then
  pass "Health checks defined ($HEALTH_CHECKS services)"
else
  fail "Expected 5+ health checks, found $HEALTH_CHECKS"
fi

# Check for a2a-gateway service
if grep -q 'a2a-gateway:' "$PROD_COMPOSE"; then
  pass "A2A gateway service included"
else
  fail "A2A gateway service missing"
fi

# Check for mcp-server service
if grep -q 'mcp-server:' "$PROD_COMPOSE"; then
  pass "MCP server service included"
else
  fail "MCP server service missing"
fi

echo ""

# ── 2. TLS certificate generation ───────────────────────────────

echo "--- TLS Certificate Script ---"

TLS_SCRIPT="${PROJECT_DIR}/scripts/generate-tls.sh"

if [ -f "$TLS_SCRIPT" ]; then
  pass "generate-tls.sh exists"
else
  fail "generate-tls.sh missing"
fi

if [ -x "$TLS_SCRIPT" ]; then
  pass "generate-tls.sh is executable"
else
  fail "generate-tls.sh is not executable"
fi

# Check script generates CA + server certs
if grep -q 'ca.crt' "$TLS_SCRIPT" && grep -q 'server.crt' "$TLS_SCRIPT"; then
  pass "Script generates CA and server certificates"
else
  fail "Script missing CA/server cert generation"
fi

# Check for SAN configuration
if grep -q 'subjectAltName' "$TLS_SCRIPT"; then
  pass "SAN (Subject Alternative Name) configured"
else
  fail "SAN not configured in cert generation"
fi

# Check for service DNS names in SAN
for svc in beads-bridge status-api a2a-gateway mcp-server; do
  if grep -q "$svc" "$TLS_SCRIPT"; then
    pass "SAN includes $svc"
  else
    fail "SAN missing $svc"
  fi
done

# Check for --force flag support
if grep -q '\-\-force' "$TLS_SCRIPT"; then
  pass "Supports --force flag for regeneration"
else
  fail "Missing --force flag support"
fi

# Check for restrictive permissions
if grep -q 'chmod 600' "$TLS_SCRIPT"; then
  pass "Sets restrictive permissions on private keys"
else
  fail "Missing restrictive permissions on keys"
fi

echo ""

# ── 3. TLS config in broodlink-config ───────────────────────────

echo "--- TLS Configuration ---"

CONFIG_LIB="${PROJECT_DIR}/crates/broodlink-config/src/lib.rs"

if grep -q 'TlsConfig' "$CONFIG_LIB"; then
  pass "TlsConfig struct defined"
else
  fail "TlsConfig struct missing"
fi

if grep -q 'cert_path' "$CONFIG_LIB" && grep -q 'key_path' "$CONFIG_LIB" && grep -q 'ca_path' "$CONFIG_LIB"; then
  pass "TLS cert_path, key_path, ca_path fields present"
else
  fail "TLS config fields incomplete"
fi

if grep -q 'tls_interservice' "$CONFIG_LIB"; then
  pass "tls_interservice profile flag exists"
else
  fail "tls_interservice profile flag missing"
fi

echo ""

# ── 4. Read replica configuration ───────────────────────────────

echo "--- Read Replica Configuration ---"

if grep -q 'ReadReplicaConfig' "$CONFIG_LIB"; then
  pass "ReadReplicaConfig struct defined"
else
  fail "ReadReplicaConfig struct missing"
fi

BRIDGE_MAIN="${PROJECT_DIR}/rust/beads-bridge/src/main.rs"

if grep -q 'pg_read' "$BRIDGE_MAIN"; then
  pass "beads-bridge has read replica pool field"
else
  fail "beads-bridge missing read replica pool"
fi

if grep -q 'pg_read_pool' "$BRIDGE_MAIN"; then
  pass "beads-bridge has pg_read_pool helper"
else
  fail "beads-bridge missing pg_read_pool helper"
fi

echo ""

# ── 5. NATS multi-URL config ────────────────────────────────────

echo "--- NATS Cluster Configuration ---"

if grep -q 'cluster_urls' "$CONFIG_LIB" || grep -q 'nats_cluster' "$CONFIG_LIB"; then
  pass "NATS cluster config fields exist"
else
  fail "NATS cluster config missing"
fi

echo ""

# ── 6. Conditional TLS in services ──────────────────────────────

echo "--- Service TLS Integration ---"

for svc in beads-bridge status-api a2a-gateway; do
  SVC_MAIN="${PROJECT_DIR}/rust/${svc}/src/main.rs"
  if [ -f "$SVC_MAIN" ]; then
    if grep -q 'tls_interservice' "$SVC_MAIN" || grep -q 'axum_server' "$SVC_MAIN"; then
      pass "$svc has TLS binding support"
    else
      fail "$svc missing TLS binding support"
    fi
  else
    fail "$svc main.rs not found"
  fi
done

# Check axum-server in dependencies
for svc in beads-bridge status-api a2a-gateway; do
  CARGO="${PROJECT_DIR}/rust/${svc}/Cargo.toml"
  if grep -q 'axum-server' "$CARGO"; then
    pass "$svc Cargo.toml includes axum-server"
  else
    fail "$svc Cargo.toml missing axum-server"
  fi
done

echo ""

# ── 7. Telemetry in prod compose ────────────────────────────────

echo "--- Telemetry ---"

if grep -q 'jaeger' "$PROD_COMPOSE"; then
  pass "Jaeger included in prod compose"
else
  fail "Jaeger missing from prod compose"
fi

if grep -q '4317' "$PROD_COMPOSE"; then
  pass "OTLP port (4317) exposed"
else
  fail "OTLP port not exposed"
fi

echo ""

# ── Summary ─────────────────────────────────────────────────────

TOTAL=$((PASS + FAIL))
echo "=== Results: $PASS/$TOTAL passed ==="

if [ "$FAIL" -gt 0 ]; then
  echo "FAILED ($FAIL failures)"
  exit 1
fi

echo "All deployment tests passed."

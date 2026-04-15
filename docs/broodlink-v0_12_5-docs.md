# Broodlink v0.12.5 — Security Hardening

> Multi-agent AI orchestration system
> Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later
> Generated: 2026-04-15

This document covers changes in v0.12.5. For previous versions, see the
[CHANGELOG](../CHANGELOG.md) and [v0.12.3 docs](broodlink-v0_12_3-docs.md).

---

## Table of Contents

1. [Overview](#1-overview)
2. [SQL Injection Prevention](#2-sql-injection-prevention)
3. [Network Binding Hardening](#3-network-binding-hardening)
4. [NATS Token Authentication](#4-nats-token-authentication)
5. [CORS Lockdown](#5-cors-lockdown)
6. [Ollama Proxy Auth Gate](#6-ollama-proxy-auth-gate)
7. [Webhook HMAC-SHA256 Signing](#7-webhook-hmac-sha256-signing)
8. [Session Token Upgrade](#8-session-token-upgrade)
9. [JWT Algorithm Validation](#9-jwt-algorithm-validation)
10. [Shell Command Allowlist](#10-shell-command-allowlist)
11. [TLS Enforcement](#11-tls-enforcement)
12. [Default Password Elimination](#12-default-password-elimination)
13. [Path Traversal Prevention](#13-path-traversal-prevention)
14. [Error Message Sanitization](#14-error-message-sanitization)
15. [API Key Removal from Static Site](#15-api-key-removal-from-static-site)
16. [Upgrade Guide](#16-upgrade-guide)

---

## 1. Overview

v0.12.5 is a security-focused release that hardens every layer of the
Broodlink stack. No new features are added. All changes are
backwards-compatible at the API level, but operators must ensure secrets are
properly configured before upgrading (see Upgrade Guide).

**Scope:** 15 security fixes across 8 Rust binaries, 3 library crates,
4 shell scripts, 2 container compose files, and the Hugo static site.

---

## 2. SQL Injection Prevention

**Files:** `rust/heartbeat/src/main.rs`

The stale-agent deactivation query in the heartbeat cycle previously used
`format!` string interpolation to embed the cutoff timestamp directly in SQL.
This has been replaced with parameterized `sqlx::query().bind()` to prevent
SQL injection.

Before: `format!("UPDATE agent_profiles SET active = false WHERE active = true AND last_seen < '{cutoff}'")`
After: `sqlx::query("UPDATE ... WHERE ... last_seen < ?").bind(cutoff)`

---

## 3. Network Binding Hardening

**Files:** `rust/broodlink/src/main.rs`, `rust/beads-bridge/src/main.rs`,
`rust/status-api/src/main.rs`, `rust/a2a-gateway/src/main.rs`

All HTTP services previously bound to `0.0.0.0` (all interfaces), exposing
them to the network. They now bind to `127.0.0.1` (localhost only). For
external access, deploy behind a reverse proxy (nginx, Caddy, etc.).

---

## 4. NATS Token Authentication

**Files:** `crates/broodlink-config/src/lib.rs`, `crates/broodlink-runtime/src/lib.rs`,
`config.toml`, `rust/heartbeat/src/main.rs`, `rust/beads-bridge/src/main.rs`,
`rust/status-api/src/main.rs`, `rust/a2a-gateway/src/main.rs`,
`scripts/secrets-init.sh`

All services now resolve `BROODLINK_NATS_TOKEN` from SOPS-encrypted secrets
and pass it to NATS via `ConnectOptions::with_token()`. The `[nats]` config
section has a new `credentials_key` field pointing to the secret name.

The NATS server itself must be configured to require the matching token.
A TODO in `process_manager.rs` tracks generating the server-side config
automatically.

---

## 5. CORS Lockdown

**Files:** `rust/broodlink/src/main.rs`

The broodlink binary previously used `CorsLayer::permissive()`, which allowed
requests from any origin. It now uses an explicit allowlist:

- `http://localhost:1313` (Hugo dashboard)
- `http://localhost:3300` (dev frontend)

Only GET and POST methods are permitted. Headers are restricted to
`Content-Type`, `Authorization`, `X-Broodlink-Api-Key`, and
`X-Broodlink-Session`.

---

## 6. Ollama Proxy Auth Gate

**Files:** `rust/broodlink/src/main.rs`

The `/ollama/*` reverse proxy endpoint, which forwards requests to the local
Ollama instance, was previously unauthenticated. It now requires either a
valid `X-Broodlink-Api-Key` header or an `X-Broodlink-Session` header.
Unauthenticated requests receive a 401 response.

---

## 7. Webhook HMAC-SHA256 Signing

**Files:** `rust/heartbeat/src/main.rs`, `rust/heartbeat/Cargo.toml`,
`scripts/secrets-init.sh`

Outbound webhook notifications are now signed using HMAC-SHA256. The
`BROODLINK_WEBHOOK_SIGNING_KEY` secret is resolved from SOPS at startup.
Each webhook delivery includes an `X-Broodlink-Signature: sha256=<hex>`
header. Consumers can verify payload integrity by computing the same HMAC
over the raw request body.

New dependencies: `hmac` and `sha2` (workspace-level).

---

## 8. Session Token Upgrade

**Files:** `rust/status-api/src/main.rs`

Dashboard session tokens were previously UUID v4 (128 bits of randomness).
They are now 256-bit cryptographically random hex strings generated via
`rand::thread_rng().gen::<[u8; 32]>()`. This doubles the entropy and
eliminates the structural predictability of UUID formatting.

---

## 9. JWT Algorithm Validation

**Files:** `rust/beads-bridge/src/main.rs`

Before attempting to decode a JWT, beads-bridge now inspects the token header
and rejects any token that does not use RS256. This prevents algorithm
confusion attacks where an attacker submits a token signed with HS256 using
the public key as the HMAC secret. Rejected tokens are logged to the
`audit_log` table via `log_auth_failure()`.

---

## 10. Shell Command Allowlist

**Files:** `rust/a2a-gateway/src/main.rs`

The chat tool `run_command` previously relied solely on a blocklist of
dangerous commands. It now uses a three-layer defence:

1. **Metacharacter rejection:** Rejects any command containing shell operators
   (`|`, `$(`, backtick, `&&`, `||`, `;`, `>`, `<`, `>>`, `<<`).
2. **Allowlist:** Only ~50 pre-approved binaries are permitted (ls, cat, git,
   cargo, npm, python3, jq, curl, etc.).
3. **Legacy blocklist:** The existing `blocked_commands` config check remains
   as defence-in-depth.

---

## 11. TLS Enforcement

**Files:** `rust/beads-bridge/src/main.rs`, `rust/status-api/src/main.rs`,
`rust/a2a-gateway/src/main.rs`

When the deployment profile is not `dev`, all services now call
`process::exit(1)` if TLS is disabled. Previously they only logged a warning
and continued running without encryption.

---

## 12. Default Password Elimination

**Files:** `docker-compose.yml`, `podman-compose.yaml`,
`scripts/secrets-init.sh`, `scripts/db-setup.sh`, `scripts/create-admin.sh`,
`scripts/backfill-search-index.sh`, `scripts/infra-start.sh`

All `changeme` fallback passwords have been removed:

- Container compose files now use `${VAR:?Set VAR}` syntax, which causes an
  immediate failure with a descriptive error if the variable is unset.
- Shell scripts check for empty values and exit with an error message.
- `secrets-init.sh` now generates random passwords using `openssl rand`
  instead of shipping static defaults.

---

## 13. Path Traversal Prevention

**Files:** `crates/broodlink-fs/src/attachments.rs`

The `read_attachment()` function previously checked for `..` using simple
string matching (`relative_path.contains("..")`), which could be bypassed
with encoded or non-standard path representations. It now uses a two-layer
approach:

1. **Component check:** Iterates `Path::components()` and rejects any
   `ParentDir` component. Works on non-existent paths.
2. **Symlink protection:** Uses `canonicalize()` + `starts_with()` to ensure
   the resolved path stays within the attachments directory after symlink
   resolution.

---

## 14. Error Message Sanitization

**Files:** `rust/broodlink/src/main.rs`

Client-facing error responses no longer include internal details such as
database error messages, file paths, or stack traces. Internal errors are
logged server-side and a generic message is returned to the client.

---

## 15. API Key Removal from Static Site

**Files:** `status-site/hugo.toml`,
`status-site/themes/broodlink-status/layouts/_default/baseof.html`,
`rust/broodlink/src/dashboard.rs`

The `statusApiKey` was previously baked into the Hugo config and emitted as
an HTML `<meta>` tag at build time. This has been removed entirely. The
broodlink binary now injects the API key at runtime by rewriting HTML
responses served via `rust-embed`, inserting a
`<script>window.BroodlinkConfig={...}</script>` tag before `</head>`.

---

## 16. Upgrade Guide

1. **Re-run secrets-init.sh** to generate `BROODLINK_NATS_TOKEN` and
   `BROODLINK_WEBHOOK_SIGNING_KEY` if not already present:
   ```bash
   bash scripts/secrets-init.sh
   ```

2. **Re-encrypt secrets** with the new fields:
   ```bash
   sops --encrypt secrets.skeleton.json > secrets.enc.json
   rm secrets.skeleton.json
   ```

3. **Regenerate .secrets/env** (re-run secrets-init.sh after encrypting).

4. **Configure NATS server token** to match `BROODLINK_NATS_TOKEN`.

5. **Update reverse proxy** if you relied on services binding to `0.0.0.0` —
   they now only listen on `127.0.0.1`.

6. **Update webhook consumers** to verify `X-Broodlink-Signature` headers
   if you use outbound webhooks.

7. **Bump version** in `Cargo.toml` and `status-site/hugo.toml` to `0.12.5`.

8. **Build and test:**
   ```bash
   cargo fmt --all -- --check
   cargo test --workspace
   cargo build --release
   ```

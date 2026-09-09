#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Security audit for the Broodlink repo.
# Ensures no secrets, credentials, ledger data, or private files are tracked.
# Run before every public push: bash tests/security-audit.sh
#
# Install as a pre-commit hook:
#   bash scripts/install-git-hooks.sh

set -euo pipefail

PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

# Values allowed in the public .env.example template. Anything else is a leak.
is_safe_env_example_value() {
  local val="$1"
  case "$val" in
    "" | "dev-api-key" | "./config.toml") return 0 ;;
    http://localhost:* | https://localhost:*) return 0 ;;
  esac
  return 1
}

# All files tracked by git
TRACKED=$(git ls-files)

echo "=== Secret & Credential Checks ==="

# No private keys
if echo "$TRACKED" | grep -qiE '\.(pem|key|p12|pfx|jks)$'; then
  fail "Private key files tracked: $(echo "$TRACKED" | grep -iE '\.(pem|key|p12|pfx|jks)$')"
else
  pass "No private key files tracked"
fi

# No SSH private keys by common filenames
if echo "$TRACKED" | grep -qE '(^|/)id_(rsa|dsa|ecdsa|ed25519)$'; then
  fail "SSH private key tracked"
else
  pass "No SSH private keys tracked"
fi

# .env.example is the only env file that may be tracked
LEAKED_ENV=$(echo "$TRACKED" | grep -E '(^|/)\.env' | grep -vE '\.env\.example$' || true)
if [[ -n "$LEAKED_ENV" ]]; then
  fail ".env files tracked: $LEAKED_ENV"
else
  pass "No .env files tracked (except .env.example template)"
fi

# No sops/age secret files
if echo "$TRACKED" | grep -qiE '(sops\.yaml|\.age-identity|secrets\.enc)'; then
  fail "Secret config files tracked"
else
  pass "No sops/age secret files tracked"
fi

# No JWT tokens in source
if git grep -qlE 'eyJ[A-Za-z0-9_-]{20,}\.' -- '*.rs' '*.toml' '*.js' '*.html' '*.py' 2>/dev/null; then
  fail "JWT token pattern found in source files"
else
  pass "No JWT tokens in source"
fi

# No hardcoded passwords in production source (tests may use placeholders).
# Exclude config key names and documented local-dev placeholders.
PW_HITS=$(git grep -nEi '(password|passwd|secret)\s*=\s*"[^"]{8,}"' \
  -- '*.rs' '*.toml' '*.js' '*.py' \
  ':!tests/' ':!status-site/tests/' ':!agents/tests/' \
  2>/dev/null || true)
PW_HITS=$(printf '%s\n' "$PW_HITS" | grep -vEi \
  'password_key|password_file|secret.*provider|secret.*key_name|test-password|secret123|changeme|dev-api-key' \
  || true)
if [[ -n "$PW_HITS" ]]; then
  fail "Hardcoded password/secret pattern found in source"
  printf '%s\n' "$PW_HITS" | head -5
else
  pass "No hardcoded passwords in production source"
fi

# High-confidence credential patterns. Prefix-only UI detectors (e.g. /^sk-ant-/)
# do not match these because they require a long token body.
# Patterns live only in this script and .gitleaks.toml (both excluded).
CRED_FOUND=0
CRED_PATTERNS=(
  'AKIA[0-9A-Z]{16}'
  'ghp_[A-Za-z0-9]{36}'
  'github_pat_[A-Za-z0-9_]{20,}'
  'xox[baprs]-[A-Za-z0-9-]{10,}'
  'sk-ant-[A-Za-z0-9_-]{20,}'
  'sk-proj-[A-Za-z0-9_-]{20,}'
  'xai-[A-Za-z0-9]{20,}'
  'AIza[0-9A-Za-z_-]{35}'
  '-----BEGIN (RSA |OPENSSH |EC |DSA )?PRIVATE KEY-----'
  'BSA[A-Za-z0-9_-]{25,}'
)
for pat in "${CRED_PATTERNS[@]}"; do
  if git grep -nE "$pat" -- ':!.gitleaks.toml' ':!tests/security-audit.sh' >/dev/null 2>&1; then
    fail "Credential pattern matched: $pat"
    git grep -nE "$pat" -- ':!.gitleaks.toml' ':!tests/security-audit.sh' | head -5 || true
    CRED_FOUND=1
  fi
done
if [[ "$CRED_FOUND" -eq 0 ]]; then
  pass "No high-confidence credential patterns in tracked files"
fi

echo ""
echo "=== .env.example policy ==="

if [[ ! -f .env.example ]]; then
  fail ".env.example missing"
elif ! echo "$TRACKED" | grep -qxF '.env.example'; then
  fail ".env.example should be tracked as the public template"
else
  pass ".env.example is tracked as the safe template"
  ENV_BAD=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ -z "${line// /}" ]] && continue
    if [[ "$line" != *=* ]]; then
      fail ".env.example has a non-assignment line"
      ENV_BAD=1
      continue
    fi
    key="${line%%=*}"
    val="${line#*=}"
    if ! is_safe_env_example_value "$val"; then
      fail ".env.example ${key} has a non-placeholder value"
      ENV_BAD=1
    fi
  done < .env.example
  if [[ "$ENV_BAD" -eq 0 ]]; then
    pass ".env.example values are empty or documented placeholders"
  fi
fi

echo ""
echo "=== Ledger Data Checks ==="

# No Dolt database files
if echo "$TRACKED" | grep -qiE '(\.dolt/|dolt_log|agent_ledger)'; then
  fail "Dolt database files tracked"
else
  pass "No Dolt database files tracked"
fi

# No SQL dump/backup files (migrations/ is allowed — those are schema DDL)
if echo "$TRACKED" | grep -iE '\.(sql|dump|bak)$' | grep -qvE '^migrations/'; then
  fail "SQL dump files tracked (outside migrations/): $(echo "$TRACKED" | grep -iE '\.(sql|dump|bak)$' | grep -vE '^migrations/')"
else
  pass "No SQL dump files tracked (migrations/ excluded)"
fi

# No JSON data exports
if echo "$TRACKED" | grep -qiE '(memories|audit.log|work.log|conversations)\.json'; then
  fail "Data export JSON files tracked"
else
  pass "No data export files tracked"
fi

echo ""
echo "=== Private File Checks ==="

# No bot code (lives outside repo)
if echo "$TRACKED" | grep -qE '(bot\.py|dolt_client\.py|telegram)'; then
  fail "Bot/client code tracked (should live outside repo)"
else
  pass "No bot/client code tracked"
fi

# No user home paths hardcoded (except in tests/examples)
if git grep -qlE '/Users/[a-z]+/' -- '*.rs' '*.toml' '*.js' 2>/dev/null | grep -v 'tests/' >/dev/null 2>&1; then
  fail "Hardcoded home directory paths found in non-test source"
else
  pass "No hardcoded home paths in source"
fi

# No AI assistant config directories
if echo "$TRACKED" | grep -qE '(^|/)\.claude/|(^|/)\.cursor/|(^|/)\.copilot/'; then
  fail "AI assistant config directories tracked"
else
  pass "No AI assistant config directories tracked"
fi

echo ""
echo "=== .gitignore Verification ==="

GITIGNORE=".gitignore"
for pattern in "*.pem" "*.key" "*.p12" "id_rsa" ".env" "!.env.example" "target/" ".sops.yaml" ".DS_Store" ".secrets/" "secrets.enc.json"; do
  if grep -qF "$pattern" "$GITIGNORE" 2>/dev/null; then
    pass ".gitignore contains $pattern"
  else
    fail ".gitignore missing $pattern"
  fi
done

# Pattern-level checks (ignore index state) so tracked templates stay committable
# while real secret filenames stay blocked.
if git check-ignore -q --no-index .env; then
  pass "gitignores .env"
else
  fail "gitignore does not cover .env"
fi
if git check-ignore -q --no-index secrets.enc.json; then
  pass "gitignores secrets.enc.json"
else
  fail "gitignore does not cover secrets.enc.json"
fi
if git check-ignore -q --no-index .secrets/env; then
  pass "gitignores .secrets/env"
else
  fail "gitignore does not cover .secrets/env"
fi
if git check-ignore -q --no-index id_rsa; then
  pass "gitignores id_rsa"
else
  fail "gitignore does not cover id_rsa"
fi
if git check-ignore -q --no-index -- .env.example; then
  fail ".env.example is ignored and cannot be committed"
else
  pass ".env.example is not ignored"
fi

echo ""
echo "=== CI & hook policy ==="

if [[ -f .gitleaks.toml ]]; then
  pass ".gitleaks.toml present"
else
  fail ".gitleaks.toml missing"
fi

if [[ -f .githooks/pre-commit ]]; then
  pass "pre-commit hook template present"
else
  fail ".githooks/pre-commit missing"
fi

if grep -qE 'gitleaks|security-audit\.sh' .github/workflows/ci.yml; then
  pass "CI runs a secret scan"
else
  fail "CI workflow missing secret scan"
fi

if [[ -f .github/workflows/build.yml ]] \
  && grep -qE '^name: Build$' .github/workflows/build.yml \
  && grep -qE 'cargo build --workspace --release' .github/workflows/build.yml; then
  pass "Build is a separate GitHub Actions workflow"
else
  fail "Build workflow missing or not named Build"
fi

if grep -qE 'cargo build --workspace --release' .github/workflows/ci.yml; then
  fail "CI workflow still contains the release build (must live in Build)"
else
  pass "CI workflow does not include the release build"
fi

echo ""
echo "=== Policy self-tests ==="

if is_safe_env_example_value "" \
  && is_safe_env_example_value "dev-api-key" \
  && is_safe_env_example_value "./config.toml" \
  && ! is_safe_env_example_value "super-secret-value-do-not-commit"; then
  pass "env-example placeholder validator"
else
  fail "env-example placeholder validator"
fi

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[[ "$FAIL" -eq 0 ]]

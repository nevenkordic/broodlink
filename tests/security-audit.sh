#!/usr/bin/env bash
# Broodlink — Multi-agent AI orchestration
# Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Security audit for the Broodlink repo.
# Ensures no secrets, credentials, ledger data, or private files are tracked.
# Run before every public push: bash tests/security-audit.sh
#
# Also usable as a pre-commit hook:
#   ln -sf ../../tests/security-audit.sh .git/hooks/pre-commit

set -euo pipefail

PASS=0
FAIL=0

pass() { PASS=$((PASS + 1)); printf "  PASS: %s\n" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf "  FAIL: %s\n" "$1"; }

# All files tracked by git
TRACKED=$(git ls-files)

echo "=== Secret & Credential Checks ==="

# No private keys
if echo "$TRACKED" | grep -qiE '\.(pem|key|p12|pfx|jks)$'; then
  fail "Private key files tracked: $(echo "$TRACKED" | grep -iE '\.(pem|key|p12|pfx|jks)$')"
else
  pass "No private key files tracked"
fi

# No .env files
if echo "$TRACKED" | grep -qE '(^|/)\.env'; then
  fail ".env files tracked: $(echo "$TRACKED" | grep -E '(^|/)\.env')"
else
  pass "No .env files tracked"
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

# No hardcoded passwords (common patterns)
if git grep -qlEi '(password|passwd|secret)\s*=\s*"[^"]{4,}"' -- '*.rs' '*.toml' '*.js' '*.py' 2>/dev/null | grep -v 'password_key\|password_file\|secret.*provider\|secret.*key_name\|Cargo' >/dev/null 2>&1; then
  fail "Hardcoded password/secret pattern found in source"
else
  pass "No hardcoded passwords in source"
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

# Verify critical patterns are in .gitignore
GITIGNORE=".gitignore"
for pattern in "*.pem" "*.key" ".env" "target/" ".sops.yaml" ".DS_Store"; do
  if grep -qF "$pattern" "$GITIGNORE" 2>/dev/null; then
    pass ".gitignore contains $pattern"
  else
    fail ".gitignore missing $pattern"
  fi
done

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1

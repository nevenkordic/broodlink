# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Broodlink, please report it responsibly.

**Email:** security@broodlink.ai

**Scope:** All Broodlink services, libraries, and infrastructure configuration in this repository.

**What to include:**
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

## Response Timeline

- **Acknowledgment:** Within 48 hours
- **Initial assessment:** Within 5 business days
- **Fix timeline:** Depends on severity; critical issues are prioritized

## Out of Scope

- Denial-of-service attacks against production infrastructure
- Social engineering
- Issues in third-party dependencies (report upstream; notify us if it affects Broodlink)

## Secrets

This repository is public. Never commit credentials, API keys, private keys,
or decrypted SOPS files.

**Keep out of git**
- `.env` and `.env.*` (except the public `.env.example` template)
- `secrets.enc.json`, `secrets.skeleton.json`, `.sops.yaml`, `.age-identity`
- `.secrets/`, `*.pem`, `*.key`, SSH private keys, keystores
- Runtime `data/` (includes workspace encryption keys)

**How secrets are supplied**
- Local/dev: environment variables or SOPS (`scripts/secrets-init.sh`)
- Production: Infisical (`secrets.provider = "infisical"`)
- `.env.example` documents names only; values must stay empty or placeholders
  such as `dev-api-key`

**Before you push**
- `bash tests/security-audit.sh` (also runs in CI)
- `bash scripts/install-git-hooks.sh` to block secret commits locally
- CI additionally runs [gitleaks](https://github.com/gitleaks/gitleaks) on the
  working tree and git history

**If a secret is committed**
1. Rotate the credential immediately (the git history is public)
2. Email security@broodlink.ai with the file, commit, and rotation status
3. Do not try to "fix" a leaked live secret by deleting it in a later commit
   alone — treat it as compromised

Enable GitHub Push Protection and secret scanning on the repository so
partner-pattern secrets are blocked at push time as well.

## Disclosure

We practice coordinated disclosure. Please allow us reasonable time to address the issue before public disclosure.

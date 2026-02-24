# Contributing to Broodlink

Thank you for considering contributing to Broodlink.

## Contributor License Agreement

By submitting a contribution to Broodlink you agree that your contribution
is licensed under AGPLv3 (or later) AND you grant Neven Kordic <neven@broodlink.ai> a
perpetual, worldwide, non-exclusive, royalty-free license to use your
contribution under any license terms, including proprietary terms, for the
purpose of operating hosted Broodlink services.

## How to Contribute

1. Fork the repository
2. Create a feature branch (`git checkout -b feat/my-feature`)
3. Make your changes following the code rules below
4. Ensure `cargo deny check` passes
5. Ensure `cargo test --workspace` passes
6. Ensure all source files have the AGPL-3.0-or-later header
7. Commit using conventional commits: `<type>(<scope>): <description>`
8. Submit a pull request

## Code Rules

- No `unwrap()` or `expect()` in production code
- No secrets in committed files
- AGPL-3.0-or-later header in every source file
- `cargo deny check` must pass
- WCAG 2.1 AA for all Hugo site changes

## Security Rules

- Every new POST endpoint in status-api must call `require_role()` with the
  appropriate level (Admin for mutations, Operator for operational actions)
- Regex patterns must be anchored (`^...$`) to prevent ReDoS
- User-supplied URLs must pass `validate_webhook_url()` (SSRF protection)
- Shell scripts must use parameterized SQL (psql `-v` or `\set`), never string
  interpolation
- Passwords must be bcrypt-hashed; minimum 12 characters enforced in scripts
- Constant-time comparison for all secret/token validation
- No logging of user-supplied secrets (passwords, auth codes, tokens)

## Commit Types

- `feat`: new feature
- `fix`: bug fix
- `chore`: maintenance
- `docs`: documentation
- `refactor`: code restructuring
- `test`: test additions/changes
- `ci`: CI/CD changes

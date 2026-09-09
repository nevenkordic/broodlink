# Broodlink capability plan

> Multi-agent AI orchestration system
> Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later

Four workstreams. Extra chat surfaces (new messengers, voice) are out of scope.

Suggested order: setup CLI → self-authoring formulas → isolated workers → runtimes.

---

## 1. Self-authoring formulas

Formulas already live in TOML + `formula_registry`. After a non-trivial task
succeeds (many steps, retries, or a verification pass), Broodlink should draft
a formula from the work log and wait for operator confirm before writing
`custom/`.

**Ship**
- Heartbeat or coordinator: detect “worth saving” (step count, duration, verify=pass)
- LLM draft → same Formula schema as the visual editor
- Confirm in dashboard / `broodctl`; never auto-publish system formulas
- Searchable by name and tags; version bump on later improvements of the same skill

**Reuse**
`create_formula`, definition hash skip, custom/ TOML write-through.

---

## 2. Operator setup CLI

Install still feels like ops (`bootstrap`, compose, secrets). Add a product path
that gets a working chat without touching config.toml by hand.

**Ship**
- `broodctl setup` (or `broodlink setup`): pick model, enable tool groups, write env/config
- `broodctl model` / `broodctl tools` for later changes
- One happy path: install → setup → first message
- Keep `broodctl up` for infrastructure

---

## 3. Isolated workers

Delegation exists (request/accept/decline) but workers share the parent runtime
and context. Add spawn: parent keeps the conversation; a child runs one
workstream and returns a result.

**Ship**
- `spawn_worker` tool: goal, allowed tools, timeout, isolation backend
- Child talks to beads-bridge with its own JWT and budget
- Parallel children; parent joins on complete/fail
- Optional: child exposes tools over a tiny RPC so a script can call them
  without stuffing the parent context

---

## 4. Pluggable runtimes

Work should not be glued to the dashboard host.

**Ship**
- Runtime trait: `local` | `docker` | `ssh` | `remote-idle` (hibernate when unused)
- Coordinator picks runtime per worker (default local)
- Same JWT/audit path regardless of where the process runs
- Config: `[runtimes.<name>]` with backend and connection fields

---

## Done when

| Stream | Acceptance |
|---|---|
| Formulas | A verified multi-step task can become a confirmed custom formula |
| Setup | New machine: install + setup + chat, no hand-edited toml |
| Workers | Two children run in parallel; parent only sees summaries |
| Runtimes | Same task runs local and in Docker with identical audit rows |

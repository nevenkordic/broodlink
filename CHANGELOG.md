# Changelog

All notable changes to Broodlink are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Conventional Commits](https://www.conventionalcommits.org/).

## [0.7.0] - 2026-02-23

### Added

- **Conversational Agent Gateway**: Slack, Teams, and Telegram webhook ingestion
  in a2a-gateway with automatic chat session creation and platform-specific reply
  delivery. New Postgres tables: `chat_sessions`, `chat_messages`, `chat_reply_queue`
  (migration 019).
- **Formula Registry**: Persistent formula storage in Postgres `formula_registry`
  table (migration 020) with CRUD tools (`create_formula`, `get_formula`,
  `update_formula`, `delete_formula`, `list_formulas`). Seed script
  (`scripts/seed-formulas.sh`) populates system formulas from
  `.beads/formulas/*.formula.toml`.
- **Role-Based Dashboard Access**: Session-based RBAC with viewer/operator/admin
  roles. Postgres tables: `dashboard_users`, `dashboard_sessions` (migration 021).
  Dual auth middleware (session token + API key fallback). Bcrypt password hashing.
  Login/logout/me auth endpoints. User management CRUD in status-api.
- **Dashboard login page** (`/login/`) with auth.js client module using
  sessionStorage for session tokens.
- **Dashboard chat page** (`/chat/`) with real-time session monitoring, platform
  filtering, and message thread view.
- **Users tab** in control panel with create/edit/toggle/reset-password actions
  and role enforcement (viewer hides write controls, non-admin hides Users tab).
- **Formulas tab** in control panel for formula CRUD and toggle.
- `scripts/create-admin.sh` for bootstrapping dashboard admin users with
  pgcrypto fallback (no Python bcrypt dependency required).
- Heartbeat: chat session expiry and dashboard session cleanup cycles.
- 9 new beads-bridge tools (75 -> 84): `list_chat_sessions`, `reply_to_chat`,
  `close_chat_session`, `assign_chat_agent`, `create_formula`, `get_formula`,
  `update_formula`, `delete_formula`, `list_formulas`.
- 3 new status-api endpoint groups: `/api/v1/chat/*`, `/api/v1/formulas/*`,
  `/api/v1/users/*`, `/api/v1/auth/*`.
- Integration test suites: `tests/chat-integration.sh` (9 tests),
  `tests/formula-registry.sh` (11 tests), `tests/dashboard-auth.sh` (19 tests).
- 45 new unit tests (204 -> 249) including UserRole ordering, bcrypt roundtrip,
  require_role, serde, and deserialization tests.

### Fixed

- `grep -q` + `pipefail` SIGPIPE bug in formula-registry.sh (early grep exit
  killed the piped script).
- Bash `${2:-{}}` brace parsing bug in dashboard-auth.sh (trailing `}` appended
  to every POST body, causing "trailing characters" JSON parse errors).
- Stale chat session reuse in integration tests (unique timestamp-based channel
  IDs per run).

## [0.6.0] - 2026-02-22

### Added

- **Agent Budget Enforcement**: `tool_cost_map` and `budget_transactions` tables
  (migration 014). `check_budget()` middleware deducts tokens per tool call.
  `BudgetExhausted` error (402). Daily replenishment via heartbeat. Tools:
  `get_budget`, `set_budget`, `get_cost_map`.
- **Dead-Letter Queue Tooling**: `dead_letter_queue` table (migration 015).
  Coordinator persists failed tasks with auto-retry and exponential backoff.
  Tools: `inspect_dlq`, `retry_dlq_task`, `purge_dlq`. Status-api: `GET /api/v1/dlq`.
- **Workflow Branching & Error Handling**: Conditional steps (`when` expressions),
  per-step retries, parallel step groups, step timeouts, and `on_failure` error
  handlers (migration 016).
- **Multi-Agent Collaboration**: Task decomposition with merge strategies
  (concatenate, vote, best). Shared workspaces for cross-agent context
  (migration 017). Tools: `decompose_task`, `create_workspace`, `workspace_read`,
  `workspace_write`, `merge_results`.
- **Webhook Gateway**: Slack slash commands, Teams bot messages, Telegram updates
  in a2a-gateway. Outbound notifications for agent.offline, task.failed,
  budget.low, workflow events, guardrail violations (migration 018).
- **OTLP Telemetry**: W3C TraceContext propagation, Jaeger enabled by default.
- **Knowledge Graph Expiry**: TTL-based entity cleanup, edge weight decay,
  orphan removal, `graph_prune` tool, freshness scoring.
- **JWT Key Rotation**: kid-based multi-key validation, JWKS endpoint
  (`/.well-known/jwks.json`), `scripts/rotate-jwt-keys.sh`.
- **Dashboard Control Panel** (`/control/`): Tabbed admin interface with Agents,
  Guardrails, Budgets, Tasks, Workflows, DLQ, Chat, Formulas, Webhooks tabs.
  Agent toggle, budget set, task cancel, webhook CRUD, guardrail management.
- 9 new beads-bridge tools (66 -> 75).
- 5 new Postgres migrations (014-018).
- v0.6.0 regression test suite (105 tests).
- DOM cross-check test suite for HTML/JS ID consistency.

## [0.5.0] - 2026-02-20

### Added

- **Knowledge Graph**: Entity extraction from memories via Ollama LLM.
  Postgres tables: `kg_entities`, `kg_edges`, `kg_entity_memories` (migration 013).
  Qdrant collection `broodlink_kg_entities` for entity similarity search.
- Operation-aware embedding outbox: "embed" for full pipeline, "kg_extract" for
  extraction only.
- 4 new beads-bridge tools (62 -> 66): `graph_search`, `graph_traverse`,
  `graph_update_edge`, `graph_stats`.
- Dashboard `/knowledge-graph/` page with entity type chart, most-connected
  entities, and edge browser.
- `scripts/backfill-knowledge-graph.sh` for retroactive entity extraction.
- KG regression tests and DOM cross-check suite.

### Fixed

- Dashboard element ID mismatches between HTML templates and JS files.
- API field mapping errors in dashboard JS modules.

## [0.4.0] - 2026-02-20

### Added

- **Hybrid Memory Search**: BM25 full-text (Postgres tsvector) + vector
  (Qdrant) fusion with temporal decay and optional reranking. Postgres
  `memory_search_index` table (migration 012).
- `hybrid_search` tool with configurable BM25/vector weights, decay parameters,
  and graceful degradation to single-backend when one is unavailable.
- 1 new beads-bridge tool (61 -> 62).
- Hybrid search integration tests.

### Fixed

- mcp-server JWT path mismatch.
- Dashboard metrics hardening for missing data.

## [0.3.0] - 2026-02-20

Initial public release.

### Added

- **Core Architecture**: 7 Rust services (beads-bridge, coordinator, heartbeat,
  embedding-worker, status-api, mcp-server, a2a-gateway) with dual database
  design (Dolt for versioned state, Postgres for hot paths).
- **beads-bridge**: 61-tool API with JWT RS256 authentication, rate limiting,
  circuit breakers, and SSE streaming.
- **Coordinator**: NATS-based task routing with weighted agent scoring, atomic
  claiming, exponential backoff, dead-letter queue, and sequential workflow
  orchestration.
- **Heartbeat**: 5-minute sync cycle with Dolt commit, Beads issue sync, agent
  metrics computation, daily summary generation, and stale agent deactivation.
- **Embedding Worker**: Outbox-driven pipeline with Ollama `nomic-embed-text`
  embeddings and Qdrant vector upsert. Circuit breakers for resilience.
- **Status-API**: Dashboard REST API with API key auth, CORS, and SSE stream proxy.
- **MCP Server**: Model Context Protocol server (streamable HTTP + legacy stdio)
  proxying all bridge tools.
- **A2A Gateway**: Google Agent-to-Agent protocol gateway with AgentCard
  discovery and cross-system task delegation.
- **Hugo Dashboard**: Operations dashboard with pages for agents, decisions,
  memory, commits, audit log, beads, approvals, delegations, guardrails, and
  A2A gateway. WCAG 2.1 AA compliant.
- **Shared Crates**: broodlink-config, broodlink-secrets (SOPS + Infisical),
  broodlink-telemetry (OTLP), broodlink-runtime (CircuitBreaker, NATS, signals).
- **Infrastructure**: Podman Compose for dev (Postgres, NATS, Qdrant, Ollama,
  Jaeger) and prod (3-node NATS cluster, PG replicas, TLS).
- 11 database migrations (001-011).
- Agent onboarding script with JWT generation and system prompt templating.
- Python agent SDK for any OpenAI-compatible LLM.
- Bootstrap script for one-shot setup.
- 129 unit tests, E2E test suite, integration test suites.

[0.7.0]: https://github.com/broodlink/broodlink/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/broodlink/broodlink/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/broodlink/broodlink/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/broodlink/broodlink/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/broodlink/broodlink/releases/tag/v0.3.0

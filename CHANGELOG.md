# Changelog

All notable changes to Broodlink are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Conventional Commits](https://www.conventionalcommits.org/).

## [Unreleased]

### Added

- **Bidirectional Formula Sync**: System TOML formulas (`.beads/formulas/`) sync
  to Postgres via heartbeat (TOML wins for `is_system=true` rows, skips when
  `definition_hash` matches). User formulas write through to
  `.beads/formulas/custom/` on every create/update. Custom TOMLs backfill to
  Postgres on startup (insert only, never overwrite). Heartbeat safety net
  rewrites missing user formula TOMLs within one cycle. Migration 026 adds
  `definition_hash` column.
- **System Formula Protection**: API and bridge reject edits to `is_system=true`
  formulas with a clear error ("cannot modify system formula — create a copy
  with a new name"). Name collision guard prevents user formulas from shadowing
  system formula names.
- **Shared `broodlink-formulas` crate**: Formula TOML parsing, JSONB conversion,
  `definition_hash` (SHA-256), `persist_formula_toml` (atomic write), parameter
  format conversion (TOML dict ↔ JSONB array), `validate_formula_name`. 12 unit
  tests. Used by coordinator, heartbeat, beads-bridge, and status-api.
- **Ollama Self-Healing**: When the primary chat model (`chat_model`) hits an OOM
  or other error, a2a-gateway attempts recovery (unload model, wait, retry). If
  recovery fails, enters **degraded mode** — routes all subsequent messages to
  `chat_fallback_model` (e.g. `qwen3:1.7b`) which answers the user's actual
  question instead of returning an error. Degraded mode skips the primary model
  entirely (no 3s recovery penalty per message). After a 5-minute cooldown, the
  next message probes the primary model again to detect recovery. Fully automatic
  — no operator intervention needed.
- **GitHub Repository Setup**: Branch protection on `main` (require PR reviews,
  CI status checks, no force push). Issue templates for bug reports, feature
  requests, and formula submissions. PR template with checklist. `SECURITY.md`
  responsible disclosure policy. `CODE_OF_CONDUCT.md` (Contributor Covenant v2.1).
- `scripts/seed-formulas.sh` updated with deprecation notice (heartbeat handles
  sync automatically) and `ON CONFLICT DO UPDATE WHERE is_system = true` for
  idempotent re-seeding with `definition_hash`.
- `formulas_custom_dir` config field in `[beads]` section (default:
  `.beads/formulas/custom`) with tilde expansion.
- 12 new unit tests in broodlink-formulas crate (249 → 261 workspace total).
- **Proactive Skills** — Three new autonomous capabilities that make Broodlink
  proactive instead of purely reactive:
  - **Scheduled Task Promotion**: `scheduled_tasks` table tracks one-shot and
    recurring tasks. Coordinator polls every 60 seconds, promotes due tasks into
    `task_queue`, publishes NATS `task_available`, handles one-shot disable and
    recurring advancement (`next_run_at += recurrence_secs`). Optional formula
    execution on fire.
  - **Notification Rules & Incident Detection**: Heartbeat evaluates
    `notification_rules` each cycle. Condition types: `service_event_error`
    (error spike in last 15 min), `dlq_spike` (unresolved DLQ count),
    `budget_low` (agents below token threshold). Cooldown enforcement prevents
    alert storms. Optional `auto_postmortem` triggers `incident-postmortem`
    workflow formula automatically.
  - **Notification Dispatch**: beads-bridge `send_notification` tool inserts into
    `notification_log` and publishes NATS `notification.send`. a2a-gateway
    subscribes and delivers to Telegram (via bot API) or Slack (via incoming
    webhook). Delivery status tracked in `notification_log` (pending/sent/failed).
  - 6 new beads-bridge tools (84 → 90): `schedule_task`, `list_scheduled_tasks`,
    `cancel_scheduled_task`, `send_notification`, `create_notification_rule`,
    `list_notification_rules`.
  - Migration 028: 3 new Postgres tables (`scheduled_tasks`, `notification_rules`,
    `notification_log`).
  - `NotificationsConfig` struct in broodlink-config (`[notifications]` section).
- **Remember Tool in Chat**: a2a-gateway chat model can call `remember(topic,
  content)` to store memories during conversation. Tool definitions refactored
  from single if/else to Vec builder (web_search + remember, gated by config).
- **`incident-postmortem` formula**: 4-step workflow (gather_evidence →
  root_cause_analysis → prevention_plan → final_report) in
  `.beads/formulas/custom/`.
- 3 new skills registered: `schedule-task` (claude-code), `incident-response`
  (a2a-gateway), `notification-dispatch` (a2a-gateway).

## [0.7.0] - 2026-02-23

### Added

- **Conversational Agent Gateway**: Slack, Teams, and Telegram webhook ingestion
  in a2a-gateway with automatic chat session creation and platform-specific reply
  delivery. New Postgres tables: `chat_sessions`, `chat_messages`, `chat_reply_queue`
  (migration 019).
- **Telegram Tool Calling with Brave Search**: Direct Ollama LLM chat loop with
  `web_search` tool calling for real-time queries (weather, news, sports scores).
  Tighter system prompt restricts tool use to real-time data only. Configurable
  `num_ctx` (default 4096) and `tool_context_messages` (default 4) for CPU
  inference efficiency. `.env` file loader with `unsafe set_var` for secrets.
- **Chat Efficiency Safeguards**: Ollama concurrency semaphore (configurable,
  default 1) returns busy message instantly instead of queuing. Brave search
  result cache with TTL (default 5 min). Duplicate message suppression with
  dedup window (default 30s) checked before any DB/bridge work. Periodic typing
  indicator refresh every 4s via `tokio::select!`. Busy/dedup replies excluded
  from `chat_messages` to prevent conversation history pollution. Dedup
  check-and-insert uses single write lock (no TOCTOU race). Shared
  `normalize_search_query` for cache and dedup key normalization.
- **Platform Credentials**: `platform_credentials` table (migration 024) for
  storing Telegram bot tokens and other platform secrets with JSONB metadata.
  Telegram poll offset persisted across restarts via `meta->>'poll_offset'`.
  status-api publishes `{prefix}.platform.credentials_changed` via NATS on
  bot registration and disconnection; a2a-gateway subscribes and immediately
  invalidates its `telegram_creds` cache (eliminates 60s stale cache window).
- **Telegram Access Code Authentication**: Random 8-char alphanumeric access
  code generated on bot registration (true RNG via `rand` crate) and stored in
  `platform_credentials.meta->>'auth_code'`. New users must send the code to
  authenticate; verified user IDs added to `allowed_user_ids` in JSONB meta.
  a2a-gateway enforces the allow list at both webhook and polling entry points.
- **Cascade Delete on Bot Disconnect**: `handler_telegram_disconnect` now
  cascade-deletes all related data (`chat_reply_queue`, `chat_messages`,
  `chat_sessions`, `platform_credentials`) in a single CTE query.
- **Telegram Long-Polling**: `getUpdates`-based polling loop with configurable
  offset persistence, automatic token cache invalidation on errors, and typing
  indicators. Credentials re-fetched after `getUpdates` returns (not before),
  so `allowed_users` is always current when processing updates.
- **Auth Code Security Hardening**: Message text from unauthenticated users is
  never logged (prevents auth code leakage). Inbound webhook log line moved
  after the auth gate (only logged for authenticated users). Auth code
  messages return before reaching chat history storage or LLM context. Wrong
  code and random messages receive the same generic prompt (no information
  leakage about code validity).
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
- **Dashboard Telegram status card** shows access code and authorized user
  count in the Telegram tab.
- `scripts/create-admin.sh` for bootstrapping dashboard admin users with
  pgcrypto fallback (no Python bcrypt dependency required).
- Heartbeat: chat session expiry and dashboard session cleanup cycles.
- New config fields: `[ollama].num_ctx`, `[a2a].ollama_concurrency`,
  `[a2a].dedup_window_secs`, `[a2a].busy_message`, `[chat].chat_model`,
  `[chat.tools].web_search_enabled`, `[chat.tools].max_tool_rounds`,
  `[chat.tools].search_result_count`, `[chat.tools].tool_context_messages`,
  `[chat.tools].search_cache_ttl_secs`.
- `scripts/start-gateway.sh` wrapper for `.env` sourcing.
- 6 new beads-bridge tools (78 -> 84): `list_chat_sessions`, `reply_to_chat`,
  `create_formula`, `get_formula`, `update_formula`, `list_formulas`.
- 4 new status-api endpoint groups: `/api/v1/chat/*`, `/api/v1/formulas/*`,
  `/api/v1/users/*`, `/api/v1/auth/*`.
- Integration test suites: `tests/chat-integration.sh` (9 tests),
  `tests/formula-registry.sh` (11 tests), `tests/dashboard-auth.sh` (19 tests).
- 45 new unit tests (204 -> 249) including UserRole ordering, bcrypt roundtrip,
  require_role, serde, and deserialization tests.

### Security

- **RBAC on all mutation endpoints**: Every POST handler in status-api now
  enforces `require_role()` — Admin for agent toggle, budget set, webhook CRUD,
  Telegram register/disconnect, formula create/update/toggle, approval policy
  upsert; Operator for task cancel, chat assign/close, approval review.
- **Security headers**: HSTS (`max-age=63072000; includeSubDomains`), CSP
  (`default-src 'self'`), `X-Frame-Options: DENY`, `X-Content-Type-Options:
  nosniff`, `Cache-Control: no-store`, `Permissions-Policy` on all 5 HTTP
  services (beads-bridge, status-api, mcp-server, a2a-gateway streamable HTTP).
- **SSRF protection**: `validate_webhook_url()` blocks private/internal
  networks (localhost, link-local, RFC 1918, metadata endpoints) on webhook and
  Telegram registration URLs.
- **Request body limits**: 10 MiB cap on all HTTP request bodies via
  `DefaultBodyLimit`.
- **Query LIMIT clamping**: All paginated status-api queries clamped to 1–1000
  via `clamp_limit()`.
- **Fail-closed guardrails**: `check_guardrails()` rejects on DB errors instead
  of silently allowing. `evaluate_condition()` defaults to `false` for unknown
  expressions.
- **Input validation**: Regex anchoring (`^...$`) on all patterns to prevent
  ReDoS. Path traversal prevention in KG entity names. Whitespace/length limits
  on user-supplied strings.
- **SQL parameterization**: Shell scripts (`db-setup.sh`, `create-admin.sh`,
  `backfill-*.sh`) now use psql `-v` variable binding or `\set` piped via stdin
  instead of string interpolation. Dolt password args properly quoted.
- **Password policy**: `create-admin.sh` enforces 12-character minimum with
  interactive confirmation prompt. Bcrypt cost factor configurable (default 12).
- **Session invalidation**: `POST /auth/logout-all` invalidates all sessions
  for the current user.
- **SSE stream caps**: Maximum 100 concurrent streams, 1-hour TTL with reaper.
- **Brave search cache hard cap**: 200-entry maximum with LRU eviction.
- **Container hardening**: `read_only: true`, `no-new-privileges`, dropped
  capabilities in podman-compose.
- **Constant-time comparison**: API key validation uses byte-level XOR
  comparison to prevent timing side-channels.
- **CI security scanning**: `cargo-deny` license and advisory audit in build
  pipeline.

### Fixed

- Dashboard Telegram registration form stuck on "Registering..." after
  successful bot registration (form now clears before reloading status).
- `grep -q` + `pipefail` SIGPIPE bug in formula-registry.sh (early grep exit
  killed the piped script).
- Bash `${2:-{}}` brace parsing bug in dashboard-auth.sh (trailing `}` appended
  to every POST body, causing "trailing characters" JSON parse errors).
- Stale chat session reuse in integration tests (unique timestamp-based channel
  IDs per run).
- `create-admin.sh` podman fallback for psql `-v` variable binding (uses piped
  `\set` statements instead).
- Test webhook URL updated for SSRF validation compatibility.
- Hugo minified HTML grep patterns in test assertions.

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
- 12 new beads-bridge tools (66 -> 78).
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

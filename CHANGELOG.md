# Changelog

All notable changes to Broodlink are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Conventional Commits](https://www.conventionalcommits.org/).

## [0.11.0] - 2026-02-28

### Added

- **Chat Tool Expansion**: Four new chat tools in a2a-gateway —
  `fetch_webpage` (direct HTTP GET with HTML tag stripping, 8000-char
  truncation), `schedule_task` (one-time or recurring via bridge),
  `list_scheduled_tasks`, `cancel_scheduled_task`. All tool descriptions
  are generic (no site-specific logic) so the model learns monitoring
  patterns from conversation context.
- **Auto-Fetch URLs from Conversation History**: When a user asks a
  follow-up question, the system queries the DB for the last 5 inbound
  messages containing URLs (beyond the chat context window), fetches the
  most recent URL, truncates to 3000 chars, and injects the live content
  into the system prompt. The model reads and answers — no tool calling
  needed. Eliminates the unreliable tool-selection behaviour of small
  models.
- **Retry with Reduced Tool Set**: When qwen3:30b-a3b produces
  thinking-only output (exhausts `num_predict` on internal reasoning),
  the system retries with a focused 3-tool set (schedule_task, web_search,
  fetch_webpage) and a simplified system prompt. Includes recent
  conversation history (last 6 turns) so the model has context about
  previously-discussed URLs and topics.
- **Multi-Modal Chat Attachments**: Slack, Teams, and Telegram file uploads
  are downloaded, stored to `attachments_dir`, and persisted as chat message
  attachments. `PendingAttachment` struct carries metadata (type, MIME,
  filename, bytes, platform file ID) through the message pipeline.
  `classify_attachment_type()` buckets files into image/audio/video/document.
  Slack file downloads use bot token auth. Telegram file downloads via
  `getFile` API. Three new status-api endpoints:
  `GET /chat/sessions/:id/attachments`,
  `GET /chat/attachments/:id/download`,
  `GET /chat/attachments/:id/thumbnail`.
  `reqwest` multipart feature enabled for future upload support.
- **Python SDK Overhaul**: CLI rewritten from argparse to Click with
  subcommand groups: `agent list`, `memory search|store|recall|stats`,
  `tool call|list`. Backward-compatible `--listen` mode preserved.
  Package version bumped to 0.11.0. Typed exports for all model classes
  (`Memory`, `Task`, `Agent`, `Issue`, `Formula`, etc.) via `__all__`.
- **Dashboard Redesign**: All 15 dashboard pages (everything except
  Verification) upgraded to consistent design system. Metric cards
  (`.card-grid > .card.metric-card > .value + .label`) added to Beads,
  Delegations, Audit, Commits, Approvals, Guardrails, A2A, and Chat.
  All tables wrapped in `.table-container` for consistent borders.
  Sidebar reorganised into three sections (Intelligence, Operations,
  System) with plain-English labels. ~210 lines of new CSS for chat
  sessions, modal overlay, platform badges, status badges, agent cards,
  guardrail cards, A2A structured cards, policy editor, and empty states.
- **A2A Structured Card**: Replaced raw `<pre>` JSON dump with a
  structured card showing agent name, description, URL, provider, version,
  and skill tags. Task mappings rendered in a proper table.
- **Verification Page Enhancements**: Configurable date range filter
  (7/14/30/60/90 days), error outcome tracking in metrics and charts,
  improved Chart.js dark-theme defaults, styled error state for failed
  data loads.
- **Attachment Storage Module**: New `crates/broodlink-fs/src/attachments.rs`
  for file persistence with tilde-expanded `attachments_dir`.
- New config fields: `[chat].attachments_dir`,
  `[chat].voice_transcription_enabled`, `[chat].chat_transcription_model`,
  `[chat].transcription_url`, `[chat].max_attachment_bytes`.
- Migration 030: `chat_attachments` table (message_id FK, type, MIME,
  filename, file path, thumbnail path, file size, platform file ID).
- Python SDK: `AsyncBroodlinkClient` / `BroodlinkClient` with namespace
  accessors (`memory`, `agents`, `tasks`, `kg`), typed models via
  Pydantic, `NATSHelper`, `BaseAgent` framework, ML utilities. Full
  test suite under `agents/tests/`.
- `chrono` dependency added to a2a-gateway for date/time injection.

### Fixed

- **Telegram streaming double-send**: When streaming is enabled, the
  polling loop both edited the placeholder message AND returned the reply
  for a second `sendMessage` call. Fixed by returning `None` from
  `process_telegram_update()` when streaming has already delivered via
  edit.
- **Byte-boundary panics on emoji/multi-byte content**: Five instances of
  unsafe `&str[..N]` byte slicing replaced with `floor_char_boundary()` —
  streaming think-tag filtering, title truncation, content truncation,
  write preview, and Telegram message guard. Prevents crashes on messages
  containing emoji or non-ASCII characters.
- **Think tag leakage in summarization**: Summarization path used
  `think: false`, causing qwen3 MoE to leak chain-of-thought into
  content without tags (making `strip_think_tags` useless). Changed to
  `think: true` with `num_predict: 1024` so thinking separates cleanly
  into `message.thinking` field.
- **Verbose chat responses**: Strengthened system prompt from "Keep
  responses concise" to explicit "1-3 short sentences, no markdown
  headers, no bullet lists, no disclaimers." Tightened confidence
  instruction, summarization prompt, URL context instruction, and
  fallback model prompt to all enforce brevity.
- **Telegram message length overflow**: Added 4000-char truncation guard
  with `[truncated]` suffix in `deliver_telegram()` to prevent Telegram
  API 400 errors on messages exceeding the 4096-char limit.
- **Date/time awareness**: Injected `chrono::Utc::now()` formatted as
  AEST into every system prompt and retry prompt so the model knows the
  current date for scheduling and time-sensitive queries.
- **Verification analytics query**: Grouped by `details->>'outcome'`
  instead of `event_type` prefix stripping, since a2a-gateway writes
  all verification events as `verification_completed` with the actual
  outcome (verified/corrected/timeout/error) in the JSONB details field.
- **Approval "all" gate type**: Added `"all"` to the allowed `gate_type`
  values in the policy upsert validation.
- **Chat metric cards unstyled**: Chat page used `.metric-bar` /
  `.metric-value` / `.metric-label` classes with zero CSS. Replaced with
  standard `.card-grid > .card.metric-card` pattern.
- **Memory/Knowledge Graph wrong card classes**: Used `.card-value` /
  `.card-label` (no CSS) instead of `.value` / `.label`. Fixed to match
  design system.
- **Dashboard tables inconsistent**: Bare `<table>` tags (Overview, Beads,
  Audit, Commits, Knowledge Graph, Onboarding) and `.table-wrap` /
  `.data-table` classes (Delegations, Approvals, Guardrails, Onboarding)
  had no CSS. All wrapped in `.table-container`.
- **Delegations page**: JS generated full table HTML inside empty div.
  Replaced with pre-defined `<table>` with `<thead>` visible on load;
  JS fills `<tbody>` only.
- **`fetchApi()` POST support**: Added optional `opts` parameter to
  `utils.js` `fetchApi()` for POST/PUT requests, matching the pattern
  used by the control panel.
- **Query Expansion**: Memory search calls Ollama to generate 2–3 alternative
  phrasings before embedding. Each variant runs through semantic/hybrid search
  in parallel; results are merged by max score. Skips queries < 3 words.
  Graceful fallback to original query on any error. Configurable model,
  timeout, and enable flag in `[memory_search]`.
- **Smart Chunk Boundaries**: Embedding worker splits content at natural
  boundaries (headings, code fences, table separators, blank lines) instead
  of raw word counts. Chunks never split mid-code-block. Falls back to
  word-based splitting when no natural boundaries exist. Overlap preserved
  across chunks.
- **Streaming Responses**: Telegram messages stream progressively via
  `editMessageText` instead of waiting for full LLM completion. Rate-limited
  to ~1 edit/800ms and 30-token minimum delta. Tool calls pause the stream,
  show "Using tool: {name}...", then resume. Shared `prepare_chat_context()`
  eliminates 400 lines of duplication between streaming and non-streaming
  paths.
- **Verification Timeout Caveat**: `verify_response()` returns a `VerifyResult`
  enum (Verified/Corrected/Timeout/Error) instead of `Option<String>`.
  Timeouts and errors append `"[Unverified: ...]"` caveat to the response
  instead of silently passing through. All verification calls log structured
  events to `service_events`.
- **Verification Analytics Dashboard**: New `/verification/` page with daily
  verification counts (triggered/verified/corrected/timeout), average
  duration, and confidence histogram. Two new status-api endpoints:
  `GET /api/v1/verification/analytics`, `GET /api/v1/verification/confidence`.
- **Agent Negotiation Protocol**: Agents can decline tasks or request context
  before committing. `decline_task` re-routes to another agent (up to
  `max_declines_per_task`, then dead-letter). `request_task_context` pauses
  the task and publishes questions to the assigning agent. Timed-out context
  requests auto-reset to pending. New Postgres table `task_negotiations`,
  new columns on `task_queue` (`decline_count`, `declined_agents`,
  `context_questions`, `context_requested_by`, `context_requested_at`).
  Migration 029. Two new NATS subjects. Two new beads-bridge tools
  (94 → 96).
- **Multi-Ollama Load Balancing**: `OllamaPool` distributes inference across
  multiple Ollama instances. Least-loaded healthy instance selected per
  request. Background health check probes each instance every 30 seconds
  via `GET /api/tags` (3-failure threshold marks unhealthy, single success
  recovers). RAII `OllamaPermit` guard tracks active requests. Backward
  compatible: single `url` field still works, `urls` array enables pool.
- **Agent Onboarding Web UI**: New `/onboarding/` dashboard page with form
  to register agents and generate RS256 JWT tokens. Backend validates
  `agent_id` pattern, creates 1-year JWT, registers in Dolt, saves token
  file. Copy-to-clipboard button, status badges, XSS-safe rendering.
  Two new status-api endpoints: `POST /api/v1/agents/onboard`,
  `GET /api/v1/agents/tokens`.
- New config fields: `[memory_search].query_expansion_enabled`,
  `[memory_search].query_expansion_model`,
  `[memory_search].query_expansion_timeout_seconds`,
  `[memory_search].smart_chunking`, `[chat].streaming_enabled`,
  `[chat].streaming_edit_interval_ms`, `[chat].streaming_min_tokens`,
  `[collaboration].max_declines_per_task`,
  `[collaboration].context_request_timeout_minutes`,
  `[ollama].urls` (multi-instance).
- Migration 029: `task_negotiations` table, negotiation columns on
  `task_queue`.
- 7 new regression tests for onboarding (agent_id validation, JWT claims,
  tilde expansion, payload defaults, token path format).
- 346 workspace unit tests (up from 271).
- **Onboarding POST broken**: `onboarding.js` used `fetchApi()` (GET-only)
  for the POST request. Replaced with proper `postApi()` helper matching
  the control panel pattern.
- **Onboarding XSS**: Agent IDs and display names were rendered as raw HTML.
  All user data now goes through `escapeHtml()`.
- **Missing onboarding CSS**: Alerts, JWT output textarea, form selects, and
  copy button had no styles. Added themed alert-success/alert-error, form
  select matching input style, JWT wrap layout, copy button, loading cell.
- **Backend agent_id validation**: `POST /agents/onboard` now rejects IDs
  with spaces, special characters, or empty strings (matches HTML pattern).

## [0.8.0] - 2026-02-26

### Added

- **Model Upgrade — qwen3:30b-a3b MoE**: Switched primary chat model from
  qwen3:32b (dense, ~25 tok/s) to qwen3:30b-a3b (MoE, 3B active params,
  ~95 tok/s on M4 Max). 3x faster inference at equivalent quality. ~18GB VRAM.
- **Confidence Scoring**: Every LLM response includes `[CONFIDENCE: N/5]`
  (1=guessing to 5=certain). Tag is parsed internally and stripped before
  delivery to the user. Enables automatic quality gating.
- **Verify-then-Respond**: When confidence < 3 and `verifier_model` is
  configured, the response is sent to deepseek-r1:14b for fact-checking
  against user query and memory context. Verifier returns VERIFIED or
  CORRECTED with a replacement response. Low temperature (0.1), short
  timeout, no tools.
- **Dynamic System Prompts**: Per-agent `system_prompt` field in config.toml
  `[agents.*]` sections. a2a-gateway loads system prompt from config at
  runtime instead of using hardcoded strings. Fallback to sensible default
  if not configured.
- **Semantic Search Metadata Filters**: `agent_id`, `date_from`, and
  `date_to` parameters on the `semantic_search` tool. Builds Qdrant
  `filter.must` conditions for targeted retrieval.
- **Hybrid Search Rerank Default**: `hybrid_search` rerank parameter now
  defaults to `config.memory_search.reranker_enabled` instead of hardcoded
  `false`.
- **Content Chunking**: Embedding worker splits long memories (> `chunk_max_tokens`)
  into overlapping word-based windows before embedding. Each chunk stored as a
  separate Qdrant point with deterministic UUID (XOR-derived from base ID).
  Original `memory_id` preserved in all chunk payloads for dedup at search time.
- **FormulaStep Extensions**: Three new optional fields on `FormulaStep`:
  `examples` (few-shot examples appended to prompt), `system_prompt`
  (step-level override), `output_schema` (JSON schema for output validation).
  All backward-compatible via `serde(default)`.
- **Deadlock Detection**: Coordinator `task_timeout_loop` (60s interval) finds
  tasks claimed longer than `task_claim_timeout_minutes` (default 30), resets
  them to pending, re-publishes `task_available` to NATS, and logs the event.
- **A2A Task Dispatch**: a2a-gateway subscribes to NATS
  `{prefix}.{env}.agent.a2a-gateway.task` for coordinator-dispatched tasks.
  `handle_dispatched_task()` performs LLM inference and completes the task
  via bridge. Respects Ollama semaphore to prevent overload.
- **Formula Examples**: Few-shot `examples` added to `synthesize_report`
  (research), `produce_review` (daily-review), and `document_learnings`
  (knowledge-gap) formula steps. `build-feature` formula: `test` and
  `document` steps marked `group = 1` for parallel execution.
- New config fields: `[chat].verifier_model`, `[chat].verifier_timeout_seconds`,
  `[agents.*].system_prompt`, `[collaboration].task_claim_timeout_minutes`,
  `[memory_search].chunk_max_tokens`, `[memory_search].chunk_overlap_tokens`.
- 10 new unit tests in a2a-gateway (dispatch payload parsing, confidence
  scoring, strip confidence tag, strip think tags). 271 workspace total.

### Fixed

- **Think Tag Leakage in Chat**: qwen3:30b-a3b MoE with `think:false` dumps
  chain-of-thought directly into `message.content` — sometimes with only a
  closing `</think>` tag, sometimes with no tags at all (when `num_predict`
  is hit mid-thought). Fixed by switching all Ollama API calls to
  `think:true`, which properly separates thinking into `message.thinking`
  field. `strip_think_tags()` retained as safety net, now handles orphaned
  `</think>` without opening tag.
- **Qdrant Chunk ID Validation**: Chunk point IDs were generated as
  `{uuid}-chunk-{i}` which Qdrant rejected (not a valid UUID). Fixed with
  deterministic UUID generation via byte XOR on the base ID.

## [Unreleased]

## [0.10.0] - 2026-02-27

### Added

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

## [0.9.0] - 2026-02-26

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

[0.11.0]: https://github.com/broodlink/broodlink/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/broodlink/broodlink/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/broodlink/broodlink/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/broodlink/broodlink/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/broodlink/broodlink/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/broodlink/broodlink/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/broodlink/broodlink/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/broodlink/broodlink/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/broodlink/broodlink/releases/tag/v0.3.0

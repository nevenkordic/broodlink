# Broodlink v0.7.0 — Complete Technical Documentation

> Multi-agent AI orchestration system
> Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later
> Generated: 2026-02-23

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Directory Structure](#2-directory-structure)
3. [Configuration (config.toml)](#3-configuration)
4. [Rust Services](#4-rust-services)
   - [beads-bridge](#41-beads-bridge-port-3310)
   - [coordinator](#42-coordinator-no-http)
   - [heartbeat](#43-heartbeat-no-http)
   - [embedding-worker](#44-embedding-worker-no-http)
   - [status-api](#45-status-api-port-3312)
   - [mcp-server](#46-mcp-server-port-3311)
   - [a2a-gateway](#47-a2a-gateway-port-3313)
5. [Shared Crates](#5-shared-crates)
6. [Database Schemas](#6-database-schemas)
7. [NATS Subjects](#7-nats-subjects)
8. [Python Agent SDK](#8-python-agent-sdk)
9. [Dashboard (Hugo)](#9-dashboard-hugo)
10. [Workflow Formulas](#10-workflow-formulas)
11. [Scripts](#11-scripts)
12. [Test Suites](#12-test-suites)
13. [Deployment](#13-deployment)
14. [Authentication & Secrets](#14-authentication--secrets)
15. [Resilience Patterns](#15-resilience-patterns)
16. [Cargo Workspace](#16-cargo-workspace)
17. [Examples](#17-examples)

---

## 1. Architecture Overview

Broodlink is a production-grade multi-agent AI orchestration system where AI agents operate as employees in an organisation. Agents receive tasks, execute them via tool calls, and report results — all with full audit trails.

**Core components:**
- 7 Rust services (5 original + 2 added in v0.3.0)
- 4 shared Rust crates
- Python agent SDK (OpenAI-compatible LLM wrapper)
- Hugo dashboard (WCAG 2.1 AA accessible)
- Dual database: Dolt (versioned brain) + Postgres (hot paths)
- Vector search: Qdrant + Ollama embeddings
- Hybrid memory search: BM25 (Postgres tsvector) + semantic (Qdrant) fusion (v0.4.0)
- Knowledge graph: Entity/relationship extraction from memories with graph traversal (v0.6.0)
- Message bus: NATS with JetStream

**Data flow:**
```
Agent → beads-bridge (JWT auth, rate limit, guardrails) → check_budget (before tool dispatch) → deduct_budget (after success)
                                                        → Dolt/Postgres/Qdrant/NATS
                                                        → audit_log (every call)
                                                        → memory_search_index (on store/delete)
Coordinator ← NATS task_available → claims task → dispatches to agent via NATS
           ← NATS workflow_start → loads formula → chains step tasks sequentially
           → dead_letter_queue (Postgres) → auto-retry with exponential backoff
Heartbeat → 5-min cycle: commit, sync, summarise, expire, compute metrics, sync search index, KG backfill, KG expiry, budget replenishment, chat/session cleanup
Embedding-worker → outbox polling → Ollama → Qdrant → LLM entity extraction → kg_entities/kg_edges
MCP Server → wraps beads-bridge for MCP-compatible agents
A2A Gateway → Google A2A protocol for cross-platform agent interop
           → webhook gateway (Slack/Teams/Telegram) → inbound commands / outbound notifications
           → chat gateway → chat_sessions/chat_messages → task creation → reply_queue → platform delivery
           → direct Ollama chat (Telegram polling) → tool calling (Brave Search) → platform delivery
           → efficiency: concurrency semaphore, search cache, dedup, typing refresh
```

**v0.5.0 highlights:**
- Knowledge graph memory: LLM entity extraction from memories, graph storage (Postgres `kg_entities`/`kg_edges`), Qdrant entity embeddings (`broodlink_kg_entities`)
- 4 new graph tools: `graph_search`, `graph_traverse`, `graph_update_edge`, `graph_stats` (66 tools total)
- Entity resolution with fuzzy matching (exact name → embedding similarity → new entity)
- Multi-hop graph traversal via recursive CTE with cycle prevention
- Edge reinforcement: repeated relationships increase weight
- Temporal edges: `valid_from`/`valid_to` for relationship lifecycle
- Heartbeat KG backfill: queues unprocessed memories for entity extraction
- Dashboard Knowledge Graph page with entity/edge stats, type distribution, top connected entities
- Dashboard fixes: corrected element ID mismatches, API field mappings (counts_by_status, assigned_agent, content_length)

**v0.7.0 highlights:**
- Conversational Agent Gateway: Slack, Teams, and Telegram webhook ingestion in a2a-gateway with automatic chat session creation, context threading, and platform-specific reply delivery
- Telegram Tool Calling with Brave Search: Direct Ollama LLM chat loop with `web_search` tool for real-time queries (weather, news, sports scores). Configurable `num_ctx`, `tool_context_messages`, `max_tool_rounds`. `.env` file loader for secrets.
- Chat Efficiency Safeguards: Ollama concurrency semaphore (configurable, default 1), Brave search result cache (5-min TTL), duplicate message suppression (30s dedup window, checked before DB/bridge work), periodic typing indicator refresh (every 4s via `tokio::select!`), busy/dedup replies excluded from `chat_messages` to prevent history pollution, dedup uses single write lock (no TOCTOU), shared `normalize_search_query` for cache and dedup keys
- Telegram Long-Polling: `getUpdates`-based polling loop with offset persistence in `platform_credentials.meta`, automatic token cache invalidation, typing indicators. Credentials re-fetched after `getUpdates` returns (not before the blocking call), so `allowed_users` is always current when processing updates.
- Platform Credentials: `platform_credentials` table (migration 023) + `meta` JSONB column (migration 024) for bot tokens and runtime state (poll offsets, errors). Access code authentication: random 8-char alphanumeric code generated on registration, stored in `meta->>'auth_code'`; verified user IDs stored in `meta->>'allowed_user_ids'`. Cascade delete on bot disconnect removes `chat_reply_queue`, `chat_messages`, `chat_sessions`, and `platform_credentials` in a single CTE query. NATS-based credential cache invalidation: status-api publishes `{prefix}.platform.credentials_changed` on register/disconnect; a2a-gateway subscribes and immediately sets `telegram_creds` to `None` (eliminates 60s stale cache window).
- Auth Code Security Hardening: Message text from unauthenticated users is never logged (prevents auth code leakage in logs). Inbound webhook log line moved after the auth gate (only logged for authenticated users). Auth code attempts log only "auth code accepted" or "not authenticated" without message content. Auth code messages return/continue before reaching chat history storage or LLM context. Wrong code and random messages receive the same generic response ("Send the access code to use this bot.") — no information leakage about code validity.
- Formula Registry: Persistent formula storage in Postgres `formula_registry` table with CRUD tools (`create_formula`, `get_formula`, `update_formula`, `delete_formula`, `list_formulas`) and seed script for system formulas
- Role-Based Dashboard Access: Session-based RBAC with viewer/operator/admin roles, bcrypt password hashing, login/logout/me auth endpoints, user management CRUD
- Dashboard chat page (`/chat/`) with real-time session monitoring, platform filtering, and message thread view
- Dashboard login page (`/login/`) with auth.js client module using sessionStorage for session tokens
- Users tab in control panel with create/edit/toggle/reset-password actions and role enforcement
- Formulas tab in control panel for formula CRUD and toggle
- Chat session expiry and dashboard session cleanup cycles in heartbeat
- 6 new beads-bridge tools (78 → 84): chat session + formula registry tools; 6 more proactive tools (84 → 90): schedule_task, list_scheduled_tasks, cancel_scheduled_task, send_notification, create_notification_rule, list_notification_rules
- 5 new Postgres migrations (019-021, 023-024)
- 3 new status-api endpoint groups: `/api/v1/chat/*`, `/api/v1/formulas/*`, `/api/v1/users/*`, `/api/v1/auth/*`
- 45 new unit tests (204 → 249)
- 3 new integration test suites: chat-integration (9 tests), formula-registry (11 tests), dashboard-auth (19 tests)
- **Security hardening** (8-pass audit): RBAC on all mutation endpoints, HSTS/CSP/X-Frame-Options on all HTTP services, SSRF protection on webhook URLs, 10 MiB body limits, query LIMIT clamping, fail-closed guardrails and conditions, anchored regex (ReDoS prevention), SQL parameterization in shell scripts, 12-char password policy, session invalidation endpoint, SSE stream caps, Brave cache hard cap, constant-time API key comparison, container hardening, CI security scanning
- Working multi-agent example: `examples/research-report/` — two agents (researcher + writer) collaborate end-to-end using 10 bridge tools, Ollama LLM inference, inter-agent messaging, knowledge graph queries, and persistent memory

**v0.6.0 highlights:**
- Budget enforcement: per-agent token budgets with check/deduct middleware, daily replenishment, BudgetExhausted (402) error
- Dead-letter queue: persistent DLQ in Postgres with auto-retry (exponential backoff), inspect/retry/purge tools
- Workflow branching: conditional steps (`when` expressions), per-step retries with backoff, parallel step groups, step timeouts, `on_failure` error handlers
- KG expiry: entity TTL, edge weight decay, periodic cleanup in heartbeat, `graph_prune` tool
- JWT key rotation: kid-based multi-key validation, JWKS endpoint at `/.well-known/jwks.json`, graceful key rollover
- Multi-agent collaboration: `decompose_task`, `create_workspace`, `workspace_read`/`workspace_write`, `merge_results` tools
- Webhook gateway: Slack slash commands, Teams bot messages, Telegram updates, outbound event notifications (agent.offline, task.failed, budget.low, workflow events, guardrail violations)
- OTLP telemetry: W3C TraceContext propagation activated, Jaeger enabled by default
- Dashboard control panel: `/control/` with 10 tabs (Agents, Budgets, Tasks, Workflows, Guardrails, Webhooks, DLQ, Chat, Formulas, Users), summary metrics, toast notifications, card-based budgets, progress bars
- Qdrant cleanup: `delete_memory` now removes stale vectors from Qdrant (fixes BM25 hybrid search failures)
- Tool count: 66 → 78 (+12 new tools)
- 5 new Postgres migrations (014-018)

**v0.4.0 highlights:**
- Hybrid BM25 + semantic vector memory search with temporal decay and optional reranking
- Postgres full-text search index (`memory_search_index` with GIN-indexed tsvector)
- Heartbeat sync: Dolt `agent_memory` → Postgres `memory_search_index` (catch-up for direct writes)
- `store_memory` / `delete_memory` write-through to search index
- Configurable decay, reranker model, content truncation (`[memory_search]` config)
- mcp-server JWT path fix (`.pem` → `.token`) and non-fatal degradation
- Auto-generated JWTs for service agents (mcp-server, a2a-gateway)
- Dashboard Memory Entries metric now shows actual total count

**Line counts (~39,000+ total):**

| Component | Lines |
|-----------|------:|
| beads-bridge | ~7,904 |
| coordinator | ~2,755 |
| status-api | ~3,846 |
| embedding-worker | 1,576 |
| mcp-server (all) | 1,340 |
| heartbeat | ~1,642 |
| a2a-gateway | ~3,540 |
| broodlink-config | ~1,251 |
| broodlink-secrets | 379 |
| broodlink-runtime | 227 |
| broodlink-telemetry | ~263 |
| style.css | ~1,349 |
| JS modules (all) | ~3,148 |
| Python agent (all) | 811 |
| Examples (all) | 736 |
| Migrations (all) | ~950 |
| Scripts (all) | 1,403 |
| Test suites (shell) | ~5,512 |
| config.toml | ~184 |

---

## 2. Directory Structure

```
broodlink/
├── .beads/formulas/           # 4 system + custom workflow formula definitions
│   ├── build-feature.formula.toml
│   ├── daily-review.formula.toml
│   ├── knowledge-gap.formula.toml
│   ├── research.formula.toml
│   └── custom/
│       └── incident-postmortem.formula.toml
├── agents/                    # Python agent SDK
│   ├── broodlink_agent/       # Package (12 modules, 829 lines)
│   └── requirements.txt
├── examples/                  # Working multi-agent demos (736 lines)
│   └── research-report/       # Two agents collaborate on a research report
│       ├── run.sh             # Orchestrator (143 lines)
│       ├── researcher.py      # Agent 1: research + KG + store findings (257 lines)
│       └── writer.py          # Agent 2: retrieve + synthesize + report (276 lines)
├── crates/                    # Shared Rust libraries
│   ├── broodlink-config/      # Configuration loading (~1,251 lines)
│   ├── broodlink-runtime/     # CircuitBreaker, shutdown, NATS (227 lines)
│   ├── broodlink-secrets/     # Secrets management (379 lines)
│   └── broodlink-telemetry/   # OpenTelemetry integration (~263 lines)
├── launchagents/              # 8 macOS LaunchAgent plist templates
├── migrations/                # 26 SQL migration files (~960 lines)
├── rust/                      # 7 Rust services
│   ├── a2a-gateway/           # A2A protocol + webhook + chat gateway + Ollama tool calling (~3,540 lines)
│   ├── beads-bridge/          # Main tool API (~8,200 lines, 90 tools)
│   ├── coordinator/           # Task routing + workflow orchestration + DLQ + workflow branching + task decomposition (~2,755 lines)
│   ├── embedding-worker/      # Vector + KG extraction pipeline (1,576 lines)
│   ├── heartbeat/             # Health monitor + search sync + KG backfill + KG expiry + budget replenishment + chat/session cleanup (~1,642 lines)
│   ├── mcp-server/            # MCP protocol server (1,340 lines)
│   └── status-api/            # Dashboard API + control panel + auth + user management (~3,846 lines)
├── scripts/                   # Build, deploy, setup (14 scripts, 1,403 lines)
├── status-site/               # Hugo dashboard
│   ├── content/               # 15 content pages
│   └── themes/broodlink-status/
│       ├── layouts/           # HTML templates
│       └── static/            # JS (17 files), CSS (1 file)
├── templates/                 # Agent system prompt template
├── tests/                     # 21 test suites (18 shell + 3 integration)
├── Cargo.toml                 # Workspace (11 members)
├── config.toml                # Central configuration (~184 lines)
├── deny.toml                  # License/dependency audit
├── podman-compose.yaml        # Dev compose (5 infra + 5 services)
└── podman-compose.prod.yaml   # Prod compose (clustered, replicated)
```

---

## 3. Configuration

**File:** `config.toml` (~184 lines)
**Loading:** `BROODLINK_CONFIG` env var → file path, with `BROODLINK_*` env var overrides
**IMPORTANT:** Do NOT set `BROODLINK_PROFILE` (breaks config parsing)

### Complete Config Struct Hierarchy

```rust
pub struct Config {
    pub broodlink: BroodlinkConfig,          // env, version
    pub profile: ProfileConfig,              // deployment profile flags
    pub residency: ResidencyConfig,          // data governance
    pub dolt: DoltConfig,                    // Dolt/MySQL connection
    pub postgres: PostgresConfig,            // Postgres connection
    pub nats: NatsConfig,                    // NATS connection
    pub qdrant: QdrantConfig,               // Qdrant connection
    pub beads: BeadsConfig,                  // Beads workflow engine
    pub ollama: OllamaConfig,               // Ollama embeddings + chat (num_ctx)
    pub secrets: SecretsConfig,             // Secrets provider
    pub status_api: StatusApiConfig,        // Dashboard API
    pub rate_limits: RateLimitsConfig,      // Rate limiting
    pub agents: HashMap<String, AgentConfig>,// Pre-configured agents
    pub telemetry: TelemetryConfig,         // OpenTelemetry
    pub mcp_server: McpServerConfig,        // MCP server
    pub routing: RoutingConfig,             // Smart routing weights
    pub a2a: A2aConfig,                     // A2A gateway + efficiency (semaphore, dedup)
    pub beads_bridge: BeadsBridgeConfig,    // Bridge port
    pub tls: TlsConfig,                     // Inter-service TLS
    pub postgres_read_replicas: ReadReplicaConfig, // Read replicas
    pub memory_search: MemorySearchConfig,  // v0.4.0: Hybrid search + v0.5.0: KG config
    pub budget: BudgetConfig,                 // v0.6.0: Budget enforcement
    pub dlq: DlqConfig,                       // v0.6.0: Dead-letter queue
    pub collaboration: CollaborationConfig,    // v0.6.0: Multi-agent collab
    pub webhooks: WebhookConfig,              // v0.6.0: Webhook gateway
    pub jwt: JwtConfig,                        // v0.6.0: JWT key rotation
    pub chat: ChatConfig,                       // v0.7.0: Conversational gateway + tools (ChatToolsConfig)
    pub dashboard_auth: DashboardAuthConfig,    // v0.7.0: Role-based dashboard access
    pub notifications: NotificationsConfig,     // post-v0.7.0: Proactive notification rules
}
```

### All Fields with Types and Defaults

| Section | Field | Type | Default |
|---------|-------|------|---------|
| **broodlink** | env | String | — |
| | version | String | — |
| **profile** | name | String | — |
| | tls_interservice | bool | false |
| | nats_cluster | bool | false |
| | dolt_replication | bool | false |
| | postgres_replicas | u32 | 0 |
| | qdrant_distributed | bool | false |
| | secrets_provider | String | "sops" |
| | audit_residency | bool | false |
| **residency** | region | String | — |
| | data_classification | String | — |
| | allowed_egress | Vec\<String\> | [] |
| **dolt** | host | String | — |
| | port | u16 | — |
| | database | String | — |
| | user | String | — |
| | password_key | String | — |
| | min_connections | u32 | 2 |
| | max_connections | u32 | 5 |
| **postgres** | host | String | — |
| | port | u16 | — |
| | database | String | — |
| | user | String | — |
| | password_key | String | — |
| | min_connections | u32 | 5 |
| | max_connections | u32 | 20 |
| **nats** | url | String | — |
| | subject_prefix | String | — |
| | cluster_urls | Vec\<String\> | [] |
| **qdrant** | url | String | — |
| | collection | String | — |
| | api_key | String | — |
| **beads** | workspace | String | — |
| | bd_binary | String | — |
| | formulas_dir | String | — |
| **ollama** | url | String | — |
| | embedding_model | String | — |
| | timeout_seconds | u64 | 30 |
| | num_ctx | u32 | 4096 |
| **secrets** | provider | String | — |
| | sops_file | Option\<String\> | None |
| | age_identity | Option\<String\> | None |
| | infisical_url | Option\<String\> | None |
| **status_api** | port | u16 | — |
| | cors_origins | Vec\<String\> | [] |
| | api_key_name | String | — |
| **rate_limits** | requests_per_minute_per_agent | u32 | 60 |
| | burst | u32 | 10 |
| **agents.\*** | agent_id | String | — |
| | role | String | — |
| | transport | String | — |
| | cost_tier | String | — |
| | preferred_formulas | Vec\<String\> | [] |
| **telemetry** | enabled | bool | false |
| | otlp_endpoint | String | "http://localhost:4317" |
| | sample_rate | f64 | 1.0 |
| **mcp_server** | enabled | bool | true |
| | transport | String | "http" |
| | port | u16 | 3311 |
| | bridge_url | String | "http://localhost:3310" |
| | agent_id | String | "mcp-server" |
| | jwt_token_path | Option\<String\> | None |
| | session_timeout_minutes | u32 | 30 |
| | cors_origins | Vec\<String\> | [] |
| **routing** | max_concurrent_default | u32 | 5 |
| | new_agent_bonus | f64 | 1.0 |
| **routing.weights** | capability | f64 | 0.35 |
| | success_rate | f64 | 0.25 |
| | availability | f64 | 0.20 |
| | cost | f64 | 0.15 |
| | recency | f64 | 0.05 |
| **a2a** | enabled | bool | false |
| | port | u16 | 3313 |
| | api_key_name | String | "BROODLINK_A2A_API_KEY" |
| | bridge_url | String | "http://localhost:3310" |
| | bridge_jwt_path | Option\<String\> | None |
| | ollama_concurrency | u32 | 1 |
| | busy_message | String | "I'm currently processing..." |
| | dedup_window_secs | u64 | 30 |
| **beads_bridge** | port | u16 | 3310 |
| **tls** | cert_path | Option\<String\> | None |
| | key_path | Option\<String\> | None |
| | ca_path | Option\<String\> | None |
| **postgres_read_replicas** | urls | Vec\<String\> | [] |
| **memory_search** | decay_lambda | f64 | 0.01 |
| | reranker_model | String | "snowflake-arctic-embed2:137m" |
| | reranker_enabled | bool | true |
| | max_content_length | usize | 2000 |
| | kg_enabled | bool | true |
| | kg_extraction_model | String | "qwen3:1.7b" |
| | kg_entity_similarity_threshold | f64 | 0.85 |
| | kg_max_hops | u32 | 3 |
| | kg_extraction_timeout_seconds | u64 | 120 |
| | kg_entity_ttl_days | u32 | 365 |
| | kg_edge_decay_rate | f64 | 0.01 |
| | kg_cleanup_interval_hours | u32 | 24 |
| | kg_min_mention_count | u32 | 1 |
| **budget** | enabled | bool | true |
| | daily_replenishment | i64 | 100000 |
| | replenish_hour_utc | u32 | 0 |
| | low_budget_threshold | i64 | 1000 |
| | default_tool_cost | i64 | 1 |
| **dlq** | auto_retry_enabled | bool | true |
| | max_retries | u32 | 3 |
| | backoff_base_ms | u64 | 1000 |
| | check_interval_secs | u64 | 60 |
| **collaboration** | max_sub_tasks | u32 | 10 |
| | workspace_ttl_hours | u32 | 24 |
| **webhooks** | enabled | bool | true |
| | slack_signing_secret | String | "" |
| | teams_app_id | String | "" |
| | telegram_bot_token | String | "" |
| **jwt** | keys_dir | String | "~/.broodlink" |
| | grace_period_hours | u32 | 24 |
| **chat** | enabled | bool | true |
| | chat_model | String | "qwen3:32b" |
| | max_context_messages | u32 | 10 |
| | session_timeout_hours | u32 | 24 |
| | reply_timeout_seconds | u64 | 300 |
| | reply_retry_attempts | u32 | 3 |
| | default_agent_role | String | "worker" |
| | greeting_enabled | bool | true |
| | greeting_message | String | "Hello! I'm a Broodlink agent..." |
| **chat.tools** | web_search_enabled | bool | false |
| | max_tool_rounds | u32 | 3 |
| | search_result_count | u32 | 5 |
| | tool_context_messages | u32 | 4 |
| | search_cache_ttl_secs | u64 | 300 |
| **dashboard_auth** | enabled | bool | false |
| | session_ttl_hours | u32 | 8 |
| | bcrypt_cost | u32 | 12 |
| | max_sessions_per_user | u32 | 5 |
| **notifications** | enabled | bool | true |
| | default_cooldown_minutes | u32 | 30 |

### v0.4.0 + v0.6.0 Config: `[memory_search]`

```toml
[memory_search]
decay_lambda = 0.01                                # exponential decay rate (half-life ~69 days, 0 = disabled)
reranker_model = "snowflake-arctic-embed2:137m"    # dedicated reranker model
reranker_enabled = true                            # set false to disable reranking globally
max_content_length = 2000                          # truncate content in results (chars, 0 = no limit)
kg_enabled = true                                  # enable knowledge graph entity extraction
kg_extraction_model = "qwen3:1.7b"                 # model for entity/relationship extraction
kg_entity_similarity_threshold = 0.85              # cosine threshold for entity deduplication
kg_max_hops = 3                                    # max traversal depth for graph_traverse
kg_extraction_timeout_seconds = 120                # timeout for LLM extraction calls
kg_entity_ttl_days = 365                           # entity expiry (days since last_seen)
kg_edge_decay_rate = 0.01                          # edge weight decay rate per day
kg_cleanup_interval_hours = 24                     # how often heartbeat runs KG cleanup
kg_min_mention_count = 1                           # entities below this are candidates for cleanup
```

### v0.6.0 Config: New Sections

```toml
[budget]
enabled = true                          # enable budget enforcement middleware
daily_replenishment = 100000            # tokens replenished per agent per day
replenish_hour_utc = 0                  # hour (UTC) to replenish budgets
low_budget_threshold = 1000             # trigger budget.low webhook event
default_tool_cost = 1                   # cost for tools not in tool_cost_map

[dlq]
auto_retry_enabled = true               # automatically retry dead-lettered tasks
max_retries = 3                         # max retry attempts before permanent failure
backoff_base_ms = 1000                  # exponential backoff base (1s, 2s, 4s, ...)
check_interval_secs = 60               # how often to check for retryable DLQ entries

[collaboration]
max_sub_tasks = 10                      # max sub-tasks per decompose_task call
workspace_ttl_hours = 24                # shared workspace auto-expiry

[webhooks]
enabled = true                          # enable webhook gateway
slack_signing_secret = ""               # Slack request signing secret
teams_app_id = ""                       # Microsoft Teams app ID
telegram_bot_token = ""                 # Telegram bot token

[jwt]
keys_dir = "~/.broodlink"              # directory containing JWT keypairs
grace_period_hours = 24                 # old keys remain valid after rotation
```

### v0.7.0 Config: New Sections

```toml
[chat]
enabled = true                             # enable conversational agent gateway
chat_model = "qwen3:32b"                   # Ollama model for direct LLM chat
max_context_messages = 10                  # recent messages included in task context
session_timeout_hours = 24                 # inactive sessions auto-close after this
reply_timeout_seconds = 300                # max wait for agent reply before timeout
reply_retry_attempts = 3                   # max delivery retries for platform replies
default_agent_role = "worker"              # role for routing chat tasks
greeting_enabled = true                    # send greeting on first message
greeting_message = "Hello! I'm a Broodlink agent. How can I help?"

[chat.tools]
web_search_enabled = true                  # enable Brave Search tool calling
max_tool_rounds = 3                        # max LLM→tool→LLM round-trips per message
search_result_count = 5                    # number of Brave results per query
tool_context_messages = 4                  # conversation history messages when tools active (keeps prompt small for CPU)
search_cache_ttl_secs = 300                # Brave result cache TTL (5 min, 0 = disabled)

[dashboard_auth]
enabled = false                            # enable session-based dashboard auth (disabled = API key only)
session_ttl_hours = 8                      # session token expiry
bcrypt_cost = 12                           # bcrypt hash cost factor
max_sessions_per_user = 5                  # oldest sessions deleted when exceeded
```

### v0.7.0 Config: Efficiency Fields on Existing Sections

```toml
[ollama]
num_ctx = 4096                             # KV cache context window (affects VRAM; 4096 sufficient for tool calling)

[a2a]
ollama_concurrency = 1                     # max concurrent Ollama chat calls (semaphore permits)
dedup_window_secs = 30                     # suppress duplicate messages within this window
# busy_message = "I'm currently processing another request. Please try again in a moment."
```

### Dev Config Values

```toml
[broodlink]
env = "local", version = "0.1.0"

[profile]
name = "dev", tls_interservice = false

[dolt]
host = "127.0.0.1", port = 3307, database = "agent_ledger", user = "root"

[postgres]
host = "127.0.0.1", port = 5432, database = "broodlink_hot", user = "postgres"

[nats]
url = "nats://localhost:4222", subject_prefix = "broodlink"

[qdrant]
url = "http://localhost:6333", collection = "broodlink_memory"

[ollama]
url = "http://localhost:11434", embedding_model = "nomic-embed-text", timeout_seconds = 120, num_ctx = 4096

[agents.claude]
role = "strategist", transport = "mcp", cost_tier = "high"
preferred_formulas = ["research", "build-feature"]

[agents.qwen3]
role = "worker", transport = "pymysql", cost_tier = "low"
preferred_formulas = ["daily-review", "knowledge-gap"]
```

---

## 4. Rust Services

### 4.1 beads-bridge (Port 3310)

**File:** `rust/beads-bridge/src/main.rs` (~7,904 lines)
**Purpose:** Central tool execution gateway for all agents
**Auth:** JWT RS256 Bearer token
**Deps:** Dolt, Postgres, NATS, Qdrant, Ollama

#### Routes

| Method | Path | Handler | Auth |
|--------|------|---------|------|
| GET | /health | health_handler | No |
| GET | /api/v1/tools | tools_metadata_handler | No |
| POST | /api/v1/tool/:tool_name | tool_dispatch | JWT |
| GET | /api/v1/stream/:stream_id | sse_stream_handler | JWT |
| GET | /api/v1/.well-known/jwks.json | jwks_handler | No |

#### Tool Registry (90 tools)

**Memory (5):** store_memory, recall_memory, delete_memory, semantic_search, **hybrid_search** (v0.4.0)
**Knowledge Graph (5):** **graph_search**, **graph_traverse**, **graph_update_edge**, **graph_stats** (v0.5.0), **graph_prune** (v0.6.0)
**Work (2):** log_work, get_work_log
**Projects (3):** list_projects, add_project, update_project
**Skills (2):** list_skills, add_skill
**Conversations (1):** log_conversation
**Beads (7):** beads_list_issues, beads_get_issue, beads_update_status, beads_create_issue, beads_list_formulas, beads_run_formula, beads_get_convoy
**Messaging (2):** send_message, read_messages
**Decisions (2):** log_decision, get_decisions
**Queue (6):** create_task, get_task, list_tasks, claim_task, complete_task, fail_task
**Agent (1):** agent_upsert
**Utility (10):** health_check, get_config_info, list_agents, get_agent, get_audit_log, get_daily_summary, get_commits, get_memory_stats, get_convoy_status, ping
**Guardrails (3):** set_guardrail, list_guardrails, get_guardrail_violations
**Approvals (6):** set_approval_policy, list_approval_policies, create_approval_gate, resolve_approval, list_approvals, get_approval
**Routing (1):** get_routing_scores
**Delegation (6):** delegate_task, accept_delegation, reject_delegation, complete_delegation, get_delegation, list_delegations
**Streaming (2):** start_stream, emit_stream_event
**A2A (2):** a2a_discover, a2a_delegate
**Workflow (1):** start_workflow
**Budget (3):** **get_budget**, **set_budget**, **get_cost_map** (v0.6.0)
**DLQ (3):** **inspect_dlq**, **retry_dlq_task**, **purge_dlq** (v0.6.0)
**Collaboration (2):** **decompose_task**, **merge_results** (v0.6.0)
**Chat (4):** **list_chat_sessions**, **reply_to_chat**, **close_chat_session**, **assign_chat_agent** (v0.7.0)
**Formula Registry (5):** **create_formula**, **get_formula**, **update_formula**, **delete_formula**, **list_formulas** (v0.7.0)
**Proactive (6):** **schedule_task**, **list_scheduled_tasks**, **cancel_scheduled_task**, **send_notification**, **create_notification_rule**, **list_notification_rules** (post-v0.7.0)

#### AppState

```rust
pub struct AppState {
    pub dolt: MySqlPool,
    pub pg: PgPool,
    pub pg_read: Option<PgPool>,          // v0.3.0: read replica pool
    pub nats: async_nats::Client,
    pub config: Arc<Config>,
    pub secrets: Arc<dyn SecretsProvider>,
    pub rate_limiter: RateLimiter,
    pub rate_override_buckets: RwLock<HashMap<String, TokenBucket>>,
    pub qdrant_breaker: CircuitBreaker,
    pub ollama_breaker: CircuitBreaker,
    pub reranker_breaker: CircuitBreaker, // v0.4.0: dedicated reranker circuit breaker
    pub kg_extraction_breaker: CircuitBreaker, // v0.5.0: KG entity extraction circuit breaker
    pub budget_breaker: CircuitBreaker,       // v0.6.0: Budget enforcement circuit breaker
    pub start_time: Instant,
    pub jwt_decoding_keys: Vec<(String, jsonwebtoken::DecodingKey)>, // v0.6.0: kid-based multi-key
    pub jwt_validation: jsonwebtoken::Validation,
}
```

#### v0.4.0: Hybrid Memory Search

The `hybrid_search` tool fuses BM25 keyword search (Postgres tsvector) with semantic vector search (Qdrant) using min-max normalised weighted scoring, optional temporal decay, and optional reranking.

**Parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| query | String | required | Search query |
| limit | i64 | 10 | Max results |
| agent_id | String | None | Filter by agent |
| semantic_weight | f64 | 0.6 | Weight for vector scores |
| keyword_weight | f64 | 0.4 | Weight for BM25 scores |
| decay | bool | true | Apply temporal decay |
| rerank | bool | false | Apply reranker pass |

**Algorithm:**

1. Run BM25 (`bm25_search`) and semantic (`semantic_search_raw`) in parallel via `tokio::join!`
2. BM25: `ts_rank_cd(tsv, plainto_tsquery(...), 32)` on Postgres `memory_search_index`
3. Semantic: Ollama embedding → Qdrant cosine search
4. Min-max normalise each score set to [0, 1]
5. Build candidate map keyed by `(agent_id, topic)`, accumulate `fused_score += norm_score * weight`
6. Fill missing content for semantic-only results from Postgres (fallback: Dolt)
7. Apply temporal decay: `fused_score *= e^(-lambda * age_days)` where `lambda = config.memory_search.decay_lambda`
8. Sort by fused_score descending
9. Optional reranking: embed `"query: {q} document: {topic}: {content}"` via dedicated reranker model, replace fused_score with L2 norm of embedding, re-sort
10. Truncate to limit

**Graceful degradation:**
- BM25 fails → `method: "semantic_fallback"` (vector-only results)
- Qdrant/Ollama circuit open → `method: "bm25_fallback"` (keyword-only results)
- Both fail → `method: "none"`, empty results
- Both succeed → `method: "hybrid"` (or `"hybrid_reranked"` if reranker used)

**Response:**
```json
{
  "results": [
    {
      "memory_id": 42,
      "agent_id": "claude",
      "topic": "some-topic",
      "content": "truncated content...",
      "tags": "tag1, tag2",
      "score": 0.847,
      "bm25_score": 0.723,
      "vector_score": 0.912,
      "created_at": "2026-02-20 ...",
      "updated_at": "2026-02-20 ..."
    }
  ],
  "query": "...",
  "total": 5,
  "method": "hybrid"
}
```

**Helper functions (v0.4.0):**

| Function | Purpose |
|----------|---------|
| `get_ollama_embedding` | Get 768-dim vector from Ollama, with circuit breaker |
| `semantic_search_raw` | Qdrant search returning `(topic, agent_id, score, created_at)` |
| `bm25_search` | Postgres tsvector search returning full row + BM25 score |
| `min_max_normalize` | Normalise scores to [0, 1] |
| `temporal_decay` | `e^(-lambda * age_days)`, parses multiple date formats |
| `truncate_content` | Truncate to `max_content_length` chars with `…` suffix |
| `cosine_similarity` | Dot product / (norm_a * norm_b) |

**Modified tools (v0.4.0):**

- **`store_memory`**: After Dolt upsert and outbox write, now also UPSERTs into Postgres `memory_search_index` (non-fatal on failure). v0.6.0: outbox payload now includes `memory_id` and `tags` for KG entity-memory linking.
- **`delete_memory`**: After Dolt delete, now also DELETEs from Postgres `memory_search_index` (non-fatal). v0.6.0: also removes stale vectors from Qdrant `broodlink_memory` collection (fixes BM25 hybrid search failures from orphaned vectors)

#### v0.5.0: Knowledge Graph Tools

**`graph_search`** — Text search (`ILIKE`) + embedding similarity (Qdrant `broodlink_kg_entities`). Merges/dedup results. Optionally fetches edges per entity via JOIN on `kg_edges`.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| query | String | required | Search query |
| entity_type | String | None | Filter by entity type |
| include_edges | bool | false | Include edges for each entity |
| limit | i64 | 10 | Max results |

**`graph_traverse`** — Recursive CTE in Postgres. Base case finds start entity by name, recursive case follows edges respecting direction/relation_type filters with cycle prevention (`NOT entity_id = ANY(path)`). Max hops capped by config `kg_max_hops`.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| start_entity | String | required | Entity name to start from |
| max_hops | i32 | 2 | Max traversal depth |
| relation_types | String | None | Comma-separated relation type filter |
| direction | String | "both" | "outgoing", "incoming", or "both" |

**`graph_update_edge`** — Lookup entity_ids by name, find active edge, then either invalidate (`valid_to = NOW()`) or update description. Writes audit_log.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| source_entity | String | required | Source entity name |
| target_entity | String | required | Target entity name |
| relation_type | String | required | Relationship type |
| action | String | required | "invalidate" or "update_description" |
| description | String | None | New description (for update_description) |

**`graph_stats`** — Parallel `tokio::join!` of 7 count/aggregate queries. Returns entity/edge totals, type distribution, top relations, most connected entities.

**Response (graph_stats):**
```json
{
  "total_entities": 42,
  "total_edges": 128,
  "active_edges": 115,
  "historical_edges": 13,
  "entity_types": [{"type": "service", "count": 15}, ...],
  "top_relations": [{"relation": "DEPENDS_ON", "count": 32}, ...],
  "most_connected": [{"name": "redis-cache", "connections": 8}, ...]
}
```

#### Error Types

| Variant | HTTP Status |
|---------|-------------|
| Auth | 401 |
| Validation | 400 |
| NotFound | 404 |
| RateLimited | 429 |
| CircuitOpen | 503 |
| Guardrail | 403 |
| BudgetExhausted | 402 |
| Database/Nats/Internal | 500 |

#### Request/Response Format

```
POST /api/v1/tool/{tool_name}
Authorization: Bearer {JWT}
Content-Type: application/json

{"params": {"key": "value"}}

→ {"tool": "tool_name", "success": true, "data": {...}}
```

#### v0.6.0: Budget Enforcement Middleware

Budget enforcement wraps every tool dispatch with check/deduct logic:

1. **`check_budget`** (before dispatch): Queries `agent_profiles.budget_tokens` for the calling agent. If budget <= 0 and `budget.enabled`, returns `BudgetExhausted` (HTTP 402). Reads tool cost from `tool_cost_map` (Postgres), falling back to `budget.default_tool_cost`.
2. **`deduct_budget`** (after success): Decrements `agent_profiles.budget_tokens` by tool cost, inserts `budget_transactions` row with trace_id for audit. If balance drops below `budget.low_budget_threshold`, publishes `budget.low` event for webhook notifications.

Read-only tools (same whitelist as guardrails) are exempt from budget checks.

#### Unit Tests (83)

Circuit breaker (3), rate limiter (2), parameter extraction (8), tool registry (3), error responses (7), guardrail policy matching (5), tool request parsing (3), tool response serialisation (1), claims deserialisation (2), **hybrid search helpers (19)**: min-max normalise (3), temporal decay (4), truncate content (3), cosine similarity (2), BM25 query construction (2), score fusion (3), fallback method detection (2), **knowledge graph (18)**: graph search parameter extraction, graph traverse parameter extraction, graph update edge parameter extraction, graph stats response, entity resolution, edge reinforcement, cycle prevention, readonly tool classification, **chat tools (5)** (v0.7.0): list_chat_sessions param extraction, reply_to_chat param extraction, chat tools in registry, list_chat_sessions readonly, reply_to_chat not readonly, **formula tools (7)** (v0.7.0): validate_formula valid/missing_step_name/invalid_param_type/bad_on_failure_ref/empty_steps, formula tools in registry, formula readonly tools.

---

### 4.2 coordinator (No HTTP)

**File:** `rust/coordinator/src/main.rs` (~2,755 lines)
**Purpose:** NATS-driven task routing with smart scoring + workflow orchestration + DLQ + task decomposition + scheduled task promotion
**Deps:** Dolt, Postgres, NATS

#### Task Routing Algorithm

1. Receive `TaskAvailablePayload` on NATS
2. Query Dolt `agent_profiles` for active agents matching role
3. Query Postgres `agent_metrics` for each candidate
4. Score all candidates via `compute_score()`
5. Sort descending by score, attempt claim on highest first
6. On claim contention, try next candidate (not backoff on same)
7. Backoff only after ALL candidates exhausted (exp: 1s, 2s, 4s, 8s, 16s)
8. Dead-letter after MAX_CLAIM_RETRIES (5) exhausted

#### Scoring Formula

```
score = capability * 0.35
      + success_rate * 0.25
      + availability * 0.20
      + cost * 0.15
      + recency * 0.05

Where:
  capability:   1.0 if formula matches agent capabilities, 0.5 otherwise
  success_rate: from agent_metrics, or new_agent_bonus (1.0) if no history
  availability: (1.0 - current_load / max_concurrent).clamp(0.0, 1.0)
  cost:         low=1.0, medium=0.6, high=0.3
  recency:      1.0 if last_seen < 60s ago, 0.0 if > 30min, linear between
```

#### Key Structs

```rust
TaskAvailablePayload { task_id, task_type?, role?, cost_tier? }
TaskDispatchPayload { task_id, title, description?, priority, formula_name?, convoy_id?, assigned_by }
WorkflowStartPayload { workflow_id, convoy_id, formula_name, params, started_by }
TaskCompletedPayload { task_id, agent_id, result_data? }
TaskFailedPayload { task_id, agent_id }
EligibleAgent { agent_id, cost_tier, capabilities?, max_concurrent, last_seen? }
AgentMetrics { success_rate: f64, current_load: i32 }
ScoredAgent { agent, score: f64, breakdown: ScoreBreakdown }
ScoreBreakdown { capability, success_rate, availability, cost, recency }
RoutingDecisionPayload { task_id, agent_id, score, breakdown, candidates_evaluated, attempt }
MetricsPayload { service, tasks_claimed, tasks_dead_lettered, claim_retries_total, uptime_seconds }
FormulaFile { formula: FormulaMetadata, steps: Vec<FormulaStep>, on_failure: Option<String> } // v0.6.0: on_failure error handler
FormulaStep { name, agent_role, tools?, prompt, input?, output, when?: String, retries?: u32, backoff?: u64, timeout_seconds?: u64, group?: String } // v0.6.0: conditional, retries, parallel groups, timeouts
DeadLetterEntry { id, task_id, reason, source_service, retry_count, max_retries, next_retry_at, resolved, resolved_by, created_at } // v0.6.0
TaskDecomposition { id, parent_task_id, child_task_id, merge_strategy, created_at } // v0.6.0
```

#### Workflow Orchestration

Sequential formula step chaining — the coordinator subscribes to 3 additional NATS subjects:

1. **`workflow_start`** — `handle_workflow_start`: loads formula TOML from `.beads/formulas/`, validates steps, updates `workflow_runs` (status→running, total_steps), creates step 0 task via `create_step_task`
2. **`task_completed`** — `handle_task_completed`: checks if task has `workflow_run_id`; if yes, loads formula, accumulates `result_data` into `step_results[output_name]`, creates next step task or marks workflow completed
3. **`task_failed`** — `handle_task_failed`: checks if task has `workflow_run_id`; if yes, marks workflow as failed (no further steps)

`create_step_task` renders the formula step prompt with `{{key}}` template replacement, appends prior step outputs as context (based on step's `input` field), inserts into `task_queue` with workflow metadata, and publishes `task_available` to trigger routing to the step's `agent_role`.

#### v0.6.0: Dead-Letter Queue Persistence

When a task exhausts all claim retries (MAX_CLAIM_RETRIES = 5), it is now persisted to the `dead_letter_queue` table in Postgres (previously only published to NATS). A background loop checks the DLQ every `dlq.check_interval_secs` (default 60s) and auto-retries entries where `retry_count < max_retries` and `next_retry_at <= NOW()`. Backoff is exponential: `backoff_base_ms * 2^retry_count`. After `max_retries`, the entry remains unresolved for manual inspection via `inspect_dlq` / `retry_dlq_task` / `purge_dlq` tools.

#### v0.6.0: Workflow Branching

FormulaStep now supports:
- **`when`** — Conditional expression evaluated against `step_results`. If false, step is skipped.
- **`retries`** — Per-step retry count with exponential `backoff` (ms).
- **`timeout_seconds`** — Step-level timeout. On timeout, the step fails and triggers retries or `on_failure`.
- **`group`** — Steps with the same `group` name run in parallel. The workflow waits for all group members to complete before advancing.
- **`on_failure`** (FormulaFile level) — Name of a formula step to run when any step fails (error handler).

#### v0.6.0: Multi-Agent Task Decomposition

The `decompose_task` tool splits a parent task into sub-tasks, tracked via `task_decompositions` table. The coordinator monitors sub-task completion and triggers `merge_results` when all children complete. Merge strategies: `concatenate` (default), `vote` (majority result), `best` (highest-scoring result).

#### Scheduled Task Promotion (post-v0.7.0)

Background loop (`scheduled_task_loop`) runs on 60-second interval alongside DLQ retry loop:

1. Query `scheduled_tasks WHERE enabled = TRUE AND next_run_at <= NOW() ORDER BY next_run_at LIMIT 10`
2. For each due task:
   - INSERT into `task_queue` (id, trace_id, title, description, priority, formula_name)
   - Publish NATS `{prefix}.{env}.coordinator.task_available`
   - Increment `run_count`, set `last_run_at = NOW()`
   - If one-shot (`recurrence_secs IS NULL`): set `enabled = false`
   - If recurring: advance `next_run_at += make_interval(secs => recurrence_secs)`; if `max_runs` reached, disable

#### Unit Tests (31)

Backoff calculation (1), cost tier ordering (1), cost tier score values (1), compute_score variants (4), rank_agents (2), recency score (4), task payload deserialisation (3), routing decision (1), render_prompt (4), workflow payload deserialisation (3), formula parsing (5), formula input types (2).

---

### 4.3 heartbeat (No HTTP)

**File:** `rust/heartbeat/src/main.rs` (~1,642 lines)
**Purpose:** 5-minute health/sync cycle
**Interval:** 300 seconds
**Deps:** Dolt, Postgres, NATS, bd CLI

#### Cycle Steps

1. **Count Dolt writes** — `SELECT COUNT(*) FROM dolt_status`
2. **Dolt commit** — `CALL DOLT_ADD('-A')` then `CALL DOLT_COMMIT('-m', ?)`
3. **Sync Beads issues** — run `bd issue list --json`, upsert into `beads_issues` (Dolt)
4. **Update daily summary** — gather counts from PG/Dolt, upsert into `daily_summary` (Dolt)
5. **Activate recent agents** — check `audit_log` and `agent_memory` for 15-min activity
5e. **Sync memory_search_index** (v0.4.0) — query Dolt `agent_memory` for rows updated since last sync, upsert into Postgres `memory_search_index` (catches direct Dolt writes not going through `store_memory`)
5f. **Sync KG unprocessed memories** (v0.5.0) — find MAX(memory_id) in `kg_entity_memories`, fetch Dolt `agent_memory` rows with id > max, insert into outbox with `operation='kg_extract'` for entity extraction by embedding-worker
5g. **KG cleanup cycle** (v0.6.0) — Runs every `kg_cleanup_interval_hours` (default 24h): decay edge weights (`weight *= (1 - kg_edge_decay_rate)` per day since last update), soft-delete low-weight edges (`weight < 0.1`), remove stale entities (`last_seen` older than `kg_entity_ttl_days` AND `mention_count < kg_min_mention_count`), orphan cleanup (entities with no remaining edges or memory links)
5h. **Budget replenishment** (v0.6.0) — At `budget.replenish_hour_utc` (default midnight UTC): for each active agent, set `budget_tokens = budget.daily_replenishment` if current balance < daily_replenishment. Inserts `budget_transactions` audit row with action "replenish".
5i. **Chat session expiry** (v0.7.0) — Closes chat sessions where `last_message_at` exceeds `chat.session_timeout_hours` (default 24h). Sets status to "expired".
5j. **Dashboard session cleanup** (v0.7.0) — Deletes expired rows from `dashboard_sessions` where `expires_at < NOW()`.
5k. **Formula registry sync** (post-v0.7.0) — Bidirectional TOML↔Postgres sync with definition_hash skip.
5l. **Skill registry sync** (post-v0.7.0) — Syncs skills from config.toml agent definitions to Dolt.
5n. **Notification rule evaluation** (post-v0.7.0) — Queries `notification_rules WHERE enabled = TRUE`. Evaluates conditions: `service_event_error` (error count in 15 min > threshold), `dlq_spike` (unresolved DLQ > threshold), `budget_low` (agents below budget threshold). Checks cooldown (`last_triggered_at + cooldown_minutes`). On trigger: renders template with `{{count}}`/`{{agents}}`/`{{type}}` variables, inserts `notification_log`, publishes NATS `{prefix}.{env}.notification.send`, updates `last_triggered_at`. If `condition_config.auto_postmortem = true` on `service_event_error`, also publishes `workflow_start` for `incident-postmortem` formula.
6. **Deactivate stale agents** — `last_seen < NOW() - 1 HOUR` → `active = false`
7. **Expire approval gates** — pending gates past `expires_at` → status `expired`, linked tasks too
8. **Compute agent_metrics** — for each active agent: completed/failed counts, avg duration, load, success_rate → upsert `agent_metrics` (PG)
9. **Publish health** — NATS `{prefix}.{env}.health`
10. **Publish status** — NATS `{prefix}.{env}.status`
11. **Write audit_log** — Postgres
12. **Write residency_log** — Postgres (if `audit_residency` enabled)

#### v0.4.0: sync_memory_search_index

```rust
async fn sync_memory_search_index(dolt: &MySqlPool, pg: &PgPool) -> Result<()> {
    // 1. Find MAX(updated_at) from Postgres memory_search_index
    // 2. Fetch Dolt agent_memory rows WHERE updated_at > last_sync LIMIT 100
    // 3. For each row: UPSERT into memory_search_index with tags JSON→CSV conversion
    //    ON CONFLICT (agent_id, topic) DO UPDATE
}
```

This ensures that memories written directly to Dolt (e.g., by agents using pymysql) are eventually reflected in the BM25 search index.

#### Key Structs

```rust
HealthPayload { service, version, trace_id, uptime_seconds, dolt_ok, postgres_ok, nats_ok, timestamp }
StatusPayload { service, trace_id, cycle_duration_ms, dolt_writes, beads_synced, agents_activated,
                agents_deactivated, daily_summary_updated, timestamp }
BeadsIssue { bead_id, title, description?, status?, assignee?, convoy_id?, dependencies?, formula?,
             created_at?, updated_at? }
```

#### Unit Tests

None currently (heartbeat logic is tested via integration tests in `tests/run-all.sh`).

---

### 4.4 embedding-worker (No HTTP)

**File:** `rust/embedding-worker/src/main.rs` (1,576 lines)
**Purpose:** Outbox → Ollama embeddings → Qdrant vector upsert → LLM entity extraction → knowledge graph
**Poll interval:** 2 seconds
**Batch size:** 10
**Deps:** Postgres, Ollama, Qdrant, NATS

#### Pipeline

**For `operation = 'embed'` (standard memory storage):**
1. Poll `outbox` table for pending rows
2. Extract payload: `{content, topic, agent_id, memory_id, tags}`
3. POST to Ollama `/api/embeddings` with model `nomic-embed-text` → 768-dim vector
4. Upsert to Qdrant `broodlink_memory` collection (named vector "default")
5. Update Dolt `agent_memory.embedding_ref` with Qdrant point UUID
6. **KG entity extraction** (v0.6.0, non-fatal): call `extract_entities()` → `resolve_entity()` → `resolve_edges()`
7. Mark outbox row as `done`

**For `operation = 'kg_extract'` (KG-only, used by heartbeat backfill):**
1. Skip embedding/Qdrant steps
2. Run only KG entity extraction pipeline (step 6 above)
3. Mark outbox row as `done`

**Resilience:** Circuit breakers on Ollama, Qdrant, and KG extraction (5 failures, 30s recovery each). Max 3 attempts per outbox row before marking `failed`. KG extraction is always non-fatal — embedding succeeds even if extraction fails.

#### v0.6.0: Entity Extraction Pipeline

**`extract_entities()`** — Calls Ollama `/api/generate` with structured extraction prompt. Uses `kg_extraction_model` (default: `qwen3:1.7b`). Strips markdown code fences, parses JSON into `ExtractedEntities`. Circuit breaker protected. Timeout from config `kg_extraction_timeout_seconds`.

**`resolve_entity()`** — Entity resolution with deduplication:
1. **Exact match:** `SELECT entity_id FROM kg_entities WHERE lower(name) = lower($1)`
2. **Fuzzy match:** Embed entity name → search Qdrant `broodlink_kg_entities` → merge if cosine >= `kg_entity_similarity_threshold`
3. **New entity:** INSERT into `kg_entities` + upsert embedding to Qdrant kg collection
4. **Link:** INSERT into `kg_entity_memories` (ON CONFLICT DO NOTHING)

**`resolve_edges()`** — For each relationship: lookup source/target entity_ids. If active edge exists: increment `weight` by 0.1 (reinforcement). If not: INSERT new edge with `valid_to = NULL`.

#### Key Types

```rust
OllamaEmbedRequest { model: String, input: Vec<String> }
OllamaEmbedResponse { embeddings: Vec<Vec<f64>> }
OllamaGenerateRequest { model: String, prompt: String, stream: bool } // v0.6.0
OllamaGenerateResponse { response: String }                           // v0.6.0
ExtractedEntities { entities: Vec<ExtractedEntity>, relationships: Vec<ExtractedRelationship> }
ExtractedEntity { name, entity_type, description }
ExtractedRelationship { source, target, relation_type, description }
QdrantPoint { id: String, vector: HashMap<String, Vec<f64>>, payload: serde_json::Value }
QdrantUpsertRequest { points: Vec<QdrantPoint> }
```

#### Unit Tests (31)

Circuit breaker (5), Qdrant serialisation (2), **KG extraction (24)**: entity extraction prompt (2), JSON parsing with code fences (3), entity resolution (4), edge resolution (3), edge reinforcement (2), cycle prevention (2), circuit breaker integration (3), outbox operation dispatch (3), non-fatal degradation (2).

---

### 4.5 status-api (Port 3312)

**File:** `rust/status-api/src/main.rs` (~3,846 lines)
**Purpose:** JSON API for dashboard, control panel, auth, and user management
**Auth:** `X-Broodlink-Api-Key` header + `X-Broodlink-Session` header (v0.7.0 dual auth)
**Deps:** Dolt, Postgres, NATS, Qdrant

#### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | /api/v1/agents | Agent profiles + latest work_log |
| GET | /api/v1/tasks | Task queue breakdown |
| GET | /api/v1/decisions | Recent decisions (Dolt) |
| GET | /api/v1/convoys | Beads convoy status |
| GET | /api/v1/beads | Beads issues (filterable by status) |
| GET | /api/v1/health | Dependency health with latencies |
| GET | /api/v1/activity | Work log feed |
| GET | /api/v1/memory/stats | Memory counts + top topics |
| GET | /api/v1/commits | Dolt commit history |
| GET | /api/v1/summary | Daily summary |
| GET | /api/v1/audit | Audit log (filter: agent_id, operation) |
| GET | /api/v1/approvals | Approval gates list |
| GET | /api/v1/approval-policies | Approval policies |
| POST | /api/v1/approval-policies | Create/update policy |
| POST | /api/v1/approval-policies/:policy_id/toggle | Toggle policy active |
| POST | /api/v1/approvals/:gate_id/review | Review approval gate |
| GET | /api/v1/agent-metrics | Agent metrics (success_rate, load, scores) |
| GET | /api/v1/negotiations | Delegations |
| GET | /api/v1/guardrails | Guardrail policies |
| GET | /api/v1/violations | Guardrail violations |
| GET | /api/v1/streams | Active streams |
| GET | /api/v1/stream/:stream_id | SSE proxy to beads-bridge |
| GET | /api/v1/a2a/tasks | A2A task mappings |
| GET | /api/v1/a2a/card | Local AgentCard |
| GET | /api/v1/kg/stats | KG entity/edge counts, type distribution, top connected (v0.6.0) |
| GET | /api/v1/kg/entities | Paginated entity list (query params: type, limit) (v0.6.0) |
| GET | /api/v1/kg/edges | Recent edges (query params: relation_type, limit) (v0.5.0) |
| POST | /api/v1/agents/:agent_id/toggle | Toggle agent active/inactive (v0.6.0) |
| GET | /api/v1/budgets | All agent budgets + transaction history (v0.6.0) |
| POST | /api/v1/budgets/:agent_id/set | Set agent budget tokens (v0.6.0) |
| POST | /api/v1/tasks/:task_id/cancel | Cancel a pending/claimed task (v0.6.0) |
| GET | /api/v1/workflows | Workflow runs list with step progress (v0.6.0) |
| GET | /api/v1/dlq | Dead-letter queue entries (v0.6.0) |
| GET | /api/v1/webhooks | Registered webhook endpoints (v0.6.0) |
| POST | /api/v1/webhooks | Create webhook endpoint (v0.6.0) |
| POST | /api/v1/webhooks/:id/toggle | Toggle webhook active/inactive (v0.6.0) |
| POST | /api/v1/webhooks/:id/delete | Delete webhook endpoint (v0.6.0) |
| GET | /api/v1/webhook-log | Webhook delivery log (v0.6.0) |
| GET | /api/v1/chat/sessions | Chat sessions (filterable by platform, status) (v0.7.0) |
| GET | /api/v1/chat/sessions/:id/messages | Chat message history for session (v0.7.0) |
| POST | /api/v1/chat/sessions/:id/close | Close a chat session (v0.7.0) |
| POST | /api/v1/chat/sessions/:id/assign | Assign agent to chat session (v0.7.0) |
| GET | /api/v1/chat/stats | Chat stats: active sessions, messages today, pending replies, platforms (v0.7.0) |
| GET | /api/v1/formulas | List formulas from registry (v0.7.0) |
| POST | /api/v1/formulas | Create new formula (operator+) (v0.7.0) |
| GET | /api/v1/formulas/:name | Get formula by name (v0.7.0) |
| POST | /api/v1/formulas/:name/update | Update formula definition, bumps version (operator+) (v0.7.0) |
| POST | /api/v1/formulas/:name/toggle | Toggle formula enabled/disabled (v0.7.0) |
| GET | /api/v1/users | List dashboard users (admin only) (v0.7.0) |
| POST | /api/v1/users | Create dashboard user (admin only) (v0.7.0) |
| POST | /api/v1/users/:id/role | Change user role (admin only) (v0.7.0) |
| POST | /api/v1/users/:id/toggle | Toggle user active/inactive (admin only) (v0.7.0) |
| POST | /api/v1/users/:id/reset-password | Reset user password (admin only) (v0.7.0) |
| POST | /api/v1/auth/login | Login with username/password, returns session token (v0.7.0) |
| POST | /api/v1/auth/logout | Logout, deletes session (v0.7.0) |
| GET | /api/v1/auth/me | Current user info from session token (v0.7.0) |

**Response format:** All endpoints return `{ data: <payload>, updated_at: ISO8601, status: "ok" }`

#### v0.7.0: Role-Based Access Control

```rust
pub enum UserRole {
    Viewer,      // read-only dashboard access
    Operator,    // can write/modify (formulas, chat, tasks)
    Admin,       // full control (user management, all operations)
}
```

**Dual auth middleware:** Checks `X-Broodlink-Session` header first (session-based auth with role enforcement). Falls back to `X-Broodlink-Api-Key` (API key auth grants Admin role). When `dashboard_auth.enabled = false`, API key auth is the only path.

**`require_role(ctx, minimum)`:** Role hierarchy check (Admin > Operator > Viewer). Returns HTTP 403 if insufficient.

**Session management:** Login creates a session token (UUID) stored in `dashboard_sessions` with TTL from `dashboard_auth.session_ttl_hours`. Max sessions enforced per user (oldest deleted when `max_sessions_per_user` exceeded). Bcrypt cost from `dashboard_auth.bcrypt_cost` (default 12).

#### v0.7.0: Telegram Bot Management

**Registration:** `handler_telegram_register` validates the bot token via Telegram `getMe` API, stores credentials in `platform_credentials`, and generates a random 8-char alphanumeric access code (via `rand` crate) stored in `meta->>'auth_code'`. After successful registration, publishes `{prefix}.platform.credentials_changed` via NATS so a2a-gateway can immediately invalidate its credential cache. New dependency: `rand = "0.8"` in `rust/status-api/Cargo.toml`.

**Disconnect:** `handler_telegram_disconnect` cascade-deletes all related data in a single CTE query: `chat_reply_queue` (by session), `chat_messages` (by session), `chat_sessions` (platform = telegram), and `platform_credentials` (platform = telegram). After successful deletion, publishes `{prefix}.platform.credentials_changed` via NATS to invalidate the a2a-gateway credential cache. Disconnect confirmation in the dashboard UI warns about data removal.

#### Unit Tests (55)

OK response structure (1), error auth 401 (1), error internal 500 (1), JSON content-type (1), **v0.6.0 KG endpoints (6)**: kg/stats response structure (2), kg/entities pagination (2), kg/edges filtering (2), **v0.7.0 auth/user tests (45)**: UserRole ordering (1), UserRole from_str_loose (1), UserRole display (1), UserRole serde roundtrip (1), UserRole all variants (1), require_role admin_has_all (1), require_role operator_restricted (1), require_role viewer_read_only (1), auth_context api_key_is_admin (1), forbidden_error 403 (1), forbidden_error body (1), bcrypt hash_verify roundtrip (1), login_request deserialization (1), create_user_request deserialization (1), create_user_request defaults (1), change_role_request deserialization (1), reset_password_request deserialization (1), plus additional chat/formula endpoint tests.

---

### 4.6 mcp-server (Port 3311)

**Files:** `rust/mcp-server/src/main.rs` (323 lines), `handler.rs` (441 lines), `streamable_http.rs` (436 lines), `protocol.rs` (140 lines)
**Purpose:** MCP protocol wrapper around beads-bridge
**Deps:** beads-bridge (HTTP), status-api (HTTP)

#### v0.4.0 Fix: JWT Path and Non-Fatal Degradation

The default JWT path now uses `.token` extension (matching `onboard-agent.sh` convention), and a missing JWT file is non-fatal — the service starts and serves health checks, but bridge calls will fail:

```rust
// Default path: ~/.broodlink/jwt-{agent_id}.token (was .pem)
let jwt_token = match std::fs::read_to_string(&jwt_path) {
    Ok(t) => t.trim().to_string(),
    Err(e) => {
        warn!(path = %jwt_path, error = %e, "failed to read JWT token — bridge calls will fail");
        String::new() // non-fatal: service starts, bridge calls fail
    }
};
```

#### Transport Modes

| Mode | Config | Protocol |
|------|--------|----------|
| `"http"` | transport = "http" | MCP Streamable HTTP (2024-11-05 spec) |
| `"sse"` | transport = "sse" | Legacy SSE (GET /sse + POST /message) |
| `"stdio"` | transport = "stdio" | Line-delimited JSON-RPC over stdin/stdout |

#### Streamable HTTP Transport

- Single `/mcp` endpoint
- POST: JSON-RPC request → response (or SSE for streaming)
- GET: SSE channel for server-initiated messages
- Session management via `Mcp-Session-Id` header
- Session reaper every 60s (evicts sessions past `session_timeout_minutes`)
- Origin header validation
- CORS configured from `mcp_server.cors_origins`

#### MCP Methods Handled

| Method | Description |
|--------|-------------|
| initialize | Returns capabilities, protocol version (2024-11-05) |
| notifications/initialized | Acknowledged |
| notifications/cancelled | Returns 202 |
| tools/list | Fetches from beads-bridge, converts OpenAI → MCP format |
| tools/call | Proxies to beads-bridge tool endpoint |
| resources/list | 6 resources (agents, tasks, approvals, metrics, guardrails, summary) |
| resources/read | Fetches from status-api |
| prompts/list | 2 prompts (agent-status, task-overview) |
| prompts/get | Returns prompt messages |
| ping | Returns {} |

#### Unit Tests (23)

Origin validation (3), session management (3), MCP protocol handling (17 in handler.rs).

---

### 4.7 a2a-gateway (Port 3313)

**File:** `rust/a2a-gateway/src/main.rs` (~3,540 lines)
**Purpose:** Google A2A protocol gateway for cross-platform agent interop + webhook gateway + conversational agent gateway + direct Ollama LLM chat with Brave Search tool calling
**Auth:** Bearer token (configurable API key)
**Deps:** Postgres, beads-bridge (HTTP), NATS, Ollama (chat inference), Brave Search API (optional)

#### Routes

| Method | Path | Description |
|--------|------|-------------|
| GET | /.well-known/agent.json | Dynamic AgentCard from Beads formulas |
| POST | /a2a/tasks/send | JSON-RPC task submission |
| POST | /a2a/tasks/get | Query task status |
| POST | /a2a/tasks/cancel | Cancel running task |
| POST | /a2a/tasks/sendSubscribe | Streaming task (SSE with 5s polling) |
| GET | /health | Health check |
| POST | /webhook/slack | Slack slash command handler (v0.6.0) |
| POST | /webhook/teams | Teams bot message handler (v0.6.0) |
| POST | /webhook/telegram | Telegram update handler (v0.6.0) |

#### v0.6.0: Webhook Gateway

**Inbound command parsing:** All platforms support the same command set via platform-specific parsing:
- `/broodlink agents` — List active agents
- `/broodlink toggle <agent_id>` — Toggle agent active/inactive
- `/broodlink budget <agent_id>` — Show agent budget
- `/broodlink task cancel <task_id>` — Cancel a task
- `/broodlink workflow start <formula> <params>` — Start a workflow
- `/broodlink dlq` — Show DLQ summary

**Outbound notifications:** Subscribes to NATS events and dispatches to registered webhook endpoints based on their `events` subscription list:
- `agent.offline` — Agent deactivated by heartbeat
- `task.failed` — Task failed
- `budget.low` — Agent budget below threshold
- `workflow.completed` / `workflow.failed` — Workflow state changes
- `guardrail.violation` — Guardrail policy triggered

Slack: uses signing secret verification. Teams: validates app ID. Telegram: validates bot token.

#### v0.7.0: Conversational Agent Gateway

Chat messages from Slack/Teams/Telegram webhooks are now routed through a full conversational pipeline instead of being treated as one-off slash commands:

**Inbound flow:**
1. Platform webhook delivers message → `handle_chat_message()`
2. UPSERT into `chat_sessions` (new or existing session by platform + channel + user)
3. Store message in `chat_messages` (direction=inbound)
4. If first message and `chat.greeting_enabled`: queue greeting reply
5. Fetch recent conversation history (up to `chat.max_context_messages`) for context
6. Create task via beads-bridge `create_task` with chat metadata in dependencies:
   ```json
   {"chat": {"chat_session_id": "...", "platform": "slack", "channel_id": "...", "user_id": "...", "thread_id": "...", "reply_url": "..."}}
   ```
7. Task title: `"Chat: {first 80 chars of message}"`

**Reply delivery:**
- Background loop runs every 2 seconds, fetching up to 10 pending replies from `chat_reply_queue`
- Platform-specific delivery: Slack (response_url or chat.postMessage), Teams (v3/conversations), Telegram (sendMessage API)
- Respects threading (Slack thread_ts, Telegram message_thread_id)
- On failure: increments attempts, stores error_msg. After `reply_retry_attempts` (default 3): marks as "failed"

**Task completion handling:**
- Subscribes to NATS `task_completed` and `task_failed` events
- Looks up task's chat metadata from dependencies
- Extracts result content (tries result_data string, .response, .text fields)
- Queues reply in `chat_reply_queue` and stores as outbound message in `chat_messages`

#### v0.7.0: Telegram Long-Polling + Direct LLM Chat

**Polling loop:** `telegram_polling_loop()` uses `getUpdates` with 30s long-poll timeout. Poll offset persisted in `platform_credentials.meta->>'poll_offset'` (survives restarts). Credentials fetched from `platform_credentials` table with 5-min cache in `AppState.telegram_creds` via `get_telegram_creds()` which returns a 4-tuple `(token, secret, allowed_user_ids, auth_code)`. The token is fetched at the top of the loop (needed for the `getUpdates` API call), but the full credential tuple (including `allowed_users`) is re-fetched AFTER `getUpdates` returns, right before processing updates. This eliminates a race where disconnect+re-register during the 30s blocking poll could leave `allowed_users` stale. Additionally, a2a-gateway subscribes to `{prefix}.platform.credentials_changed` via NATS; when received, `telegram_creds` is immediately set to `None`, forcing a fresh DB fetch on the next access.

**Access code authentication:** When a bot is registered via the dashboard, a random 8-char alphanumeric code is generated and stored in `platform_credentials.meta->>'auth_code'`. New users must send this code to the bot to authenticate. Once verified, their Telegram user ID is atomically appended to `meta->>'allowed_user_ids'` via `add_telegram_allowed_user()`, which also invalidates the credential cache. Both the webhook handler and polling loop check the allow list before processing messages; unauthorized users receive a generic prompt ("Send the access code to use this bot.") with no distinction between wrong code and random message (prevents information leakage about code validity).

**Code generation details:**
- Character set: `[0-9a-z]` (36 chars, 8 positions = 36^8 ≈ 2.8 trillion combinations)
- RNG: `rand::thread_rng()` backed by OS CSPRNG (`getrandom` → macOS `SecRandomCopyBytes`, Linux `/dev/urandom`)
- The code never expires (valid until bot is disconnected and re-registered, which generates a new code)
- No rate limiting on attempts (36^8 keyspace makes brute force infeasible; Telegram's own rate limits add further protection)
- Code is displayed in dashboard Telegram tab — admin shares it privately with authorized users
- Dashboard also shows count of authorized users

**`add_telegram_allowed_user()` implementation:**
```rust
// Atomically appends user_id to the JSONB allowed_user_ids array
UPDATE platform_credentials
SET meta = jsonb_set(
    COALESCE(meta, '{}'::jsonb),
    '{allowed_user_ids}',
    (COALESCE(meta->'allowed_user_ids', '[]'::jsonb) || to_jsonb($1::bigint))
), updated_at = NOW()
WHERE platform = 'telegram'
```
After the DB update, `telegram_creds` cache is set to `None` (forces fresh DB fetch on next access).

**Auth code security hardening:** Message text from unauthenticated users is never logged (prevents auth code leakage in logs). The inbound webhook/polling log line is emitted only AFTER the auth gate passes (authenticated users only). Auth code verification logs only "auth code accepted" or "not authenticated" — never the message content. Auth code messages return/continue before reaching chat history storage (`chat_messages`) or LLM context, ensuring access codes never appear in conversation history.

**Telegram message flow (polling mode):**
```
getUpdates (30s long-poll)
    │
    ▼
Re-fetch credentials from DB (post-poll, always fresh)
    │
    ▼
Is user in allowed_user_ids? ──yes──► Process message normally
    │ no                                      │
    ▼                                         ▼
Is auth_code configured? ──no──► Silent reject (no reply)
    │ yes                         (only if allow list exists)
    ▼
Does message == auth_code? ──yes──► add_telegram_allowed_user()
    │ no                               invalidate cache
    ▼                                  reply "Authenticated!"
Reply "Send the access code..."       continue (skip this msg)
    │
    ▼                         ┌─────────────────────────┐
(continue, skip this msg)    │ Normal message processing │
                              ├─────────────────────────┤
                              │ 1. Dedup check (single   │
                              │    write lock, before DB) │
                              │ 2. Semaphore try_acquire  │
                              │ 3. Chat session upsert    │
                              │ 4. Store inbound message  │
                              │ 5. Fetch history          │
                              │ 6. tokio::select! {       │
                              │      call_ollama_chat()   │
                              │      typing refresh (4s)  │
                              │    }                      │
                              │ 7. Strip <think> tags     │
                              │ 8. Store outbound message │
                              │ 9. Deliver reply          │
                              └─────────────────────────┘
```

**Direct Ollama chat:** Instead of routing every message through the coordinator, conversational messages go directly to `call_ollama_chat()` which calls Ollama's `/api/chat` endpoint with the configured `chat_model` (default: qwen3:32b).

**Tool calling loop:** When `chat.tools.web_search_enabled = true` and `BROODLINK_BRAVE_API_KEY` is set:
1. System prompt instructs model to ONLY use `web_search` for real-time data (weather, news, scores, live status)
2. Ollama returns `tool_calls` array → gateway executes each tool
3. Tool results appended as `role: "tool"` messages → sent back to Ollama
4. Loop continues up to `max_tool_rounds` (default 3) or until model returns text
5. `<think>...</think>` tags stripped from final output

**Brave Search integration:**
- Endpoint: `https://api.search.brave.com/res/v1/web/search`
- Auth: `X-Subscription-Token` header with `BROODLINK_BRAVE_API_KEY`
- Query params: `q` (search query), `count` (from `search_result_count`, default 5)
- Response handling: Extracts `web.results[]` array, formats each as `"Title: {title}\nURL: {url}\nSnippet: {description}"`, joins with `\n\n`
- On API error: returns error string to LLM (model can retry or answer without search)
- Cache: normalized query key (trim + lowercase + collapse whitespace) → cached result string. Evicts expired entries when map exceeds 100 items

**System prompt (tool instruction):**
```
You have access to a web_search tool. ONLY use it for:
(1) real-time data like news, weather, stock prices, sports scores,
(2) events or information from the last 30 days,
(3) specific URLs, product availability, or live status checks.
Do NOT search for general knowledge, definitions, programming concepts,
or anything you can answer from your training data.
When you do search, cite your sources.
```

**Efficiency safeguards:**

| Feature | Mechanism | Config |
|---------|-----------|--------|
| Concurrency limit | `tokio::sync::Semaphore` with `try_acquire()` — busy message returned instantly if unavailable | `a2a.ollama_concurrency` (default 1) |
| Search cache | `RwLock<HashMap<String, (String, Instant)>>` with TTL check, eviction at 100 entries | `chat.tools.search_cache_ttl_secs` (default 300) |
| Dedup suppression | `RwLock<HashMap<String, Instant>>` keyed by `platform:chat_id:normalized_text`, checked **before** `handle_chat_message()` (avoids wasted DB/bridge work), single write lock (no TOCTOU) | `a2a.dedup_window_secs` (default 30) |
| Typing refresh | `tokio::select!` races LLM call against 4s typing loop, auto-cancelled on completion | Always on when semaphore acquired |
| History protection | Busy/dedup replies skip `chat_messages` INSERT — only real LLM responses stored | Always on |
| Context limiting | When tools active, history capped to `tool_context_messages` (saves KV cache for CPU inference) | `chat.tools.tool_context_messages` (default 4) |
| Query normalization | `normalize_search_query()` (trim + lowercase + collapse whitespace) shared by brave cache and dedup keys | Always on |

**`.env` file loader:** At startup, `main()` reads `.env` from disk and calls `unsafe { std::env::set_var() }` for each line. Required because newer Rust makes `set_var` unsafe (silently no-ops without `unsafe`). Secrets stay in `.env` (gitignored), not in `config.toml`.

**`.env` format:**
```bash
# .env — gitignored, loaded by a2a-gateway at startup
BROODLINK_BRAVE_API_KEY=BSA...your-brave-api-key...
# Telegram bot token is stored in platform_credentials (DB), not here.
# Only secrets that don't have a DB-backed storage go in .env.
```

**Dedicated Ollama client:** `AppState.ollama_client` uses `reqwest::Client::builder().timeout(Duration)` which covers the entire request lifecycle (connect + send + body read), unlike `RequestBuilder::timeout()` which only covers until headers arrive. Critical for Ollama's `stream: false` mode where headers arrive immediately but the body takes 30-60s.

#### A2A Status Mapping

| Internal Status | A2A Status |
|-----------------|------------|
| pending | Submitted |
| claimed / in_progress | Working |
| awaiting_approval | InputRequired |
| completed | Completed |
| failed / expired | Failed |
| rejected | Canceled |
| other | Unknown |

#### Key Structs

```rust
AppState {
    config, pg: PgPool, bridge_url, bridge_jwt,
    http_client,                   // general HTTP (Telegram API, Brave Search, bridge calls)
    ollama_client,                 // dedicated client with builder-level timeout for Ollama
    api_key?,
    telegram_creds: RwLock<Option<(String, Option<String>, Vec<i64>, Option<String>, Instant)>>,  // cached (token, secret, allowed_user_ids, auth_code, fetched_at)
    ollama_semaphore: Semaphore,   // limits concurrent LLM calls
    brave_cache: RwLock<HashMap<String, (String, Instant)>>,            // search result TTL cache
    inflight_chats: RwLock<HashMap<String, Instant>>,                   // dedup tracking
}
A2aJsonRpc { jsonrpc, id?, method, params }
A2aTaskStatus { Submitted, Working, InputRequired, Completed, Canceled, Failed, Unknown }
A2aTaskResponse { id, status: A2aTaskStatusInfo, artifacts? }
A2aTaskStatusInfo { state: A2aTaskStatus, message? }
GatewayError { Bridge, Db, NotFound, BadRequest }
```

#### Unit Tests (11)

Auth validation (4), status mapping (5), JSON-RPC format (1), serialisation (1).

---

## 5. Shared Crates

### 5.1 broodlink-config (~1,251 lines)

See [Section 3](#3-configuration) for full struct hierarchy.

**Key function:** `Config::load()` — reads `BROODLINK_CONFIG` env var, merges `BROODLINK_*` overrides.

**v0.4.0 addition:** `MemorySearchConfig` struct with `decay_lambda`, `reranker_model`, `reranker_enabled`, `max_content_length` fields and defaults.

**v0.5.0 addition:** 5 new fields on `MemorySearchConfig`: `kg_enabled`, `kg_extraction_model`, `kg_entity_similarity_threshold`, `kg_max_hops`, `kg_extraction_timeout_seconds`.

**v0.6.0 addition:** `BudgetConfig`, `DlqConfig`, `CollaborationConfig`, `WebhookConfig`, `JwtConfig` structs. 4 new KG expiry fields on `MemorySearchConfig`: `kg_entity_ttl_days`, `kg_edge_decay_rate`, `kg_cleanup_interval_hours`, `kg_min_mention_count`.

**v0.7.0 addition:** `ChatConfig` struct (9 fields: enabled, chat_model, max_context_messages, session_timeout_hours, reply_timeout_seconds, reply_retry_attempts, default_agent_role, greeting_enabled, greeting_message). `ChatToolsConfig` struct (6 fields: web_search_enabled, brave_api_key, max_tool_rounds, search_result_count, tool_context_messages, search_cache_ttl_secs). `DashboardAuthConfig` struct (4 fields: enabled, session_ttl_hours, bcrypt_cost, max_sessions_per_user). `OllamaConfig` gains `num_ctx` (default 4096). `A2aConfig` gains `ollama_concurrency` (default 1), `busy_message`, `dedup_window_secs` (default 30).

**Unit Tests (4):** load valid config (1), load missing file (1), default pool sizes (1), config defaults (1).

### 5.2 broodlink-runtime (227 lines)

**Purpose:** Shared infrastructure utilities used by all services.

**Exports:**

```rust
pub struct CircuitBreaker { ... }
impl CircuitBreaker {
    pub fn new(name: &str, threshold: u32, half_open_secs: u64) -> Self;
    pub fn is_open() -> bool;
    pub fn record_success();
    pub fn record_failure();
    pub fn check() -> Result<(), String>;
    pub fn name() -> &str;
}

pub async fn shutdown_signal();  // SIGINT/SIGTERM handler
pub async fn connect_nats(config: &NatsConfig) -> Result<async_nats::Client>;
```

**Unit Tests (5):** starts closed (1), opens after threshold (1), resets on success (1), half-open after timeout (1), check returns name (1).

### 5.3 broodlink-secrets (379 lines)

**Providers:**
- `SopsProvider` — decrypts via `sops --decrypt --extract`, 300s cache TTL, 5s timeout
- `InfisicalProvider` — REST API to Infisical, 3 retries with exp backoff, 10s timeout

**Interface:**
```rust
#[async_trait]
pub trait SecretsProvider: Send + Sync {
    async fn get_secret(&self, key: &str) -> Result<String, SecretsError>;
}
```

**Unit Tests (5):** tilde expansion (1), missing sops file (1), missing age identity (1), unknown provider (1), sops creation (1).

### 5.4 broodlink-telemetry (~265 lines)

**Function:** `init_telemetry(service_name: &str, config: &TelemetryConfig) -> Result<TelemetryGuard>`

When enabled: installs W3C trace context propagator, configures OTLP exporter to `otlp_endpoint`, sets trace sampling rate. Returns guard that flushes on drop.

When disabled: returns no-op guard.

**v0.6.0 addition:** W3C TraceContext propagator now activated by default for all services. Added `extract_trace_context(headers)` and `inject_trace_context(headers)` helpers for cross-service trace propagation via HTTP headers. Used by beads-bridge, status-api, a2a-gateway, and coordinator for end-to-end distributed tracing.

**Unit Tests (5):** disabled by default (1), config defaults (1), guard drop (1), error display (1), TOML deserialisation (1).

---

## 6. Database Schemas

### 6.1 Dolt (agent_ledger, MySQL protocol, port 3307)

**Migration 001:** `001_dolt_brain.sql` (82 lines)

#### agent_memory
| Column | Type | Notes |
|--------|------|-------|
| id | BIGINT AUTO_INCREMENT PK | |
| topic | VARCHAR(255) | Kebab-case key |
| content | LONGTEXT | |
| agent_name | VARCHAR(100) | |
| tags | JSON | Comma-separated |
| embedding_ref | VARCHAR(36) NULL | Qdrant point UUID |
| created_at | TIMESTAMP DEFAULT CURRENT_TIMESTAMP | |
| updated_at | TIMESTAMP DEFAULT CURRENT_TIMESTAMP ON UPDATE | |

UNIQUE INDEX on (topic, agent_name).

#### decisions (APPEND ONLY)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGINT AUTO_INCREMENT PK | |
| trace_id | VARCHAR(36) | |
| agent_id | VARCHAR(100) | |
| decision | TEXT | |
| reasoning | TEXT | |
| alternatives | JSON | |
| outcome | TEXT | |
| created_at | TIMESTAMP | |

#### agent_profiles
| Column | Type | Notes |
|--------|------|-------|
| agent_id | VARCHAR(100) PK | |
| display_name | VARCHAR(255) | |
| role | ENUM(strategist,worker,researcher,monitor) | |
| transport | ENUM(mcp,pymysql,api,nats) | |
| cost_tier | ENUM(low,medium,high) | |
| preferred_formula_types | JSON | |
| capabilities | JSON | |
| active | BOOLEAN DEFAULT true | |
| last_seen | TIMESTAMP NULL | |
| max_concurrent | INT DEFAULT 5 | Migration 005b |
| budget_tokens | BIGINT DEFAULT 0 | Migration 008b |
| created_at | TIMESTAMP | |
| updated_at | TIMESTAMP | |

#### beads_issues
| Column | Type | Notes |
|--------|------|-------|
| bead_id | VARCHAR(20) PK | |
| title | VARCHAR(255) | |
| description | TEXT | |
| status | VARCHAR(50) | |
| assignee | VARCHAR(100) | |
| convoy_id | VARCHAR(50) | Parent convoy/epic |
| dependencies | JSON | |
| formula | VARCHAR(100) | |
| created_at / updated_at / synced_at | TIMESTAMP | |

#### daily_summary
| Column | Type | Notes |
|--------|------|-------|
| id | BIGINT AUTO_INCREMENT PK | |
| summary_date | DATE UNIQUE | |
| summary_text | TEXT | |
| agent_activity | JSON | |
| tasks_completed / decisions_made / memories_stored | INT | |
| created_at | TIMESTAMP | |

### 6.2 Postgres (broodlink_hot, port 5432)

**Migrations:** 002–024 (25 files, ~960 lines)

#### task_queue (Migration 002)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | UUID |
| trace_id | VARCHAR(36) | |
| title | VARCHAR(255) | |
| description | TEXT | |
| priority | INT DEFAULT 0 | |
| status | VARCHAR(20) | pending/claimed/in_progress/completed/failed/retrying/awaiting_approval/rejected/expired |
| assigned_agent | VARCHAR(100) NULL | |
| formula_name | VARCHAR(100) | |
| convoy_id | VARCHAR(50) | |
| dependencies | JSONB | |
| retry_count | INT DEFAULT 0 | |
| max_retries | INT DEFAULT 3 | |
| parent_task_id | VARCHAR(36) NULL | Migration 006 |
| delegation_id | VARCHAR(36) NULL | Migration 006 |
| requires_approval | BOOLEAN DEFAULT false | Migration 004 |
| approval_id | VARCHAR(36) NULL | Migration 004 |
| result_data | JSONB NULL | Migration 010, task output |
| workflow_run_id | VARCHAR(36) NULL | Migration 011, links to workflow_runs |
| step_index | INT NULL | Migration 011, step position in workflow |
| step_name | VARCHAR(100) NULL | Migration 011, formula step name |
| timeout_at | TIMESTAMPTZ NULL | Migration 016, v0.6.0: step timeout deadline |
| parent_task_id | VARCHAR(36) NULL | Migration 017, v0.6.0: decomposition parent |
| workspace_id | VARCHAR(36) NULL | Migration 017, v0.6.0: shared workspace link |
| claimed_at / completed_at | TIMESTAMPTZ NULL | |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

INDEX: `(status, priority DESC, created_at)`

#### workflow_runs (Migration 011)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | UUID |
| convoy_id | VARCHAR(50) UNIQUE | wf-{formula}-{id_prefix} |
| formula_name | VARCHAR(100) | Formula TOML name |
| status | VARCHAR(20) DEFAULT 'pending' | pending/running/completed/failed |
| current_step | INT DEFAULT 0 | Active step index |
| total_steps | INT | Total steps from formula |
| params | JSONB DEFAULT '{}' | Formula parameters (e.g. {topic: "..."}) |
| step_results | JSONB DEFAULT '{}' | Accumulated output from each step |
| started_by | VARCHAR(100) | Agent that initiated the workflow |
| current_task_id | VARCHAR(36) NULL | Active step task |
| parallel_pending | INT DEFAULT 0 | Migration 016, v0.6.0: parallel group pending count |
| error_handler_task_id | VARCHAR(36) NULL | Migration 016, v0.6.0: on_failure handler task |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |
| completed_at | TIMESTAMPTZ NULL | |

#### memory_search_index (Migration 012, v0.4.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| memory_id | BIGINT NOT NULL | Matches Dolt agent_memory.id |
| agent_id | VARCHAR(100) NOT NULL | |
| topic | VARCHAR(255) NOT NULL | |
| content | TEXT NOT NULL | |
| tags | VARCHAR(500) | Comma-separated |
| tsv | TSVECTOR GENERATED ALWAYS AS (...) STORED | Weighted: A=topic, B=tags, C=content |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |
| updated_at | TIMESTAMPTZ DEFAULT NOW() | |

UNIQUE: `(agent_id, topic)`. GIN index on `tsv`. B-tree index on `agent_id`.

The tsvector is automatically generated with field weighting:
```sql
setweight(to_tsvector('english', coalesce(topic, '')), 'A') ||
setweight(to_tsvector('english', coalesce(tags, '')), 'B') ||
setweight(to_tsvector('english', coalesce(content, '')), 'C')
```

#### messages (Migration 002)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | |
| trace_id | VARCHAR(36) | |
| thread_id | VARCHAR(36) NULL | |
| sender / recipient | VARCHAR(100) | |
| subject | VARCHAR(255) | |
| body | TEXT | |
| status | VARCHAR(20) | unread/read/actioned |
| created_at | TIMESTAMPTZ | |

#### work_log (Migration 002, APPEND ONLY)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| trace_id | VARCHAR(36) | |
| agent_id | VARCHAR(100) | |
| action | VARCHAR(100) | |
| details | TEXT | |
| files_changed | JSONB | |
| created_at | TIMESTAMPTZ | |

#### audit_log (Migration 002, APPEND ONLY)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| trace_id | VARCHAR(36) | |
| agent_id | VARCHAR(100) | |
| service | VARCHAR(100) | |
| operation | VARCHAR(100) | |
| parameters | JSONB | |
| result_status | VARCHAR(20) | ok/error/timeout |
| result_summary | TEXT | |
| duration_ms | INT | |
| created_at | TIMESTAMPTZ | |

#### outbox (Migration 002)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| trace_id | VARCHAR(36) | |
| operation | VARCHAR(50) | embed/sync |
| payload | JSONB | |
| status | VARCHAR(20) | pending/processing/done/failed |
| attempts | INT DEFAULT 0 | |
| created_at / processed_at | TIMESTAMPTZ | |

#### approval_policies (Migration 004)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | |
| name | VARCHAR(100) UNIQUE | |
| description | TEXT | |
| gate_type | VARCHAR(50) | |
| conditions | JSONB | { severity, agent_ids, tool_names } |
| auto_approve | BOOLEAN DEFAULT false | |
| auto_approve_threshold | FLOAT DEFAULT 0.8 | |
| expiry_minutes | INT DEFAULT 60 | |
| active | BOOLEAN DEFAULT true | |
| created_at / updated_at | TIMESTAMPTZ | |

#### approval_gates (Migration 004)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | |
| trace_id | VARCHAR(36) | |
| task_id | VARCHAR(36) NULL | |
| tool_name | VARCHAR(100) NULL | |
| agent_id | VARCHAR(100) | |
| gate_type | VARCHAR(50) | pre_dispatch/pre_completion/budget/custom |
| payload | JSONB | |
| status | VARCHAR(20) | pending/approved/rejected/expired/auto_approved |
| reason / review_note | TEXT | |
| confidence | FLOAT (0-1) | |
| policy_id | VARCHAR(36) NULL | |
| requested_by / reviewed_by | VARCHAR(100) | |
| expires_at / created_at / reviewed_at | TIMESTAMPTZ | |

#### agent_metrics (Migration 005)
| Column | Type | Notes |
|--------|------|-------|
| agent_id | VARCHAR(100) PK | |
| tasks_completed / tasks_failed | INT DEFAULT 0 | |
| avg_duration_ms | INT DEFAULT 0 | |
| current_load | INT DEFAULT 0 | |
| success_rate | REAL DEFAULT 1.0 | |
| last_task_at / updated_at | TIMESTAMPTZ | |

#### delegations (Migration 006)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | |
| trace_id | VARCHAR(36) | |
| parent_task_id | VARCHAR(36) | |
| from_agent / to_agent | VARCHAR(100) | |
| title | VARCHAR(255) | |
| description | TEXT | |
| status | VARCHAR(20) | pending/accepted/rejected/in_progress/completed/failed |
| result | JSONB | |
| created_at / updated_at | TIMESTAMPTZ | |

#### streams (Migration 007)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | |
| agent_id | VARCHAR(100) | |
| tool_name | VARCHAR(100) | |
| task_id | VARCHAR(36) NULL | |
| status | VARCHAR(20) | active/completed/failed/expired |
| created_at / closed_at | TIMESTAMPTZ | |

#### guardrail_policies (Migration 008)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| name | VARCHAR(100) UNIQUE | |
| rule_type | VARCHAR(30) | tool_block/rate_override/content_filter/scope_limit |
| config | JSONB | |
| enabled | BOOLEAN DEFAULT true | |
| created_at / updated_at | TIMESTAMPTZ | |

#### guardrail_violations (Migration 008, APPEND ONLY)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| trace_id | VARCHAR(36) | |
| agent_id / policy_name / tool_name | VARCHAR | |
| details | TEXT | |
| created_at | TIMESTAMPTZ | |

#### a2a_task_map (Migration 009)
| Column | Type | Notes |
|--------|------|-------|
| external_id | VARCHAR(255) PK | A2A task ID |
| internal_id | VARCHAR(36) REFERENCES task_queue(id) | |
| source_agent | VARCHAR(255) | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

#### residency_log (Migration 002, APPEND ONLY)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| trace_id | VARCHAR(36) | |
| region | VARCHAR(50) | |
| profile | VARCHAR(20) | |
| services | JSONB | |
| checked_at | TIMESTAMPTZ DEFAULT NOW() | |

Only written when `profile.audit_residency = true`.

#### kg_entities (Migration 013, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| entity_id | VARCHAR(36) UNIQUE NOT NULL | UUID |
| name | VARCHAR(255) NOT NULL | Canonical entity name |
| entity_type | VARCHAR(50) NOT NULL | person, service, concept, location, technology, organization, event, other |
| description | TEXT | LLM-generated entity summary |
| properties | JSONB DEFAULT '{}' | Arbitrary key-value attributes |
| embedding_ref | VARCHAR(36) | Qdrant point ID for entity name embedding |
| source_agent | VARCHAR(100) | Agent that first created this entity |
| mention_count | INT DEFAULT 1 | How many memories reference this entity |
| first_seen / last_seen | TIMESTAMPTZ DEFAULT NOW() | |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

Indexes: `lower(name)`, `entity_type`, `source_agent`.

#### kg_edges (Migration 013, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| edge_id | VARCHAR(36) UNIQUE NOT NULL | UUID |
| source_id | VARCHAR(36) FK → kg_entities(entity_id) | |
| target_id | VARCHAR(36) FK → kg_entities(entity_id) | |
| relation_type | VARCHAR(100) NOT NULL | LEADS, DEPENDS_ON, RUNS_ON, etc. |
| description | TEXT | LLM-generated relationship description |
| weight | FLOAT DEFAULT 1.0 | Relationship strength (reinforced on re-extraction) |
| properties | JSONB DEFAULT '{}' | |
| source_memory_id | BIGINT | Which memory this was extracted from |
| source_agent | VARCHAR(100) | |
| valid_from | TIMESTAMPTZ DEFAULT NOW() | Temporal: when relationship became true |
| valid_to | TIMESTAMPTZ | Temporal: when relationship ended (NULL = still active) |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

UNIQUE: `(source_id, target_id, relation_type, valid_to)`. Indexes: `source_id`, `target_id`, `relation_type`, `source_memory_id`, active edges partial index.

#### kg_entity_memories (Migration 013, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| entity_id | VARCHAR(36) FK → kg_entities(entity_id) | PK part 1 |
| memory_id | BIGINT NOT NULL | PK part 2, references Dolt agent_memory.id |
| agent_id | VARCHAR(100) NOT NULL | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

PRIMARY KEY: `(entity_id, memory_id)`. Index on `memory_id`.

#### budget_transactions (Migration 014, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| agent_id | VARCHAR(100) NOT NULL | |
| tool_name | VARCHAR(100) NOT NULL | |
| cost_tokens | BIGINT NOT NULL | |
| balance_after | BIGINT NOT NULL | |
| trace_id | VARCHAR(36) | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

#### tool_cost_map (Migration 014, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| tool_name | VARCHAR(100) PK | |
| cost_tokens | BIGINT NOT NULL | |
| description | TEXT | |

#### dead_letter_queue (Migration 015, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| task_id | VARCHAR(36) NOT NULL | |
| reason | TEXT | |
| source_service | VARCHAR(100) | |
| retry_count | INT DEFAULT 0 | |
| max_retries | INT DEFAULT 3 | |
| next_retry_at | TIMESTAMPTZ | |
| resolved | BOOLEAN DEFAULT false | |
| resolved_by | VARCHAR(100) | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

#### task_decompositions (Migration 017, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| parent_task_id | VARCHAR(36) NOT NULL | |
| child_task_id | VARCHAR(36) NOT NULL | |
| merge_strategy | VARCHAR(50) DEFAULT 'concatenate' | concatenate, vote, best |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

#### shared_workspaces (Migration 017, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | UUID |
| name | VARCHAR(255) NOT NULL | |
| owner_agent | VARCHAR(100) NOT NULL | |
| participants | JSONB DEFAULT '[]' | |
| context | JSONB DEFAULT '{}' | |
| status | VARCHAR(20) DEFAULT 'active' | active, closed, expired |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

#### webhook_endpoints (Migration 018, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | UUID |
| name | VARCHAR(255) NOT NULL | |
| platform | VARCHAR(50) NOT NULL | slack, teams, telegram, generic |
| webhook_url | TEXT | Outbound URL |
| verify_token | VARCHAR(255) | Inbound verification |
| events | JSONB DEFAULT '[]' | Subscribed event types |
| active | BOOLEAN DEFAULT true | |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

#### webhook_log (Migration 018, v0.6.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| endpoint_id | VARCHAR(36) FK | |
| direction | VARCHAR(20) NOT NULL | inbound, outbound |
| event_type | VARCHAR(100) NOT NULL | |
| payload | JSONB | |
| status | VARCHAR(20) | delivered, failed |
| error_msg | TEXT | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

#### chat_sessions (Migration 019, v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | UUID |
| platform | VARCHAR(50) NOT NULL | slack, teams, telegram |
| channel_id | VARCHAR(255) NOT NULL | Platform channel/chat ID |
| user_id | VARCHAR(255) NOT NULL | Platform user ID |
| user_display_name | VARCHAR(255) | |
| thread_id | VARCHAR(255) | Platform thread reference |
| assigned_agent | VARCHAR(100) | Preferred agent for routing |
| context | JSONB DEFAULT '{}' | Conversation context metadata |
| message_count | INT DEFAULT 0 | |
| last_message_at | TIMESTAMPTZ | |
| status | VARCHAR(20) DEFAULT 'active' | active, closed, expired |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

UNIQUE: `(platform, channel_id, user_id)`. Index on `platform`.

#### chat_messages (Migration 019, v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| session_id | VARCHAR(36) FK → chat_sessions(id) | |
| direction | VARCHAR(20) NOT NULL | inbound, outbound |
| content | TEXT NOT NULL | |
| task_id | VARCHAR(36) | Associated task (if any) |
| platform_message_id | VARCHAR(255) | Platform-specific message ID |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

Index on `session_id`.

#### chat_reply_queue (Migration 019, v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| session_id | VARCHAR(36) FK → chat_sessions(id) | |
| task_id | VARCHAR(36) | Task that generated the reply |
| content | TEXT NOT NULL | |
| platform | VARCHAR(50) NOT NULL | |
| reply_url | TEXT | Platform-specific reply endpoint |
| channel_id | VARCHAR(255) | |
| thread_id | VARCHAR(255) | |
| status | VARCHAR(20) DEFAULT 'pending' | pending, delivered, failed |
| attempts | INT DEFAULT 0 | |
| error_msg | TEXT | |
| delivered_at | TIMESTAMPTZ | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

Index on `(status)` WHERE status = 'pending'.

#### formula_registry (Migration 020, v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | UUID |
| name | VARCHAR(100) UNIQUE NOT NULL | Formula identifier |
| display_name | VARCHAR(255) | |
| description | TEXT | |
| version | INT DEFAULT 1 | Increments on update |
| definition | JSONB NOT NULL | Full formula definition (steps, params, on_failure) |
| author | VARCHAR(100) | |
| tags | JSONB DEFAULT '[]' | |
| is_system | BOOLEAN DEFAULT false | true for formulas seeded from TOML files |
| enabled | BOOLEAN DEFAULT true | |
| usage_count | INT DEFAULT 0 | |
| last_used_at | TIMESTAMPTZ | |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

Index on `enabled`.

#### dashboard_users (Migration 021, v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | UUID |
| username | VARCHAR(100) UNIQUE NOT NULL | |
| password_hash | VARCHAR(255) NOT NULL | bcrypt hash |
| role | VARCHAR(20) DEFAULT 'viewer' | viewer, operator, admin |
| display_name | VARCHAR(255) | |
| active | BOOLEAN DEFAULT true | |
| last_login | TIMESTAMPTZ | |
| created_at / updated_at | TIMESTAMPTZ DEFAULT NOW() | |

#### dashboard_sessions (Migration 021, v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | VARCHAR(36) PK | Session token (UUID) |
| user_id | VARCHAR(36) FK → dashboard_users(id) | |
| expires_at | TIMESTAMPTZ NOT NULL | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

Index on `expires_at`.

#### platform_credentials (Migration 023, v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| platform | VARCHAR(20) PK | CHECK IN ('telegram', 'slack', 'teams') |
| bot_token | TEXT NOT NULL | Platform bot token |
| secret_token | TEXT | Webhook verification secret |
| webhook_url | TEXT | Configured webhook URL |
| bot_username | VARCHAR(255) | Bot display name |
| bot_id | VARCHAR(64) | Platform bot ID |
| enabled | BOOLEAN DEFAULT TRUE | |
| meta | JSONB DEFAULT '{}' | Runtime state: `poll_offset` (int), `auth_code` (string, 8-char alphanumeric), `allowed_user_ids` (int array) — Migration 024 |
| registered_at | TIMESTAMPTZ | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |
| updated_at | TIMESTAMPTZ DEFAULT NOW() | |

#### scheduled_tasks (Migration 028, post-v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| title | VARCHAR(255) NOT NULL | Task title |
| description | TEXT DEFAULT '' | |
| priority | INT DEFAULT 0 | |
| formula_name | VARCHAR(100) | Optional formula to execute |
| params | JSONB DEFAULT '{}' | Formula params or task metadata |
| next_run_at | TIMESTAMPTZ NOT NULL | When this task next fires |
| recurrence_secs | BIGINT | NULL = one-shot, >0 = repeat interval |
| last_run_at | TIMESTAMPTZ | |
| run_count | INT DEFAULT 0 | |
| max_runs | INT | NULL = unlimited |
| enabled | BOOLEAN DEFAULT TRUE | Set FALSE after one-shot fires or max_runs reached |
| created_by | VARCHAR(100) | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

INDEX: `idx_scheduled_tasks_due ON scheduled_tasks (next_run_at) WHERE enabled = TRUE`

#### notification_rules (Migration 028, post-v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| name | VARCHAR(100) NOT NULL UNIQUE | Rule identifier |
| condition_type | VARCHAR(50) NOT NULL | service_event_error, dlq_spike, budget_low |
| condition_config | JSONB NOT NULL DEFAULT '{}' | `{threshold, auto_postmortem}` |
| channel | VARCHAR(20) NOT NULL | telegram, slack |
| target | VARCHAR(255) NOT NULL | Chat ID or webhook URL |
| template | TEXT | Message with `{{count}}`, `{{agents}}`, `{{type}}` placeholders |
| cooldown_minutes | INT DEFAULT 30 | |
| enabled | BOOLEAN DEFAULT TRUE | |
| last_triggered_at | TIMESTAMPTZ | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

#### notification_log (Migration 028, post-v0.7.0)
| Column | Type | Notes |
|--------|------|-------|
| id | BIGSERIAL PK | |
| rule_id | BIGINT FK → notification_rules(id) | NULL for direct send_notification |
| rule_name | VARCHAR(100) | |
| channel | VARCHAR(20) NOT NULL | telegram, slack |
| target | VARCHAR(255) NOT NULL | |
| message | TEXT NOT NULL | |
| status | VARCHAR(20) DEFAULT 'pending' | pending, sent, failed |
| error_msg | TEXT | |
| created_at | TIMESTAMPTZ DEFAULT NOW() | |

INDEX: `idx_notification_log_time ON notification_log (created_at DESC)`

### 6.3 Qdrant (port 6333)

**Collection: `broodlink_memory`**
- **Vector:** 768 dimensions, Cosine distance
- **Named vector:** "default"
- **Payload:** `{topic, agent_id, memory_id, created_at}`

**Collection: `broodlink_kg_entities`** (v0.6.0)
- **Vector:** 768 dimensions, Cosine distance
- **Named vector:** "default"
- **Payload:** `{entity_id, name, entity_type, agent_id}`
- **Purpose:** Entity name embedding similarity for deduplication during resolution

---

## 7. NATS Subjects

All prefixed with `{config.nats.subject_prefix}.{config.broodlink.env}` (default: `broodlink.local`).

### beads-bridge publishes

| Subject | When |
|---------|------|
| `{p}.{e}.beads-bridge.tool_call` | Every tool invocation |
| `{p}.{e}.messaging.new` | New agent message |
| `{p}.{e}.coordinator.task_available` | Task created |
| `{p}.{e}.coordinator.task_completed` | Task completed (triggers workflow step chaining) |
| `{p}.{e}.coordinator.task_failed` | Task failed (triggers workflow failure) |
| `{p}.{e}.coordinator.task_claimed` | Task claimed |
| `{p}.{e}.coordinator.workflow_start` | Workflow initiated via start_workflow tool |
| `{p}.{e}.coordinator.workflow_completed` | All workflow steps finished |
| `{p}.{e}.approvals.pending` | Approval gate created |
| `{p}.{e}.approvals.auto_approved` | Auto-approved by policy |
| `{p}.{e}.approvals.resolved` | Approval resolved |
| `{p}.{e}.tasks.task_available` | Task re-queued after approval |
| `{p}.{e}.agent.{to}.delegation` | Delegation sent |
| `{p}.{e}.agent.{from}.delegation_response` | Delegation accepted/rejected/completed |
| `{p}.{e}.stream.{stream_id}` | Streaming events |
| `{p}.{e}.notification.send` | Notification dispatch (from send_notification tool) |

### coordinator

| Subject | Direction |
|---------|-----------|
| `{p}.{e}.coordinator.task_available` | Subscribe |
| `{p}.{e}.coordinator.task_completed` | Subscribe |
| `{p}.{e}.coordinator.task_failed` | Subscribe |
| `{p}.{e}.coordinator.workflow_start` | Subscribe |
| `{p}.{e}.agent.{agent_id}.task` | Publish (dispatch) |
| `{p}.{e}.coordinator.dead_letter` | Publish |
| `{p}.{e}.coordinator.routing_decision` | Publish |
| `{p}.{e}.coordinator.workflow_completed` | Publish |
| `broodlink.metrics.coordinator` | Publish (every 60s) |

### embedding-worker

| Subject | Direction |
|---------|-----------|
| `{p}.{e}.embed.dead_letter` | Publish (outbox row exceeds max attempts) |

### heartbeat

| Subject | Direction |
|---------|-----------|
| `{p}.{e}.health` | Publish |
| `{p}.{e}.status` | Publish |
| `{p}.{e}.approvals.expired` | Publish |
| `{p}.{e}.notification.send` | Publish (when notification rule triggers) |
| `{p}.{e}.coordinator.workflow_start` | Publish (auto-postmortem on incident) |

### status-api

| Subject | Direction |
|---------|-----------|
| `{p}.platform.credentials_changed` | Publish (on Telegram register/disconnect) |

### a2a-gateway

| Subject | Direction |
|---------|-----------|
| `{p}.{e}.coordinator.task_completed` | Subscribe (chat reply delivery) |
| `{p}.{e}.coordinator.task_failed` | Subscribe (chat failure handling) |
| `{p}.platform.credentials_changed` | Subscribe (invalidates telegram_creds cache) |
| `{p}.{e}.notification.send` | Subscribe (delivers to Telegram/Slack, updates notification_log) |

---

## 8. Python Agent SDK

**Location:** `agents/broodlink_agent/` (12 modules, 829 lines)
**Requirements:** `aiohttp>=3.9`, `nats-py>=2.7`, `tenacity>=8.0`

### Modules

| Module | Lines | Purpose |
|--------|------:|---------|
| config.py | 64 | AgentConfig dataclass + env loading |
| bridge.py | 115 | BridgeClient + CircuitBreaker |
| llm.py | 79 | LLMClient (OpenAI-compatible) |
| tools.py | 58 | Tool execution loop |
| prompt.py | 48 | System prompt + memory loading |
| listener.py | 212 | NATS listener mode + delegation |
| interactive.py | 76 | Interactive REPL mode |
| memory.py | 90 | Context window management |
| cli.py | 55 | Argparse entry point |
| broodlink-agent.py | 18 | Convenience launcher |
| \_\_init\_\_.py | 6 | Package init |
| \_\_main\_\_.py | 8 | Entry point |

### AgentConfig

| Field | Env Var | Default |
|-------|---------|---------|
| agent_id | BROODLINK_AGENT_ID | — |
| agent_jwt | BROODLINK_AGENT_JWT | — |
| bridge_url | BROODLINK_BRIDGE_URL | http://localhost:3310/api/v1/tool |
| lm_url | LM_STUDIO_URL | http://localhost:1234 |
| lm_model | LM_MODEL | — |
| lm_fallback_url | LM_FALLBACK_URL | None |
| lm_fallback_model | LM_FALLBACK_MODEL | None |
| max_tool_rounds | MAX_TOOL_ROUNDS | 5 |
| max_history | MAX_HISTORY | 20 |
| max_context_tokens | MAX_CONTEXT_TOKENS | 8000 |
| request_timeout | REQUEST_TIMEOUT | 120 |
| nats_url | BROODLINK_NATS_URL | nats://localhost:4222 |
| nats_prefix | BROODLINK_NATS_PREFIX | broodlink |
| env | BROODLINK_ENV | local |
| think_mode | BROODLINK_THINK_MODE | no_think |
| agent_role | BROODLINK_AGENT_ROLE | worker |
| cost_tier | BROODLINK_COST_TIER | medium |
| enable_streaming | BROODLINK_ENABLE_STREAMING | true |

### CircuitBreaker (bridge.py)

```python
class CircuitBreaker:
    state: "closed" | "open" | "half-open"
    failure_threshold: 5
    recovery_timeout: 30.0
    failure_count: int
    last_failure_time: float
```

### LLM Fallback (llm.py)

Primary → catch exception → try fallback (if configured) → raise. Uses `_send_request()` helper with OpenAI-compatible `/v1/chat/completions` endpoint.

### Context Window Management (memory.py)

- `estimate_tokens(text)` — ~4 chars/token
- `summarize_history(messages, max_tokens, keep_recent=4)` — keeps system + last N messages, summarises middle
- `store_summary(bridge, topic, content)` — persist to long-term memory

### NATS Listener (listener.py)

- Subscribes to `{prefix}.{env}.agent.{id}.task`
- Subscribes to `{prefix}.{env}.agent.{id}.delegation`
- Handles tasks: construct prompt → chat_turn → complete_task/fail_task
- Handles delegations: auto-accepts via `accept_delegation` bridge call
- Conditional streaming: starts stream at task start, emits events per round
- Graceful shutdown: SIGINT/SIGTERM → unsubscribe, drain, deactivate

---

## 9. Dashboard (Hugo)

**Location:** `status-site/`
**Theme:** broodlink-status
**Config:** `hugo.toml` — baseURL localhost:1313, theme params for API URL/key/refresh

### Pages (15)

| Page | URL | JS Module | API Endpoints |
|------|-----|-----------|---------------|
| Dashboard | / | dashboard.js | /agents, /tasks, /summary, /health, /activity, /memory/stats |
| Agents | /agents/ | agents.js | /agents, /agent-metrics |
| Decisions | /decisions/ | decisions.js | /decisions |
| Memory | /memory/ | memory.js | /memory/stats |
| Commits | /commits/ | commits.js | /commits |
| Audit Log | /audit/ | audit.js | /audit?agent_id&operation |
| Beads | /beads/ | beads.js | /beads?status |
| Approvals | /approvals/ | approvals.js | /approvals, /approval-policies |
| Delegations | /delegations/ | delegations.js | /negotiations |
| Guardrails | /guardrails/ | guardrails.js | /guardrails, /violations |
| A2A | /a2a/ | a2a.js | /a2a/card, /a2a/tasks |
| Knowledge Graph | /knowledge-graph/ | knowledge-graph.js | /kg/stats, /kg/entities, /kg/edges (v0.5.0) |
| Control Panel | /control/ | control.js | /agents, /budgets, /tasks, /workflows, /guardrails, /violations, /dlq, /webhooks, /webhook-log, /users, /formulas (v0.6.0+v0.7.0) |
| Chat | /chat/ | chat.js | /chat/sessions, /chat/stats, /chat/sessions/:id/messages, /chat/sessions/:id/close (v0.7.0) |
| Login | /login/ | auth.js | /auth/login, /auth/logout, /auth/me (v0.7.0) |

### v0.6.0 Dashboard Fixes

**Pending Tasks metric** — Now uses `counts_by_status.pending` from the API instead of filtering the `recent` array (which only returns 20 items and missed tasks not in the recent list).

**Task Agent column** — Now uses `assigned_agent` field (the actual API field) instead of `agent_id`/`agent_name`.

**Memory chart** — Uses `content_length` integer from API with `unit: 'chars'` label instead of raw `content.length` string length. API no longer sends full content blobs. Returns `agents_count` and `topics_count` summary stats.

**Decisions layout** — Fixed `id="decision-search"` to `id="decisions-search"`, added missing `decisions-count` element.

**Memory layout** — Added missing `memory-total`/`memory-updated` metric cards, fixed `id="topics-chart"` to `id="memory-topics"`.

### JavaScript (17 files, ~3,148 lines)

**utils.js** (213 lines) — `window.Broodlink` namespace: fetchApi(), formatRelativeTime(), statusDot(), escapeHtml(), debounce(). Config from `<meta>` tags or `window.BroodlinkConfig`.

**dashboard.js** (359 lines) — Parallel fetches (6 endpoints), metric cards, health grid, agent roster, activity feed, task table with stream progress bars, SSE `connectStream()`, memory/stats integration. v0.6.0: uses `counts_by_status.pending` and `assigned_agent`.

**agents.js** (92 lines) — Agent cards with `renderMetrics()`: score badge, success rate, load bar, avg response time.

**approvals.js** (379 lines) — Status filter, approval gate cards, policy editor (CRUD), review modal.

**decisions.js** (189 lines) — Client-side search, confidence badges (0-40 low/red, 40-80 medium/amber, 80+ high/green).

**memory.js** (171 lines) — Stats cards, bar chart of top topics via `BroodlinkCharts.renderBarChart()`. v0.6.0: uses `content_length` from API with `unit: 'chars'` label.

**charts.js** (123 lines) — Pure CSS/HTML bar charts, WCAG 2.1 AA accessible (role="img", aria-label).

**guardrails.js** (125 lines) — Policy list, violation log.

**delegations.js** (87 lines) — Delegation cards with status.

**audit.js** (60 lines) — Filterable audit table with status dots.

**beads.js** (57 lines) — Issue cards, status filter.

**commits.js** (45 lines) — Dolt commit table (8-char hash, author, message, date).

**a2a.js** (62 lines) — AgentCard display (JSON), A2A task mappings table.

**knowledge-graph.js** (112 lines) — IIFE pattern. Fetches `/api/v1/kg/stats`, `/api/v1/kg/entities`, `/api/v1/kg/edges`. Renders metric cards (total entities, active edges, historical edges), entity type distribution chart, top relationship types chart, most connected entities table, recent edges table. Auto-refresh via `setInterval(load, BL.REFRESH_INTERVAL)`. (v0.5.0)

**chat.js** (147 lines) — IIFE pattern. Fetches `/api/v1/chat/sessions` and `/api/v1/chat/stats`. Platform icon badges (S/T/TG for Slack/Teams/Telegram). Session cards with platform, user, message count, agent, status. Message modal for thread view. Close session action. Platform and status filter dropdowns. Auto-refresh every 10s. Exports: `window.ChatPage.viewMessages()`, `window.ChatPage.closeSession()`. (v0.7.0)

**auth.js** (129 lines) — Session-based auth client module. Uses sessionStorage for token, role, expires, username. Functions: `getToken()`, `getRole()`, `isAuthenticated()`, `canWrite()` (operator+), `isAdmin()`, `login(username, password)`, `logout()`, `requireAuth()` (redirects to /login/ if unauthenticated), `injectRoleBadge()` (renders username + role badge + logout button). All pages call `requireAuth()` on load when dashboard_auth is enabled. Public via `window.BLAuth.*`. (v0.7.0)

**control.js** (~798 lines) — IIFE pattern. Tabbed admin interface with 10 panels (Agents, Budgets, Tasks, Workflows, Guardrails, Webhooks, DLQ, Chat, Formulas, Users, Telegram). Summary metrics (4 parallel fetches). Toast notifications replacing alerts. Badge helper for status styling. Empty state SVGs. Budget card layout with color-coded progress bars. Workflow progress indicators. Users tab (admin only): create/edit/toggle/reset-password with role enforcement. Formulas tab: CRUD, toggle, definition JSON display. Telegram tab: status card displays access code and authorized user count; registration form clears properly before reloading status. XSS protection via escapeHtml. Auto-refresh via setInterval. Users tab hidden for non-admin roles. Exports: toggleAgent, setBudget, cancelTask, retryDlq, addWebhook, toggleWebhook, deleteWebhook, createUser, changeUserRole, toggleUser, resetPassword, createFormula, toggleFormula. (v0.6.0+v0.7.0)

### CSS (~1,349 lines)

**Custom Properties:**
- `--bg-primary: #0b1120`, `--bg-secondary: #111827`, `--bg-tertiary: #1e293b`
- `--text-primary: #e2e8f0` (13.5:1 contrast), `--text-secondary: #94a3b8` (4.6:1)
- `--accent: #3a6ea5`, `--green/#amber/#red/#blue` (all 4.5:1+)
- Fonts: Inter (sans), JetBrains Mono (mono)

**Layout:** CSS Grid 2-column (sidebar 15rem + main). Responsive: ≤48rem → single column.

**Accessibility:** skip-link, focus-visible ring, .visually-hidden, @media(prefers-reduced-motion), status combines icon+text+color.

**v0.3.0 additions:** `.agent-metrics`, `.agent-score-badge`, `.success-rate-good/warn/bad`, `.agent-metric-bar`, `.stream-bar/.stream-bar-fill`, `.page-desc`, `[data-tooltip]` (CSS-only tooltips).

**v0.6.0 additions:** `.ctrl-section-header`, `.btn-primary/.btn-danger/.btn-ghost/.btn-sm`, `.ctrl-btn-group`, `.ctrl-budget-grid/.ctrl-budget-card/.ctrl-budget-bar`, `.ctrl-badge` variants (ok/pending/failed/claimed/offline), `.ctrl-progress/.ctrl-progress-bar`, `.ctrl-empty`, `.ctrl-toast-container/.ctrl-toast-success/.ctrl-toast-error` with enter/exit animations, responsive breakpoints.

**v0.7.0 additions:** `.chat-session-card`, `.chat-platform-badge` (slack/teams/telegram colour variants), `.chat-modal`, `.chat-message` (inbound/outbound), `.login-form`, `.login-container`, `.role-badge` (viewer/operator/admin), `.auth-bar`, `.users-table`.

---

## 10. Workflow Formulas

**Location:** `.beads/formulas/` (4 system + 1 custom TOML files)
**Execution:** Agent calls `start_workflow(formula_name, params)` → coordinator loads formula TOML → creates step 0 task → on completion, creates step 1 with accumulated results → repeats until all steps done or a step fails. State tracked in `workflow_runs` table.

### research.formula.toml
Steps: gather_sources (researcher) → analyze_sources (researcher) → synthesize_report (writer)
Tools: semantic_search, recall_memory, store_memory, log_decision
Params: `topic` (string, required)

### build-feature.formula.toml
Steps: plan (architect) → implement (developer) → test (tester) → document (writer)
Tools: semantic_search, recall_memory, store_memory, complete_task, log_work, log_decision
Params: `feature_name` (string, required), `feature_description` (string, required)

### daily-review.formula.toml
Steps: gather_metrics (analyst) → identify_blockers (analyst) → generate_summary (writer)
Tools: list_tasks, get_decisions, get_work_log, semantic_search, store_memory, log_decision
Params: `date` (string, optional, default "today")

### knowledge-gap.formula.toml
Steps: audit_memory (analyst) → prioritize (architect) → fill_gaps (researcher)
Tools: semantic_search, recall_memory, store_memory, log_decision
Params: `max_gaps` (integer, optional, default 5)

### custom/incident-postmortem.formula.toml (post-v0.7.0)
Steps: gather_evidence (analyst) → root_cause_analysis (analyst) → prevention_plan (architect) → final_report (writer)
Tools: get_audit_log, list_tasks, get_decisions, recall_memory, store_memory, log_decision, log_work
Params: `incident_type` (string, required), `time_window_hours` (integer, optional, default 24)
Auto-triggered by heartbeat notification rules when `condition_config.auto_postmortem = true`.

---

## 11. Scripts

| Script | Lines | Purpose |
|--------|------:|---------|
| bootstrap.sh | 120 | One-shot setup: prerequisites, infrastructure, secrets, databases, build, onboard, start |
| start-services.sh | 110 | Start/stop all 7 services + Hugo, auto-generate JWTs for service agents |
| build.sh | 28 | cargo deny → cargo test → cargo build --release → hugo --minify |
| db-setup.sh | 144 | Create Postgres/Dolt databases, run all 30 migrations, create Qdrant collections |
| secrets-init.sh | 112 | Generate age keypair, .sops.yaml, RSA keypair for JWT, env file |
| onboard-agent.sh | 137 | Generate RS256 JWT, register agent via beads-bridge |
| backfill-search-index.sh | 66 | One-time backfill of Postgres memory_search_index from Dolt agent_memory (v0.4.0) |
| backfill-knowledge-graph.sh | ~60 | One-time backfill of KG entities from existing memories (v0.6.0) |
| dev.sh | 32 | Start/stop/logs for podman-compose + Hugo dev server |
| generate-tls.sh | 139 | Self-signed CA + server cert with SAN for all services |
| launchagents.sh | 74 | Install/uninstall/status/restart macOS LaunchAgents |
| rotate-jwt-keys.sh | ~40 | Generate new JWT keypair with kid fingerprint, retire old keys after grace period (v0.6.0) |
| seed-formulas.sh | ~55 | Seed Postgres formula_registry from `.beads/formulas/*.formula.toml` (v0.7.0) |
| create-admin.sh | ~80 | Create dashboard admin user with bcrypt hash (pgcrypto fallback, no Python dependency required) (v0.7.0) |
| start-gateway.sh | ~10 | Start a2a-gateway with `.env` sourcing for secrets (v0.7.0) |

### v0.7.0 Script Changes

**seed-formulas.sh** — Scans `$FORMULA_DIR/*.formula.toml` (default: `.beads/formulas/`). Python script converts TOML → JSON definition (extracts formula metadata, parameters array, steps, on_failure handler). Inserts into `formula_registry` with `is_system=true`, `author='system'`, ON CONFLICT (name) DO NOTHING (idempotent). Connects via podman exec → local psql fallback.

**create-admin.sh** — Creates dashboard users with bcrypt password hashing. Supports `--username`, `--password`, `--role` (default: admin), `--display-name`, `--cost` (bcrypt cost, default: 12). Tries 3 bcrypt methods in order: Python `bcrypt` module → Python `passlib` module → Postgres `pgcrypto` extension. Generates UUID, inserts into `dashboard_users` with ON CONFLICT (username) DO NOTHING. Connects via local psql → podman exec fallback.

### v0.4.0 Script Changes

**start-services.sh** — Now auto-generates JWT tokens for `mcp-server` and `a2a-gateway` service agents before starting services. Uses RS256 signing with the same private key used by `onboard-agent.sh`. Only runs if token file is missing and `jwt-private.pem` exists (idempotent). Tokens stored at `~/.broodlink/jwt-{agent}.token` (mode 600).

**backfill-search-index.sh** — New script for one-time migration from Dolt `agent_memory` to Postgres `memory_search_index`. Uses UPSERT (safe to re-run). Required after applying migration 012 on existing data.

**backfill-knowledge-graph.sh** (v0.6.0) — One-time backfill of knowledge graph entities from existing memories. Queries all Dolt `agent_memory` IDs, checks `kg_entity_memories` to skip already-processed, inserts outbox rows with `operation='kg_extract'` for unprocessed. Rate-limited (1/sec), prints progress, safe to re-run.

### generate-tls.sh (v0.3.0)

Generates: `tls/certs/ca.crt`, `ca.key`, `server.crt`, `server.key`
SAN includes: localhost, beads-bridge, status-api, a2a-gateway, mcp-server, coordinator, heartbeat, embedding-worker, *.broodlink.local, 127.0.0.1, ::1
CA validity: 10 years. Server cert: 1 year. Key size: 4096-bit RSA.

---

## 12. Test Suites

### Rust Unit Tests (249 total)

| Crate | Tests |
|-------|------:|
| beads-bridge | 83 |
| coordinator | 35 |
| embedding-worker | 31 |
| mcp-server | 23 |
| a2a-gateway | 28 |
| status-api | 28 |
| broodlink-runtime | 5 |
| broodlink-config | 4 |
| broodlink-secrets | 5 |
| broodlink-telemetry | 7 |

**v0.4.0 additions (beads-bridge):** min-max normalise (3), temporal decay (4), truncate content (3), cosine similarity (2), BM25 query construction (2), score fusion (3), fallback method detection (2).

**v0.5.0 additions:** beads-bridge +18 (KG tool tests), embedding-worker +24 (KG extraction pipeline tests), status-api +6 (KG endpoint tests).

**v0.6.0 additions:** +8 tests across beads-bridge (budget middleware, DLQ tools), coordinator (workflow branching, DLQ retry), status-api (control panel endpoints).

**v0.7.0 additions:** +45 tests. coordinator +4 (formula lookup, task routing). a2a-gateway +17 (chat dedup, Brave cache, semaphore, auth gate, strip_think_tags, normalize_search_query). beads-bridge +12 (chat tool param extraction, registry, readonly classification, formula validation, formula tool registry). status-api +10 (UserRole ordering/display/serde/from_str, require_role hierarchy, AuthContext, bcrypt roundtrip, request deserialization). broodlink-telemetry +2 (guard config, OTLP endpoint).

### End-to-End Tests (16 sections, ~97 assertions)

| Section | Coverage |
|---------|----------|
| 1. Service Health | beads-bridge + status-api health, 5 dependency checks, 4 broodlink_runtime checks |
| 2. Authentication | JWT validation (valid/missing/bad), API key validation |
| 3. Tool Calls | 14 tool round-trips (CRUD memory, tasks, work log, decisions), unknown tool error |
| 4. Status API | 15 endpoints return data |
| 5. Background Services | Coordinator/heartbeat/embedding-worker log verification |
| 6. SSE Stream | start_stream → emit_stream_event → SSE endpoint |
| 7. NATS Integration | 5/5 services connected, broodlink_runtime usage |
| 8. Error Handling | Rapid calls, malformed JSON, missing params, post-error health |
| 9. Database Round-trip | Memory CRUD + task create/list verification |
| 10. Embedding & Semantic Search | Semantic search, hybrid search (method, BM25 scores, content, weights, decay, BM25 round-trip, delete cleanup) |
| 11. Dashboard Metrics | Summary decisions field, memory/stats total_count |
| 12. All Services Health | mcp-server, a2a-gateway, AgentCard, Hugo dashboard |
| 13. Dashboard Page Completeness | 15 pages all return HTTP 200 (including /knowledge-graph/, /control/, /chat/, /login/) |
| 14. Knowledge Graph Tools | graph_search, graph_traverse, graph_stats, graph_update_edge round-trips (v0.5.0) |
| 15. Dashboard Control Panel | /control/ tabs render, budget/agent toggle, DLQ inspect, webhook CRUD (v0.6.0) |
| 16. Chat & Auth Endpoints | /chat/sessions, /chat/stats, /auth/login, /auth/me, /formulas, /users round-trips (v0.7.0) |

**v0.4.0 additions:** Sections 10–13 (hybrid search verification, BM25 round-trip, search index sync, delete cleanup, memory/stats metric, all 7 service health checks, dashboard page completeness checks).

**v0.5.0 additions:** Section 14 (KG tool round-trips), dashboard page count 11→12.

**v0.6.0 additions:** Section 15 (dashboard control panel), dashboard page count 12→13.

**v0.7.0 additions:** Section 16 (chat/auth/formula/user endpoints), dashboard page count 13→15.

### Shell Test Suites (18 suites + 3 integration, plus Rust unit tests = 22 in run-all.sh)

| Suite | Tests | Type |
|-------|------:|------|
| Bridge tool round-trips | 57 | Integration (requires services) |
| Agent regression | 64 | Structural + syntax |
| Frontend smoke | 37 | Integration (requires Hugo) |
| Deployment config | 34 | Structural |
| DOM ID cross-check | ~46 | Structural (v0.5.0) |
| API smoke | 35 | Integration (requires status-api) |
| JS regression | 43 | Structural |
| A2A protocol | 15 | Structural + integration |
| Stream delivery | 16 | Structural + integration |
| Knowledge Graph | ~20 | Integration (requires services, v0.5.0) |
| MCP protocol | 12 | Structural + integration |
| Coordinator routing | 10 | Structural + integration |
| Security audit | ~12 | Structural |
| LaunchAgent portability | ~8 | Structural |
| Bridge auth rejection | ~6 | Integration |
| Bridge error cases | ~6 | Integration |
| v0.6.0 regression | ~105 | Structural + integration (v0.6.0) |
| Control panel | ~116 | Integration (requires services, v0.6.0) |
| Chat integration | 9 | Integration (requires all services, v0.7.0) |
| Formula registry | 11 | Integration (requires status-api + Postgres, v0.7.0) |
| Dashboard auth | 19 | Integration (requires status-api + Postgres, v0.7.0) |

### Run All

```bash
cargo test --workspace          # 249 unit tests
bash tests/e2e.sh               # ~97 end-to-end tests
bash tests/run-all.sh           # 22 suites (21 shell + 1 cargo test)
bash tests/chat-integration.sh  # 9 chat integration tests
bash tests/formula-registry.sh  # 11 formula registry tests
bash tests/dashboard-auth.sh    # 19 dashboard auth tests
```

---

## 13. Deployment

### Dev (podman-compose.yaml, 150 lines)

**Infrastructure:** dolt (3307), postgres (5432), nats (4222/8222), qdrant (6333), ollama (11434)
**Optional:** jaeger (16686/4317) — always enabled (v0.6.0: no longer behind profile)
**Services:** beads-bridge (3310), coordinator, heartbeat, embedding-worker, status-api (3312)
**Volumes:** dolt-data, pg-data, nats-data, qdrant-data, ollama-data

### Prod (podman-compose.prod.yaml, v0.3.0)

**Additions over dev:**
- **3-node NATS cluster** (nats-1/2/3, ports 4222/4223/4224, cluster routes on 6222)
- **Postgres primary + replica** (5432/5433, WAL replication, `pg_basebackup`)
- **TLS volume mounts** (`./tls/certs:/etc/broodlink/tls:ro`) on 7 services
- **Health checks** on all infrastructure services (dolt, postgres-primary/replica, nats-1, qdrant, ollama)
- **A2A gateway** (3313) and **MCP server** (3311) services
- **Jaeger** always enabled (not behind profile)
- **Dependency conditions** using `service_healthy`

### Service Startup (native)

```bash
BROODLINK_CONFIG=./config.toml nohup ./target/release/<service> > /tmp/broodlink-<service>.log 2>&1 &
```

### macOS LaunchAgents

8 plist templates in `launchagents/` with `__BROOD_DIR__` and `__HOMEBREW_PREFIX__` placeholders. Managed via `scripts/launchagents.sh install|uninstall|status|restart`.

---

## 14. Authentication & Secrets

### JWT RS256

- **Generation:** `scripts/onboard-agent.sh` — RSA 4096-bit keypair, 1-year expiry
- **Service agent JWTs:** `scripts/start-services.sh` auto-generates for `mcp-server` and `a2a-gateway` if missing (v0.4.0)
- **Claims:** `{sub, agent_id, iat, exp}`
- **Storage:** `~/.broodlink/jwt-{agent_id}.token` (mode 600)
- **Validation:** beads-bridge auth middleware extracts Bearer token, verifies RS256 signature
- **Public key:** from secrets (`BROODLINK_JWT_PUBLIC_KEY`)

### JWT Key Rotation (v0.6.0)

- **kid-based multi-key validation:** beads-bridge `AppState` now holds `jwt_decoding_keys: Vec<(String, DecodingKey)>` keyed by `kid` (key ID). JWT headers must include `kid` to select the correct validation key.
- **JWKS endpoint:** `GET /api/v1/.well-known/jwks.json` returns all active public keys in JWKS format for external services to validate tokens.
- **Rotation script:** `scripts/rotate-jwt-keys.sh` generates a new RSA 4096-bit keypair with a `kid` derived from the public key fingerprint (SHA-256, first 8 hex chars). The old key remains valid for `jwt.grace_period_hours` (default 24h) to allow graceful rollover.
- **Grace period:** During the grace period, both old and new keys are accepted. After expiry, the old key is removed from the active set.

### Secrets Providers

| Provider | Usage | Config |
|----------|-------|--------|
| SOPS | Dev | age + SOPS encryption, `secrets.enc.json`, cache 300s |
| Infisical | Prod | REST API, 3 retries, 10s timeout |

### Status-API Auth

Dual-mode authentication middleware:
- **API key**: `X-Broodlink-Api-Key` header matched (constant-time comparison) against `BROODLINK_STATUS_API_KEY`. Grants `Admin` role.
- **Session token**: `X-Broodlink-Session` header looked up in `dashboard_sessions` with expiry check. Grants the user's assigned role (viewer/operator/admin).

**RBAC enforcement (v0.7.0 security hardening):** Every mutation endpoint calls `require_role()`:
- **Admin**: agent toggle, budget set, webhook create/toggle/delete, Telegram register/disconnect, formula create/update/toggle, approval policy upsert, user management
- **Operator**: task cancel, chat assign/close, approval review
- **Viewer**: read-only access to all GET endpoints

**Password policy:** `create-admin.sh` enforces 12-character minimum. Bcrypt hashing with configurable cost (default 12). Interactive confirmation prompt when run without `--password`.

**Session management:** `POST /auth/logout-all` invalidates all sessions for the current user.

### A2A Gateway Auth

Bearer token matched against env var named by `config.a2a.api_key_name`.

### Security Headers (v0.7.0)

All HTTP services (beads-bridge, status-api, mcp-server, a2a-gateway) apply security headers via middleware:

| Header | Value |
|--------|-------|
| `Strict-Transport-Security` | `max-age=63072000; includeSubDomains` |
| `Content-Security-Policy` | `default-src 'self'` |
| `X-Frame-Options` | `DENY` |
| `X-Content-Type-Options` | `nosniff` |
| `Cache-Control` | `no-store` |
| `Pragma` | `no-cache` |
| `Permissions-Policy` | `camera=(), microphone=(), geolocation=()` |

### Input Validation (v0.7.0)

| Control | Scope |
|---------|-------|
| **Body size limit** | 10 MiB on all HTTP services via `DefaultBodyLimit` |
| **Query LIMIT clamping** | All paginated queries clamped to 1–1000 via `clamp_limit()` |
| **SSRF protection** | `validate_webhook_url()` blocks localhost, link-local, RFC 1918, metadata endpoints |
| **Regex anchoring** | All patterns use `^...$` to prevent ReDoS |
| **Path traversal** | KG entity names validated to prevent directory traversal |
| **SQL parameterization** | Shell scripts use psql `-v` binding or piped `\set` instead of string interpolation |

### Container Hardening (v0.7.0)

Production compose (`podman-compose.prod.yaml`) applies:
- `read_only: true` — immutable root filesystem
- `security_opt: no-new-privileges` — prevents privilege escalation
- Dropped capabilities — only essential caps retained

---

## 15. Resilience Patterns

### Rate Limiting (beads-bridge)

Token bucket per agent: `requests_per_minute_per_agent` (60) with `burst` (10). Refill rate = 1 token/sec. HTTP 429 when exhausted.

### Circuit Breakers

| Service | Location | Threshold | Recovery |
|---------|----------|-----------|----------|
| Qdrant | beads-bridge | 5 failures | 30s half-open |
| Ollama | beads-bridge | 5 failures | 30s half-open |
| Reranker | beads-bridge | 5 failures | 30s half-open (v0.4.0) |
| Qdrant | embedding-worker | 5 failures | 30s half-open |
| Ollama | embedding-worker | 5 failures | 30s half-open |
| KG Extraction | embedding-worker | 5 failures | 30s half-open (v0.6.0) |
| Bridge | Python agent | 5 failures | 30s recovery |

States: CLOSED → OPEN (reject all, HTTP 503) → HALF_OPEN (test one) → CLOSED.

### Retries

| Operation | Strategy |
|-----------|----------|
| Coordinator task claim | Exponential backoff: 1s, 2s, 4s, 8s, 16s (max 5) |
| Embedding worker outbox | Max 3 attempts |
| Python bridge calls | 3 attempts with tenacity |
| Infisical secrets | 3 attempts, exp backoff (100ms base) |

### SSE Stream Limits (v0.7.0)

- Maximum 100 concurrent streams per beads-bridge instance
- 1-hour TTL with periodic reaper (60s interval)
- Clients exceeding the stream cap receive a `stream_limit_reached` error

### Guardrails (beads-bridge)

Checked before every tool execution via `check_guardrails()`. **Fail-closed** (v0.7.0): DB errors during guardrail check reject the call instead of allowing it through. Rule types:
- **tool_block:** Deny specific tools for specific agents
- **rate_override:** Per-agent token bucket rate limits (config: `{"agents": {"claude": 30, "qwen3": 120}}` RPM). Secondary limiter alongside the global rate limiter.
- **content_filter:** Regex matching against serialized tool params (config: `{"patterns": ["secret|password"], "tools": ["store_memory"], "agents": ["*"]}`). Blocks on match.
- **scope_limit:** Restrict tool parameters

HTTP 403 when blocked. Violations logged to `guardrail_violations`.

### Approval Gates (beads-bridge)

Human-in-the-loop for sensitive operations. Gate types: pre_dispatch, pre_completion, budget, custom. Auto-approve via policies with confidence threshold (default 0.8). Gates expire after configurable minutes (default 60). Heartbeat handles expiry.

**Enforcement:** `check_guardrails()` queries active `pre_dispatch` approval policies. If a policy explicitly lists `tool_names` in its conditions and the current agent+tool matches, an approved/auto_approved gate must exist (within 1 hour) or the call is rejected with 403. Read-only tools (30+ tools like `recall_memory`, `list_agents`, `get_task`, `graph_search`, `graph_traverse`, `graph_stats`, etc.) are always exempt via `is_readonly_tool()` whitelist.

### Budget Enforcement (v0.6.0)

Per-agent token budgets enforced as middleware in beads-bridge:
- **check_budget** runs before every tool dispatch (except read-only tools). Returns HTTP 402 `BudgetExhausted` if agent's `budget_tokens <= 0`.
- **deduct_budget** runs after successful tool execution. Decrements budget by tool cost (from `tool_cost_map` or `budget.default_tool_cost`).
- **Daily replenishment** by heartbeat at `budget.replenish_hour_utc`.
- **Low budget alert** triggers `budget.low` webhook event when balance drops below `budget.low_budget_threshold`.
- Budgets are managed via `get_budget`, `set_budget`, `get_cost_map` tools and the dashboard control panel.

### Dead-Letter Queue Auto-Retry (v0.6.0)

Failed tasks that exhaust coordinator claim retries are persisted to `dead_letter_queue` (Postgres) instead of being lost:
- **Auto-retry loop** in coordinator checks DLQ every `dlq.check_interval_secs` (default 60s).
- **Exponential backoff:** `next_retry_at = NOW() + backoff_base_ms * 2^retry_count`.
- **Max retries:** After `dlq.max_retries` (default 3) failed retries, entry remains unresolved.
- **Manual management** via `inspect_dlq`, `retry_dlq_task`, `purge_dlq` tools and dashboard DLQ panel.
- Webhook `task.failed` event fires when a DLQ entry reaches max retries.

### Hybrid Search Graceful Degradation (v0.4.0)

The `hybrid_search` tool degrades gracefully when backends are unavailable:
- Qdrant or Ollama circuit open → BM25-only fallback (keyword search still works)
- Postgres memory_search_index unavailable → semantic-only fallback (vector search still works)
- Both down → empty results, no crash
- Reranker circuit open → skip reranking, use fusion scores as-is
- `store_memory` / `delete_memory` Postgres index writes are non-fatal (warns on failure)

### Telegram Bot Security (v0.7.0)

**Access control layers:**

| Layer | Mechanism | Effect |
|-------|-----------|--------|
| Bot token | Stored in `platform_credentials` (DB), not config files | Token never in git or logs |
| Webhook secret | `X-Telegram-Bot-Api-Secret-Token` header verified via constant-time comparison | Prevents forged webhook calls |
| Access code | 8-char alphanumeric from CSPRNG, shared privately by admin | Prevents unauthorized bot usage |
| Allow list | `meta->>'allowed_user_ids'` checked at both entry points | Only verified users reach LLM |
| Log sanitization | Message text logged only for authenticated users | Auth codes never appear in logs |
| History exclusion | Auth code messages return before `handle_chat_message()` / `process_telegram_update()` | Codes never stored in `chat_messages` or sent to LLM |
| Cache invalidation | NATS `credentials_changed` event on register/disconnect | No stale allow list window |
| Credential refresh | Re-fetched after `getUpdates` returns (not before 30s blocking poll) | Eliminates disconnect/re-register race |
| Cascade delete | Disconnect removes all chat data + credentials atomically | No orphaned secrets |

**Threat model:**
- **Public Telegram user IDs:** User IDs are public (visible to anyone in a shared group). The allow list is an authorization control, not a secret. The access code provides the authentication layer.
- **Brute force on access code:** 36^8 ≈ 2.8 trillion combinations. At Telegram's rate limits (~30 messages/sec per bot), brute force would take ~3,000 years.
- **Code leakage via logs:** Mitigated by logging message text only after auth gate passes. Auth attempt logs contain user_id and outcome only, never message content.
- **Stale cache after re-registration:** Mitigated by NATS-based instant cache invalidation + post-poll credential refresh. Maximum stale window: 0s (NATS) to 30s (in-flight getUpdates).

**Error scenarios:**
- Telegram API unreachable during `getUpdates` → 5s backoff, retry
- Telegram returns error (e.g., token revoked) → clear token cache, 10s backoff
- NATS `credentials_changed` not delivered → 60s cache TTL is safety net
- Ollama unavailable → semaphore-guarded, returns busy message to other callers
- Brave API key invalid → error string returned to LLM, model answers without search

### Telegram Credential Lifecycle

```
Dashboard: Register Bot
    │
    ├── Validate token (Telegram getMe API)
    ├── Generate auth_code (CSPRNG, 8 chars)
    ├── Store in platform_credentials (meta = {auth_code})
    ├── Publish NATS credentials_changed
    └── Return auth_code to dashboard UI
            │
            ▼
User sends auth_code to bot
    │
    ├── Gateway verifies code match
    ├── add_telegram_allowed_user() → JSONB append + cache invalidate
    └── Reply "Authenticated!"
            │
            ▼
Normal operation (user in allow list)
    │
    ▼
Dashboard: Disconnect Bot
    │
    ├── Call Telegram deleteWebhook
    ├── Cascade delete: replies → messages → sessions → credentials
    ├── Publish NATS credentials_changed
    └── Gateway: cache invalidated → polling loop enters "no token" sleep
```

---

## 16. Cargo Workspace

**Members (11):**
```
crates/broodlink-secrets, crates/broodlink-config, crates/broodlink-telemetry, crates/broodlink-runtime
rust/beads-bridge, rust/coordinator, rust/heartbeat, rust/embedding-worker
rust/status-api, rust/mcp-server, rust/a2a-gateway
```

**Metadata:** version 0.7.0, edition 2021, license AGPL-3.0-or-later

### Workspace Dependencies

| Crate | Version | Features |
|-------|---------|----------|
| tokio | 1 | full |
| serde | 1 | derive |
| serde_json | 1 | |
| thiserror | 1 | |
| anyhow | 1 | |
| tracing | 0.1 | |
| tracing-subscriber | 0.3 | json, env-filter |
| config | 0.14 | |
| uuid | 1 | v4 |
| sqlx | 0.8 | mysql, postgres, runtime-tokio-rustls, chrono, json, uuid |
| async-nats | 0.35 | |
| reqwest | 0.11 | json, rustls-tls |
| axum | 0.7 | json |
| tower | 0.4 | |
| tower-http | 0.5 | cors, trace |
| chrono | 0.4 | serde |
| rustls | 0.22 | |
| tokio-rustls | 0.25 | |
| jsonwebtoken | 9 | |
| async-trait | 0.1 | |
| tempfile | 3 | (dev) |
| opentelemetry | 0.27 | trace |
| opentelemetry_sdk | 0.27 | rt-tokio |
| opentelemetry-otlp | 0.27 | tonic |
| tracing-opentelemetry | 0.28 | |
| axum-server | 0.7 | tls-rustls |
| futures | 0.3 | |
| tokio-stream | 0.1 | |
| toml | 0.8 | |
| regex | 1 | (beads-bridge only) |
| rand | 0.8 | (status-api only) |

### License Audit (deny.toml)

**Allowed:** MIT, Apache-2.0, AGPL-3.0-only, BSD-2/3-Clause, ISC, MPL-2.0, Unicode, Zlib, CDLA-Permissive, OFL
**Banned deps:** openssl-sys, native-tls (rustls enforced)
**Ignored advisories:** RUSTSEC-2025-0134 (rustls-pemfile), RUSTSEC-2023-0071 (rsa)

---

## 17. Examples

**Location:** `examples/` (4 files, 736 lines)

Working multi-agent demos that prove the system works end-to-end — not toy scripts, but multi-step workflows with real LLM reasoning, persistent memory, inter-agent messaging, and knowledge graph queries.

### research-report/ — Two Agents Collaborate on a Research Report

A researcher agent investigates a topic, then hands off to a writer agent that synthesizes a structured report. Both agents use the beads-bridge HTTP API directly (no SDK import — pure `aiohttp` + direct HTTP calls).

**Files:**

| File | Lines | Purpose |
|------|------:|---------|
| run.sh | 143 | Orchestrator: prerequisite checks, runs agents sequentially, displays output |
| researcher.py | 257 | Agent 1 (qwen3 JWT): registers, searches memories + KG, generates research via Ollama, stores findings/takeaways, sends handoff message |
| writer.py | 276 | Agent 2 (claude JWT): reads handoff, retrieves findings, synthesizes report via Ollama, stores report, logs decision |

**Bridge tools used (10 distinct):**
`agent_upsert`, `semantic_search`, `graph_search`, `store_memory`, `recall_memory`, `send_message`, `read_messages`, `log_work`, `log_decision`

**Architecture:**
```
researcher.py (qwen3 JWT)          writer.py (claude JWT)
  │                                   │
  ├─ agent_upsert                     ├─ agent_upsert
  ├─ semantic_search (memories)       ├─ read_messages (handoff)
  ├─ graph_search (KG entities)       ├─ recall_memory (findings)
  ├─ Ollama chat/completions          ├─ recall_memory (takeaways)
  ├─ store_memory (findings)          ├─ semantic_search (extra context)
  ├─ store_memory (takeaways)         ├─ Ollama chat/completions
  ├─ send_message → claude            ├─ store_memory (report)
  └─ log_work                         ├─ log_decision
                                      └─ log_work
```

**Run:**
```bash
cd examples/research-report && bash run.sh

# Custom topic:
TOPIC="quantum computing applications" bash run.sh

# Custom model:
MODEL="qwen3:32b" bash run.sh
```

**Prerequisites:** beads-bridge on :3310, Ollama on :11434 with qwen3:1.7b, JWT tokens for qwen3 + claude agents, Python 3.8+ with `aiohttp`.

**Artifacts persisted:** Research findings, takeaways, and final report stored in Broodlink memory (queryable via `recall_memory topic_search="research:"`), plus work log and decision log entries.

---

## Changelog: v0.6.0 → v0.7.0

| Area | Change |
|------|--------|
| **Tools** | Added 6 tools (78 → 84): `list_chat_sessions`, `reply_to_chat`, `create_formula`, `get_formula`, `update_formula`, `list_formulas`; post-v0.7.0: added 6 proactive tools (84 → 90): `schedule_task`, `list_scheduled_tasks`, `cancel_scheduled_task`, `send_notification`, `create_notification_rule`, `list_notification_rules` |
| **beads-bridge** | +644 lines (~7,260 → ~7,904): 2 chat tool handlers, 4 formula tool handlers, formula validation, chat/formula tool schemas |
| **status-api** | +1,457 lines (~2,389 → ~3,846): 5 chat endpoints, 5 formula endpoints, 5 user management endpoints, 3 auth endpoints, UserRole enum, dual auth middleware (session + API key), bcrypt password hashing, Telegram bot registration with access code generation (`rand = "0.8"`), cascade delete on disconnect (CTE removing chat_reply_queue, chat_messages, chat_sessions, platform_credentials), NATS publish of `credentials_changed` on register/disconnect |
| **a2a-gateway** | +1,995 lines (~1,545 → ~3,540): `handle_chat_message()` session/message pipeline, `reply_delivery_loop()` background task, platform-specific delivery (Slack/Teams/Telegram), `handle_task_completion_for_chat()`, NATS task_completed/task_failed subscriptions, NATS `credentials_changed` subscription (immediate cache invalidation), `call_ollama_chat()` direct LLM with tool calling loop, `execute_brave_search()`, Telegram long-polling with offset persistence and post-getUpdates credential refresh, `.env` loader, `AppState` efficiency fields (ollama_semaphore, brave_cache, inflight_chats, ollama_client), dedup before DB/bridge, typing refresh via `tokio::select!`, `normalize_search_query()`, `chat_dedup_key()`, `strip_think_tags()`, `add_telegram_allowed_user()` (atomic JSONB append + cache invalidation), `get_telegram_creds()` expanded to 4-tuple (token, secret, allowed_user_ids, auth_code), access code authentication with allow list enforcement at webhook + polling entry points, auth code security hardening (no message text in logs for unauthenticated users, auth codes never stored in chat history) |
| **heartbeat** | +121 lines (~1,521 → ~1,642): chat session expiry cycle, dashboard session cleanup cycle; post-v0.7.0: +~150 lines: notification rule evaluation (step 5n), `check_notification_rules()` with condition evaluation, cooldown, template rendering, auto-postmortem |
| **coordinator** | +110 lines (~2,645 → ~2,755): formula_registry lookup (Postgres first, TOML fallback); post-v0.7.0: +~80 lines: `scheduled_task_loop()` (60s polling), `check_scheduled_tasks()` (promotes due tasks to task_queue) |
| **broodlink-config** | +269 lines (~982 → ~1,251): `ChatConfig` (9 fields), `ChatToolsConfig` (6 fields), `DashboardAuthConfig` (4 fields), `OllamaConfig.num_ctx`, `A2aConfig` +3 fields (ollama_concurrency, busy_message, dedup_window_secs); post-v0.7.0: `NotificationsConfig` (2 fields) |
| **config.toml** | +24 lines (~160 → ~184): `[chat]`, `[chat.tools]`, `[dashboard_auth]` sections, efficiency fields on `[a2a]` and `[ollama]`; post-v0.7.0: `[notifications]` section, 3 new skill_definitions, updated agent skills arrays |
| **Migrations 019-021, 023-024, 028** | New: `chat_sessions`, `chat_messages`, `chat_reply_queue` (019), `formula_registry` (020), `dashboard_users`, `dashboard_sessions` (021), `platform_credentials` (023), `platform_credentials.meta` JSONB (024); post-v0.7.0: `scheduled_tasks`, `notification_rules`, `notification_log` (028) |
| **Dashboard** | New: `/chat/` page (session monitoring, platform filtering, message threads), `/login/` page (session-based auth). Control panel: added Users tab (admin CRUD), Formulas tab (formula CRUD + toggle), Telegram tab (access code display, authorized user count, registration form fix). auth.js client module for session management. Page count: 13 → 15 |
| **JS modules** | New: `chat.js` (147 lines), `auth.js` (129 lines); `control.js` expanded (534 → ~891 lines); 15 → 17 files total (~2,600 → ~3,241 lines) |
| **CSS** | +88 lines (~1,261 → ~1,349): chat session cards, platform badges, chat modal, login form, role badges, users table |
| **Scripts** | New: `seed-formulas.sh` (~55 lines, TOML→Postgres formula seeding), `create-admin.sh` (~80 lines, bcrypt user creation with pgcrypto fallback); 12 → 14 scripts (1,073 → 1,403 lines) |
| **Unit tests** | 204 → 249 (+45): beads-bridge +12 (chat/formula tools), status-api +45 (UserRole, require_role, AuthContext, bcrypt, request deserialization) |
| **Integration tests** | 3 new suites: `chat-integration.sh` (9 tests), `formula-registry.sh` (11 tests), `dashboard-auth.sh` (19 tests); 18 → 21 suites |
| **E2E tests** | +2 assertions (~95 → ~97): section 16 (chat/auth/formula/user endpoints), dashboard page count 13→15 |
| **Examples** | New: `examples/research-report/` (4 files, 736 lines) — two-agent demo using 10 bridge tools, Ollama LLM, inter-agent messaging, KG queries, persistent memory |
| **Security** | 8-pass security audit: RBAC `require_role()` on all 20+ POST endpoints (Admin/Operator), HSTS+CSP+X-Frame-Options+nosniff on 5 middleware instances, `validate_webhook_url()` SSRF blocking, `DefaultBodyLimit` 10 MiB, `clamp_limit()` on all queries, fail-closed guardrails and conditions, anchored regex, SQL parameterization in 4 shell scripts, 12-char password minimum, `POST /auth/logout-all`, SSE caps (100 streams, 1h TTL), brave_cache 200-entry hard cap, constant-time API key comparison, `read_only`/`no-new-privileges` container hardening, `cargo-deny` CI scanning |

## Changelog: v0.4.0 → v0.5.0

| Area | Change |
|------|--------|
| **Tools** | Added `graph_search`, `graph_traverse`, `graph_update_edge`, `graph_stats` (66 tools total, was 62) |
| **beads-bridge** | +767 lines (5,469 → 6,236): 4 KG tool handlers, entity/edge queries, recursive CTE traversal |
| **embedding-worker** | +711 lines (865 → 1,576): LLM entity extraction, entity resolution, edge resolution, Qdrant kg collection |
| **status-api** | +322 lines (1,675 → 1,997): 3 KG endpoints, memory stats improvements (content_length, agents_count, topics_count) |
| **heartbeat** | +83 lines (1,008 → 1,091): `sync_kg_unprocessed_memories` step |
| **broodlink-config** | +43 lines (717 → 760): 5 new `kg_*` fields on MemorySearchConfig |
| **config.toml** | +4 lines (122 → 126): KG config fields |
| **Migration 013** | New: `kg_entities`, `kg_edges`, `kg_entity_memories` tables (Postgres, 67 lines) |
| **Qdrant** | New collection: `broodlink_kg_entities` (768-dim cosine) |
| **Dashboard** | New Knowledge Graph page; fixed Pending Tasks metric (counts_by_status), task Agent column (assigned_agent), Memory chart (content_length with unit label), decisions/memory layout ID mismatches |
| **JS modules** | New: `knowledge-graph.js` (112 lines); 14 files total (was 13) |
| **Scripts** | New: `backfill-knowledge-graph.sh` (11 scripts total, was 10) |
| **Unit tests** | 148 → 196 (+18 beads-bridge KG, +24 embedding-worker KG, +6 status-api KG) |
| **Shell test suites** | 15 → 17 (+DOM cross-check, +KG integration); expanded JS regression (15→27), API smoke (20→29), frontend smoke (33→35) |
| **store_memory** | Outbox payload now includes `memory_id` and `tags` for KG entity-memory linking |
| **AppState** | New field: `kg_extraction_breaker: CircuitBreaker` (embedding-worker) |

## Changelog: v0.3.0 → v0.4.0

| Area | Change |
|------|--------|
| **Tools** | Added `hybrid_search` (62 tools total, was 61) |
| **beads-bridge** | +747 lines (4,722 → 5,469): hybrid search, BM25, temporal decay, reranking, search index writes |
| **heartbeat** | +41 lines (967 → 1,008): `sync_memory_search_index` step |
| **broodlink-config** | +70 lines (647 → 717): `MemorySearchConfig` struct |
| **mcp-server** | +10 lines (313 → 323): JWT path `.pem` → `.token`, non-fatal degradation |
| **config.toml** | +10 lines (112 → 122): `[memory_search]` section |
| **Migration 012** | New: `memory_search_index` table (Postgres, 26 lines) |
| **AppState** | New field: `reranker_breaker: CircuitBreaker` |
| **store_memory** | Now writes to Postgres `memory_search_index` (non-fatal) |
| **delete_memory** | Now deletes from Postgres `memory_search_index` (non-fatal) |
| **Dashboard** | Memory Entries metric uses `/api/v1/memory/stats` total_count |
| **start-services.sh** | Auto-generates JWTs for mcp-server and a2a-gateway |
| **Scripts** | New: `backfill-search-index.sh` (10 scripts total, was 7) |
| **Unit tests** | 129 → 148 (+19 hybrid search tests in beads-bridge) |
| **E2E tests** | ~73 → ~95 (+22: sections 10–13 for hybrid search, services health, dashboard pages) |
| **.gitignore** | Added `.claude/`, `.cursor/` |

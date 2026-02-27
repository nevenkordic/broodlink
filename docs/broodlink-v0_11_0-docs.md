# Broodlink v0.11.0 — Intelligence & Scale Upgrade

> Multi-agent AI orchestration system
> Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later
> Generated: 2026-02-27

This document covers changes in v0.11.0. For previous versions, see
[broodlink-v0_8_0-docs.md](broodlink-v0_8_0-docs.md).

---

## Table of Contents

1. [Overview](#1-overview)
2. [Query Expansion](#2-query-expansion)
3. [Smart Chunk Boundaries](#3-smart-chunk-boundaries)
4. [Streaming Responses](#4-streaming-responses)
5. [Verification Timeout Caveat](#5-verification-timeout-caveat)
6. [Verification Analytics Dashboard](#6-verification-analytics-dashboard)
7. [Agent Negotiation Protocol](#7-agent-negotiation-protocol)
8. [Multi-Ollama Load Balancing](#8-multi-ollama-load-balancing)
9. [Agent Onboarding Web UI](#9-agent-onboarding-web-ui)
10. [Migration 029](#10-migration-029)
11. [Config Reference](#11-config-reference)
12. [Test Coverage](#12-test-coverage)

---

## 1. Overview

v0.11.0 upgrades Broodlink across memory quality, UX, reliability, agent
protocol, infrastructure, and dashboard. 1 new Postgres migration,
2 new dashboard pages, 2 new beads-bridge tools.

**Key metrics:**

| Metric | v0.8.0 | v0.11.0 |
|--------|--------|---------|
| beads-bridge tools | 90 | 96 |
| Unit tests | 271 | 346 |
| Dashboard pages | 14 | 16 |
| SQL migrations | 28 | 29 |
| Postgres tables | 35 | 37 |

---

## 2. Query Expansion

**Problem:** Memory search takes the verbatim query. Searching "database setup"
misses results titled "PostgreSQL configuration" or "DB connection pooling".

### How It Works

Before embedding a search query, `expand_query()` calls Ollama to generate 2–3
alternative phrasings:

```
User query: "database setup"
  → Expanded: ["database setup", "PostgreSQL configuration", "DB connection pooling"]
```

Each variant runs through semantic search (or hybrid search) in parallel via
`futures::future::join_all`. Results are merged by `(agent_id, topic)` key,
taking the maximum score per candidate.

### Implementation

```rust
async fn expand_query(
    client: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    timeout: Duration,
    query: &str,
) -> Vec<String>
```

- Calls Ollama `/api/generate` with a focused prompt
- Uses `think: false`, `num_predict: 100`, `temperature: 0.7`
- Parses response lines, deduplicates, prepends original query
- Returns `vec![original]` on any error (graceful fallback)
- Skips expansion for queries < 3 words (too short to benefit)

### Integration Points

| Tool | File | Behavior |
|------|------|----------|
| `semantic_search` | `rust/beads-bridge/src/main.rs` | Expand → embed each variant → Qdrant search in parallel → merge |
| `hybrid_search` | `rust/beads-bridge/src/main.rs` | Expand → BM25 + semantic per variant → merge candidates |
| `fetch_memory_context` | `rust/a2a-gateway/src/main.rs` | Benefits automatically via bridge_call to semantic_search |

### Config

```toml
[memory_search]
query_expansion_enabled = true
query_expansion_model = "qwen3:1.7b"
query_expansion_timeout_seconds = 15
```

**File:** `rust/beads-bridge/src/main.rs` — `expand_query()`, `tool_semantic_search()`, `tool_hybrid_search()`

---

## 3. Smart Chunk Boundaries

**Problem:** `chunk_content()` splits at word counts. Code blocks, tables, and
headings get split mid-structure, degrading embedding quality.

### Algorithm

`chunk_content_smart()` uses a priority-based boundary detection system:

```
SplitPriority (highest → lowest):
  1. Heading     — lines starting with # or ##
  2. CodeFence   — ``` boundary lines (never splits inside a code block)
  3. TableSep    — |---| separator lines
  4. Paragraph   — blank lines
```

Steps:
1. Pre-compute cumulative word counts per line
2. Scan lines to identify split points with priorities
3. Track code fence state (open/close) — never split inside a code block
4. Greedy loop: accumulate lines up to `max_tokens`, split at highest-priority
   boundary within range
5. If no natural boundary found, fall back to nearest line at `max_tokens`
6. Overlap: rewind next chunk start by `overlap` words (capped at 3 lines)

### Integration

In `process_outbox_row()`:

```rust
let chunks = if config.smart_chunking {
    chunk_content_smart(content, max_tokens, overlap)
} else {
    chunk_content(content, max_tokens, overlap)
};
```

### Config

```toml
[memory_search]
smart_chunking = true
```

**File:** `rust/embedding-worker/src/main.rs` — `chunk_content_smart()`, `SplitPriority`

---

## 4. Streaming Responses

**Problem:** Telegram/Slack waits for the full LLM response. At ~95 tok/s, 5+
seconds of silence before any text appears.

### Architecture

```
User message
    │
    ▼
Send "Thinking..." placeholder ──→ Telegram (message_id)
    │
    ▼
Stream from Ollama (NDJSON)
    │
    ├──→ Every 800ms + 30 tokens: editMessageText
    ├──→ On tool call: pause, show "Using tool: {name}...", execute, resume
    └──→ Final: editMessageText with complete response
```

### Key Components

| Component | Purpose |
|-----------|---------|
| `stream_ollama_request()` | Reads NDJSON byte stream from Ollama, emits `StreamChunk` |
| `filter_think_streaming()` | Stateful filter for partial `<think>`/`</think>` tags across token boundaries |
| `send_telegram_placeholder()` | Sends "Thinking..." message, returns `message_id` |
| `edit_telegram_message()` | Calls Telegram `editMessageText`, handles "not modified" gracefully |
| `call_ollama_chat_streaming()` | Full streaming variant with periodic progress callback |
| `prepare_chat_context()` | Shared setup (system prompt, model, memory, tools) — eliminates ~400 lines of duplication |

### Rate Limiting

- Edit every **800ms** AND **30 tokens** minimum delta
- Stays within Telegram's ~30 edits/minute rate limit
- Configurable via `streaming_edit_interval_ms` and `streaming_min_tokens`

### Tool Calls During Streaming

When a tool call arrives mid-stream:
1. Pause the stream
2. Edit message to show "Using tool: {name}..."
3. Execute the tool
4. Start a new streaming round with tool result in history

### Config

```toml
[chat]
streaming_enabled = true
streaming_edit_interval_ms = 800
streaming_min_tokens = 30
```

**File:** `rust/a2a-gateway/src/main.rs` — `stream_ollama_request()`, `call_ollama_chat_streaming()`, `prepare_chat_context()`

---

## 5. Verification Timeout Caveat

**Problem:** `verify_response()` returned `Option<String>` — both "verified"
(no correction needed) and "timed out" mapped to `None`. Unverified responses
passed through silently.

### New Return Type

```rust
enum VerifyResult {
    Verified,
    Corrected(String),
    Timeout,
    Error(String),
}
```

### Behavior Changes

| Result | v0.8.0 | v0.11.0 |
|--------|--------|---------|
| Verified | Return clean text | Return clean text |
| Corrected | Return corrected text | Return corrected text |
| Timeout | Return clean text (silent) | Append `"\n\n_[Unverified: verification timed out]_"` |
| Error | Return clean text (silent) | Append caveat, log error |

### Structured Logging

Every verification call logs to `service_events` with:

```json
{
  "confidence": 2,
  "duration_ms": 1523,
  "outcome": "corrected",
  "original_len": 456
}
```

This feeds the Verification Analytics Dashboard (Section 6).

**File:** `rust/a2a-gateway/src/main.rs` — `verify_response()`, `VerifyResult`

---

## 6. Verification Analytics Dashboard

**Prerequisite:** Section 5 (structured `verification_*` events in `service_events`).

### status-api Endpoints

**`GET /api/v1/verification/analytics`**

Returns daily verification counts and average duration:

```json
{
  "data": {
    "daily": [
      {
        "date": "2026-02-27",
        "triggered": 42,
        "verified": 30,
        "corrected": 8,
        "timeout": 4,
        "avg_duration_ms": 1200
      }
    ]
  }
}
```

**`GET /api/v1/verification/confidence`**

Returns histogram of confidence scores (1–5):

```json
{
  "data": {
    "histogram": [
      {"score": 1, "count": 5},
      {"score": 2, "count": 12},
      {"score": 3, "count": 45},
      {"score": 4, "count": 80},
      {"score": 5, "count": 30}
    ]
  }
}
```

### Dashboard Page

Located at `/verification/`. Uses Chart.js for:
- Bar chart: daily verification counts by outcome
- Pie chart: outcome distribution
- Line chart: average duration trend
- Histogram: confidence score distribution

**Files:**
- `rust/status-api/src/main.rs` — `handler_verification_analytics()`, `handler_verification_confidence()`
- `status-site/content/verification/_index.md`
- `status-site/themes/broodlink-status/layouts/verification/list.html`
- `status-site/themes/broodlink-status/static/js/verification.js`

---

## 7. Agent Negotiation Protocol

**Problem:** Tasks are assigned to agents without any ability to decline,
redirect, or request clarification. An agent assigned a task outside its
expertise has no recourse.

### Database Schema (Migration 029)

```sql
-- New columns on task_queue
ALTER TABLE task_queue ADD COLUMN decline_count INT DEFAULT 0;
ALTER TABLE task_queue ADD COLUMN declined_agents JSONB DEFAULT '[]';
ALTER TABLE task_queue ADD COLUMN context_questions JSONB;
ALTER TABLE task_queue ADD COLUMN context_requested_by VARCHAR(100);
ALTER TABLE task_queue ADD COLUMN context_requested_at TIMESTAMPTZ;

-- New table
CREATE TABLE task_negotiations (
    id BIGSERIAL PRIMARY KEY,
    task_id VARCHAR(36) NOT NULL,
    agent_id VARCHAR(100) NOT NULL,
    action VARCHAR(30) NOT NULL,  -- declined, context_requested, context_provided, redirected
    reason TEXT,
    suggested_agent VARCHAR(100),
    questions JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW()
);
```

### Task Decline Flow

```
Agent receives task → decline_task(task_id, reason, suggested_agent?)
    │
    ▼
Coordinator: handle_task_declined()
    ├── Log to task_negotiations
    ├── Increment decline_count
    ├── Append agent to declined_agents JSONB
    │
    ├── decline_count >= max_declines_per_task (3)?
    │       → Dead-letter the task
    │
    └── Else: Reset to pending, re-publish task_available
             (routing excludes declined_agents)
```

### Context Request Flow

```
Agent receives task → request_task_context(task_id, questions)
    │
    ▼
Coordinator: handle_task_context_request()
    ├── Log to task_negotiations
    ├── Set task status to 'context_requested'
    ├── Store questions in context_questions
    ├── Publish questions to NATS (parent agent)
    │
    └── Timeout (context_request_timeout_minutes, default 30)?
            → Reset to pending, re-publish task_available
```

### Declined Agent Filtering

In `handle_task_available()`, after `find_eligible_agents()` returns candidates:

```rust
agents.retain(|a| !task.declined_agents.contains(&a.agent_id));
```

This prevents re-assigning a task to an agent that already declined it.

### NATS Subjects

| Subject | Publisher | Subscriber |
|---------|-----------|------------|
| `broodlink.{env}.coordinator.task_declined` | beads-bridge | coordinator |
| `broodlink.{env}.coordinator.task_context_request` | beads-bridge | coordinator |
| `broodlink.{env}.agent.{id}.task_context` | coordinator | target agent |

### beads-bridge Tools

**`decline_task`** — `{task_id, reason, suggested_agent?}`

Verifies task is assigned to the calling agent before publishing decline.

**`request_task_context`** — `{task_id, questions: [String]}`

Validates questions array is non-empty, publishes context request.

### Config

```toml
[collaboration]
max_declines_per_task = 3
context_request_timeout_minutes = 30
```

**Files:**
- `rust/coordinator/src/main.rs` — `handle_task_declined()`, `handle_task_context_request()`, `detect_timed_out_context_requests()`
- `rust/beads-bridge/src/main.rs` — `tool_decline_task()`, `tool_request_task_context()`

---

## 8. Multi-Ollama Load Balancing

**Problem:** Single Ollama instance. Long inference blocks all other requests
behind the concurrency semaphore.

### Architecture

```rust
struct OllamaPool {
    instances: Vec<OllamaInstance>,
    semaphore: Semaphore,  // global concurrency limit
}

struct OllamaInstance {
    url: String,
    semaphore: Semaphore,       // per-instance limit
    active_requests: AtomicU32, // current load
    healthy: AtomicBool,        // health status
}

struct OllamaPermit<'a> {
    url: String,
    instance_idx: usize,
    pool: &'a OllamaPool,
    _permit: SemaphorePermit<'a>,
}
```

### Instance Selection

`pick_instance()` selects the **least-loaded healthy** instance:

1. Filter to healthy instances only
2. Sort by `active_requests` (ascending)
3. Return first (lowest load)
4. If all unhealthy, fall back to first instance (best-effort)

### RAII Guard

`OllamaPermit` implements `Drop` to automatically decrement
`active_requests` when the permit goes out of scope, preventing counter drift.

### Health Checking

Background task `ollama_health_check_loop()` runs every 30 seconds:

1. `GET /api/tags` to each instance
2. On success: mark healthy, reset failure counter
3. On failure: increment failure counter
4. After 3 consecutive failures: mark unhealthy
5. Single success after unhealthy: recover immediately

### Integration

All callers use `pool.acquire().await` or `pool.try_acquire()` instead of
the previous `state.ollama_semaphore`. The permit carries the selected URL:

```rust
let permit = state.ollama_pool.acquire().await?;
let url = &permit.url;
// ... use url for Ollama API calls ...
// permit dropped automatically → active_requests decremented
```

### Backward Compatibility

Single `url` field still works. `OllamaConfig.effective_urls()` returns
`vec![url]` when `urls` is empty:

```rust
impl OllamaConfig {
    pub fn effective_urls(&self) -> Vec<String> {
        if self.urls.is_empty() {
            vec![self.url.clone()]
        } else {
            self.urls.clone()
        }
    }
}
```

### Config

```toml
[ollama]
url = "http://localhost:11434"   # single instance (backward compat)
urls = []                         # multi-instance: ["http://host1:11434", "http://host2:11434"]
```

**File:** `rust/a2a-gateway/src/main.rs` — `OllamaPool`, `OllamaPermit`, `ollama_health_check_loop()`

---

## 9. Agent Onboarding Web UI

**Problem:** Onboarding agents requires running shell scripts. No dashboard
visibility.

### status-api Endpoints

**`POST /api/v1/agents/onboard`** (Admin role required)

Request:
```json
{
  "agent_id": "my-agent",
  "display_name": "My Agent",
  "role": "worker",
  "cost_tier": "medium"
}
```

Response:
```json
{
  "agent_id": "my-agent",
  "jwt_token": "eyJ...",
  "expires_at": "2027-02-27T06:06:49+00:00",
  "token_path": "/Users/neven/.broodlink/jwt-my-agent.token"
}
```

Steps:
1. Validate `agent_id` — alphanumeric, hyphens, underscores only
2. Load RSA private key from `{jwt.keys_dir}/jwt-private.pem` (tilde expanded)
3. Generate RS256 JWT with 1-year expiry (`sub`, `agent_id`, `iat`, `exp`)
4. Register agent in Dolt `agent_profiles` (upsert)
5. Dolt commit
6. Save token to `{keys_dir}/jwt-{agent_id}.token`

**`GET /api/v1/agents/tokens`** (Operator role required)

Returns all registered agents with JWT token presence metadata (`has_jwt: true/false`
based on file existence check).

### Dashboard Page

Located at `/onboarding/`. Features:

- **Onboard form**: Agent ID (validated), Display Name, Role (select), Cost Tier (select)
- **JWT display**: Textarea with Copy button (clipboard API with fallback)
- **Status badges**: Active/JWT columns use `ctrl-badge-ok`/`ctrl-badge-failed`
- **Relative timestamps**: Created column uses `BL.formatRelativeTime()`
- **XSS protection**: All rendered data passes through `BL.escapeHtml()`
- **Proper POST**: Uses `postApi()` helper with `Content-Type: application/json` and auth headers (not `fetchApi` which is GET-only)

### CSS Components Added

| Class | Purpose |
|-------|---------|
| `.alert` / `.alert-success` / `.alert-error` | Themed alert boxes |
| `.form-group select` | Styled select dropdowns matching input fields |
| `.jwt-wrap` / `.jwt-output` | JWT textarea + copy button layout |
| `.btn-copy` | Copy button with hover state |
| `.onboard-meta` | CSS grid for metadata display (dt/dd) |
| `.loading-cell` | Centered loading/empty state in tables |

**Files:**
- `rust/status-api/src/main.rs` — `handler_agent_onboard()`, `handler_agent_tokens()`
- `status-site/content/onboarding/_index.md`
- `status-site/themes/broodlink-status/layouts/onboarding/list.html`
- `status-site/themes/broodlink-status/static/js/onboarding.js`
- `status-site/themes/broodlink-status/static/css/style.css`

---

## 10. Migration 029

**File:** `migrations/029_negotiation_protocol.sql`

**Database:** PostgreSQL

### Schema Changes

```sql
-- task_queue additions
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS decline_count INT DEFAULT 0;
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS declined_agents JSONB DEFAULT '[]';
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS context_questions JSONB;
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS context_requested_by VARCHAR(100);
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS context_requested_at TIMESTAMPTZ;

-- New table
CREATE TABLE IF NOT EXISTS task_negotiations (
    id BIGSERIAL PRIMARY KEY,
    task_id VARCHAR(36) NOT NULL,
    agent_id VARCHAR(100) NOT NULL,
    action VARCHAR(30) NOT NULL
        CHECK (action IN ('declined','context_requested','context_provided','redirected')),
    reason TEXT,
    suggested_agent VARCHAR(100),
    questions JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX idx_task_negotiations_task ON task_negotiations(task_id);
```

### Apply

```bash
psql -h 127.0.0.1 -p 5432 -U broodlink -d broodlink \
  -f migrations/029_negotiation_protocol.sql
```

---

## 11. Config Reference

New fields added in v0.11.0:

```toml
[memory_search]
query_expansion_enabled = true          # Enable query expansion for search
query_expansion_model = "qwen3:1.7b"    # Model for generating alternative phrasings
query_expansion_timeout_seconds = 15    # Timeout for expansion call
smart_chunking = true                   # Markdown-aware chunk splitting

[chat]
streaming_enabled = true                # Stream responses via Telegram edits
streaming_edit_interval_ms = 800        # Minimum ms between edits
streaming_min_tokens = 30               # Minimum tokens before first edit

[collaboration]
max_declines_per_task = 3               # Max agent declines before dead-letter
context_request_timeout_minutes = 30    # Timeout for context requests

[ollama]
urls = []                               # Multi-instance URLs (empty = use single url)
```

### Config Structs

| Struct | Field | Type | Default |
|--------|-------|------|---------|
| `MemorySearchConfig` | `query_expansion_enabled` | `bool` | `true` |
| `MemorySearchConfig` | `query_expansion_model` | `String` | `"qwen3:1.7b"` |
| `MemorySearchConfig` | `query_expansion_timeout_seconds` | `u64` | `15` |
| `MemorySearchConfig` | `smart_chunking` | `bool` | `true` |
| `ChatConfig` | `streaming_enabled` | `bool` | `true` |
| `ChatConfig` | `streaming_edit_interval_ms` | `u64` | `800` |
| `ChatConfig` | `streaming_min_tokens` | `usize` | `30` |
| `CollaborationConfig` | `max_declines_per_task` | `u32` | `3` |
| `CollaborationConfig` | `context_request_timeout_minutes` | `u32` | `30` |
| `OllamaConfig` | `urls` | `Vec<String>` | `[]` |

---

## 12. Test Coverage

346 total workspace tests:

| Crate | Tests | Delta |
|-------|-------|-------|
| a2a-gateway | 60 | +22 |
| beads-bridge | 91 | +8 |
| broodlink-config | 4 | — |
| broodlink-formulas | 12 | — |
| broodlink-fs | 19 | — |
| broodlink-runtime | 5 | — |
| broodlink-secrets | 5 | — |
| broodlink-telemetry | 7 | — |
| coordinator | 45 | +10 |
| embedding-worker | 40 | +9 |
| mcp-server | 23 | — |
| status-api | 35 | +7 |

### New Tests by Feature

**Query Expansion (beads-bridge):**
- `test_strip_think_tags_*` (5 tests for think-tag helper used during expansion parsing)

**Smart Chunking (embedding-worker):**
- `test_smart_chunk_heading_boundaries`
- `test_smart_chunk_preserves_code_fences`
- `test_smart_chunk_paragraph_boundaries`
- `test_smart_chunk_table_not_split`
- `test_smart_chunk_fallback_no_boundaries`
- `test_smart_chunk_overlap_present`
- `test_smart_chunk_short_text`
- `test_smart_chunk_empty_text`

**Streaming (a2a-gateway):**
- `test_filter_think_streaming_*` (4 tests: basic, partial tag, no tags, edge cases)

**Verification Caveat (a2a-gateway):**
- `test_verify_result_timeout_caveat`
- `test_verify_result_verified_no_caveat`

**Negotiation Protocol (coordinator):**
- `test_task_declined_payload_deserialization`
- `test_task_declined_payload_minimal`
- `test_task_context_request_payload_deserialization`
- `test_declined_agents_filter_excludes_declined`
- `test_declined_agents_empty_filter_keeps_all`

**Multi-Ollama (a2a-gateway):**
- `test_ollama_pool_single_instance`
- `test_ollama_pool_pick_least_loaded`
- `test_ollama_pool_unhealthy_skipped`
- `test_ollama_pool_all_unhealthy_fallback`
- `test_ollama_pool_health_reporting`

**Onboarding (status-api):**
- `test_onboard_payload_valid_agent_id`
- `test_onboard_payload_missing_agent_id`
- `test_onboard_payload_defaults`
- `test_onboard_jwt_claims_structure`
- `test_onboard_tilde_expansion`
- `test_onboard_token_path_format`
- `test_onboard_agent_id_validation_pattern`

---

## Files Changed

| File | Changes |
|------|---------|
| `Cargo.toml` | Version 0.7.0 → 0.11.0 |
| `config.toml` | Query expansion, streaming, negotiation, multi-Ollama config |
| `crates/broodlink-config/src/lib.rs` | New fields on `MemorySearchConfig`, `ChatConfig`, `CollaborationConfig`, `OllamaConfig` |
| `rust/a2a-gateway/src/main.rs` | Streaming, `OllamaPool`, `VerifyResult`, `expand_query()` integration, health check loop |
| `rust/beads-bridge/src/main.rs` | Query expansion in search tools, `decline_task`/`request_task_context` tools (94 → 96) |
| `rust/coordinator/src/main.rs` | Negotiation handlers, declined agent filtering, context timeout detection, NATS subscriptions |
| `rust/embedding-worker/src/main.rs` | `chunk_content_smart()`, `SplitPriority` enum |
| `rust/status-api/src/main.rs` | Onboarding endpoints, verification analytics endpoints, `jsonwebtoken` dependency |
| `rust/status-api/Cargo.toml` | Added `jsonwebtoken` |
| `migrations/029_negotiation_protocol.sql` | `task_negotiations` table, negotiation columns on `task_queue` |
| `status-site/hugo.toml` | Version 0.7.0 → 0.11.0 |
| `status-site/content/onboarding/_index.md` | New page |
| `status-site/content/verification/_index.md` | New page |
| `status-site/themes/.../layouts/onboarding/list.html` | Onboarding form + agent table |
| `status-site/themes/.../layouts/verification/list.html` | Chart.js analytics page |
| `status-site/themes/.../layouts/partials/sidebar.html` | Added Verification + Onboarding nav links |
| `status-site/themes/.../static/js/onboarding.js` | `postApi()`, `escapeHtml()`, copy-to-clipboard, badges |
| `status-site/themes/.../static/js/verification.js` | Chart.js integration, data fetching |
| `status-site/themes/.../static/css/style.css` | Alerts, form selects, JWT output, copy button, loading cell |

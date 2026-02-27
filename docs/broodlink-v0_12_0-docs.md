# Broodlink v0.12.0 — Multi-Modal, SDK & Dashboard Redesign

> Multi-agent AI orchestration system
> Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later
> Generated: 2026-02-27

This document covers changes in v0.12.0. For previous versions, see the
[CHANGELOG](../CHANGELOG.md).

---

## Table of Contents

1. [Overview](#1-overview)
2. [Multi-Modal Chat Attachments](#2-multi-modal-chat-attachments)
3. [Python SDK Overhaul](#3-python-sdk-overhaul)
4. [Dashboard Redesign](#4-dashboard-redesign)
5. [Verification Page Enhancements](#5-verification-page-enhancements)
6. [Attachment Storage Module](#6-attachment-storage-module)
7. [Migration 030](#7-migration-030)
8. [Config Reference](#8-config-reference)
9. [Bug Fixes](#9-bug-fixes)
10. [Test Coverage](#10-test-coverage)

---

## 1. Overview

v0.12.0 adds multi-modal attachment handling across all chat platforms,
rewrites the Python SDK with typed clients and a Click CLI, and redesigns
the entire dashboard for structural consistency.

**Key metrics:**

| Metric | v0.11.0 | v0.12.0 |
|--------|---------|---------|
| beads-bridge tools | 96 | 96 |
| Unit tests | 346 | 346+ |
| Dashboard pages | 16 | 16 |
| SQL migrations | 29 | 30 |
| Postgres tables | 37 | 38 |
| Python SDK version | 0.2.0 | 0.12.0 |

---

## 2. Multi-Modal Chat Attachments

**Problem:** Chat messages are text-only. Users sending images, documents,
or voice notes via Slack/Teams/Telegram have their files ignored.

### Architecture

```
Platform message (with file)
    |
    v
Platform handler (Slack/Teams/Telegram)
    |
    +-- Download file (platform-specific auth)
    +-- Build PendingAttachment { type, mime, name, bytes, platform_file_id }
    |
    v
handle_chat_message(text, attachments)
    |
    +-- store_attachment() -> (relative_path, sha256)
    +-- INSERT into chat_attachments
    +-- generate_thumbnail() for images
    |
    v
status-api serves via 3 new endpoints
```

### PendingAttachment

Carries attachment metadata through the message pipeline before storage:

```rust
struct PendingAttachment {
    attachment_type: String,        // image, audio, video, document
    mime_type: Option<String>,      // e.g. "image/jpeg"
    file_name: Option<String>,      // original filename
    bytes: Vec<u8>,                 // raw file content
    platform_file_id: Option<String>, // Slack file ID, Telegram file_id
}
```

### Platform-Specific Download

| Platform | Method | Auth |
|----------|--------|------|
| **Slack** | `GET url_private` | `Authorization: Bearer {bot_token}` |
| **Telegram** | `getFile` API -> download `file_path` | Bot token in URL |
| **Teams** | Attachment URL from activity | Bot Framework token |

**Slack:** `file_share` subtype events are now accepted (previously filtered
out by the `subtype.is_none()` check). Files array is extracted from the
event, each file's `url_private` is downloaded with the Slack bot token.

**Telegram:** Photo messages pick the largest resolution from the `photo`
array (last element). Document messages use the `document` object. Both
call `download_telegram_file()` which does `getFile` -> download.

### Attachment Classification

```rust
fn classify_attachment_type(mime: &str, file_name: &str) -> &str
```

| MIME prefix | Result |
|-------------|--------|
| `image/*` | `"image"` |
| `audio/*` | `"audio"` |
| `video/*` | `"video"` |
| Everything else | `"document"` |

Falls back to file extension check (`.jpg`/`.png` -> image, `.mp3`/`.ogg`
-> audio, `.mp4`/`.mov` -> video).

### status-api Endpoints

**`GET /api/v1/chat/sessions/:session_id/attachments`**

Returns all attachments for a chat session, ordered by `created_at DESC`.

**`GET /api/v1/chat/attachments/:attachment_id/download`**

Serves the raw file bytes with correct `Content-Type` and
`Content-Disposition: attachment` headers. Path traversal blocked.

**`GET /api/v1/chat/attachments/:attachment_id/thumbnail`**

Serves the JPEG thumbnail (200px max width) if one exists, otherwise
returns 404.

### Files

| File | Changes |
|------|---------|
| `rust/a2a-gateway/src/main.rs` | `PendingAttachment`, `classify_attachment_type()`, `download_slack_file()`, Slack/Telegram/Teams file handling, attachment storage in `handle_chat_message()` |
| `rust/status-api/src/main.rs` | `handler_session_attachments()`, `handler_download_attachment()`, `handler_attachment_thumbnail()` |
| `crates/broodlink-fs/src/attachments.rs` | `store_attachment()`, `read_attachment()`, `generate_thumbnail()`, `sha256_hex()` |
| `Cargo.toml` | `reqwest` `multipart` feature enabled |
| `rust/a2a-gateway/Cargo.toml` | Version bump |
| `rust/status-api/Cargo.toml` | Added `broodlink-fs` dependency |

---

## 3. Python SDK Overhaul

**Problem:** The Python package was a thin argparse wrapper with no typed
client library. Users had to make raw HTTP calls to beads-bridge.

### New Architecture

```
broodlink_agent/
    __init__.py          # v0.12.0, typed exports, __all__
    cli.py               # Click CLI with subcommands
    config.py            # AgentConfig (env-based)
    client.py            # BroodlinkClient + AsyncBroodlinkClient
    agent.py             # BaseAgent framework
    _transport.py        # HTTP transport layer
    nats_helper.py       # NATS connection helper
    models/
        __init__.py
        core.py          # Pydantic models (Memory, Task, Agent, Issue, ...)
    namespaces/
        __init__.py
        base.py          # BaseNamespace
        memory.py        # memory.search(), memory.store(), memory.recall()
        agents.py        # agents.list()
        tasks.py         # tasks.list(), tasks.create()
        projects.py      # projects.list()
        decisions.py     # decisions.list()
        beads.py         # beads.list_issues()
        knowledge_graph.py  # kg.entities(), kg.edges()
        messaging.py     # messaging.send(), messaging.read()
        notifications.py # notifications.send()
        scheduling.py    # scheduling.list(), scheduling.create()
        work.py          # work.log()
    ml/
        __init__.py
        batch.py         # Batch processing utilities
        dataframes.py    # DataFrame helpers
        graph.py         # Graph analysis utilities
```

### Click CLI

Replaces argparse with Click subcommand groups. Backward compatible:
`broodlink --listen` still works.

```bash
# Agent management
broodlink agent list

# Memory operations
broodlink memory search "database setup"
broodlink memory store "my-topic" "content here" --tags "tag1,tag2"
broodlink memory recall --topic "database"
broodlink memory stats

# Direct tool calls
broodlink tool call store_memory '{"topic":"test","content":"hello"}'
broodlink tool list

# Legacy mode (backward compat)
broodlink --listen
broodlink --no-think
```

### Typed Client

```python
from broodlink_agent import BroodlinkClient, AsyncBroodlinkClient

# Synchronous
with BroodlinkClient(config) as client:
    results = client.memory.search("query", limit=5)
    client.memory.store("topic", "content", tags="tag1,tag2")
    agents = client.agents.list()
    tools = client.fetch_tools()
    result = client.call("tool_name", {"param": "value"})

# Asynchronous
async with AsyncBroodlinkClient(config) as client:
    results = await client.memory.search("query")
```

### Typed Models (Pydantic)

All API responses are deserialized into typed models:

| Model | Fields |
|-------|--------|
| `Memory` | topic, content, agent_id, tags, created_at |
| `SemanticResult` | topic, content, score, agent_id |
| `MemoryStats` | total_count, recent_update, top_topics |
| `Task` | id, title, description, status, priority |
| `Agent` | agent_id, display_name, role, active, cost_tier |
| `Decision` | id, decision, reasoning, alternatives, outcome |
| `WorkLog` | id, agent_name, action, details, files_changed |
| `Message` | id, to, content, subject, read |
| `KGEntity` | id, name, entity_type, description, weight |
| `KGEdge` | id, source_id, target_id, relation, weight |
| `Issue` | id, title, description, status, assignee |
| `Formula` | id, name, description, version, is_system |
| `Notification` | id, channel, content, status |
| `ScheduledTask` | id, title, next_run_at, recurrence_secs |

### Files

| File | Changes |
|------|---------|
| `agents/broodlink_agent/__init__.py` | Version 0.12.0, typed exports |
| `agents/broodlink_agent/cli.py` | Click CLI with agent/memory/tool subcommands |
| `agents/broodlink_agent/client.py` | New: sync + async clients |
| `agents/broodlink_agent/agent.py` | New: BaseAgent framework |
| `agents/broodlink_agent/_transport.py` | New: HTTP transport |
| `agents/broodlink_agent/nats_helper.py` | New: NATS connection helper |
| `agents/broodlink_agent/models/core.py` | New: Pydantic model definitions |
| `agents/broodlink_agent/namespaces/*.py` | New: 12 namespace modules |
| `agents/pyproject.toml` | New: project metadata + dependencies |
| `agents/tests/` | New: 8 test modules |

---

## 4. Dashboard Redesign

**Problem:** Dashboard pages used inconsistent HTML structure. Many tables
lacked `.table-container` wrappers, metric cards used non-existent CSS
classes (`.card-value`/`.card-label`, `.metric-bar`/`.metric-value`), and
several pages had raw JSON dumps or inline styles.

### Design Principles

The Verification page (`/verification/`) was the gold standard. All other
15 pages were upgraded to match:

1. **Metric cards**: `.card-grid > .card.metric-card > .value + .label`
2. **Table wrappers**: `.table-container` for consistent borders/radius
3. **Empty states**: `<div class="empty-state">` pattern
4. **No inline styles**: CSS classes for layout
5. **Chart.js** for charts where applicable

### Sidebar Navigation

Reorganised from a flat list to three semantic groups:

```
INTELLIGENCE          OPERATIONS          SYSTEM
  Overview              Tasks & Issues      Safety Rules
  Conversations         Handoffs            Activity Log
  Agents                Approvals           External Agents
  Memory                                    Add Agent
  Connections                               Version History
  Decisions                                 Settings
  Quality
```

Labels changed to plain English (e.g., "Beads" -> "Tasks & Issues",
"Delegations" -> "Handoffs", "Knowledge Graph" -> "Connections",
"Verification" -> "Quality", "Guardrails" -> "Safety Rules",
"Audit" -> "Activity Log", "A2A" -> "External Agents",
"Onboarding" -> "Add Agent", "Commits" -> "Version History",
"Control" -> "Settings").

### Pages Updated

| Page | Changes |
|------|---------|
| Overview (`/`) | Wrapped task table in `.table-container` |
| Conversations (`/chat/`) | Replaced unstyled `.metric-bar` with `.card-grid > .card.metric-card`; added modal CSS |
| Agents (`/agents/`) | Added CSS for `.agent-header`, `.agent-meta`, `.agent-latest` |
| Tasks & Issues (`/beads/`) | Added 4 metric cards (Total/Open/In Progress/Closed), `.table-container`, renamed "Bead ID" -> "ID" |
| Handoffs (`/delegations/`) | Added 4 metric cards (Total/Pending/Completed/Failed), pre-defined `<table>` with `<thead>` visible on load |
| Approvals (`/approvals/`) | Added 4 metric cards, `.table-wrap` -> `.table-container`, styled policy editor with `.policy-form-grid` |
| Memory (`/memory/`) | Fixed `.card-value` -> `.value`, `.card-label` -> `.label`, added `.metric-card`; replaced inline `<dl>` with metric card pattern |
| Connections (`/knowledge-graph/`) | Fixed card classes, wrapped both tables in `.table-container` |
| Decisions (`/decisions/`) | No changes needed (already well-structured) |
| Safety Rules (`/guardrails/`) | Added 3 metric cards, `.table-wrap` -> `.table-container` |
| Activity Log (`/audit/`) | Added 3 metric cards (Total/Success Rate/Unique Agents), `.table-container` |
| External Agents (`/a2a/`) | Replaced raw `<pre>` JSON dump with structured card; added 2 metric cards, pre-defined task table |
| Add Agent (`/onboarding/`) | Removed `.data-table`, wrapped table in `.table-container` |
| Version History (`/commits/`) | Added 2 metric cards (Total/Latest), `.table-container` |
| Settings (`/control/`) | Wrapped all 13 tables in `.table-container` |

### New CSS (~210 lines)

| Component | Classes |
|-----------|---------|
| Chat sessions | `.chat-session-card`, `.card-row`, `.card-actions` |
| Platform badges | `.platform-badge`, `.platform-badge.slack`, `.teams`, `.telegram` |
| Status badges | `.badge`, `.badge-ok`, `.badge-offline` |
| Modal overlay | `.modal`, `.modal-content`, `.modal-header`, `.btn-close` |
| Chat messages | `.chat-messages`, `.chat-msg`, `.chat-msg-inbound`, `.chat-msg-outbound`, `.chat-msg-header`, `.chat-msg-content` |
| Empty states | `.empty-state` |
| Agent cards | `.agent-header`, `.agent-meta`, `.agent-latest` |
| Generic card parts | `.card-header`, `.card-body`, `.card-footer` |
| A2A structured card | `.a2a-info-grid`, `.a2a-skills-list`, `.a2a-skill-tag` |
| Policy editor | `.policy-editor`, `.policy-form-grid`, `.btn-approve` |

### JavaScript Updates

| File | Changes |
|------|---------|
| `beads.js` | `updateMetrics()` counting issues by status |
| `delegations.js` | Rewritten: fills pre-defined `<tbody>`, `updateMetrics()` |
| `audit.js` | `updateMetrics()` with success rate and unique agent count |
| `commits.js` | `updateMetrics()` with total and latest commit time |
| `approvals.js` | `updateMetrics()`, `Promise.all` for parallel API calls |
| `guardrails.js` | `updateMetrics()`, `Promise.all` for parallel API calls |
| `a2a.js` | Rewritten: structured card with `<dl>` and skill tags, fills `<tbody>` |
| `memory.js` | Stats rendered as `.card.metric-card` instead of inline-styled `<dl>` |
| `utils.js` | `fetchApi()` now accepts optional `opts` for POST/PUT requests |

### Files

All layouts in `status-site/themes/broodlink-status/layouts/`, all JS in
`status-site/themes/broodlink-status/static/js/`, CSS in
`status-site/themes/broodlink-status/static/css/style.css`.

---

## 5. Verification Page Enhancements

The existing Verification page (`/verification/`) received functional
upgrades while keeping its status as the design reference:

- **Configurable date range**: Dropdown filter for 7/14/30/60/90 days,
  default 30. JS reloads data on change.
- **Error outcome tracking**: Added `error` to the metrics totals and
  chart datasets. Previously only tracked verified/corrected/timeout.
- **Chart.js dark-theme defaults**: Global `Chart.defaults.color`,
  `borderColor`, `font.family`, `font.size` set once on page load.
- **Styled error state**: Failed data load shows centered, styled message
  instead of plain text.

**File:** `status-site/themes/broodlink-status/static/js/verification.js`

---

## 6. Attachment Storage Module

New module in `crates/broodlink-fs/src/attachments.rs` for file persistence.

### Functions

**`store_attachment(attachments_dir, file_name, bytes, max_size)`**

Stores bytes to `{attachments_dir}/{YYYY-MM}/{uuid}.{ext}`. Returns
`(relative_path, sha256_hex)`. Atomic write via temp file + rename. Rejects
empty files and files exceeding `max_size`. Tilde expansion on
`attachments_dir`.

**`read_attachment(attachments_dir, relative_path)`**

Reads attachment bytes. Rejects path traversal (`..` in path).

**`generate_thumbnail(attachments_dir, source_relative, max_width)`**

Uses the `image` crate to create a JPEG thumbnail at `max_width` pixels.
Stored alongside the original with `_thumb.jpg` suffix.

**`sha256_hex(bytes)`**

Returns SHA-256 hex digest.

### Tests (10 tests)

- `test_store_attachment_path_format` — verifies YYYY-MM/uuid.ext layout
- `test_store_attachment_hash_dedup` — identical content = identical hash
- `test_store_attachment_rejects_too_large` — max_size enforcement
- `test_store_attachment_rejects_empty` — empty file rejected
- `test_read_attachment_roundtrip` — store then read
- `test_read_attachment_rejects_traversal` — `../` blocked
- `test_sha256_hex` — known hash vector
- `test_generate_thumbnail_invalid_image` — non-image data errors
- `test_store_attachment_no_extension` — defaults to `.bin`

---

## 7. Migration 030

**File:** `migrations/030_chat_attachments.sql`

**Database:** PostgreSQL

### Schema

```sql
CREATE TABLE IF NOT EXISTS chat_attachments (
    id              VARCHAR(36)   PRIMARY KEY,
    message_id      BIGINT        NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    session_id      VARCHAR(36)   NOT NULL REFERENCES chat_sessions(id),
    attachment_type VARCHAR(20)   NOT NULL,   -- image, audio, video, document
    mime_type       VARCHAR(100),
    file_name       VARCHAR(500),
    file_size_bytes BIGINT,
    file_hash       VARCHAR(64),              -- SHA-256 hex
    storage_path    TEXT,                      -- relative path in attachments_dir
    extracted_text  TEXT,                      -- OCR / document text extraction
    transcription   TEXT,                      -- audio/video transcription
    thumbnail_path  TEXT,                      -- relative path to thumbnail
    platform_file_id TEXT,                     -- Slack file ID, Telegram file_id
    metadata        JSONB         DEFAULT '{}',
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_chat_attachments_message ON chat_attachments(message_id);
CREATE INDEX idx_chat_attachments_session ON chat_attachments(session_id, created_at DESC);
CREATE INDEX idx_chat_attachments_type    ON chat_attachments(attachment_type);
CREATE INDEX idx_chat_attachments_hash    ON chat_attachments(file_hash);
```

### Apply

```bash
psql -h 127.0.0.1 -p 5432 -U broodlink -d broodlink \
  -f migrations/030_chat_attachments.sql
```

---

## 8. Config Reference

New fields added in v0.12.0:

```toml
[chat]
attachments_dir = "~/.broodlink/attachments"   # Local storage for uploaded files
voice_transcription_enabled = false             # Enable Whisper transcription
chat_transcription_model = "whisper-large-v3-turbo"  # Whisper model
transcription_url = "http://localhost:8180"     # Whisper API endpoint
max_attachment_bytes = 20971520                 # 20 MiB max file size
```

### Config Structs

| Struct | Field | Type | Default |
|--------|-------|------|---------|
| `ChatConfig` | `attachments_dir` | `String` | `"~/.broodlink/attachments"` |
| `ChatConfig` | `voice_transcription_enabled` | `bool` | `false` |
| `ChatConfig` | `chat_transcription_model` | `String` | `"whisper-large-v3-turbo"` |
| `ChatConfig` | `transcription_url` | `String` | `"http://localhost:8180"` |
| `ChatConfig` | `max_attachment_bytes` | `u64` | `20971520` |

---

## 9. Bug Fixes

### Verification Analytics Query

**Problem:** `handler_verification_analytics()` grouped by `event_type`
prefix stripping (`verification_completed` -> `completed`). But a2a-gateway
writes all verification events as `event_type = 'verification_completed'`
with the actual outcome in `details->>'outcome'`.

**Fix:** Query now groups by `COALESCE(details->>'outcome', ...)` instead
of stripping the `verification_` prefix from `event_type`. Outcomes map to
verified/corrected/timeout/error; unknowns bucket into error.

**File:** `rust/status-api/src/main.rs` — `handler_verification_analytics()`

### Approval "all" Gate Type

**Problem:** Policy upsert validation rejected `gate_type = "all"`.

**Fix:** Added `"all"` to the allowed values list alongside `pre_dispatch`,
`pre_completion`, `budget`, `custom`.

**File:** `rust/status-api/src/main.rs` — `handler_upsert_approval_policy()`

### `fetchApi()` POST Support

**Problem:** `fetchApi()` in `utils.js` was GET-only. Chat page and other
pages needing POST had to duplicate fetch logic.

**Fix:** `fetchApi()` now accepts an optional `opts` parameter for method,
headers, and body.

**File:** `status-site/themes/broodlink-status/static/js/utils.js`

---

## 10. Test Coverage

### New Tests

**Attachment Storage (broodlink-fs):** 10 tests covering store/read
roundtrip, path format, hash dedup, size limits, empty rejection, path
traversal prevention, thumbnail error handling, extension defaults.

**Multi-modal Config (broodlink-config):** `test_v012_multimodal_config_defaults`
verifying all new ChatConfig fields have correct defaults.

**Python SDK (agents/tests/):** 8 test modules covering CLI commands,
client initialization, model deserialization, namespace operations,
backward compatibility, and end-to-end flows.

---

## Files Changed (Summary)

| File | Changes |
|------|---------|
| `Cargo.toml` | `reqwest` multipart feature |
| `Cargo.lock` | Dependency updates |
| `config.toml` | New `[chat]` attachment fields |
| `crates/broodlink-config/src/lib.rs` | 5 new `ChatConfig` fields + defaults + test |
| `crates/broodlink-fs/Cargo.toml` | Added `sha2`, `hex`, `image`, `chrono`, `uuid` deps |
| `crates/broodlink-fs/src/lib.rs` | Re-export `attachments` module |
| `crates/broodlink-fs/src/attachments.rs` | New: store, read, thumbnail, hash (226 lines) |
| `rust/a2a-gateway/src/main.rs` | Multi-modal attachment pipeline (+736 lines) |
| `rust/beads-bridge/src/main.rs` | Minor (attachment-related message threading) |
| `rust/status-api/src/main.rs` | 3 attachment endpoints, verification fix, approval fix (+281 lines) |
| `rust/status-api/Cargo.toml` | Added `broodlink-fs` |
| `agents/broodlink_agent/__init__.py` | v0.12.0, typed exports |
| `agents/broodlink_agent/cli.py` | Click CLI rewrite |
| `agents/broodlink_agent/*.py` | New: client, agent, transport, nats_helper |
| `agents/broodlink_agent/models/` | New: Pydantic models |
| `agents/broodlink_agent/namespaces/` | New: 12 namespace modules |
| `agents/pyproject.toml` | New: package metadata |
| `agents/tests/` | New: 8 test modules |
| `migrations/030_chat_attachments.sql` | New: chat_attachments table |
| `status-site/themes/.../layouts/*.html` | 15 pages updated (structural fixes) |
| `status-site/themes/.../layouts/partials/sidebar.html` | Sidebar grouping + labels |
| `status-site/themes/.../static/css/style.css` | ~210 lines new CSS |
| `status-site/themes/.../static/js/*.js` | 10 JS files updated |
| `status-site/content/*/_index.md` | Updated page titles/weights |

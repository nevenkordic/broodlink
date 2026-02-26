# Broodlink v0.8.0 — Agent Intelligence Upgrade

> Multi-agent AI orchestration system
> Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later
> Generated: 2026-02-26

This document covers changes in v0.8.0. For full system documentation, see
[broodlink-v0_7_0-docs.md](broodlink-v0_7_0-docs.md).

---

## Table of Contents

1. [Overview](#1-overview)
2. [Model Configuration](#2-model-configuration)
3. [Confidence Scoring & Verification](#3-confidence-scoring--verification)
4. [Dynamic System Prompts](#4-dynamic-system-prompts)
5. [Semantic Search Filters](#5-semantic-search-filters)
6. [Content Chunking](#6-content-chunking)
7. [FormulaStep Extensions](#7-formulastep-extensions)
8. [Deadlock Detection](#8-deadlock-detection)
9. [A2A Task Dispatch](#9-a2a-task-dispatch)
10. [Config Reference](#10-config-reference)
11. [Test Coverage](#11-test-coverage)

---

## 1. Overview

v0.8.0 upgrades Broodlink's agent intelligence with faster models, hallucination
prevention, improved RAG, and operational resilience. No new database migrations
are required — all changes are code and config level.

**Key metrics (Mac Studio M4 Max, 128GB):**

| Metric | v0.7.0 | v0.8.0 |
|--------|--------|--------|
| Primary model | qwen3:32b (dense) | qwen3:30b-a3b (MoE) |
| Inference speed | ~25 tok/s | ~95 tok/s |
| VRAM (primary) | ~20 GB | ~18.9 GB |
| Verifier model | — | deepseek-r1:14b (~9 GB) |
| Unit tests | 261 | 271 |

---

## 2. Model Configuration

### Primary: qwen3:30b-a3b (Mixture of Experts)

30B total parameters, only 3B active per token. Delivers 3–4x faster inference
than the previous dense 32B model at equivalent benchmark quality.

```toml
[chat]
chat_model = "qwen3:30b-a3b"
chat_fallback_model = "qwen3:1.7b"
```

### Verifier: deepseek-r1:14b

Strong reasoning model used exclusively for fact-checking low-confidence
responses. Loaded on demand (not kept resident).

```toml
[chat]
verifier_model = "deepseek-r1:14b"
verifier_timeout_seconds = 60
```

### Think Mode

All Ollama API calls use `"think": true`. This is critical for qwen3 MoE models
which leak chain-of-thought into `message.content` when `think:false` is set.
With `think:true`, thinking goes to `message.thinking` and `message.content`
contains only the final response. `strip_think_tags()` is retained as a safety
net.

---

## 3. Confidence Scoring & Verification

### Confidence Tags

The system prompt includes:

```
At the end of every response, include a confidence rating in this exact format:
[CONFIDENCE: N/5] where N is 1=guessing, 2=uncertain, 3=reasonable, 4=confident, 5=certain.
```

Two helper functions parse and strip the tag:

- `parse_confidence(text) -> Option<u8>` — extracts the N value via regex
- `strip_confidence_tag(text) -> String` — removes the `[CONFIDENCE: N/5]` tag

The tag is always stripped before delivering the response to the user.

### Verify-then-Respond Pipeline

```
User query → LLM response → parse confidence
                                  │
                          confidence >= 3? → return clean response
                                  │ no
                          verifier_model configured? → return clean response
                                  │ yes
                          verify_response() → deepseek-r1:14b
                                  │
                          VERIFIED? → return original
                          CORRECTED? → return corrected response
```

The verifier receives the original response, user query, and memory context.
It operates at temperature 0.1 with a 512-token limit and the configured
timeout. Only called when confidence is below 3.

**File:** `rust/a2a-gateway/src/main.rs` — `verify_response()`, `call_ollama_chat()`

---

## 4. Dynamic System Prompts

Each agent can have a `system_prompt` in config.toml:

```toml
[agents.a2a-gateway]
system_prompt = "You are Broodlink, a helpful and concise AI assistant..."

[agents.claude]
system_prompt = "You are a strategic architect and planner..."
```

At runtime, the a2a-gateway loads the prompt from config:

```rust
let base_prompt = state.config.agents.get("a2a-gateway")
    .and_then(|a| a.system_prompt.as_deref())
    .unwrap_or("You are Broodlink, an AI assistant. Keep responses concise.");
```

This applies to both the primary chat path and the fallback chat path.

**Struct:** `AgentConfig.system_prompt: Option<String>` in `crates/broodlink-config/src/lib.rs`

---

## 5. Semantic Search Filters

The `semantic_search` tool now accepts optional metadata filters:

| Parameter | Type | Description |
|-----------|------|-------------|
| `query` | string | Search query (required) |
| `limit` | integer | Max results (default 5) |
| `agent_id` | string | Filter by agent name |
| `date_from` | string | ISO date, results after this date |
| `date_to` | string | ISO date, results before this date |

Filters are translated to Qdrant `filter.must` conditions:

- `agent_id` → `match` filter on `payload.agent_id`
- `date_from` → `range` filter (`gte`) on `payload.created_at`
- `date_to` → `range` filter (`lte`) on `payload.created_at`

### Hybrid Search Rerank Default

The `hybrid_search` tool's `rerank` parameter now defaults to
`config.memory_search.reranker_enabled` (default `true`) instead of hardcoded
`false`.

**File:** `rust/beads-bridge/src/main.rs` — `tool_semantic_search()`, `tool_hybrid_search()`

---

## 6. Content Chunking

When a memory exceeds `chunk_max_tokens` (default 500 words), the embedding
worker splits it into overlapping windows before embedding.

### Algorithm

```
chunk_content(text, max_tokens=500, overlap=50):
    words = text.split_whitespace()
    if words.len() <= max_tokens:
        return [text]
    chunks = []
    start = 0
    while start < words.len():
        end = min(start + max_tokens, words.len())
        chunks.push(words[start..end].join(" "))
        start += max_tokens - overlap
    return chunks
```

### Qdrant Storage

Each chunk is stored as a separate Qdrant point. Chunk IDs are deterministic
UUIDs derived from the base embedding UUID via byte XOR:

```rust
let mut bytes = Uuid::parse_str(&qdrant_id).into_bytes();
bytes[14] ^= (i as u8).wrapping_add(1);
bytes[15] ^= (i as u8).wrapping_add(0xAB);
bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 1
```

All chunk payloads preserve the original `memory_id` for dedup at search time.
Chunk topics are annotated: `"Original Topic [part 1/3]"`.

### Config

```toml
[memory_search]
chunk_max_tokens = 500
chunk_overlap_tokens = 50
```

**File:** `rust/embedding-worker/src/main.rs` — `chunk_content()`, `process_outbox_row()`

---

## 7. FormulaStep Extensions

Three new optional fields on `FormulaStep` (`crates/broodlink-formulas/src/lib.rs`):

```rust
pub struct FormulaStep {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}
```

### Coordinator Rendering

In `create_step_task()`:
1. Prepend `step.system_prompt` if present
2. Append `step.examples` as `## Examples:` section
3. Append `step.output_schema` as output format guidance

In `handle_task_completed()`:
1. If `step.output_schema` is set, validate result keys exist
2. On validation failure: retry with schema error feedback (max 1 retry)

### Formula Examples

System formulas updated with few-shot examples:

- `research.formula.toml` — `synthesize_report` step: structured report format
- `daily-review.formula.toml` — `produce_review` step: review template
- `knowledge-gap.formula.toml` — `document_learnings` step: gap documentation format
- `build-feature.formula.toml` — `test` and `document` steps: `group = 1` for parallel execution

---

## 8. Deadlock Detection

The coordinator runs a `task_timeout_loop` every 60 seconds:

1. Query `task_queue` for tasks in `claimed` status longer than
   `task_claim_timeout_minutes` (default 30)
2. Reset status to `pending`, clear `assigned_agent`
3. Publish `task_available` to NATS for re-routing
4. Log timeout events to `service_events`

### Config

```toml
[collaboration]
task_claim_timeout_minutes = 30
```

**File:** `rust/coordinator/src/main.rs` — `task_timeout_loop()`

---

## 9. A2A Task Dispatch

Previously, tasks created via the A2A protocol (`/a2a/tasks/send`) were stored
in the task queue but never triggered LLM inference — the a2a-gateway only
subscribed to `task_completed` and `task_failed` NATS subjects.

v0.8.0 adds a fifth NATS subscription:

```
{prefix}.{env}.agent.a2a-gateway.task
```

When the coordinator dispatches a task to a2a-gateway, the
`handle_dispatched_task()` function:

1. Parses `task_id`, `description`, and `title` from the NATS payload
2. Acquires the Ollama semaphore (fails fast if busy)
3. Calls `call_ollama_chat()` with the task description as user message
4. Completes the task via `bridge_call("complete_task")` with the LLM response
5. On semaphore contention, fails the task via `bridge_call("fail_task")`

**File:** `rust/a2a-gateway/src/main.rs` — `handle_dispatched_task()`

---

## 10. Config Reference

New fields added in v0.8.0:

```toml
[chat]
verifier_model = "deepseek-r1:14b"       # empty = disabled
verifier_timeout_seconds = 60

[memory_search]
chunk_max_tokens = 500                     # 0 = no chunking
chunk_overlap_tokens = 50

[collaboration]
task_claim_timeout_minutes = 30

[agents.claude]
system_prompt = "You are a strategic architect..."

[agents.a2a-gateway]
system_prompt = "You are Broodlink, a helpful and concise AI assistant..."
```

### Config Structs

| Struct | Field | Type | Default |
|--------|-------|------|---------|
| `ChatConfig` | `verifier_model` | `String` | `""` |
| `ChatConfig` | `verifier_timeout_seconds` | `u64` | `60` |
| `MemorySearchConfig` | `chunk_max_tokens` | `usize` | `500` |
| `MemorySearchConfig` | `chunk_overlap_tokens` | `usize` | `50` |
| `CollaborationConfig` | `task_claim_timeout_minutes` | `u32` | `30` |
| `AgentConfig` | `system_prompt` | `Option<String>` | `None` |

---

## 11. Test Coverage

271 total workspace tests:

| Crate | Tests |
|-------|-------|
| a2a-gateway | 38 (+10) |
| beads-bridge | 83 |
| broodlink-config | 4 |
| broodlink-formulas | 12 |
| broodlink-runtime | 5 |
| broodlink-secrets | 5 |
| broodlink-telemetry | 7 |
| coordinator | 35 |
| embedding-worker | 31 |
| mcp-server | 23 |
| status-api | 28 |

### New Tests in v0.8.0

- `test_task_dispatch_payload_extracts_fields` — NATS dispatch payload parsing
- `test_task_dispatch_payload_falls_back_to_title` — fallback when description empty
- `test_task_dispatch_payload_skips_empty` — skip on missing task_id or empty prompt
- `test_parse_confidence_present` — confidence tag extraction (values 1, 3, 5)
- `test_parse_confidence_absent` — None on missing/invalid tag
- `test_strip_confidence_tag` — tag removal and whitespace normalization
- `test_strip_think_tags_paired` — standard `<think>...</think>` removal
- `test_strip_think_tags_orphaned_close` — qwen3 MoE orphaned `</think>` pattern
- `test_strip_think_tags_no_tags` — passthrough for clean text
- `test_strip_think_tags_unclosed` — unclosed `<think>` drops trailing content

---

## Files Changed

| File | Changes |
|------|---------|
| `config.toml` | Model switch, verifier config, agent system prompts, task timeout |
| `crates/broodlink-config/src/lib.rs` | `AgentConfig.system_prompt`, `ChatConfig.verifier_*`, `MemorySearchConfig.chunk_*`, `CollaborationConfig.task_claim_timeout_minutes` |
| `crates/broodlink-formulas/src/lib.rs` | `FormulaStep`: `examples`, `system_prompt`, `output_schema` fields |
| `rust/a2a-gateway/src/main.rs` | Dynamic prompts, confidence scoring, verify-then-respond, NATS task dispatch, think:true fix, 10 new tests |
| `rust/beads-bridge/src/main.rs` | Semantic search metadata filters, hybrid search rerank default |
| `rust/coordinator/src/main.rs` | Examples/system_prompt/schema rendering, output schema validation, deadlock detection loop |
| `rust/embedding-worker/src/main.rs` | Content chunking with deterministic UUID generation |
| `.beads/formulas/*.formula.toml` | Parallel groups, few-shot examples |

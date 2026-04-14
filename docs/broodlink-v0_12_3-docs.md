# Broodlink v0.12.3 — Gemma 4, Streaming Events, Trust Gates, Native Infra

> Multi-agent AI orchestration system
> Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later
> Generated: 2026-04-14

This document covers changes in v0.12.3. For previous versions, see the
[CHANGELOG](../CHANGELOG.md) and [v0.12.2 docs](broodlink-v0_12_2-docs.md).

---

## Table of Contents

1. [Overview](#1-overview)
2. [Model Migration — Gemma 4](#2-model-migration--gemma-4)
3. [Streaming Event Protocol](#3-streaming-event-protocol)
4. [Transcript Compaction](#4-transcript-compaction)
5. [Trust-Gated Deferred Initialization](#5-trust-gated-deferred-initialization)
6. [Token-Based Prompt Pre-Filter](#6-token-based-prompt-pre-filter)
7. [Deny-List Permission Pre-Filter](#7-deny-list-permission-pre-filter)
8. [Bootstrap Graph](#8-bootstrap-graph)
9. [Native Infrastructure Scripts](#9-native-infrastructure-scripts)
10. [Configuration Reference](#10-configuration-reference)

---

## 1. Overview

v0.12.3 migrates the entire model stack from Qwen 3.x to Google's Gemma 4
family (Apache 2.0, native tool calling, 256K context). Six new subsystems
land: a formalized streaming event protocol across all services, transcript
compaction for long-running agents, trust-gated tool access, lightweight
candidate pre-filtering in the coordinator, an O(1) deny-list in beads-bridge,
and a DAG-based bootstrap pipeline for deterministic startup ordering. The
infrastructure layer moves fully native — all services run via brew and direct
binaries with no container dependency.

**Key numbers:** 3 Gemma 4 models, 17 typed event kinds, 5 trust levels,
6 bootstrap stages.

---

## 2. Model Migration — Gemma 4

**Files:** `crates/broodlink-config/src/lib.rs`, `rust/broodlink/src/models.rs`,
`rust/a2a-gateway/src/main.rs`, `config.toml`, `.aider.conf.yml`

The full model stack has been migrated:

| Role | Previous | New | Size |
|------|----------|-----|------|
| Primary chat + vision | qwen3.5:35b + gemma3:27b | gemma4:31b | 19 GB |
| Code generation | qwen3-coder:30b | gemma4:26b (MoE) | 17 GB |
| Fallback / KG / expansion | qwen3.5:4b | gemma4:e4b | 9.6 GB |
| Verifier | deepseek-r1:32b | deepseek-r1:32b (retained) | 19 GB |
| Embeddings | nomic-embed-text | nomic-embed-text (retained) | 274 MB |

All Gemma 4 models share Apache 2.0 licensing, native tool calling support,
and 256K context windows. Vision is now handled by gemma4:31b directly rather
than a separate gemma3 model.

Config default functions updated:

- `default_chat_model()` → `"gemma4:31b"`
- `default_chat_code_model()` → `"gemma4:26b"`
- `default_chat_fallback_model()` → `"gemma4:e4b"`
- `default_kg_extraction_model()` → `"gemma4:e4b"`
- `default_query_expansion_model()` → `"gemma4:e4b"`
- `default_decomposition_model()` → `"gemma4:e4b"`

The A2A gateway's think/vision capability detection was updated to recognise
gemma4 as fully supporting both think mode and vision tool dispatch, unlike
legacy gemma3 models which were excluded from think mode.

The `/api/v1/models/recommended` endpoint returns Gemma 4 models in both
"Core Stack" and "Advanced" tiers.

---

## 3. Streaming Event Protocol

**Files:** `crates/broodlink-events/src/lib.rs`,
`agents/broodlink_agent/events.py`

A formalized SSE/NATS event envelope shared across all services. Every
observable action in the pipeline emits a typed `StreamEvent`.

### Rust Crate (`broodlink-events`)

Core types:

- **`StreamEvent`** — envelope with `event_id`, `stream_id`, `sequence`,
  `timestamp`, `source`, and `kind`
- **`EventSource`** — origin service: BeadsBridge, Coordinator, A2aGateway,
  StatusApi, EmbeddingWorker, Heartbeat, or Agent(agent_id)
- **`EventKind`** — tagged enum with 17 variants:
  StreamStart, StreamEnd, StreamError, RouteStart, AgentMatch,
  TaskDispatched, ToolMatch, ToolExecStart, MessageDelta,
  PermissionDenied, GuardrailTriggered, BudgetDebit, BudgetExhausted,
  TaskCompleted, VerificationResult, HeartbeatCycle, Custom
- **`StreamBuilder`** — sequence counter that produces ordered events within
  a stream

Each variant carries a typed payload struct (e.g. `StreamStartData`,
`TaskCompletedData`, `ToolMatchData`, `PermissionDeniedData`).

### Python SDK (`agents/broodlink_agent/events.py`)

Mirror of the Rust crate as dataclasses:

- `EventSource` enum with `Agent(agent_id)` variant
- `StreamStatus` enum: SUCCESS, PARTIAL, FAILED, CANCELLED
- `EventType` enum matching all 17 Rust variants
- `StreamEvent` dataclass
- `StreamBuilder` with `emit()` method

---

## 4. Transcript Compaction

**Files:** `agents/broodlink_agent/transcript.py`

Context window management for long-running agents. When the conversation
grows past a configurable token budget, the transcript is compacted to
keep the agent responsive without losing critical context.

### `TranscriptManager`

| Field | Default | Purpose |
|-------|---------|---------|
| `max_tokens` | 8192 | Token budget before compaction triggers |
| `keep_last` | 6 | Number of recent messages preserved verbatim |
| `flush_dir` | None | Optional directory for flushing full transcripts |

Key methods:

- **`append(role, content)`** — adds a message and triggers auto-compaction
  at 80% of `max_tokens`
- **`compacted_messages()`** — returns system prompt + compaction summary +
  last N messages, suitable for Ollama API calls
- **`flush_to_disk()`** — writes the full uncompacted transcript to
  `flush_dir` before discarding old messages
- **`usage_summary()`** — current token count and percentage of budget used

Token estimation uses a word-count heuristic (~0.75 tokens per word) to
avoid external tokenizer dependencies.

---

## 5. Trust-Gated Deferred Initialization

**Files:** `agents/broodlink_agent/deferred_init.py`

Phased tool access based on agent trust levels. New agents start with
restricted capabilities and gain access as trust increases.

### Trust Levels

| Level | Value | Tool Access |
|-------|-------|-------------|
| UNTRUSTED | 0 | No tools |
| PROBATION | 1 | Read-only: semantic_search, list_tasks, get_budget |
| STANDARD | 2 | Core: store_memory, create_task, send_message, etc. |
| ELEVATED | 3 | Privileged: set_budget, update_guardrail, manage_webhook |
| SYSTEM | 4 | All tools including admin operations |

### `TrustGate`

Controls which tools an agent can access at its current trust level:

- **`allows(tool_name)`** — checks if the tool is permitted
- **`filter_tools(tool_list)`** — returns only permitted tools
- **`promote()` / `demote()`** — adjusts trust level
- **`custom_deny` / `custom_allow`** — per-agent overrides

### `DeferredInitManager`

Orchestrates phased initialization hooks:

- **`register(trust_level, callback)`** — registers a function to run when
  an agent reaches the specified trust level
- **`run_eligible(agent_trust_level)`** — executes all registered hooks up
  to the agent's current trust level

---

## 6. Token-Based Prompt Pre-Filter

**Files:** `rust/coordinator/src/main.rs`

Lightweight keyword overlap scoring that runs before the full weighted
multi-factor routing. Tokenizes the incoming task prompt and each agent's
`model_domains` and `skills` list, computing a normalized overlap score.
Agents below the threshold are excluded before the more expensive scoring
pass (success rate, load, cost, recency).

This reduces routing latency for systems with many registered agents by
avoiding full scoring on clearly irrelevant candidates.

Function: `token_prefilter(prompt, agents, threshold) -> Vec<Agent>`

---

## 7. Deny-List Permission Pre-Filter

**Files:** `rust/beads-bridge/src/main.rs`

O(1) in-memory tool blocking that runs before database guardrail queries.
Catches forbidden tool calls immediately without hitting Postgres.

### `ToolDenyList`

| Field | Type | Purpose |
|-------|------|---------|
| `denied_names` | HashSet | Exact tool names always blocked |
| `denied_prefixes` | Vec | Tool name prefixes always blocked |
| `agent_overrides` | HashMap | Per-agent deny sets |

Built-in denied tools:

- `debug_dump_state`, `unsafe_raw_query`, `system_shutdown`,
  `admin_rotate_keys`

Built-in denied prefixes:

- `debug_`, `unsafe_`

Methods:

- **`from_config()`** — loads from config.toml deny-list section
- **`check(agent_id, tool_name)`** — returns Ok or PermissionDenied

The check runs after JWT validation and rate limiting but before budget
enforcement and guardrail queries, providing fast rejection of obviously
forbidden calls.

---

## 8. Bootstrap Graph

**Files:** `rust/broodlink/src/bootstrap.rs`

Formalized startup pipeline using a directed acyclic graph of stages.
Replaces ad-hoc sequential startup with deterministic, dependency-aware
ordering.

### Default Pipeline

```
Prefetch → Config → Dependencies → Databases → Services → DeferredInit → Ready
```

### `BootstrapGraph`

| Method | Purpose |
|--------|---------|
| `new()` | Empty graph |
| `default_pipeline()` | Standard 7-stage pipeline |
| `add_stage(name, deps)` | Add a stage with dependency list |
| `insert_before(target, name)` | Insert a stage before an existing one |
| `on_stage(name, handler)` | Register a handler function for a stage |
| `run()` | Topologically sort and execute all stages |

Topological sorting via Kahn's algorithm with cycle detection. If a cycle
is detected, startup aborts with a clear error listing the involved stages.

Each stage execution is recorded in `StageRecord` with start time, duration,
and success/failure status for diagnostics.

---

## 9. Native Infrastructure Scripts

**Files:** `scripts/infra-start.sh`, `scripts/dev.sh`

All infrastructure services now run natively via brew and direct binaries.
No container runtime required for development.

### `scripts/infra-start.sh`

| Command | Action |
|---------|--------|
| `start` | Start all infrastructure services |
| `stop` | Stop all infrastructure services |
| `status` | Show running status of each service |

Services managed:

| Service | Method | Data Directory |
|---------|--------|----------------|
| PostgreSQL | `brew services` | Default brew data |
| NATS | `brew services` | Default brew config |
| Ollama | `brew services` | `~/.ollama/` |
| Dolt | `dolt sql-server` (background) | `~/.broodlink/infra/dolt/` |
| Qdrant | Direct binary (background) | `~/.broodlink/infra/qdrant/` |
| Jaeger | Optional, direct binary | — |

Qdrant is auto-downloaded from GitHub releases for the host architecture
if not already present.

Logs are written to `/tmp/broodlink/`.

### `scripts/dev.sh`

Updated to use native infrastructure:

- `start`: calls `infra-start.sh start` → `start-services.sh` → Hugo
- `stop`: stops Hugo → services → infra
- `status`: calls `infra-start.sh status`

---

## 10. Configuration Reference

New and changed config fields in v0.12.3:

```toml
# Model defaults (all changed)
[chat]
model = "gemma4:31b"              # Was qwen3.5:35b
code_model = "gemma4:26b"        # Was qwen3-coder:30b
fallback_model = "gemma4:e4b"    # Was qwen3.5:4b

[memory_search]
kg_extraction_model = "gemma4:e4b"    # Was qwen3.5:4b
query_expansion_model = "gemma4:e4b"  # Was qwen3.5:4b

[collaboration.decomposition]
model = "gemma4:e4b"                  # Was qwen3.5:4b
```

All existing configuration is unchanged and backward-compatible. The model
fields accept any Ollama model name — Gemma 4 values are defaults only.

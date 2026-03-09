# Broodlink v0.12.2 — Full Orchestration, Native Chat, Cross-Platform

> Multi-agent AI orchestration system
> Copyright (C) 2025-2026 Neven Kordic <neven@broodlink.ai>
> License: AGPL-3.0-or-later
> Generated: 2026-03-09

This document covers changes in v0.12.2. For previous versions, see the
[CHANGELOG](../CHANGELOG.md) and [v0.11.0 docs](broodlink-v0_11_0-docs.md).

---

## Table of Contents

1. [Overview](#1-overview)
2. [Sub-task Decomposition](#2-sub-task-decomposition)
3. [Agent Delegation Protocol](#3-agent-delegation-protocol)
4. [Verification Pipeline](#4-verification-pipeline)
5. [Native Chat UI](#5-native-chat-ui)
6. [Download Model UI](#6-download-model-ui)
7. [Negotiations Endpoint](#7-negotiations-endpoint)
8. [Windows Support](#8-windows-support)
9. [Cross-Platform Release Workflow](#9-cross-platform-release-workflow)
10. [Install Scripts](#10-install-scripts)
11. [Configuration Reference](#11-configuration-reference)
12. [Test Coverage](#12-test-coverage)

---

## 1. Overview

v0.12.2 brings Broodlink to full orchestration capability. The coordinator now
handles the complete task lifecycle: decompose complex work into sub-tasks,
delegate between agents, verify outputs, and aggregate results. The dashboard
gains a native chat UI (no external dependencies) and an overhauled model
download experience. All binaries now compile and run on Windows alongside
macOS and Linux. A GitHub Actions release workflow builds for 5 platform
targets automatically.

**Key numbers:** 381 unit tests, 8 binaries, 18 dashboard pages, 5 release
targets.

---

## 2. Sub-task Decomposition

**Files:** `rust/coordinator/src/main.rs`, `crates/broodlink-config/src/lib.rs`

When a task arrives, the coordinator checks if decomposition is enabled and the
task exceeds the complexity threshold (`min_complexity_chars`, default 200). If
so, it calls the configured Ollama model with a structured prompt asking for a
JSON array of sub-tasks.

Each sub-task gets:
- A title formatted as `[1/N] Sub-title`
- The parent's priority and convoy_id
- A `parent_task_id` linking back to the original

The coordinator inserts child tasks into `task_queue` and records the
relationship in `task_decompositions`. The parent task status becomes
`decomposed` and is not routed directly. Each sub-task is published to NATS
for independent routing.

**Fail-open design:** If Ollama is unreachable, returns garbage, or
decomposition produces fewer than 2 sub-tasks, the original task routes
normally. Decomposition never blocks the pipeline.

### Configuration

```toml
[collaboration.decomposition]
enabled = false                  # Disabled by default
model = "qwen3.5:4b"            # Model for decomposition
min_complexity_chars = 200       # Skip short tasks
timeout_secs = 30                # Ollama call timeout
```

---

## 3. Agent Delegation Protocol

**Files:** `rust/coordinator/src/main.rs`

Agents can delegate work to other agents through the coordinator. The protocol
follows a request/response lifecycle:

1. **Request:** Agent publishes to `coordinator.delegation_request` with title,
   description, optional preferred agent, and role.
2. **Create:** Coordinator inserts a `delegations` record, creates a task in
   `task_queue` with `delegation_id`, publishes `task_available`.
3. **Accept/Decline:** Target agent responds via
   `coordinator.delegation_response`.
4. **Complete:** On completion, coordinator updates the delegation, notifies the
   originating agent via `agent.{id}.delegation_result`, and checks if the
   parent task can be marked complete.

**Parent completion:** `check_parent_task_completion()` counts child tasks and
marks the parent as `completed` when all children are done, publishing a
`task_completed` event.

Two NATS subscription loops are spawned at startup with graceful shutdown
support.

---

## 4. Verification Pipeline

**Files:** `rust/coordinator/src/main.rs`

When a task completes, the coordinator runs an automatic verification step
before finalizing. The verification calls Ollama with the task's title,
description, and output, asking for a JSON verdict.

Three outcomes:
- **Pass** — Task marked complete. Logged to `service_events`.
- **Fail(feedback)** — Agent notified via `agent.{id}.verification_result`
  with specific feedback. Logged to `service_events`.
- **Skip** — No action. Occurs when output is too short (<50 chars), Ollama
  is unreachable, or the response can't be parsed.

**Fail-open:** Verification never blocks task completion. If anything goes
wrong, the task proceeds as if verification passed.

---

## 5. Native Chat UI

**Files:** `status-site/themes/broodlink-status/layouts/chat/list.html`,
`static/js/chat.js`, `static/css/style.css`, `rust/broodlink/src/main.rs`

The dashboard now includes a full chat interface at `/chat/` that talks
directly to local Ollama models via the `/api/ollama/*` reverse proxy. No
Python, no Open WebUI, no external dependencies.

Features:
- **Model selector** — fetches available models from Ollama
- **Conversation history** — persisted in localStorage with sidebar navigation
- **Message rendering** — user/assistant bubbles with markdown support (fenced
  code blocks, inline code, bold)
- **Typing indicator** — animated dots while waiting for response
- **Auto-resize textarea** — grows with input, Enter to send, Shift+Enter for
  newlines
- **Delete conversations** — hover to reveal delete button per conversation

The agent conversation monitoring section (sessions, metrics, platform filters)
is preserved below the chat interface.

---

## 6. Download Model UI

**Files:** `rust/broodlink/src/models.rs`,
`status-site/themes/broodlink-status/layouts/models/list.html`,
`static/js/models.js`

The model download modal has been overhauled:

- **Tabbed interface** — Recommended and Custom tabs
- **Categorized recommendations** — "Core Stack" (essential for Broodlink) and
  "Advanced" (16GB+ VRAM) sections with descriptions
- **Install status** — each recommended model shows "Installed" (greyed out) or
  "Click to download", updated live after downloads
- **Real SSE progress** — streams actual download progress from Ollama via
  `GET /api/v1/models/pull/progress?name=<model>`. Shows status, byte counts,
  and percentage in a progress bar
- **Download guard** — prevents multiple concurrent downloads

The SSE endpoint proxies Ollama's newline-delimited JSON pull stream, parsing
each chunk into a structured event with `status`, `total`, `completed`,
`percent`, and `digest` fields.

---

## 7. Negotiations Endpoint

**Files:** `rust/status-api/src/main.rs`,
`status-site/themes/broodlink-status/layouts/delegations/list.html`,
`static/js/delegations.js`

New `GET /api/v1/negotiations` endpoint queries the `task_negotiations` table
(migration 029) and returns decline, context request, context provided, and
redirect events.

The Handoffs dashboard page (`/delegations/`) now includes a Negotiations
section below the delegations table, with:
- Metrics: total events, declined, context requests, redirected
- Filter by action type
- Table showing task ID, agent, action, reason, suggested agent, timestamp

---

## 8. Windows Support

**Files:** `rust/broodlink/Cargo.toml`, `src/main.rs`, `src/process_manager.rs`,
`src/setup/mod.rs`, `src/setup/api.rs`

All changes use `#[cfg(target_os = "windows")]` gates — zero impact on
macOS/Linux builds.

| Area | Windows Implementation |
|------|-----------------------|
| Process termination | `taskkill /PID` instead of `SIGTERM` |
| Home directory | Falls back to `USERPROFILE` env var |
| Browser opening | `cmd /C start` |
| Service startup | `ollama serve`, `pg_ctl start` |
| Package installation | `choco install` (Chocolatey) |
| Ollama installation | Downloads `OllamaSetup.exe`, runs silently |
| Qdrant installation | Downloads Windows MSVC binary from GitHub |
| Database setup | PowerShell (`scripts/db-setup.ps1`) |
| Binary detection | Checks for `.exe` extension |
| Dependency detection | Uses `where` instead of `which` |
| PostgreSQL detection | Checks `C:\Program Files\PostgreSQL\{16,15}\bin\` |

The `nix` crate (Unix-only) was removed and replaced with `libc` under
`[target.'cfg(unix)'.dependencies]`.

---

## 9. Cross-Platform Release Workflow

**File:** `.github/workflows/release.yml`

Triggered on version tags (`v*`). Builds 5 targets in parallel:

| Target | Runner | Archive |
|--------|--------|---------|
| `aarch64-apple-darwin` | `macos-latest` | `.tar.gz` |
| `x86_64-apple-darwin` | `macos-latest` | `.tar.gz` |
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-latest` + `cross` | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-latest` | `.zip` |

Each archive contains all 8 binaries (broodlink, beads-bridge, coordinator,
heartbeat, embedding-worker, status-api, mcp-server, a2a-gateway) plus SHA256
checksums. A final job creates a GitHub Release with all artifacts attached.

To release:
```bash
git tag v0.12.2
git push origin v0.12.2
```

---

## 10. Install Scripts

**Files:** `install.sh`, `install.ps1`

One-liner installers that detect OS/architecture, download the correct release
archive, verify the SHA256 checksum, and copy all 8 binaries into the system
PATH.

**macOS / Linux:**
```bash
curl -fsSL https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.sh | sh
```

**Windows (PowerShell):**
```powershell
irm https://raw.githubusercontent.com/nevenkordic/broodlink/main/install.ps1 | iex
```

The Unix script installs to `/usr/local/bin` (prompts for sudo if needed). The
Windows script installs to `%LOCALAPPDATA%\Broodlink\bin` and adds it to the
user's PATH automatically.

After installation, run `broodlink` to launch the setup wizard.

---

## 11. Configuration Reference


New config fields in v0.12.2:

```toml
[collaboration.decomposition]
enabled = false                  # Enable LLM-powered task decomposition
model = "qwen3.5:4b"            # Ollama model for decomposition
min_complexity_chars = 200       # Skip tasks shorter than this
timeout_secs = 30                # Ollama call timeout
```

All existing configuration is unchanged and backward-compatible.

---

## 12. Test Coverage

381 unit tests across the workspace (up from 367 in v0.11.0):

- **Coordinator:** 59 tests including sub-task deserialization, delegation
  payloads, decomposition config, condition evaluation
- **broodlink-config:** 66 tests including decomposition config defaults
- **beads-bridge:** 91 tests
- **broodlink-formulas:** 12 tests (updated for 5 system formulas)

All tests pass on macOS. Windows tests require a Windows CI runner (covered by
the release workflow).

-- Broodlink - Multi-agent AI orchestration system
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 016: Workflow branching and error handling (Postgres)
-- Adds parallel step tracking, error handler linkage, and task timeouts.

ALTER TABLE workflow_runs ADD COLUMN IF NOT EXISTS parallel_pending INT DEFAULT 0;
ALTER TABLE workflow_runs ADD COLUMN IF NOT EXISTS error_handler_task_id VARCHAR(36);
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS timeout_at TIMESTAMPTZ;

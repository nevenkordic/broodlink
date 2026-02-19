/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

-- Migration 010: Add result_data column to task_queue
-- Database: broodlink_hot (Postgres)

ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS result_data JSONB;

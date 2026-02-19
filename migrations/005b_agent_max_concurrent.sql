/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

-- Migration 005b: Add max_concurrent to agent_profiles
-- Database: agent_ledger (Dolt/MySQL)

ALTER TABLE agent_profiles ADD COLUMN IF NOT EXISTS max_concurrent INT DEFAULT 5;

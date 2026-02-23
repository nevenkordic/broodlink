-- Broodlink - Multi-agent AI orchestration system
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 024: Add meta JSONB column to platform_credentials
-- Stores runtime state like polling offsets, last error, etc.

ALTER TABLE platform_credentials ADD COLUMN IF NOT EXISTS meta JSONB DEFAULT '{}';

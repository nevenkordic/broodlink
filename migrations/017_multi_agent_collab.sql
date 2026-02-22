-- Broodlink - Multi-agent AI orchestration system
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 017: Multi-agent collaboration (Postgres)
-- Task decomposition and shared workspaces.

CREATE TABLE IF NOT EXISTS task_decompositions (
    id              BIGSERIAL    PRIMARY KEY,
    parent_task_id  VARCHAR(36)  NOT NULL,
    child_task_id   VARCHAR(36)  NOT NULL UNIQUE,
    merge_strategy  VARCHAR(32)  NOT NULL DEFAULT 'concatenate',
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_decomp_parent
    ON task_decompositions (parent_task_id);

CREATE TABLE IF NOT EXISTS shared_workspaces (
    id           VARCHAR(36)  PRIMARY KEY,
    name         VARCHAR(255) NOT NULL,
    owner_agent  VARCHAR(128) NOT NULL,
    participants JSONB        NOT NULL DEFAULT '[]',
    context      JSONB        NOT NULL DEFAULT '{}',
    status       VARCHAR(20)  NOT NULL DEFAULT 'active'
                 CHECK (status IN ('active', 'closed', 'expired')),
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS parent_task_id VARCHAR(36);
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS workspace_id VARCHAR(36);

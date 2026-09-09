-- Broodlink — Multi-agent AI orchestration
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- 032: Self-authoring formula drafts + isolated workers

CREATE TABLE IF NOT EXISTS formula_drafts (
    id               VARCHAR(36) PRIMARY KEY,
    workflow_run_id  VARCHAR(36) REFERENCES workflow_runs(id) ON DELETE SET NULL,
    suggested_name   VARCHAR(100) NOT NULL,
    display_name     VARCHAR(255) NOT NULL,
    description      TEXT,
    definition       JSONB NOT NULL,
    tags             JSONB NOT NULL DEFAULT '[]',
    source_hash      VARCHAR(64) NOT NULL,
    status           VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at      TIMESTAMPTZ,
    CONSTRAINT formula_drafts_status_check
        CHECK (status IN ('pending', 'confirmed', 'dismissed')),
    CONSTRAINT formula_drafts_workflow_unique
        UNIQUE (workflow_run_id)
);

CREATE INDEX IF NOT EXISTS idx_formula_drafts_status
    ON formula_drafts (status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_formula_drafts_name
    ON formula_drafts (suggested_name);

CREATE TABLE IF NOT EXISTS workers (
    id                 VARCHAR(36) PRIMARY KEY,
    parent_agent_id    VARCHAR(100) NOT NULL,
    child_agent_id     VARCHAR(100) NOT NULL UNIQUE,
    goal               TEXT NOT NULL,
    allowed_tools      JSONB NOT NULL DEFAULT '[]',
    timeout_secs       INT NOT NULL DEFAULT 300,
    isolation_backend  VARCHAR(32) NOT NULL DEFAULT 'local',
    runtime_name       VARCHAR(64) NOT NULL DEFAULT 'local',
    status             VARCHAR(20) NOT NULL DEFAULT 'pending',
    result_summary     TEXT,
    budget_tokens      BIGINT NOT NULL DEFAULT 0,
    audit_payload      JSONB,
    started_at         TIMESTAMPTZ,
    completed_at       TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT workers_status_check
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'timeout')),
    CONSTRAINT workers_backend_check
        CHECK (isolation_backend IN ('local', 'docker', 'ssh', 'remote-idle'))
);

CREATE INDEX IF NOT EXISTS idx_workers_parent
    ON workers (parent_agent_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_workers_status
    ON workers (status);

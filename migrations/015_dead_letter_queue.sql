-- Broodlink - Multi-agent AI orchestration system
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 015: Dead-letter queue persistence (Postgres)
-- Persists dead-lettered tasks for inspection and retry.

CREATE TABLE IF NOT EXISTS dead_letter_queue (
    id              BIGSERIAL    PRIMARY KEY,
    task_id         VARCHAR(36)  NOT NULL,
    reason          TEXT         NOT NULL DEFAULT '',
    source_service  VARCHAR(64)  NOT NULL DEFAULT 'coordinator',
    retry_count     INT          NOT NULL DEFAULT 0,
    max_retries     INT          NOT NULL DEFAULT 3,
    next_retry_at   TIMESTAMPTZ,
    resolved        BOOLEAN      NOT NULL DEFAULT FALSE,
    resolved_by     VARCHAR(128),
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_dlq_unresolved
    ON dead_letter_queue (resolved, next_retry_at)
    WHERE resolved = FALSE;

CREATE INDEX IF NOT EXISTS idx_dlq_task
    ON dead_letter_queue (task_id);

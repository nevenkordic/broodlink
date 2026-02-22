-- Broodlink - Multi-agent AI orchestration system
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 018: Webhook gateway (Postgres)
-- Inbound/outbound webhook endpoints and delivery log.

CREATE TABLE IF NOT EXISTS webhook_endpoints (
    id           VARCHAR(36)  PRIMARY KEY DEFAULT gen_random_uuid()::text,
    platform     VARCHAR(20)  NOT NULL CHECK (platform IN ('slack', 'teams', 'telegram', 'generic')),
    name         VARCHAR(255) NOT NULL,
    webhook_url  TEXT,
    verify_token TEXT,
    events       JSONB        NOT NULL DEFAULT '[]',
    active       BOOLEAN      NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_webhook_platform ON webhook_endpoints (platform);
CREATE INDEX IF NOT EXISTS idx_webhook_active   ON webhook_endpoints (active);

CREATE TABLE IF NOT EXISTS webhook_log (
    id           BIGSERIAL    PRIMARY KEY,
    endpoint_id  VARCHAR(36)  NOT NULL REFERENCES webhook_endpoints(id),
    direction    VARCHAR(10)  NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    event_type   VARCHAR(64)  NOT NULL,
    payload      JSONB        NOT NULL DEFAULT '{}',
    status       VARCHAR(20)  NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'delivered', 'failed', 'skipped')),
    error_msg    TEXT,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_webhook_log_endpoint ON webhook_log (endpoint_id);
CREATE INDEX IF NOT EXISTS idx_webhook_log_created  ON webhook_log (created_at);

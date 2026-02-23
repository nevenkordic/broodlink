-- Broodlink - Multi-agent AI orchestration system
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 023: Platform credentials (Postgres)
-- Runtime-configurable bot tokens for Telegram, Slack, Teams.

CREATE TABLE IF NOT EXISTS platform_credentials (
    platform      VARCHAR(20)  PRIMARY KEY CHECK (platform IN ('telegram', 'slack', 'teams')),
    bot_token     TEXT         NOT NULL,
    secret_token  TEXT,
    webhook_url   TEXT,
    bot_username  VARCHAR(255),
    bot_id        VARCHAR(64),
    enabled       BOOLEAN      NOT NULL DEFAULT TRUE,
    registered_at TIMESTAMPTZ,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

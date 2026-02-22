-- Broodlink — Multi-agent AI orchestration
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 019: Chat sessions for conversational agent gateway
-- Postgres only — maps external platform conversations to Broodlink tasks

-- Chat sessions: maps external platform conversations to Broodlink
CREATE TABLE IF NOT EXISTS chat_sessions (
    id VARCHAR(36) PRIMARY KEY,
    platform VARCHAR(50) NOT NULL,           -- slack, teams, telegram
    channel_id VARCHAR(255) NOT NULL,        -- platform channel/chat ID
    user_id VARCHAR(255) NOT NULL,           -- platform user ID
    user_display_name VARCHAR(255),          -- human-readable name
    thread_id VARCHAR(255),                  -- platform thread ID (for threaded replies)
    assigned_agent VARCHAR(100),             -- preferred agent (nullable, for routing hint)
    context JSONB DEFAULT '{}',              -- session context (platform-specific metadata)
    message_count INT DEFAULT 0,
    last_message_at TIMESTAMPTZ,
    status VARCHAR(20) DEFAULT 'active',     -- active, paused, closed
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(platform, channel_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_chat_sessions_platform ON chat_sessions(platform, status);

-- Chat messages: conversation history for context injection
CREATE TABLE IF NOT EXISTS chat_messages (
    id BIGSERIAL PRIMARY KEY,
    session_id VARCHAR(36) NOT NULL REFERENCES chat_sessions(id),
    direction VARCHAR(10) NOT NULL,          -- inbound (user→broodlink), outbound (broodlink→user)
    content TEXT NOT NULL,
    task_id VARCHAR(36),                     -- links to task_queue for outbound messages
    platform_message_id VARCHAR(255),        -- platform's message ID for threading
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_chat_messages_session ON chat_messages(session_id, created_at DESC);

-- Chat reply queue: pending responses to deliver back to platforms
CREATE TABLE IF NOT EXISTS chat_reply_queue (
    id BIGSERIAL PRIMARY KEY,
    session_id VARCHAR(36) NOT NULL REFERENCES chat_sessions(id),
    task_id VARCHAR(36) NOT NULL,
    content TEXT NOT NULL,
    platform VARCHAR(50) NOT NULL,
    reply_url TEXT,                           -- Slack response_url or equivalent
    channel_id VARCHAR(255) NOT NULL,
    thread_id VARCHAR(255),
    status VARCHAR(20) DEFAULT 'pending',    -- pending, delivered, failed
    attempts INT DEFAULT 0,
    error_msg TEXT,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    delivered_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_chat_reply_pending ON chat_reply_queue(status, created_at) WHERE status = 'pending';

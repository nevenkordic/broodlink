-- Broodlink - Multi-agent AI orchestration system
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Migration 014: Agent budget enforcement (Postgres)
-- Adds tool cost map and budget transaction ledger.

-- Per-tool cost lookup
CREATE TABLE IF NOT EXISTS tool_cost_map (
    tool_name   VARCHAR(128) PRIMARY KEY,
    cost_tokens BIGINT       NOT NULL DEFAULT 1,
    description TEXT         DEFAULT ''
);

-- Budget transaction ledger (append-only)
CREATE TABLE IF NOT EXISTS budget_transactions (
    id            BIGSERIAL    PRIMARY KEY,
    agent_id      VARCHAR(128) NOT NULL,
    tool_name     VARCHAR(128) NOT NULL DEFAULT '',
    cost_tokens   BIGINT       NOT NULL,
    balance_after BIGINT       NOT NULL,
    trace_id      VARCHAR(64)  DEFAULT '',
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_budget_txn_agent
    ON budget_transactions (agent_id, created_at DESC);

-- Seed default costs for expensive tools (everything else uses default_tool_cost from config)
INSERT INTO tool_cost_map (tool_name, cost_tokens, description) VALUES
    ('store_memory',        5,  'Write to Dolt + Postgres + outbox'),
    ('hybrid_search',       3,  'BM25 + vector + optional rerank'),
    ('semantic_search',     2,  'Vector similarity search'),
    ('graph_search',        2,  'KG entity search + embeddings'),
    ('graph_traverse',      3,  'Multi-hop graph traversal'),
    ('graph_prune',         5,  'KG cleanup operation'),
    ('beads_run_formula',  10,  'Workflow formula execution'),
    ('start_workflow',     10,  'Workflow orchestration'),
    ('delegate_task',       5,  'Cross-agent delegation'),
    ('a2a_delegate',        5,  'External agent delegation'),
    ('start_stream',        3,  'SSE stream creation')
ON CONFLICT (tool_name) DO NOTHING;

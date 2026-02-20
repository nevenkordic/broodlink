-- Broodlink — Multi-agent AI orchestration
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- 012: Full-text search index for hybrid memory search (BM25 + vector fusion)
-- Database: broodlink_hot (Postgres)

CREATE TABLE IF NOT EXISTS memory_search_index (
    id          BIGSERIAL PRIMARY KEY,
    memory_id   BIGINT NOT NULL,              -- matches Dolt agent_memory.id
    agent_id    VARCHAR(100) NOT NULL,
    topic       VARCHAR(255) NOT NULL,
    content     TEXT NOT NULL,
    tags        VARCHAR(500),
    tsv         TSVECTOR GENERATED ALWAYS AS (
                    setweight(to_tsvector('english', coalesce(topic, '')), 'A') ||
                    setweight(to_tsvector('english', coalesce(tags, '')), 'B') ||
                    setweight(to_tsvector('english', coalesce(content, '')), 'C')
                ) STORED,
    created_at  TIMESTAMPTZ DEFAULT NOW(),
    updated_at  TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (agent_id, topic)
);

CREATE INDEX IF NOT EXISTS idx_memory_tsv ON memory_search_index USING GIN (tsv);
CREATE INDEX IF NOT EXISTS idx_memory_agent ON memory_search_index (agent_id);

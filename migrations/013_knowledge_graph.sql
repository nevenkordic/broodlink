-- 013_knowledge_graph.sql
-- Knowledge graph tables for entity-relationship extraction from agent memories.
-- SPDX-License-Identifier: AGPL-3.0-only

-- ---------------------------------------------------------------------------
-- kg_entities — graph nodes (people, services, concepts, etc.)
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS kg_entities (
    id              BIGSERIAL PRIMARY KEY,
    entity_id       VARCHAR(36) NOT NULL UNIQUE,    -- UUID
    name            VARCHAR(255) NOT NULL,           -- canonical entity name
    entity_type     VARCHAR(50) NOT NULL,            -- person, service, concept, location, technology, organization, event, other
    description     TEXT,                             -- LLM-generated entity summary
    properties      JSONB DEFAULT '{}',              -- arbitrary key-value attributes
    embedding_ref   VARCHAR(36),                      -- Qdrant point ID for entity name embedding
    source_agent    VARCHAR(100),                     -- agent that first created this entity
    mention_count   INT DEFAULT 1,                   -- how many memories reference this entity
    first_seen      TIMESTAMPTZ DEFAULT NOW(),
    last_seen       TIMESTAMPTZ DEFAULT NOW(),
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    updated_at      TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_kg_entities_name ON kg_entities (lower(name));
CREATE INDEX IF NOT EXISTS idx_kg_entities_type ON kg_entities (entity_type);
CREATE INDEX IF NOT EXISTS idx_kg_entities_agent ON kg_entities (source_agent);

-- ---------------------------------------------------------------------------
-- kg_edges — graph relationships between entities
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS kg_edges (
    id              BIGSERIAL PRIMARY KEY,
    edge_id         VARCHAR(36) NOT NULL UNIQUE,     -- UUID
    source_id       VARCHAR(36) NOT NULL REFERENCES kg_entities(entity_id),
    target_id       VARCHAR(36) NOT NULL REFERENCES kg_entities(entity_id),
    relation_type   VARCHAR(100) NOT NULL,            -- LEADS, DEPENDS_ON, RUNS_ON, WORKS_FOR, etc.
    description     TEXT,                              -- LLM-generated relationship description (the "fact")
    weight          FLOAT DEFAULT 1.0,                -- relationship strength/confidence
    properties      JSONB DEFAULT '{}',               -- arbitrary edge attributes
    source_memory_id BIGINT,                          -- which memory this was extracted from
    source_agent    VARCHAR(100),                     -- agent that created this edge
    valid_from      TIMESTAMPTZ DEFAULT NOW(),        -- temporal: when this relationship became true
    valid_to        TIMESTAMPTZ,                      -- temporal: when this relationship ended (NULL = still active)
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    updated_at      TIMESTAMPTZ DEFAULT NOW(),

    UNIQUE (source_id, target_id, relation_type, valid_to)  -- prevent duplicate active relationships
);

CREATE INDEX IF NOT EXISTS idx_kg_edges_source ON kg_edges (source_id);
CREATE INDEX IF NOT EXISTS idx_kg_edges_target ON kg_edges (target_id);
CREATE INDEX IF NOT EXISTS idx_kg_edges_relation ON kg_edges (relation_type);
CREATE INDEX IF NOT EXISTS idx_kg_edges_memory ON kg_edges (source_memory_id);
CREATE INDEX IF NOT EXISTS idx_kg_edges_active ON kg_edges (source_id, target_id) WHERE valid_to IS NULL;

-- ---------------------------------------------------------------------------
-- kg_entity_memories — junction table linking entities to source memories
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS kg_entity_memories (
    entity_id       VARCHAR(36) NOT NULL REFERENCES kg_entities(entity_id),
    memory_id       BIGINT NOT NULL,                  -- references Dolt agent_memory.id
    agent_id        VARCHAR(100) NOT NULL,
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    PRIMARY KEY (entity_id, memory_id)
);

CREATE INDEX IF NOT EXISTS idx_kg_em_memory ON kg_entity_memories (memory_id);

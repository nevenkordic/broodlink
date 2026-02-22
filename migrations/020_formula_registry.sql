-- Broodlink v0.7.0 — Formula Registry
-- Stores workflow formulas in Postgres for dashboard management.
-- Coordinator reads from this table first, falls back to TOML files.

CREATE TABLE formula_registry (
    id VARCHAR(36) PRIMARY KEY,
    name VARCHAR(100) NOT NULL UNIQUE,       -- e.g. "research", "build-feature"
    display_name VARCHAR(255) NOT NULL,
    description TEXT,
    version INT DEFAULT 1,
    definition JSONB NOT NULL,               -- full formula content (steps, params, on_failure)
    author VARCHAR(100),                     -- who created/last edited
    tags JSONB DEFAULT '[]',                 -- categorisation tags
    is_system BOOLEAN DEFAULT false,         -- true for seed formulas (from TOML)
    enabled BOOLEAN DEFAULT true,
    usage_count INT DEFAULT 0,               -- incremented on each workflow start
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_formula_registry_enabled ON formula_registry(enabled, name);

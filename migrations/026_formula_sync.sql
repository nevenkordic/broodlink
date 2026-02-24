-- Broodlink — Formula sync support
-- Adds definition_hash for change detection during bidirectional sync.

ALTER TABLE formula_registry
    ADD COLUMN IF NOT EXISTS definition_hash VARCHAR(64);

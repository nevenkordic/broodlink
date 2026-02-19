-- Broodlink v0.3.0: A2A Gateway
-- Database: broodlink_hot (Postgres)

CREATE TABLE IF NOT EXISTS a2a_task_map (
    external_id     VARCHAR(255) NOT NULL,
    internal_id     VARCHAR(36)  NOT NULL REFERENCES task_queue(id),
    source_agent    VARCHAR(255),
    created_at      TIMESTAMPTZ  DEFAULT NOW(),
    PRIMARY KEY (external_id)
);

CREATE INDEX IF NOT EXISTS idx_a2a_internal ON a2a_task_map(internal_id);

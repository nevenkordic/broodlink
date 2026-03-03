-- Migration 031: Runtime settings for dashboard-controllable toggles
-- Postgres only (broodlink_hot)

CREATE TABLE IF NOT EXISTS runtime_settings (
    key         VARCHAR(100)   PRIMARY KEY,
    value       JSONB          NOT NULL DEFAULT 'false',
    description TEXT           DEFAULT '',
    changed_by  VARCHAR(100),
    changed_at  TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);

-- Seed: unrestricted code mode (disabled by default)
INSERT INTO runtime_settings (key, value, description)
VALUES (
    'unrestricted_code_mode',
    'false'::jsonb,
    'When enabled, file tools and run_command bypass directory restrictions. Blocked patterns, approval flow, and size limits still apply.'
)
ON CONFLICT (key) DO NOTHING;

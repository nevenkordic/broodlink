-- Broodlink — Missing index hardening
-- Adds indexes for frequently queried columns.

-- dashboard_sessions is queried by user_id on every auth check
CREATE INDEX IF NOT EXISTS idx_dashboard_sessions_user_id
    ON dashboard_sessions(user_id);

-- platform_credentials is queried by (platform, enabled) — table is tiny
-- but index is free and future-proofs
CREATE INDEX IF NOT EXISTS idx_platform_credentials_enabled
    ON platform_credentials(platform, enabled);

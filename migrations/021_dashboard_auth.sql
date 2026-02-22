-- Broodlink v0.7.0 — Dashboard Role-Based Access Control
-- Adds dashboard_users and dashboard_sessions tables for session-based auth.

CREATE TABLE dashboard_users (
    id VARCHAR(36) PRIMARY KEY,
    username VARCHAR(100) NOT NULL UNIQUE,
    password_hash VARCHAR(255) NOT NULL,     -- bcrypt hash
    role VARCHAR(20) NOT NULL DEFAULT 'viewer',  -- viewer, operator, admin
    display_name VARCHAR(255),
    active BOOLEAN DEFAULT true,
    last_login TIMESTAMPTZ,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE dashboard_sessions (
    id VARCHAR(36) PRIMARY KEY,              -- session token
    user_id VARCHAR(36) NOT NULL REFERENCES dashboard_users(id),
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_dashboard_sessions_expires ON dashboard_sessions(expires_at);

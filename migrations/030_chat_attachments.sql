-- Migration 030: Chat attachment metadata and local file storage
-- Postgres only

CREATE TABLE IF NOT EXISTS chat_attachments (
    id              VARCHAR(36)   PRIMARY KEY,
    message_id      BIGINT        NOT NULL REFERENCES chat_messages(id) ON DELETE CASCADE,
    session_id      VARCHAR(36)   NOT NULL REFERENCES chat_sessions(id),
    attachment_type VARCHAR(20)   NOT NULL,
    mime_type       VARCHAR(100),
    file_name       VARCHAR(500),
    file_size_bytes BIGINT,
    file_hash       VARCHAR(64),
    storage_path    TEXT,
    extracted_text  TEXT,
    transcription   TEXT,
    thumbnail_path  TEXT,
    platform_file_id TEXT,
    metadata        JSONB         DEFAULT '{}',
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_chat_attachments_message ON chat_attachments(message_id);
CREATE INDEX IF NOT EXISTS idx_chat_attachments_session ON chat_attachments(session_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_chat_attachments_type    ON chat_attachments(attachment_type);
CREATE INDEX IF NOT EXISTS idx_chat_attachments_hash    ON chat_attachments(file_hash);

-- Broodlink v0.7.1 — Schema Hardening
-- Adds ON DELETE CASCADE to existing FKs, makes chat_reply_queue.task_id nullable,
-- adds missing indexes, and ensures idempotent index creation.

-- ============================================================================
-- 1. ON DELETE CASCADE for existing foreign keys
-- ============================================================================

-- a2a_task_map.internal_id → task_queue(id)
ALTER TABLE a2a_task_map
    DROP CONSTRAINT IF EXISTS a2a_task_map_internal_id_fkey,
    ADD CONSTRAINT a2a_task_map_internal_id_fkey
        FOREIGN KEY (internal_id) REFERENCES task_queue(id) ON DELETE CASCADE;

-- kg_edges.source_id → kg_entities(entity_id)
ALTER TABLE kg_edges
    DROP CONSTRAINT IF EXISTS kg_edges_source_id_fkey,
    ADD CONSTRAINT kg_edges_source_id_fkey
        FOREIGN KEY (source_id) REFERENCES kg_entities(entity_id) ON DELETE CASCADE;

-- kg_edges.target_id → kg_entities(entity_id)
ALTER TABLE kg_edges
    DROP CONSTRAINT IF EXISTS kg_edges_target_id_fkey,
    ADD CONSTRAINT kg_edges_target_id_fkey
        FOREIGN KEY (target_id) REFERENCES kg_entities(entity_id) ON DELETE CASCADE;

-- kg_entity_memories.entity_id → kg_entities(entity_id)
ALTER TABLE kg_entity_memories
    DROP CONSTRAINT IF EXISTS kg_entity_memories_entity_id_fkey,
    ADD CONSTRAINT kg_entity_memories_entity_id_fkey
        FOREIGN KEY (entity_id) REFERENCES kg_entities(entity_id) ON DELETE CASCADE;

-- webhook_log.endpoint_id → webhook_endpoints(id)
ALTER TABLE webhook_log
    DROP CONSTRAINT IF EXISTS webhook_log_endpoint_id_fkey,
    ADD CONSTRAINT webhook_log_endpoint_id_fkey
        FOREIGN KEY (endpoint_id) REFERENCES webhook_endpoints(id) ON DELETE CASCADE;

-- chat_messages.session_id → chat_sessions(id)
ALTER TABLE chat_messages
    DROP CONSTRAINT IF EXISTS chat_messages_session_id_fkey,
    ADD CONSTRAINT chat_messages_session_id_fkey
        FOREIGN KEY (session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE;

-- chat_reply_queue.session_id → chat_sessions(id)
ALTER TABLE chat_reply_queue
    DROP CONSTRAINT IF EXISTS chat_reply_queue_session_id_fkey,
    ADD CONSTRAINT chat_reply_queue_session_id_fkey
        FOREIGN KEY (session_id) REFERENCES chat_sessions(id) ON DELETE CASCADE;

-- dashboard_sessions.user_id → dashboard_users(id)
ALTER TABLE dashboard_sessions
    DROP CONSTRAINT IF EXISTS dashboard_sessions_user_id_fkey,
    ADD CONSTRAINT dashboard_sessions_user_id_fkey
        FOREIGN KEY (user_id) REFERENCES dashboard_users(id) ON DELETE CASCADE;

-- ============================================================================
-- 2. Make chat_reply_queue.task_id nullable
-- ============================================================================

ALTER TABLE chat_reply_queue ALTER COLUMN task_id DROP NOT NULL;

-- Clean up placeholder UUIDs that were used before nullable was available
UPDATE chat_reply_queue
   SET task_id = NULL
 WHERE task_id = '00000000-0000-0000-0000-000000000000';

-- ============================================================================
-- 3. Idempotent indexes (covers 020/021 re-runs + new ones)
-- ============================================================================

CREATE INDEX IF NOT EXISTS idx_formula_registry_enabled
    ON formula_registry(enabled, name);

CREATE INDEX IF NOT EXISTS idx_dashboard_sessions_expires
    ON dashboard_sessions(expires_at);

-- New missing indexes
CREATE INDEX IF NOT EXISTS idx_chat_reply_queue_session
    ON chat_reply_queue(session_id);

CREATE INDEX IF NOT EXISTS idx_dashboard_sessions_user
    ON dashboard_sessions(user_id);

CREATE INDEX IF NOT EXISTS idx_chat_reply_platform_status
    ON chat_reply_queue(platform, status);

CREATE INDEX IF NOT EXISTS idx_webhook_log_type_dir
    ON webhook_log(event_type, direction);

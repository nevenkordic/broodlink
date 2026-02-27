-- v0.11.0: Agent negotiation protocol
-- Allows agents to decline tasks, request context, or suggest redirects

ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS decline_count INT DEFAULT 0;
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS declined_agents JSONB DEFAULT '[]';
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS context_questions JSONB;
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS context_requested_by VARCHAR(100);
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS context_requested_at TIMESTAMPTZ;

CREATE TABLE IF NOT EXISTS task_negotiations (
  id BIGSERIAL PRIMARY KEY,
  task_id VARCHAR(36) NOT NULL,
  agent_id VARCHAR(100) NOT NULL,
  action VARCHAR(30) NOT NULL CHECK (action IN ('declined','context_requested','context_provided','redirected')),
  reason TEXT,
  suggested_agent VARCHAR(100),
  questions JSONB,
  created_at TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_task_negotiations_task ON task_negotiations(task_id);

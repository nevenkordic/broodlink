/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 *
 * This program is free software: you can redistribute it
 * and/or modify it under the terms of the GNU Affero
 * General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your
 * option) any later version.
 *
 * This program is distributed in the hope that it will be
 * useful, but WITHOUT ANY WARRANTY; without even the
 * implied warranty of MERCHANTABILITY or FITNESS FOR A
 * PARTICULAR PURPOSE. See the GNU Affero General Public
 * License for more details.
 *
 * You should have received a copy of the GNU Affero General
 * Public License along with this program. If not, see
 * <https://www.gnu.org/licenses/>.
 */

-- Migration 004: Human-in-the-loop approval gates
-- Database: broodlink_hot (Postgres)

CREATE TABLE IF NOT EXISTS approval_policies (
  id              VARCHAR(36) PRIMARY KEY,
  name            VARCHAR(100) NOT NULL UNIQUE,
  description     TEXT,
  gate_type       VARCHAR(50) NOT NULL,
  conditions      JSONB NOT NULL,
  auto_approve    BOOLEAN DEFAULT false,
  auto_approve_threshold FLOAT DEFAULT 0.8,
  expiry_minutes  INT DEFAULT 60,
  active          BOOLEAN DEFAULT true,
  created_at      TIMESTAMPTZ DEFAULT NOW(),
  updated_at      TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS approval_gates (
  id              VARCHAR(36) PRIMARY KEY,
  trace_id        VARCHAR(36) NOT NULL,
  task_id         VARCHAR(36),
  tool_name       VARCHAR(100),
  agent_id        VARCHAR(100) NOT NULL,
  gate_type       VARCHAR(50) NOT NULL
                  CHECK (gate_type IN ('pre_dispatch','pre_completion','budget','custom')),
  payload         JSONB NOT NULL,
  status          VARCHAR(20) DEFAULT 'pending'
                  CHECK (status IN ('pending','approved','rejected','expired','auto_approved')),
  reason          TEXT,
  review_note     TEXT,
  confidence      FLOAT CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0)),
  policy_id       VARCHAR(36),
  requested_by    VARCHAR(100) NOT NULL,
  reviewed_by     VARCHAR(100),
  expires_at      TIMESTAMPTZ,
  created_at      TIMESTAMPTZ DEFAULT NOW(),
  reviewed_at     TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_approval_gates_pending
  ON approval_gates(status, created_at)
  WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_approval_gates_task
  ON approval_gates(task_id)
  WHERE task_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_approval_gates_agent
  ON approval_gates(agent_id, created_at DESC);

-- Update triggers
DROP TRIGGER IF EXISTS approval_policies_updated_at ON approval_policies;
CREATE TRIGGER approval_policies_updated_at
  BEFORE UPDATE ON approval_policies
  FOR EACH ROW EXECUTE FUNCTION update_updated_at();

-- Add requires_approval and approval_id to task_queue
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS requires_approval BOOLEAN DEFAULT false;
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS approval_id VARCHAR(36);

-- Expand task_queue status CHECK to include new statuses
-- Note: Postgres requires dropping and re-adding named constraints.
-- If constraint name is unknown, this is a safe additive ALTER.
DO $$
BEGIN
  ALTER TABLE task_queue DROP CONSTRAINT IF EXISTS task_queue_status_check;
  ALTER TABLE task_queue ADD CONSTRAINT task_queue_status_check
    CHECK (status IN ('pending','claimed','in_progress','completed','failed','retrying',
                      'awaiting_approval','rejected','expired'));
EXCEPTION WHEN others THEN
  RAISE NOTICE 'Could not update task_queue status constraint: %', SQLERRM;
END $$;

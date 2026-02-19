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

-- Migration 006: Agent-to-agent delegation and task hierarchy
-- Database: broodlink_hot (Postgres)

CREATE TABLE IF NOT EXISTS delegations (
  id               VARCHAR(36) PRIMARY KEY,
  trace_id         VARCHAR(36) NOT NULL,
  parent_task_id   VARCHAR(36),
  from_agent       VARCHAR(100) NOT NULL,
  to_agent         VARCHAR(100) NOT NULL,
  title            VARCHAR(255) NOT NULL,
  description      TEXT,
  status           VARCHAR(20) DEFAULT 'pending'
                   CHECK (status IN ('pending','accepted','rejected','in_progress','completed','failed')),
  result           JSONB,
  created_at       TIMESTAMPTZ DEFAULT NOW(),
  updated_at       TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_delegations_to_agent
  ON delegations(to_agent, status)
  WHERE status IN ('pending','in_progress');

CREATE INDEX IF NOT EXISTS idx_delegations_from_agent
  ON delegations(from_agent, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_delegations_parent
  ON delegations(parent_task_id)
  WHERE parent_task_id IS NOT NULL;

-- Add parent_task_id to task_queue for sub-task hierarchy
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS parent_task_id VARCHAR(36);
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS delegation_id VARCHAR(36);

CREATE INDEX IF NOT EXISTS idx_task_queue_parent
  ON task_queue(parent_task_id)
  WHERE parent_task_id IS NOT NULL;

-- Update trigger
DROP TRIGGER IF EXISTS delegations_updated_at ON delegations;
CREATE TRIGGER delegations_updated_at
  BEFORE UPDATE ON delegations
  FOR EACH ROW EXECUTE FUNCTION update_updated_at();

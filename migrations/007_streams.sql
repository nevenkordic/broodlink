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

-- Migration 007: Real-time streaming and progress tracking
-- Database: broodlink_hot (Postgres)

CREATE TABLE IF NOT EXISTS streams (
  id          VARCHAR(36) PRIMARY KEY,
  agent_id    VARCHAR(100) NOT NULL,
  tool_name   VARCHAR(100),
  task_id     VARCHAR(36),
  status      VARCHAR(20) DEFAULT 'active'
              CHECK (status IN ('active','completed','failed','expired')),
  created_at  TIMESTAMPTZ DEFAULT NOW(),
  closed_at   TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_streams_active
  ON streams(agent_id, status)
  WHERE status = 'active';

CREATE INDEX IF NOT EXISTS idx_streams_task
  ON streams(task_id)
  WHERE task_id IS NOT NULL;

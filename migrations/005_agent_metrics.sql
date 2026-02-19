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

-- Migration 005: Agent performance metrics for smart routing
-- Database: broodlink_hot (Postgres)

CREATE TABLE IF NOT EXISTS agent_metrics (
  agent_id         VARCHAR(100) PRIMARY KEY,
  tasks_completed  INT DEFAULT 0,
  tasks_failed     INT DEFAULT 0,
  avg_duration_ms  INT DEFAULT 0,
  current_load     INT DEFAULT 0,
  success_rate     REAL DEFAULT 1.0,
  last_task_at     TIMESTAMPTZ,
  updated_at       TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_agent_metrics_load
  ON agent_metrics(current_load, success_rate DESC);

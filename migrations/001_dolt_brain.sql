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

-- Migration 001: Dolt brain tables
-- Database: agent_ledger (existing, at 127.0.0.1:3307)
-- All: CREATE TABLE IF NOT EXISTS. No DROP. No destructive ALTER.

-- Additive alteration to existing table
ALTER TABLE agent_memory
  ADD COLUMN IF NOT EXISTS embedding_ref VARCHAR(36) NULL
  COMMENT 'Qdrant point ID';

CREATE TABLE IF NOT EXISTS decisions (
  id           BIGINT AUTO_INCREMENT PRIMARY KEY,
  trace_id     VARCHAR(36) NOT NULL,
  agent_id     VARCHAR(100) NOT NULL,
  decision     TEXT NOT NULL,
  reasoning    TEXT,
  alternatives JSON,
  outcome      TEXT,
  created_at   TIMESTAMP DEFAULT CURRENT_TIMESTAMP
  -- APPEND ONLY: no UPDATE, no DELETE
);

CREATE TABLE IF NOT EXISTS agent_profiles (
  agent_id      VARCHAR(100) PRIMARY KEY,
  display_name  VARCHAR(255) NOT NULL,
  role          ENUM('strategist','worker',
                     'researcher','monitor') NOT NULL,
  transport     ENUM('mcp','pymysql','api','nats') NOT NULL,
  cost_tier     ENUM('low','medium','high') NOT NULL,
  preferred_formula_types JSON,
  capabilities            JSON,
  active        BOOLEAN DEFAULT true,
  last_seen     TIMESTAMP NULL,
  created_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
                ON UPDATE CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS beads_issues (
  bead_id      VARCHAR(20) PRIMARY KEY,
  title        VARCHAR(255) NOT NULL,
  description  TEXT,
  status       VARCHAR(50),
  assignee     VARCHAR(100),
  convoy_id    VARCHAR(50),
  dependencies JSON,
  formula      VARCHAR(100),
  created_at   TIMESTAMP,
  updated_at   TIMESTAMP,
  synced_at    TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS daily_summary (
  id               BIGINT AUTO_INCREMENT PRIMARY KEY,
  summary_date     DATE NOT NULL UNIQUE,
  summary_text     TEXT,
  agent_activity   JSON,
  tasks_completed  INT DEFAULT 0,
  decisions_made   INT DEFAULT 0,
  memories_stored  INT DEFAULT 0,
  created_at       TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

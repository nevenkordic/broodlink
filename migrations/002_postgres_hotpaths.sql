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

-- Migration 002: Postgres hot path tables
-- Database: broodlink_hot (new, at 127.0.0.1:5432)

CREATE TABLE IF NOT EXISTS task_queue (
  id             VARCHAR(36) PRIMARY KEY,
  trace_id       VARCHAR(36) NOT NULL,
  title          VARCHAR(255) NOT NULL,
  description    TEXT,
  priority       INT DEFAULT 0,
  status         VARCHAR(20) DEFAULT 'pending'
                 CHECK (status IN ('pending','claimed',
                   'in_progress','completed',
                   'failed','retrying')),
  assigned_agent VARCHAR(100),
  formula_name   VARCHAR(100),
  convoy_id      VARCHAR(50),
  dependencies   JSONB,
  retry_count    INT DEFAULT 0,
  max_retries    INT DEFAULT 3,
  claimed_at     TIMESTAMPTZ NULL,
  completed_at   TIMESTAMPTZ NULL,
  created_at     TIMESTAMPTZ DEFAULT NOW(),
  updated_at     TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS messages (
  id          VARCHAR(36) PRIMARY KEY,
  trace_id    VARCHAR(36) NOT NULL,
  thread_id   VARCHAR(36),
  sender      VARCHAR(100) NOT NULL,
  recipient   VARCHAR(100) NOT NULL,
  subject     VARCHAR(255),
  body        TEXT NOT NULL,
  status      VARCHAR(20) DEFAULT 'unread'
              CHECK (status IN ('unread','read','actioned')),
  created_at  TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS work_log (
  id            BIGSERIAL PRIMARY KEY,
  trace_id      VARCHAR(36) NOT NULL,
  agent_id      VARCHAR(100) NOT NULL,
  action        VARCHAR(100) NOT NULL,
  details       TEXT,
  files_changed JSONB,
  created_at    TIMESTAMPTZ DEFAULT NOW()
  -- APPEND ONLY: no UPDATE, no DELETE
);

CREATE TABLE IF NOT EXISTS audit_log (
  id             BIGSERIAL PRIMARY KEY,
  trace_id       VARCHAR(36) NOT NULL,
  agent_id       VARCHAR(100) NOT NULL,
  service        VARCHAR(100) NOT NULL,
  operation      VARCHAR(100) NOT NULL,
  parameters     JSONB,
  result_status  VARCHAR(20) NOT NULL
                 CHECK (result_status IN
                   ('ok','error','timeout')),
  result_summary TEXT,
  duration_ms    INT,
  created_at     TIMESTAMPTZ DEFAULT NOW()
  -- APPEND ONLY: no UPDATE, no DELETE
);

CREATE TABLE IF NOT EXISTS outbox (
  id           BIGSERIAL PRIMARY KEY,
  trace_id     VARCHAR(36) NOT NULL,
  operation    VARCHAR(50) NOT NULL,
  payload      JSONB NOT NULL,
  status       VARCHAR(20) DEFAULT 'pending'
               CHECK (status IN
                 ('pending','processing','done','failed')),
  attempts     INT DEFAULT 0,
  created_at   TIMESTAMPTZ DEFAULT NOW(),
  processed_at TIMESTAMPTZ NULL
);

CREATE TABLE IF NOT EXISTS residency_log (
  id         BIGSERIAL PRIMARY KEY,
  trace_id   VARCHAR(36) NOT NULL,
  region     VARCHAR(50) NOT NULL,
  profile    VARCHAR(20) NOT NULL,
  services   JSONB NOT NULL,
  checked_at TIMESTAMPTZ DEFAULT NOW()
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_task_queue_status
  ON task_queue(status, priority DESC, created_at)
  WHERE status IN ('pending','claimed');

CREATE INDEX IF NOT EXISTS idx_messages_recipient
  ON messages(recipient, status)
  WHERE status = 'unread';

CREATE INDEX IF NOT EXISTS idx_work_log_agent
  ON work_log(agent_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_audit_trace
  ON audit_log(trace_id);

CREATE INDEX IF NOT EXISTS idx_audit_agent_op
  ON audit_log(agent_id, operation, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_outbox_pending
  ON outbox(status, created_at)
  WHERE status = 'pending';

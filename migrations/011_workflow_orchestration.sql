-- Broodlink — Multi-agent AI orchestration
-- Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- 011: Workflow orchestration — sequential formula step chaining

CREATE TABLE IF NOT EXISTS workflow_runs (
  id              VARCHAR(36) PRIMARY KEY,
  convoy_id       VARCHAR(50) NOT NULL UNIQUE,
  formula_name    VARCHAR(100) NOT NULL,
  status          VARCHAR(20) DEFAULT 'pending',
  current_step    INT DEFAULT 0,
  total_steps     INT NOT NULL DEFAULT 0,
  params          JSONB NOT NULL DEFAULT '{}',
  step_results    JSONB NOT NULL DEFAULT '{}',
  started_by      VARCHAR(100) NOT NULL,
  current_task_id VARCHAR(36),
  created_at      TIMESTAMPTZ DEFAULT NOW(),
  updated_at      TIMESTAMPTZ DEFAULT NOW(),
  completed_at    TIMESTAMPTZ NULL
);

ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS workflow_run_id VARCHAR(36);
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS step_index INT;
ALTER TABLE task_queue ADD COLUMN IF NOT EXISTS step_name VARCHAR(100);

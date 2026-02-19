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

-- Migration 008: Guardrail policies and violation tracking
-- Database: broodlink_hot (Postgres)

CREATE TABLE IF NOT EXISTS guardrail_policies (
  id              BIGSERIAL PRIMARY KEY,
  name            VARCHAR(100) NOT NULL UNIQUE,
  rule_type       VARCHAR(30) NOT NULL
                  CHECK (rule_type IN ('tool_block','rate_override','content_filter','scope_limit')),
  config          JSONB NOT NULL,
  enabled         BOOLEAN DEFAULT true,
  created_at      TIMESTAMPTZ DEFAULT NOW(),
  updated_at      TIMESTAMPTZ DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS guardrail_violations (
  id              BIGSERIAL PRIMARY KEY,
  trace_id        VARCHAR(36) NOT NULL,
  agent_id        VARCHAR(100) NOT NULL,
  policy_name     VARCHAR(100) NOT NULL,
  tool_name       VARCHAR(100),
  details         TEXT,
  created_at      TIMESTAMPTZ DEFAULT NOW()
  -- APPEND ONLY: no updates or deletes
);

CREATE INDEX IF NOT EXISTS idx_guardrail_violations_agent
  ON guardrail_violations(agent_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_guardrail_violations_policy
  ON guardrail_violations(policy_name, created_at DESC);

-- Update trigger for policies
DROP TRIGGER IF EXISTS guardrail_policies_updated_at ON guardrail_policies;
CREATE TRIGGER guardrail_policies_updated_at
  BEFORE UPDATE ON guardrail_policies
  FOR EACH ROW EXECUTE FUNCTION update_updated_at();
